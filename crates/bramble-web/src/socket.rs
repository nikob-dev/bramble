//! WebSocket link to the node, written against the raw browser API (web-sys) so the
//! moving parts are visible: open → message* → close → reconnect.

use bramble_core::protocol::ServerMsg;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{CloseEvent, MessageEvent, WebSocket};

const RECONNECT_MS: u64 = 2_000;

/// Connects to `/ws` on the page's own host and keeps reconnecting forever.
pub fn connect(on_message: WriteSignal<Option<ServerMsg>>, on_status: WriteSignal<Status>) {
    let Some(window) = web_sys::window() else {
        on_status.set(Status::Offline("no browser window".into()));
        return;
    };
    let location = window.location();
    let scheme = if location.protocol().as_deref() == Ok("https:") {
        "wss"
    } else {
        "ws"
    };
    let host = location.host().unwrap_or_default();
    let socket = match WebSocket::new(&format!("{scheme}://{host}/ws")) {
        Ok(socket) => socket,
        Err(error) => {
            on_status.set(Status::Offline(format!("{error:?}")));
            schedule_reconnect(on_message, on_status);
            return;
        }
    };

    let onopen = Closure::<dyn FnMut()>::new(move || on_status.set(Status::Live));
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));

    let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let Some(text) = event.data().as_string() else {
            return;
        };
        match serde_json::from_str::<ServerMsg>(&text) {
            Ok(msg) => on_message.set(Some(msg)),
            // Unknown or newer messages are skipped, never fatal: old tabs keep working.
            Err(error) => leptos::logging::warn!("skipping message from node: {error}"),
        }
    });
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    let onclose = Closure::<dyn FnMut(CloseEvent)>::new(move |_event: CloseEvent| {
        on_status.set(Status::Offline("connection lost, retrying".into()));
        schedule_reconnect(on_message, on_status);
    });
    socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));

    // The socket owns these callbacks for the rest of its life; JS frees them with it.
    onopen.forget();
    onmessage.forget();
    onclose.forget();
}

fn schedule_reconnect(on_message: WriteSignal<Option<ServerMsg>>, on_status: WriteSignal<Status>) {
    set_timeout(
        move || connect(on_message, on_status),
        std::time::Duration::from_millis(RECONNECT_MS),
    );
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Connecting,
    Live,
    Offline(String),
}
