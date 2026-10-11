use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use bramble_core::PROTOCOL_VERSION;
use bramble_core::protocol::{ClientMsg, ServerMsg};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};

use super::{Shared, api, now_unix};

pub async fn serve(bind: &str, shared: Arc<Shared>, web_dir: &Path) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, web_dir = %web_dir.display(), "node listening");
    axum::serve(listener, router(shared, web_dir)).await?;
    Ok(())
}

/// API routes, plus the browser app when it has been built (`web_dir/index.html`).
pub fn router(shared: Arc<Shared>, web_dir: &Path) -> Router {
    let api = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/state", get(state))
        .route("/api/entries", post(api::post_entry))
        .route("/api/rtp/payments", post(api::post_rtp_payment))
        .route("/api/ach/payments", post(api::post_ach_payment))
        .route("/api/ach/pending", get(api::ach_pending))
        .route("/api/statements/card-network", get(api::statement))
        .route("/api/files/{name}", get(api::file))
        .route("/api/keys/node.pem", get(api::public_key))
        .route("/ws", get(ws))
        .with_state(shared);
    with_web_app(api, web_dir)
}

/// Serves the browser app from `web_dir` for any path the API doesn't claim, when it has
/// been built (`web_dir/index.html`). Shared by banks and the operator.
pub fn with_web_app(api: Router, web_dir: &Path) -> Router {
    let index = web_dir.join("index.html");
    if index.exists() {
        api.fallback_service(ServeDir::new(web_dir).fallback(ServeFile::new(index)))
    } else {
        tracing::warn!(web_dir = %web_dir.display(), "no web app found; serving the API only");
        api
    }
}

async fn state(State(shared): State<Arc<Shared>>) -> Json<ServerMsg> {
    Json(shared.world.lock().await.snapshot(None, now_unix()))
}

async fn ws(upgrade: WebSocketUpgrade, State(shared): State<Arc<Shared>>) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| client_session(socket, shared))
}

async fn client_session(socket: WebSocket, shared: Arc<Shared>) {
    let (mut sink, mut stream) = socket.split();
    let mut updates = shared.updates.subscribe();

    let hello = shared.world.lock().await.snapshot(None, now_unix());
    if send(&mut sink, &hello).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            update = updates.recv() => match update {
                Ok(msg) => {
                    if send(&mut sink, &msg).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "client lagged; sending fresh snapshot");
                    let snapshot = shared.world.lock().await.snapshot(None, now_unix());
                    if send(&mut sink, &snapshot).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => handle_client_msg(&shared, text.as_str()).await,
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::warn!(%error, "websocket error");
                    break;
                }
            },
        }
    }
}

async fn handle_client_msg(shared: &Shared, text: &str) {
    let msg = match serde_json::from_str::<ClientMsg>(text) {
        Ok(msg) => msg,
        Err(error) => {
            tracing::warn!(%error, "rejected malformed client message");
            return;
        }
    };
    match msg {
        ClientMsg::Observe { v, observation } => {
            if v != PROTOCOL_VERSION {
                tracing::warn!(
                    v,
                    expected = PROTOCOL_VERSION,
                    "rejected client protocol version"
                );
                return;
            }
            tracing::info!(cmd = %observation.cmd, exit_code = observation.exit_code, "lab observation");
            let mut world = shared.world.lock().await;
            match world.observe(&observation, now_unix()) {
                Ok(Some(update)) => {
                    let _ = shared.updates.send(update);
                }
                Ok(None) => {}
                Err(error) => tracing::error!(%error, "failed to apply observation"),
            }
        }
    }
}

async fn send<S>(sink: &mut S, msg: &ServerMsg) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    let json = serde_json::to_string(msg).map_err(|_| ())?;
    sink.send(Message::Text(json.into())).await.map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bramble_core::session::Observation;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use super::*;
    use crate::node::test_support::{shared, world};

    /// The M3 definition of done: every client sees a change within a second.
    const SYNC_DEADLINE: Duration = Duration::from_secs(1);

    async fn start(web_dir: &Path) -> std::net::SocketAddr {
        let shared = shared("pet-a", 1, world());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = router(shared, web_dir);
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

    async fn connect(addr: std::net::SocketAddr) -> Client {
        let (client, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("connect");
        client
    }

    async fn next_state(client: &mut Client) -> ServerMsg {
        loop {
            let msg = tokio::time::timeout(SYNC_DEADLINE, client.next())
                .await
                .expect("update within the sync deadline")
                .expect("stream open")
                .expect("ws message");
            if let WsMessage::Text(text) = msg {
                return serde_json::from_str(text.as_str()).expect("server message");
            }
        }
    }

    fn step_index(msg: &ServerMsg) -> usize {
        let ServerMsg::State { lesson, .. } = msg else {
            panic!("a bank sends state messages");
        };
        lesson.as_ref().expect("lesson").step_index
    }

    async fn http(addr: std::net::SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.expect("tcp");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        response
    }

    #[tokio::test]
    async fn server_observation_from_one_client_reaches_every_client_within_a_second() {
        let addr = start(Path::new("/nonexistent")).await;
        let mut tui = connect(addr).await;
        let mut browser = connect(addr).await;
        assert_eq!(step_index(&next_state(&mut tui).await), 0, "hello snapshot");
        assert_eq!(
            step_index(&next_state(&mut browser).await),
            0,
            "hello snapshot"
        );

        let observe = ClientMsg::Observe {
            v: PROTOCOL_VERSION,
            observation: Observation {
                cmd: "bramble next".into(),
                exit_code: 0,
                cwd: "/home/learner/lab".into(),
                output: String::new(),
            },
        };
        let json = serde_json::to_string(&observe).expect("encode");
        tui.send(WsMessage::Text(json.into())).await.expect("send");

        assert_eq!(
            step_index(&next_state(&mut browser).await),
            1,
            "browser saw the TUI's step"
        );
        assert_eq!(step_index(&next_state(&mut tui).await), 1);
    }

    #[tokio::test]
    async fn server_api_payment_updates_every_clients_ledger_view() {
        let addr = start(Path::new("/nonexistent")).await;
        let mut browser = connect(addr).await;
        next_state(&mut browser).await;

        let body = r#"{"memo":"Pay Alex","postings":[{"account":"customer:you","amount_minor":1200},{"account":"customer:alex","amount_minor":-1200}]}"#;
        let response = http(
            addr,
            &format!(
                "POST /api/entries HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 201"), "{response}");

        let ServerMsg::State { ledger, .. } = next_state(&mut browser).await else {
            panic!("a bank sends state messages");
        };
        let ledger = ledger.expect("ledger view");
        assert_eq!(ledger.recent_entries[0].memo, "Pay Alex");
        let alex = ledger
            .accounts
            .iter()
            .find(|a| a.account == "customer:alex")
            .expect("alex");
        assert_eq!(alex.balance_minor, -1200);
    }

    fn post_json(path: &str, key: &str, body: &str) -> String {
        format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nIdempotency-Key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn server_ach_payment_is_queued_and_shows_in_the_pending_file() {
        let addr = start(Path::new("/nonexistent")).await;
        let get_pending = "GET /api/ach/pending HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
        assert!(
            http(addr, get_pending).await.starts_with("HTTP/1.1 204"),
            "nothing queued yet"
        );

        let body = r#"{"from_account":"customer:you","to_bank":"pet-b","to_account":"customer:alex","amount_minor":1300,"memo":"Rent share","receiver_name":"Alex Kim"}"#;
        let first = http(addr, &post_json("/api/ach/payments", "rent-1", body)).await;
        assert!(first.starts_with("HTTP/1.1 201"), "{first}");
        let again = http(addr, &post_json("/api/ach/payments", "rent-1", body)).await;
        assert!(again.starts_with("HTTP/1.1 200"), "replay: {again}");

        let pending = http(addr, get_pending).await;
        assert!(pending.starts_with("HTTP/1.1 200"), "{pending}");
        let file = pending.split("\r\n\r\n").nth(1).expect("body");
        let parsed = bramble_ledger::nacha::parse(file).expect("a valid Nacha file");
        assert_eq!(parsed.entry_count(), 1, "the retry queued nothing new");
        let entry = &parsed.batches[0].entries[0];
        assert_eq!(
            (entry.account.as_str(), entry.amount_minor),
            ("alex", 1_300)
        );
        assert_eq!(entry.individual_name, "ALEX KIM");
    }

    #[tokio::test]
    async fn server_ach_payment_updates_every_clients_network_view_within_a_second() {
        let addr = start(Path::new("/nonexistent")).await;
        let mut browser = connect(addr).await;
        let ServerMsg::State { network, .. } = next_state(&mut browser).await else {
            panic!("a bank sends state messages");
        };
        let network = network.expect("network view");
        assert_eq!((network.node_id.as_str(), network.ach_queued), ("pet-a", 0));
        assert_eq!(
            network.reserves_minor,
            bramble_ledger::rails::OPENING_RESERVES
        );

        let body = r#"{"from_account":"customer:you","to_bank":"pet-b","to_account":"customer:alex","amount_minor":1300}"#;
        let response = http(addr, &post_json("/api/ach/payments", "sync-1", body)).await;
        assert!(response.starts_with("HTTP/1.1 201"), "{response}");

        let ServerMsg::State { network, .. } = next_state(&mut browser).await else {
            panic!("a bank sends state messages");
        };
        let network = network.expect("network view");
        assert_eq!((network.ach_queued, network.ach_queued_minor), (1, 1_300));
    }

    #[tokio::test]
    async fn server_ach_payment_refuses_unknown_banks_and_overdrafts() {
        let addr = start(Path::new("/nonexistent")).await;
        for (body, why) in [
            (
                r#"{"from_account":"customer:you","to_bank":"pet-z","to_account":"customer:alex","amount_minor":100}"#,
                "not a bank",
            ),
            (
                r#"{"from_account":"customer:you","to_bank":"pet-b","to_account":"customer:alex","amount_minor":99999999}"#,
                "insufficient funds",
            ),
        ] {
            let response = http(addr, &post_json("/api/ach/payments", why, body)).await;
            assert!(response.starts_with("HTTP/1.1 422"), "{why}: {response}");
            assert!(response.contains(why), "{response}");
        }
    }

    #[tokio::test]
    async fn server_serves_the_web_app_with_spa_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("index.html"), "<h1>bramble</h1>").expect("index");
        let addr = start(dir.path()).await;
        for path in ["/", "/some/client/route"] {
            let response = http(
                addr,
                &format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
            )
            .await;
            assert!(response.starts_with("HTTP/1.1 200"), "{path}: {response}");
            assert!(response.ends_with("<h1>bramble</h1>"), "{path}");
        }
        let api = http(
            addr,
            "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(api.ends_with("ok"), "API routes win over the fallback");
    }
}
