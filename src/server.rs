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

/// 服务端状态持有 watch::Receiver
struct AppState {
    receiver: watch::Receiver<SongInfo>,
}

pub async fn start_server(port: u16, receiver: watch::Receiver<SongInfo>) {
    let state = Arc::new(AppState { receiver });

    let app = Router::new()
        .route("/api/current", get(get_current))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    if let Ok(listener) = tokio::net::TcpListener::bind(&addr).await {
        println!("🚀 本地同步服务已启动: http://{}", addr);
        println!("📡 WebSocket 接口: ws://{}/ws", addr);
        println!("📄 当前状态接口: http://{}/api/current", addr);
        axum::serve(listener, app).await.ok();
    } else {
        eprintln!("❌ 无法绑定端口 {}", port);
    }
}

async fn get_current(State(state): State<Arc<AppState>>) -> Json<SongInfo> {
    let current = state.receiver.borrow().clone();
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

    // 首次连接时，发送一次当前数据
    let initial = rx.borrow().clone();
    // axum::extract::ws::Message::Text / axum::extract::ws::Message::Text(...)
    if let Ok(json) = serde_json::to_string(&initial) {
        if socket.send(Message::Text(json)).await.is_err() {
            return;
        }
    }

    loop {
        // 等待数据更新或 socket 关闭
        tokio::select! {
            result = rx.changed() => {
                if result.is_ok() {
                    let current = rx.borrow().clone();
                    if let Ok(json) = serde_json::to_string(&current) {
                        // Axum 0.7 需要使用 Message::Text(String)
                        if socket.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                } else {
                    break; // sender 已经断开
                }
            }
            msg = socket.recv() => {
                // 如果客户端发送消息或断开连接
                if let Some(Ok(Message::Close(_))) | None = msg {
                    break;
                }
            }
        }
    }
}
