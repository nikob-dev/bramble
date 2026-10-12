use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use bramble_core::PROTOCOL_VERSION;
use bramble_core::protocol::{ClientMsg, ServerMsg};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;

use super::Shared;

pub async fn serve(bind: &str, shared: Arc<Shared>) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/state", get(state))
        .route("/ws", get(ws))
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "node listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn state(State(shared): State<Arc<Shared>>) -> Json<ServerMsg> {
    Json(shared.world.lock().await.snapshot(None))
}

async fn ws(upgrade: WebSocketUpgrade, State(shared): State<Arc<Shared>>) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| client_session(socket, shared))
}

async fn client_session(socket: WebSocket, shared: Arc<Shared>) {
    let (mut sink, mut stream) = socket.split();
    let mut updates = shared.updates.subscribe();

    let hello = shared.world.lock().await.snapshot(None);
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
                    let snapshot = shared.world.lock().await.snapshot(None);
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
            match world.observe(&observation) {
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
