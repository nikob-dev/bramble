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
        .route("/api/statements/card-network", get(api::statement))
        .route("/api/files/{name}", get(api::file))
        .route("/api/keys/node.pem", get(api::public_key))
        .route("/ws", get(ws))
        .with_state(shared);
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
    use crate::node::test_support::world;

    /// The M3 definition of done: every client sees a change within a second.
    const SYNC_DEADLINE: Duration = Duration::from_secs(1);

    async fn start(web_dir: &Path) -> std::net::SocketAddr {
        let (updates, _) = broadcast::channel(64);
        let shared = Arc::new(Shared {
            world: tokio::sync::Mutex::new(world()),
            updates,
        });
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
        let ServerMsg::State { lesson, .. } = msg;
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

        let ServerMsg::State { ledger, .. } = next_state(&mut browser).await;
        let ledger = ledger.expect("ledger view");
        assert_eq!(ledger.recent_entries[0].memo, "Pay Alex");
        let alex = ledger
            .accounts
            .iter()
            .find(|a| a.account == "customer:alex")
            .expect("alex");
        assert_eq!(alex.balance_minor, -1200);
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
