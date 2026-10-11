//! WebSocket link to the node, with automatic reconnects.

use std::time::Duration;

use bramble_core::protocol::{ClientMsg, ServerMsg};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::Message;

pub enum NodeEvent {
    Connected,
    Disconnected(String),
    Message(ServerMsg),
}

pub async fn run(
    url: String,
    events: UnboundedSender<NodeEvent>,
    mut outgoing: UnboundedReceiver<ClientMsg>,
) {
    loop {
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((socket, _)) => {
                if events.send(NodeEvent::Connected).is_err() {
                    return;
                }
                let (mut sink, mut stream) = socket.split();
                let reason = loop {
                    tokio::select! {
                        incoming = stream.next() => match incoming {
                            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMsg>(text.as_str()) {
                                Ok(msg) => {
                                    if events.send(NodeEvent::Message(msg)).is_err() {
                                        return;
                                    }
                                }
                                Err(error) => break format!("bad message from node: {error}"),
                            },
                            Some(Ok(Message::Close(_))) | None => break "node closed the connection".to_owned(),
                            Some(Ok(_)) => {}
                            Some(Err(error)) => break error.to_string(),
                        },
                        msg = outgoing.recv() => {
                            let Some(msg) = msg else { return };
                            let json = match serde_json::to_string(&msg) {
                                Ok(json) => json,
                                Err(error) => break format!("could not encode message: {error}"),
                            };
                            if let Err(error) = sink.send(Message::Text(json.into())).await {
                                break error.to_string();
                            }
                        }
                    }
                };
                if events.send(NodeEvent::Disconnected(reason)).is_err() {
                    return;
                }
            }
            Err(error) => {
                if events
                    .send(NodeEvent::Disconnected(error.to_string()))
                    .is_err()
                {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
