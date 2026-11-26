//! The preview server: HTTP and the websocket, on `127.0.0.1`.

use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdListener};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::engine::Published;
use crate::http::{self, MAX_HEAD, Request};
use crate::{Error, PROTOCOL};

/// How long a client may take to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// The scale of the pages that `/page/<n>.png` serves: 96 dpi.
const PNG_SCALE: f32 = 96.0 / 72.0;

/// The viewer page, its script and its stylesheet.
const PAGE: &str = include_str!("viewer/index.html");
const SCRIPT: &str = include_str!("viewer/viewer.js");
const STYLE: &str = include_str!("viewer/viewer.css");

/// A handle that stops the server.
pub(crate) struct Handle(watch::Sender<bool>);

impl Handle {
    /// Stops the server and closes its connections.
    pub(crate) fn stop(self) {
        let _ = self.0.send(true);
    }
}

/// What every connection shares.
struct Shared {
    token: Arc<str>,
    port: u16,
    published: watch::Receiver<Arc<Published>>,
    stop: watch::Receiver<bool>,
}

/// Binds the port on `127.0.0.1` and serves on a thread of its own.
pub(crate) fn start(
    port: u16,
    token: Arc<str>,
    published: watch::Receiver<Arc<Published>>,
) -> Result<(Handle, SocketAddr, JoinHandle<()>), Error> {
    let listener = StdListener::bind((Ipv4Addr::LOCALHOST, port))
        .map_err(|e| Error(format!("cannot listen on 127.0.0.1:{port}: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| Error(format!("cannot listen on 127.0.0.1:{port}: {e}")))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| Error(format!("cannot listen on {addr}: {e}")))?;
    let (stop_tx, stop) = watch::channel(false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|e| Error(format!("cannot start the preview server: {e}")))?;
    let shared = Arc::new(Shared {
        token,
        port: addr.port(),
        published,
        stop,
    });
    let thread = thread::Builder::new()
        .name("folio-preview-server".into())
        .spawn(move || runtime.block_on(serve(listener, shared)))
        .map_err(|e| Error(format!("cannot start the preview server: {e}")))?;
    Ok((Handle(stop_tx), addr, thread))
}

/// Accepts connections until the server stops.
async fn serve(listener: StdListener, shared: Arc<Shared>) {
    let Ok(listener) = TcpListener::from_std(listener) else {
        return;
    };
    let mut stop = shared.stop.clone();
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    let _ = stream.set_nodelay(true);
                    tasks.spawn(connection(stream, shared.clone()));
                }
            }
            _ = stop.changed() => break,
            // Reaps finished connections.
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

/// Serves one connection: one request, or a websocket.
async fn connection(mut stream: TcpStream, shared: Arc<Shared>) {
    let Ok(Some(request)) = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut stream))
        .await
        .unwrap_or(Ok(None))
    else {
        let _ = stream.write_all(&http::error("400 Bad Request")).await;
        return;
    };
    if request.method != "GET" {
        let _ = stream
            .write_all(&http::error("405 Method Not Allowed"))
            .await;
        return;
    }
    if !request.host_is_local(shared.port) || !request.has_token(&shared.token) {
        let _ = stream.write_all(&http::error("403 Forbidden")).await;
        return;
    }
    let response = match request.path.as_str() {
        "/ws" => {
            websocket(stream, &request, shared).await;
            return;
        }
        "/" => page(&shared),
        "/viewer.js" => http::response(
            "200 OK",
            "text/javascript; charset=utf-8",
            &[],
            SCRIPT.as_bytes(),
        ),
        "/viewer.css" => http::response("200 OK", "text/css; charset=utf-8", &[], STYLE.as_bytes()),
        path => {
            if let Some(n) = path
                .strip_prefix("/page/")
                .and_then(|rest| rest.strip_suffix(".png"))
            {
                png(&shared, n).await
            } else if let Some(n) = path.strip_prefix("/font/") {
                font(&shared, n)
            } else {
                http::error("404 Not Found")
            }
        }
    };
    let _ = stream.write_all(&response).await;
    let _ = stream.shutdown().await;
}

/// Reads a request head; `Ok(None)` when it is not one.
async fn read_head(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
        match Request::parse(&buf) {
            // A request with a body, or bytes after the head, is not one
            // this server takes.
            Ok(Some((request, len))) => return Ok((len == buf.len()).then_some(request)),
            Ok(None) if buf.len() < MAX_HEAD => continue,
            _ => return Ok(None),
        }
    }
}

/// The viewer page, which loads its script and stylesheet with the token.
fn page(shared: &Shared) -> Vec<u8> {
    let body = PAGE.replace("{token}", &shared.token);
    http::response(
        "200 OK",
        "text/html; charset=utf-8",
        &[(
            "Content-Security-Policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
             connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        )],
        body.as_bytes(),
    )
}

/// A page of the current revision as PNG.
async fn png(shared: &Shared, n: &str) -> Vec<u8> {
    let published = shared.published.borrow().clone();
    let Some(entry) = n
        .parse::<usize>()
        .ok()
        .and_then(|n| published.pages.get(n).cloned())
    else {
        return http::error("404 Not Found");
    };
    let fonts = published.fonts.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        let options = folio_render::Options {
            scale: PNG_SCALE,
            ..folio_render::Options::default()
        };
        let mut outlines = folio_render::Outlines::new();
        folio_render::render(&entry.list, &*fonts, &mut outlines, &options)?.png()
    })
    .await;
    match rendered {
        Ok(Ok(bytes)) => http::response("200 OK", "image/png", &[], &bytes),
        _ => http::error("500 Internal Server Error"),
    }
}

/// The bytes of a font that glyph runs name.
fn font(shared: &Shared, n: &str) -> Vec<u8> {
    let published = shared.published.borrow().clone();
    let Some(font) = n
        .parse::<usize>()
        .ok()
        .and_then(|n| published.fonts.get(n).cloned())
    else {
        return http::error("404 Not Found");
    };
    let index = font.index().to_string();
    http::response(
        "200 OK",
        "application/octet-stream",
        &[("X-Font-Index", &index)],
        font.data(),
    )
}

/// Takes a websocket request: checks its origin, completes the handshake
/// and serves the client.
async fn websocket(mut stream: TcpStream, request: &Request, shared: Arc<Shared>) {
    if !request.origin_allowed(shared.port) {
        let _ = stream.write_all(&http::error("403 Forbidden")).await;
        return;
    }
    let key = request.header("sec-websocket-key");
    let version = request.header("sec-websocket-version");
    let (true, Some(key), Some("13")) = (request.is_upgrade(), key, version) else {
        let _ = stream
            .write_all(&http::response(
                "426 Upgrade Required",
                "text/plain; charset=utf-8",
                &[("Sec-WebSocket-Version", "13")],
                b"a websocket is served here",
            ))
            .await;
        return;
    };
    let accept = derive_accept_key(key.as_bytes());
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if stream.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    let ws = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
    client(ws, shared).await;
}

/// What a client has of a page: its content hash and span generation.
type Known = Vec<(u64, u64)>;

/// Serves a websocket client: every revision it has not seen, until it
/// goes or the server stops.
async fn client(mut ws: WebSocketStream<TcpStream>, shared: Arc<Shared>) {
    let mut published = shared.published.clone();
    let mut stop = shared.stop.clone();
    let mut known = Known::new();
    let mut visible: Vec<usize> = Vec::new();
    let current = published.borrow_and_update().clone();
    if send(&mut ws, &current, &mut known, &visible).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            changed = published.changed() => {
                if changed.is_err() {
                    break;
                }
                let current = published.borrow_and_update().clone();
                if send(&mut ws, &current, &mut known, &visible).await.is_err() {
                    break;
                }
            }
            message = ws.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMessage::Visible { pages }) = serde_json::from_str(&text) {
                        visible = pages;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = stop.changed() => break,
        }
    }
    let _ = ws.close(None).await;
}

/// A message from a client.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMessage {
    /// The pages the client shows now.
    Visible { pages: Vec<usize> },
}

/// The `update` message.
#[derive(Serialize)]
struct UpdateMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    protocol: u32,
    revision: u64,
    variant: Option<&'a str>,
    target: &'a str,
    pages: usize,
    lists: &'a [usize],
    spans: &'a [usize],
    banner: Option<&'a str>,
    diagnostics: &'a [folio::diag::Resolved],
}

/// Sends a client what it does not have of a revision: the `update`
/// message, the display lists and span maps, visible pages first, then
/// `done`.
async fn send(
    ws: &mut WebSocketStream<TcpStream>,
    published: &Published,
    known: &mut Known,
    visible: &[usize],
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    let count = published.pages.len();
    let mut order: Vec<usize> = visible.iter().copied().filter(|&i| i < count).collect();
    order.dedup();
    order.extend((0..count).filter(|i| !visible.contains(i)));
    let mut lists = Vec::new();
    let mut spans = Vec::new();
    for i in order {
        let entry = &published.pages[i];
        match known.get(i) {
            Some(&(hash, generation)) if hash == entry.hash => {
                if generation != entry.generation {
                    spans.push(i);
                }
            }
            _ => lists.push(i),
        }
    }
    let update = UpdateMessage {
        kind: "update",
        protocol: PROTOCOL,
        revision: published.revision,
        variant: published.variant.as_deref(),
        target: published.target,
        pages: count,
        lists: &lists,
        spans: &spans,
        banner: published.banner.as_deref(),
        diagnostics: &published.diagnostics,
    };
    let text = serde_json::to_string(&update).expect("the update serialises");
    ws.feed(Message::text(text)).await?;
    for (&i, spans_only) in lists
        .iter()
        .map(|i| (i, false))
        .chain(spans.iter().map(|i| (i, true)))
    {
        let entry = &published.pages[i];
        let body = if spans_only {
            &entry.spans
        } else {
            &entry.bytes
        };
        let index = u32::try_from(i).expect("fewer pages than u32::MAX");
        let mut frame = Vec::with_capacity(4 + body.len());
        frame.extend_from_slice(&index.to_le_bytes());
        frame.extend_from_slice(body);
        ws.feed(Message::binary(frame)).await?;
    }
    let done = format!(r#"{{"type":"done","revision":{}}}"#, published.revision);
    ws.feed(Message::text(done)).await?;
    ws.flush().await?;
    *known = published
        .pages
        .iter()
        .map(|entry| (entry.hash, entry.generation))
        .collect();
    Ok(())
}
