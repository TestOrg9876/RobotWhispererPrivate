//! Loopback WebSocket server hosting both planes.
//!
//! Two roles share one listener, distinguished by request path:
//!   * `/ingest` — binary frames out to the decoder Web Worker (hot path).
//!   * `/rpc`    — JSON control plane, replacing Tauri's `invoke`.
//!
//! ## Why this is authenticated
//!
//! A loopback WebSocket is reachable by any local process *and by any website
//! the user happens to have open* — browsers do not apply CORS to
//! `ws://127.0.0.1:<port>`. That was already true of the ingest socket before
//! this port, where it leaked live robot telemetry. Moving the command surface
//! onto the same socket would widen that from reading data to driving the
//! robot, so both planes now require a per-launch token that only the Electron
//! shell is told, plus an `Origin` check.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

use crate::rpc::{self, RpcRequest};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Ingest,
    Rpc,
}

/// Origins permitted to open a socket. The Electron renderer is served from a
/// custom `app://` scheme; `http://localhost` covers `vite dev`. Anything else
/// — notably a real website — is refused.
fn origin_allowed(origin: Option<&str>) -> bool {
    match origin {
        // Web Workers and non-browser clients send no Origin.
        None => true,
        Some(value) => {
            value.starts_with("app://")
                || value.starts_with("http://localhost:")
                || value.starts_with("http://127.0.0.1:")
        }
    }
}

/// Length-independent comparison so the token cannot be recovered a byte at a
/// time by timing repeated connections.
fn token_matches(expected: &str, presented: &str) -> bool {
    if expected.len() != presented.len() {
        return false;
    }
    expected
        .bytes()
        .zip(presented.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn query_param(uri: &str, key: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

pub async fn serve(
    state: Arc<AppState>,
    token: String,
) -> std::io::Result<(u16, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    tracing::info!(port, "daemon listening on loopback");

    let handle = tokio::spawn(async move {
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::warn!(?err, "accept failed");
                    continue;
                }
            };
            let state = state.clone();
            let token = token.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_connection(stream, state, token).await {
                    tracing::debug!(?err, "connection ended");
                }
            });
        }
    });

    Ok((port, handle))
}

// The handshake callback's error type is `ErrorResponse`, whose size is fixed
// by tungstenite's signature — it cannot be boxed without changing their API.
#[allow(clippy::result_large_err)]
async fn handle_connection(
    stream: TcpStream,
    state: Arc<AppState>,
    token: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Nagle would batch small frames and add latency to a stream whose whole
    // point is freshness.
    let _ = stream.set_nodelay(true);

    let mut role = None;
    let ws = tokio_tungstenite::accept_hdr_async(stream, |req: &Request, resp: Response| {
        let uri = req.uri().to_string();
        let origin = req
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok());

        if !origin_allowed(origin) {
            tracing::warn!(?origin, "rejected connection from disallowed origin");
            return Err(reject(StatusCode::FORBIDDEN, "origin not allowed"));
        }

        let presented = query_param(&uri, "token").unwrap_or_default();
        if !token_matches(&token, &presented) {
            tracing::warn!("rejected connection with bad token");
            return Err(reject(StatusCode::UNAUTHORIZED, "bad token"));
        }

        let path = uri.split('?').next().unwrap_or_default().to_string();
        role = match path.as_str() {
            "/ingest" => Some(Role::Ingest),
            "/rpc" => Some(Role::Rpc),
            _ => return Err(reject(StatusCode::NOT_FOUND, "unknown path")),
        };
        Ok(resp)
    })
    .await?;

    match role {
        Some(Role::Ingest) => serve_ingest(ws, state).await,
        Some(Role::Rpc) => serve_rpc(ws, state).await,
        None => Ok(()),
    }
}

fn reject(status: StatusCode, body: &str) -> ErrorResponse {
    let mut resp = ErrorResponse::new(Some(body.to_string()));
    *resp.status_mut() = status;
    resp
}

type Ws = tokio_tungstenite::WebSocketStream<TcpStream>;

async fn serve_ingest(
    ws: Ws,
    state: Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!("ingest: worker connected");
    let (mut write, mut read) = ws.split();
    let mut frames = state.hub.attach();

    loop {
        tokio::select! {
            incoming = read.next() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            frame = frames.recv() => match frame {
                // The `Vec` moves straight into the message: no copy, which is
                // what the single-consumer channel buys us over a broadcast.
                Some(bytes) => {
                    if write.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
        }
    }
    tracing::info!("ingest: worker disconnected");
    Ok(())
}

async fn serve_rpc(
    ws: Ws,
    state: Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!("rpc: client connected");
    let (mut write, mut read) = ws.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if write.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });

    while let Some(incoming) = read.next().await {
        let text = match incoming {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        };

        let request: RpcRequest = match serde_json::from_str(&text) {
            Ok(request) => request,
            Err(err) => {
                tracing::warn!(?err, "rpc: malformed request");
                continue;
            }
        };

        // Each call runs on its own task. Opening a connection can block for
        // seconds on a slow robot; serialising here would stall every other
        // call behind it.
        let state = state.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let result = rpc::dispatch(&state, &tx, &request.method, request.params).await;
            let Some(id) = request.id else { return };
            let response = match result {
                Ok(value) => serde_json::json!({ "id": id, "ok": value }),
                Err(err) => serde_json::json!({ "id": id, "err": err }),
            };
            if let Ok(text) = serde_json::to_string(&response) {
                let _ = tx.send(text);
            }
        });
    }

    drop(tx);
    let _ = writer.await;
    tracing::info!("rpc: client disconnected");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_rejects_wrong_and_short_tokens() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc123", "abc124"));
        assert!(!token_matches("abc123", "abc"));
        assert!(!token_matches("abc123", ""));
    }

    #[test]
    fn websites_cannot_open_a_socket() {
        assert!(origin_allowed(None));
        assert!(origin_allowed(Some("app://robot-whisperer")));
        assert!(origin_allowed(Some("http://localhost:5173")));
        assert!(!origin_allowed(Some("https://evil.example")));
        assert!(!origin_allowed(Some("null")));
    }

    #[test]
    fn query_param_extraction() {
        assert_eq!(
            query_param("/rpc?token=deadbeef", "token").as_deref(),
            Some("deadbeef")
        );
        assert_eq!(
            query_param("/ingest?a=1&token=xyz", "token").as_deref(),
            Some("xyz")
        );
        assert_eq!(query_param("/rpc", "token"), None);
    }
}
