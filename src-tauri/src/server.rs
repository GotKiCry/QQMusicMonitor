use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

use crate::song_info::SongInfo;
use crate::smtc;

#[derive(serde::Deserialize)]
struct WsCommand {
    action: String,
}

/// 服务端状态持有 watch::Receiver
struct AppState {
    receiver: watch::Receiver<Arc<SongInfo>>,
}

pub async fn start_server(port: u16, receiver: watch::Receiver<Arc<SongInfo>>) {
    let state = Arc::new(AppState { receiver });

    let app = Router::new()
        .route("/api/current", get(get_current))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    if let Ok(listener) = tokio::net::TcpListener::bind(&addr).await {
        axum::serve(listener, app).await.ok();
    } else {
        eprintln!("❌ 无法绑定端口 {}", port);
    }
}

async fn get_current(State(state): State<Arc<AppState>>) -> Json<SongInfo> {
    let current = state.receiver.borrow().as_ref().clone();
    Json(current)
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> axum::response::Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.receiver.clone();

    let initial_json = {
        let initial = rx.borrow();
        serde_json::to_string(initial.as_ref())
    };
    if let Ok(json) = initial_json {
        if socket.send(Message::Text(json)).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            result = rx.changed() => {
                if result.is_ok() {
                    let current_json = {
                        let current = rx.borrow();
                        serde_json::to_string(current.as_ref())
                    };
                    if let Ok(json) = current_json {
                        if socket.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(cmd) = serde_json::from_str::<WsCommand>(&text) {
                            let _ = match cmd.action.as_str() {
                                "play_pause" => smtc::try_toggle_play_pause().await,
                                "next" => smtc::try_skip_next().await,
                                "previous" => smtc::try_skip_previous().await,
                                _ => Ok(()),
                            };
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}
