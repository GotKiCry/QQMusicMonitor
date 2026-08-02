use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::StatusCode,
    response::Response,
    routing::get,
    Json, Router,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::watch;
use tower_http::cors::CorsLayer;

use crate::song_info::SongInfo;
use crate::smtc;

#[derive(serde::Deserialize)]
struct WsCommand {
    action: String,
}

/// 允许代理的图片 CDN 主机（白名单，防止 SSRF 滥用本地服务）
const PROXY_ALLOWED_HOSTS: &[&str] = &["y.gtimg.cn", "imgcache.qq.com"];

fn is_allowed_proxy_host(url: &reqwest::Url) -> bool {
    match url.host_str() {
        Some(host) => PROXY_ALLOWED_HOSTS
            .iter()
            .any(|allowed| host == *allowed || host.ends_with(&format!(".{}", allowed))),
        None => false,
    }
}

/// 图片代理：QQ 音乐 CDN 不返回 CORS 头，壁纸/前端无法直接读取像素
/// 做主题色提取；本端点在服务端拉取图片并以 CORS 宽松头返回。
async fn proxy_image(Query(params): Query<HashMap<String, String>>) -> Response {
    let Some(url_str) = params.get("url") else {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("missing url"))
            .unwrap();
    };
    let Ok(url) = url_str.parse::<reqwest::Url>() else {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from("invalid url"))
            .unwrap();
    };
    if url.scheme() != "https" || !is_allowed_proxy_host(&url) {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::from("host not allowed"))
            .unwrap();
    }

    let Ok(resp) = reqwest::Client::new()
        .get(url)
        .header("Referer", "https://y.qq.com/")
        .send()
        .await
    else {
        return Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from("fetch failed"))
            .unwrap();
    };

    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();

    match resp.bytes().await {
        Ok(bytes) => Response::builder()
            .header("Access-Control-Allow-Origin", "*")
            .header("Content-Type", ctype)
            .header("Cache-Control", "public, max-age=86400")
            .body(Body::from(bytes))
            .unwrap(),
        Err(_) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from("read failed"))
            .unwrap(),
    }
}

/// 服务端状态持有 watch::Receiver
struct AppState {
    receiver: watch::Receiver<Arc<SongInfo>>,
}

pub async fn start_server(port: u16, receiver: watch::Receiver<Arc<SongInfo>>) {
    let state = Arc::new(AppState { receiver });

    let app = Router::new()
        .route("/api/current", get(get_current))
        .route("/api/proxy_image", get(proxy_image))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_host_whitelist() {
        let ok = "https://y.gtimg.cn/music/photo_new/T002R800x800M000002JsVIJ31j7qR.jpg?max_age=2592000";
        let ok2 = "https://imgcache.qq.com/music/a.jpg";
        let sub = "https://cdn.y.gtimg.cn/music/b.jpg";
        let bad_http = "http://y.gtimg.cn/music/c.jpg";
        let bad_host = "https://evil.com/x.jpg";
        let bad_ssrf = "https://y.gtimg.cn.evil.com/x.jpg";

        assert!(is_allowed_proxy_host(&ok.parse().unwrap()));
        assert!(is_allowed_proxy_host(&ok2.parse().unwrap()));
        assert!(is_allowed_proxy_host(&sub.parse().unwrap()));
        // scheme 检查在 handler 中与 host 检查共同生效
        assert!(bad_http.parse::<reqwest::Url>().unwrap().scheme() == "http");
        assert!(!is_allowed_proxy_host(&bad_host.parse().unwrap()));
        assert!(!is_allowed_proxy_host(&bad_ssrf.parse().unwrap()));
    }

    /// 真实网络集成测试：通过代理拉取 QQ CDN 封面并验证 CORS 头
    #[tokio::test]
    async fn proxy_image_fetches_cdn_cover() {
        let app = Router::new().route("/api/proxy_image", get(proxy_image));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let target = reqwest::Url::parse_with_params(
            "https://y.gtimg.cn/music/photo_new/T002R800x800M000002JsVIJ31j7qR.jpg",
            &[("max_age", "2592000")],
        )
        .unwrap();
        let url = reqwest::Url::parse_with_params(
            &format!("http://{}/api/proxy_image", addr),
            &[("url", target.as_str())],
        )
        .unwrap();

        let resp = reqwest::get(url).await.expect("proxy request failed");
        assert_eq!(resp.status(), 200, "proxy should return 200");
        let acao = resp
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert_eq!(acao, "*");
        let bytes = resp.bytes().await.unwrap();
        assert!(bytes.len() > 1000, "cover image should be >1KB, got {}", bytes.len());
    }
}
