//! The preview server: HTTP and the websocket, on `127.0.0.1`.

use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdListener};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::engine::{Overlay, Published, Visible};
use crate::http::{self, MAX_HEAD, Request};
use crate::{Error, Event, Hook, PROTOCOL, Source};

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
    /// The pages each client shows, which the engine lays out first.
    visible: Arc<Visible>,
    stop: watch::Receiver<bool>,
    /// Messages for every client, such as `reveal`.
    broadcast: broadcast::Sender<Arc<str>>,
    on_event: Option<Hook>,
    /// The figures `/figure/` converted last, by content hash, oldest
    /// first.
    figures: std::sync::Mutex<Vec<(u64, Arc<str>)>>,
}

/// How many converted figures `/figure/` keeps.
const FIGURES_KEPT: usize = 16;

/// Binds the port on `127.0.0.1` and serves on a thread of its own.
pub(crate) fn start(
    port: u16,
    token: Arc<str>,
    published: watch::Receiver<Arc<Published>>,
    visible: Arc<Visible>,
    broadcast: broadcast::Sender<Arc<str>>,
    on_event: Option<Hook>,
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
        visible,
        stop,
        broadcast,
        on_event,
        figures: std::sync::Mutex::new(Vec::new()),
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
            } else if let Some(n) = path.strip_prefix("/glyphs/") {
                glyphs(&shared, n, &request).await
            } else if let Some(hash) = path
                .strip_prefix("/figure/")
                .and_then(|rest| rest.strip_suffix(".svg"))
            {
                figure(&shared, hash).await
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
///
/// Its policy names no `frame-ancestors`, so that an editor can show the
/// viewer in a frame beside the source (see "Security" in the crate's
/// documentation).
fn page(shared: &Shared) -> Vec<u8> {
    let body = PAGE.replace("{token}", &shared.token);
    http::response(
        "200 OK",
        "text/html; charset=utf-8",
        &[(
            "Content-Security-Policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
             connect-src 'self'; base-uri 'none'; form-action 'none'",
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

/// The policy of a figure's SVG: it may show the images embedded in it and
/// style itself, and nothing else, should it be opened as a page.
const FIGURE_POLICY: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; sandbox";

/// A vector figure of the current revision as SVG, found by the content
/// hash of its file (`Image::content_hash`, as 16 hexadecimal digits): an
/// SVG figure, or a PDF figure converted to SVG, both as `folio-render`'s
/// SVG output draws them, at the size of the first box that holds the
/// figure. Its root does not keep the aspect ratio, so that a client may
/// stretch it to any other box, as the figure is stretched to its box.
async fn figure(shared: &Shared, hash: &str) -> Vec<u8> {
    let digits = |s: &str| s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let Some(hash) = (hash.len() == 16 && digits(hash))
        .then(|| u64::from_str_radix(hash, 16).ok())
        .flatten()
    else {
        return http::error("404 Not Found");
    };
    let answer = |svg: &str| {
        http::response(
            "200 OK",
            "image/svg+xml",
            &[("Content-Security-Policy", FIGURE_POLICY)],
            svg.as_bytes(),
        )
    };
    let published = shared.published.borrow().clone();
    let found = published.pages.iter().find_map(|entry| {
        entry
            .list
            .items
            .iter()
            .find_map(|item| match &item.command {
                folio_render::Command::Image(image)
                    if image.image.content_hash() == hash
                        && matches!(
                            image.image.format(),
                            folio_component::ImageFormat::Svg | folio_component::ImageFormat::Pdf
                        ) =>
                {
                    Some(image.clone())
                }
                _ => None,
            })
    });
    let Some(image) = found else {
        return http::error("404 Not Found");
    };
    // A figure is converted once while it is among the last ones asked
    // for: its content hash names its bytes. The one found moves to the end.
    let kept = |figures: &mut Vec<(u64, Arc<str>)>| {
        let i = figures.iter().position(|(h, _)| *h == hash)?;
        let entry = figures.remove(i);
        let svg = entry.1.clone();
        figures.push(entry);
        Some(svg)
    };
    if let Some(svg) = shared.figures.lock().ok().and_then(|mut f| kept(&mut f)) {
        return answer(&svg);
    }
    let fonts = published.fonts.clone();
    let converted = tokio::task::spawn_blocking(move || {
        let rect = folio_render::Rect {
            x: 0.0,
            y: 0.0,
            ..image.rect
        };
        let list = folio_render::DisplayList {
            width: rect.width,
            height: rect.height,
            span: folio_render::Span::DETACHED,
            items: vec![folio_render::Item {
                command: folio_render::Command::Image(folio_render::ImageItem { rect, ..image }),
                span: folio_render::Span::DETACHED,
            }],
        };
        let svg = folio_render::svg(&list, &*fonts, &mut folio_render::Outlines::new());
        match svg.strip_prefix("<svg ") {
            Some(rest) => format!("<svg preserveAspectRatio=\"none\" {rest}"),
            None => svg,
        }
    })
    .await;
    // A figure that cannot be converted is an SVG without its image.
    let Ok(svg) = converted else {
        return http::error("500 Internal Server Error");
    };
    let svg: Arc<str> = svg.into();
    if let Ok(mut figures) = shared.figures.lock()
        && kept(&mut figures).is_none()
    {
        if figures.len() == FIGURES_KEPT {
            figures.remove(0);
        }
        figures.push((hash, svg.clone()));
    }
    answer(&svg)
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

/// The largest size in pixels that `/glyphs/` rasterises glyphs at.
const MAX_GLYPH_SIZE: f32 = 2048.0;

/// The most glyphs one `/glyphs/` request may ask for.
const MAX_GLYPHS: usize = 4096;

/// The most bytes of coverage one `/glyphs/` answer holds: the glyphs past
/// it are left out, and the client asks for them again.
const MAX_GLYPH_BYTES: usize = 32 << 20;

/// Glyphs of a font rasterised at a size, for clients that draw display
/// lists themselves (SPEC §12.4): `/glyphs/<font>?size=<px>&g=<id>.<sub>,..`,
/// where `sub` (0 to 3) places the glyph's origin that many quarters of a
/// pixel right of a pixel's edge.
async fn glyphs(shared: &Shared, n: &str, request: &Request) -> Vec<u8> {
    let published = shared.published.borrow().clone();
    let font = n.parse::<u32>().ok();
    let size = request
        .param("size")
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s > 0.0 && *s <= MAX_GLYPH_SIZE);
    let wanted: Option<Vec<(u16, u8)>> = request.param("g").and_then(|list| {
        list.split(',')
            .filter(|g| !g.is_empty())
            .map(|g| {
                let (id, sub) = g.split_once('.').unwrap_or((g, "0"));
                let sub = sub.parse::<u8>().ok().filter(|s| *s < 4)?;
                Some((id.parse::<u16>().ok()?, sub))
            })
            .collect()
    });
    let (Some(font), Some(size), Some(wanted)) = (font, size, wanted) else {
        return http::error("400 Bad Request");
    };
    if wanted.len() > MAX_GLYPHS || published.fonts.get(font as usize).is_none() {
        return http::error(match wanted.len() > MAX_GLYPHS {
            true => "400 Bad Request",
            false => "404 Not Found",
        });
    }
    let fonts = published.fonts.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        let mut outlines = folio_render::Outlines::new();
        let mut out = vec![0; 4];
        let mut count = 0u32;
        for (id, sub) in wanted {
            if out.len() > MAX_GLYPH_BYTES {
                break;
            }
            count += 1;
            let bitmap = folio_render::rasterize_glyph(
                &*fonts,
                &mut outlines,
                folio_component::FontId(font),
                id,
                size,
                f32::from(sub) / 4.0,
            );
            out.extend_from_slice(&id.to_le_bytes());
            out.push(sub);
            out.push(0);
            match bitmap {
                Some(b) => {
                    out.extend_from_slice(&b.left.to_le_bytes());
                    out.extend_from_slice(&b.top.to_le_bytes());
                    out.extend_from_slice(&b.width.to_le_bytes());
                    out.extend_from_slice(&b.height.to_le_bytes());
                    out.extend_from_slice(&b.coverage);
                }
                None => out.extend_from_slice(&[0; 16]),
            }
        }
        out[..4].copy_from_slice(&count.to_le_bytes());
        out
    })
    .await;
    match rendered {
        Ok(bytes) => http::response(
            "200 OK",
            "application/octet-stream",
            &[("Cache-Control", "no-store")],
            &bytes,
        ),
        Err(_) => http::error("500 Internal Server Error"),
    }
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

/// What a client has of each page: its content and entry numbers
/// ([`PageEntry`](crate::engine::PageEntry)).
type Known = Vec<(u64, u64)>;

/// Serves a websocket client: every revision it has not seen, until it
/// goes or the server stops.
async fn client(mut ws: WebSocketStream<TcpStream>, shared: Arc<Shared>) {
    let mut published = shared.published.clone();
    let mut stop = shared.stop.clone();
    let mut broadcast = shared.broadcast.subscribe();
    let mut known = Known::new();
    let mut visible: Vec<usize> = Vec::new();
    // Forgets what the client shows however its task ends.
    let seen = Seen {
        visible: shared.visible.clone(),
        id: shared.visible.join(),
    };
    let id = seen.id;
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
                Some(Ok(Message::Text(text))) => match serde_json::from_str(&text) {
                    Ok(ClientMessage::Visible { pages }) => {
                        shared.visible.show(id, &pages);
                        visible = pages;
                    }
                    Ok(ClientMessage::Click { revision, page, x, y }) => {
                        let current = published.borrow().clone();
                        let reply = click(&shared, &current, revision, page, x, y);
                        if ws.send(Message::text(reply)).await.is_err() {
                            break;
                        }
                    }
                    // Messages of later versions.
                    Err(_) => {}
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            message = broadcast.recv() => match message {
                Ok(text) => {
                    if ws.send(Message::text(text.to_string())).await.is_err() {
                        break;
                    }
                }
                // A client that fell behind misses a reveal, which a newer
                // one replaces anyway.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = stop.changed() => break,
        }
    }
    // A client that is going no longer decides what is laid out first.
    drop(seen);
    let _ = ws.close(None).await;
}

/// A client's record of the pages it shows, forgotten when it is dropped.
struct Seen {
    visible: Arc<Visible>,
    id: u64,
}

impl Drop for Seen {
    fn drop(&mut self) {
        self.visible.leave(self.id);
    }
}

/// Answers a click on a page with the source it shows, and tells the hook
/// (SPEC §12.2): the topmost glyph or item at the point whose source is
/// known, a glyph as the characters it shows, any other item as its whole
/// node.
fn click(
    shared: &Shared,
    published: &Published,
    revision: u64,
    page: usize,
    x: f32,
    y: f32,
) -> String {
    let location = (revision == published.revision)
        .then(|| locate(published, page, x, y))
        .flatten();
    if let (Some(location), Some(hook)) = (&location, &shared.on_event) {
        hook(&Event::Clicked(Source {
            revision,
            location: location.clone(),
        }));
    }
    let message = SourceMessage {
        kind: "source",
        revision,
        location: location.as_ref(),
    };
    serde_json::to_string(&message).expect("the message serialises")
}

/// The source of what a point on a page shows.
fn locate(published: &Published, page: usize, x: f32, y: f32) -> Option<folio::diag::Location> {
    // Pages kept from an earlier revision show source that has changed,
    // as do those after the first pages of a revision until it is checked.
    let layout = published
        .layout
        .as_ref()
        .filter(|layout| layout.revision == published.revision && page < layout.laid)?;
    let entry = published.pages.get(page)?;
    let list = &entry.list;
    if !(x.is_finite() && y.is_finite()) {
        return None;
    }
    list.hit(x, y).into_iter().find_map(|hit| {
        let item = list.items.get(hit.item)?;
        let files = &*layout.files;
        match (&item.command, hit.glyph) {
            (folio_render::Command::Glyphs(run), Some(g)) => {
                let (source, within) = run.glyphs.get(g)?.source.clone();
                folio::span_map::locate_source(files, source, within)
            }
            _ => folio::span_map::locate_node(files, item.span.source),
        }
    })
}

/// The answer to a click.
#[derive(Serialize)]
struct SourceMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    revision: u64,
    location: Option<&'a folio::diag::Location>,
}

/// A message from a client.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMessage {
    /// The pages the client shows now.
    Visible { pages: Vec<usize> },
    /// A click at a point of a page, in points from its top-left corner,
    /// as the client shows the revision.
    Click {
        revision: u64,
        page: usize,
        x: f32,
        y: f32,
    },
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
    overlays: &'a [Overlay],
    fonts: u64,
    partial: bool,
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
    let mut seen = vec![false; count];
    let mut order = Vec::with_capacity(count);
    for i in visible.iter().copied().chain(0..count) {
        if i < count && !std::mem::replace(&mut seen[i], true) {
            order.push(i);
        }
    }
    let mut lists = Vec::new();
    let mut spans = Vec::new();
    for i in order {
        let entry = &published.pages[i];
        match known.get(i) {
            Some(&(content, id)) if content == entry.content => {
                if id != entry.id {
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
        overlays: &published.overlays,
        fonts: published.font_generation,
        partial: published.partial,
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
        .map(|entry| (entry.content, entry.id))
        .collect();
    Ok(())
}
