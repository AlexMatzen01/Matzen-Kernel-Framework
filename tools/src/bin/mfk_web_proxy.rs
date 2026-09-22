//! MFK Web Proxy - Serves noVNC and proxies WebSocket to QEMU VNC

use std::net::SocketAddr;
use std::sync::Arc;
use std::convert::Infallible;
use std::time::Duration;

use anyhow::{Context, Result};
use hyper::body::{Body, Bytes, Incoming};
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, Method};
use hyper_tungstenite::{is_upgrade_request, upgrade, HyperWebsocket};
use tokio::net::TcpListener;
use tungstenite::protocol::Message;
use mime_guess::from_path;
use include_dir::{include_dir, Dir};
use futures_util::{SinkExt, StreamExt};
use http_body_util::{combinators::BoxBody, Full};

static NOVNC_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/novnc");

const MAX_RETRY_ATTEMPTS: u32 = 10;
const INITIAL_RETRY_DELAY_MS: u64 = 100;
const MAX_RETRY_DELAY_MS: u64 = 2000;

fn box_body<B: Body<Data = Bytes, Error = Infallible> + Send + Sync + 'static>(body: B) -> BoxBody<Bytes, Infallible> {
    BoxBody::new(body)
}

#[derive(Clone)]
struct ProxyState {
    vnc_ws_addr: SocketAddr,
}

async fn serve_static_file(path: &str) -> Result<Response<BoxBody<Bytes, Infallible>>> {
    let file_path = path.trim_start_matches('/');
    let file_path = if file_path.is_empty() { "vnc.html" } else { file_path };

    if let Some(file) = NOVNC_DIR.get_file(file_path) {
        let mime = from_path(file_path).first_or_octet_stream();
        let body = Full::new(Bytes::copy_from_slice(file.contents()));
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, HeaderValue::from_str(mime.as_ref())?)
            .body(box_body(body))?);
    }

    // Fallback to vnc.html for SPA routing
    if let Some(file) = NOVNC_DIR.get_file("vnc.html") {
        let body = Full::new(Bytes::copy_from_slice(file.contents()));
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, HeaderValue::from_static("text/html"))
            .body(box_body(body))?);
    }

    let body = Full::new(Bytes::from_static(b"Not found"));
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(box_body(body))?)
}

async fn proxy_websocket(
    ws: HyperWebsocket,
    vnc_ws_addr: SocketAddr,
) -> Result<()> {
    let ws_stream = ws.await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Connect to QEMU VNC WebSocket with retry/backoff
    let mut retry_delay = Duration::from_millis(INITIAL_RETRY_DELAY_MS);
    let mut vnc_ws = None;

    for attempt in 1..=MAX_RETRY_ATTEMPTS {
        eprintln!("Connecting to QEMU VNC at {} (attempt {}/{})", vnc_ws_addr, attempt, MAX_RETRY_ATTEMPTS);
        match tokio_tungstenite::connect_async(format!("ws://{}", vnc_ws_addr)).await {
            Ok((ws, _)) => {
                vnc_ws = Some(ws);
                eprintln!("Successfully connected to QEMU VNC WebSocket");
                break;
            }
            Err(e) => {
                eprintln!("Failed to connect to QEMU VNC (attempt {}/{}): {}", attempt, MAX_RETRY_ATTEMPTS, e);
                if attempt == MAX_RETRY_ATTEMPTS {
                    // Send error to client before closing
                    let _ = ws_sender.send(Message::Close(Some(tungstenite::protocol::CloseFrame {
                        code: tungstenite::protocol::frame::coding::CloseCode::Error,
                        reason: "Failed to connect to upstream VNC server".into(),
                    }))).await;
                    return Err(anyhow::anyhow!("Failed to connect to QEMU VNC WebSocket after {} attempts: {}", MAX_RETRY_ATTEMPTS, e));
                }
                tokio::time::sleep(retry_delay).await;
                retry_delay = std::cmp::min(retry_delay * 2, Duration::from_millis(MAX_RETRY_DELAY_MS));
            }
        }
    }

    let vnc_ws = vnc_ws.ok_or_else(|| anyhow::anyhow!("No VNC WebSocket connection established"))?;
    let (mut vnc_sender, mut vnc_receiver) = vnc_ws.split();

    // Forward messages bidirectionally
    let client_to_vnc = async {
        while let Some(msg) = ws_receiver.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    if vnc_sender.send(Message::Binary(data)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Text(text)) => {
                    if vnc_sender.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Ping(data)) => {
                    if vnc_sender.send(Message::Ping(data)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Pong(data)) => {
                    if vnc_sender.send(Message::Pong(data)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => {
                    let _ = vnc_sender.send(Message::Close(None)).await;
                    break;
                }
                Ok(Message::Frame(_)) => {}
                Err(_) => break,
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    let vnc_to_client = async {
        while let Some(msg) = vnc_receiver.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    if ws_sender.send(Message::Binary(data)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Text(text)) => {
                    if ws_sender.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Ping(data)) => {
                    if ws_sender.send(Message::Ping(data)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Pong(data)) => {
                    if ws_sender.send(Message::Pong(data)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) => {
                    let _ = ws_sender.send(Message::Close(None)).await;
                    break;
                }
                Ok(Message::Frame(_)) => {}
                Err(_) => break,
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        _ = client_to_vnc => {},
        _ = vnc_to_client => {},
    }

    Ok(())
}

async fn handle_request(
    req: Request<Incoming>,
    state: Arc<ProxyState>,
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    let method = req.method().clone(); // Clone to avoid borrow
    let path = req.uri().path().to_string();

    // Health check endpoint
    if method == Method::GET && path == "/health" {
        // Test connection to QEMU VNC WebSocket
        let vnc_connected = tokio_tungstenite::connect_async(format!("ws://{}", state.vnc_ws_addr))
            .await
            .map(|_| true)
            .unwrap_or(false);

        let status_code = if vnc_connected { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
        let body = format!(r#"{{"status":"{}","vnc_connected":{}}}"#, 
            if vnc_connected { "healthy" } else { "unhealthy" }, 
            vnc_connected);

        let response = Response::builder()
            .status(status_code)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(box_body(Full::new(Bytes::from(body))))
            .unwrap();
        return Ok(response);
    }

    // Handle WebSocket upgrade
    if method == Method::GET && is_upgrade_request(&req) {
        let mut req_mut = req;
        match upgrade(&mut req_mut, None) {
            Ok((response, ws)) => {
                let vnc_ws_addr = state.vnc_ws_addr;
                tokio::spawn(async move {
                    if let Err(e) = proxy_websocket(ws, vnc_ws_addr).await {
                        eprintln!("WebSocket proxy error: {}", e);
                    }
                });
                return Ok(response.map(|b| box_body(b)));
            }
            Err(e) => {
                eprintln!("WebSocket upgrade failed: {}", e);
            }
        }
    }

    // Serve static files
    if method == Method::GET {
        match serve_static_file(&path).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                eprintln!("Static file error: {}", e);
                let body = Full::new(Bytes::from_static(b"Internal Server Error"));
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(box_body(body))
                    .unwrap());
            }
        }
    }

    let body = Full::new(Bytes::from_static(b"Method not allowed"));
    Ok(Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .body(box_body(body))
        .unwrap())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: {} <http_port> <vnc_ws_port>", args[0]);
        eprintln!("Example: {} 8084 8085", args[0]);
        std::process::exit(1);
    }

    let http_port: u16 = args[1].parse().context("Invalid HTTP port")?;
    let vnc_ws_port: u16 = args[2].parse().context("Invalid VNC WebSocket port")?;

    let http_addr = SocketAddr::from(([127, 0, 0, 1], http_port));
    let vnc_ws_addr = SocketAddr::from(([127, 0, 0, 1], vnc_ws_port));

    let state = Arc::new(ProxyState { vnc_ws_addr });

    let listener = TcpListener::bind(http_addr).await?;
    println!("MFK Web Proxy listening on http://{}", http_addr);
    println!("Proxying WebSocket to ws://{}", vnc_ws_addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();

        let service = service_fn(move |req| handle_request(req, state.clone()));

        let io = hyper_util::rt::TokioIo::new(stream);
        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new().serve_connection(io, service).with_upgrades().await {
                eprintln!("Connection error: {}", e);
            }
        });
    }
}