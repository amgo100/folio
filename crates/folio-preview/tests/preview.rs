//! The preview end to end: a project copied from the corpus, watched on
//! disk, served on localhost, read over a websocket.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use folio_preview::{Event, Options, Outputs, Preview, Update};
use folio_render::wire;
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message, WebSocket};

/// How long a test waits for the preview.
const TIMEOUT: Duration = Duration::from_secs(60);

/// A copy of a corpus project in a temporary directory.
fn project(name: &str) -> tempfile::TempDir {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .join(name);
    let dir = tempfile::tempdir().unwrap();
    copy(&corpus, dir.path());
    dir
}

fn copy(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir(&target).unwrap();
            copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Starts a preview whose events arrive on the returned channel.
fn start(root: &Path, options: impl FnOnce(Options) -> Options) -> (Preview, Receiver<Event>) {
    let (tx, rx) = unbounded();
    let options = options(Options::new(root).system_fonts(false)).on_event(move |event| {
        let _ = tx.send(event.clone());
    });
    (Preview::start(options).unwrap(), rx)
}

/// Waits for the update of a revision that `accept` accepts.
fn updated(events: &Receiver<Event>, accept: impl Fn(&Update) -> bool) -> Arc<Update> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.recv_deadline(deadline) {
            Ok(Event::Updated(update)) if accept(&update) => return update,
            Ok(_) => {}
            Err(_) => panic!("no update in time"),
        }
    }
}

/// Opens the websocket with a token and, if given, an `Origin` header.
fn connect(
    preview: &Preview,
    token: &str,
    origin: Option<&str>,
) -> Result<WebSocket<TcpStream>, tungstenite::Error> {
    let url = format!("ws://{}/ws?token={token}", preview.addr());
    let mut request = url.into_client_request().unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", HeaderValue::from_str(origin).unwrap());
    }
    let stream = TcpStream::connect(preview.addr()).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    tungstenite::client::client(request, stream)
        .map(|(ws, _)| ws)
        .map_err(|e| match e {
            tungstenite::HandshakeError::Failure(e) => e,
            tungstenite::HandshakeError::Interrupted(_) => panic!("blocking stream"),
        })
}

/// A revision as a client receives it: the `update` message, and the
/// wire messages that follow, by page.
struct Received {
    update: Value,
    frames: Vec<(u32, wire::Message)>,
}

/// Reads one revision from the websocket.
fn receive(ws: &mut WebSocket<TcpStream>) -> Received {
    let update = match ws.read().unwrap() {
        Message::Text(text) => serde_json::from_str::<Value>(&text).unwrap(),
        other => panic!("expected the update, got {other:?}"),
    };
    assert_eq!(update["type"], "update");
    assert_eq!(update["protocol"], 1);
    let mut frames = Vec::new();
    loop {
        match ws.read().unwrap() {
            Message::Binary(bytes) => {
                let (index, body) = bytes.split_at(4);
                let index = u32::from_le_bytes(index.try_into().unwrap());
                frames.push((index, wire::decode(body).expect("a valid wire message")));
            }
            Message::Text(text) => {
                let done: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(done["type"], "done");
                assert_eq!(done["revision"], update["revision"]);
                return Received { update, frames };
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// Reads revisions until one at least `revision`.
fn receive_from(ws: &mut WebSocket<TcpStream>, revision: u64) -> Received {
    loop {
        let received = receive(ws);
        if received.update["revision"].as_u64().unwrap() >= revision {
            return received;
        }
    }
}

/// Sends a raw HTTP request and returns the status line and the body.
fn get(preview: &Preview, target: &str, headers: &[(&str, &str)]) -> (String, Vec<u8>) {
    let mut stream = TcpStream::connect(preview.addr()).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut request = format!("GET {target} HTTP/1.1\r\n");
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
        request.push_str(&format!("Host: {}\r\n", preview.addr()));
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a response head");
    let head = String::from_utf8_lossy(&response[..end]).into_owned();
    let status = head.lines().next().unwrap().to_owned();
    (status, response[end + 4..].to_vec())
}

fn edit(root: &Path, path: &str, from: &str, to: &str) -> PathBuf {
    let file = root.join(path);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains(from), "`{from}` is in {path}");
    std::fs::write(&file, text.replacen(from, to, 1)).unwrap();
    file
}

#[test]
fn edits_on_disk_send_only_the_pages_that_changed() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o);
    let first = updated(&events, |u| u.pages > 0);
    assert!(first.banner.is_none(), "{first:?}");
    assert_eq!(first.lists, (0..first.pages).collect::<Vec<_>>());

    let mut ws = connect(&preview, preview.token(), None).unwrap();
    let received = receive_from(&mut ws, first.revision);
    let pages = received.update["pages"].as_u64().unwrap() as usize;
    assert_eq!(pages, first.pages);
    assert_eq!(received.update["variant"], "exam");
    assert_eq!(received.update["target"], "pdf");
    assert_eq!(received.frames.len(), pages, "a new client gets every page");
    assert!(
        received
            .frames
            .iter()
            .all(|(_, m)| matches!(m, wire::Message::List(_)))
    );

    // Through the file-system watcher: a word on the last page.
    edit(dir.path(), "main.fol", "weight 260 N", "weight 261 N");
    let next = updated(&events, |u| u.revision > first.revision);
    assert!(next.banner.is_none());
    assert_eq!(next.pages, first.pages);
    assert!(!next.lists.is_empty());
    assert!(
        next.lists.len() < next.pages,
        "only changed pages: {next:?}"
    );
    let received = receive_from(&mut ws, next.revision);
    let sent: Vec<usize> = received.update["lists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as usize)
        .collect();
    assert_eq!(sent, next.lists);
    assert_eq!(received.frames.len(), next.lists.len() + next.spans.len());

    // The pages are served as images too.
    let token = preview.token().to_owned();
    let (status, body) = get(&preview, &format!("/page/0.png?token={token}"), &[]);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(body.starts_with(b"\x89PNG"));
    let (status, body) = get(&preview, &format!("/?token={token}"), &[]);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(
        String::from_utf8(body)
            .unwrap()
            .contains("/viewer.js?token=")
    );
    let (status, _) = get(&preview, &format!("/page/999.png?token={token}"), &[]);
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    preview.stop();
}

#[test]
fn visible_pages_come_first() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 1);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    receive_from(&mut ws, first.revision);
    let last = first.pages - 1;
    ws.send(Message::text(format!(
        r#"{{"type":"visible","pages":[{last}]}}"#
    )))
    .unwrap();
    // A change to every page: the body's font size.
    let file = edit(
        dir.path(),
        "styles/exam.fsty",
        "font-size: 11pt",
        "font-size: 11.5pt",
    );
    preview.changed([file]);
    let next = updated(&events, |u| u.revision > first.revision);
    assert!(next.lists.contains(&last), "{next:?}");
    let received = receive_from(&mut ws, next.revision);
    assert_eq!(received.update["lists"][0], last);
    assert_eq!(received.frames[0].0 as usize, last);
}

#[test]
fn wrong_tokens_hosts_and_origins_are_refused() {
    let dir = project("exam");
    let (preview, _events) = start(dir.path(), |o| o.watch(false));
    let token = preview.token().to_owned();
    let addr = preview.addr().to_string();
    let port = preview.addr().port();

    let refused = |result: Result<WebSocket<TcpStream>, tungstenite::Error>| match result {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("the websocket was opened"),
    };
    // A wrong token, a token one character short, and none.
    let wrong: String = token.chars().rev().collect();
    assert_eq!(refused(connect(&preview, &wrong, None)), 403);
    assert_eq!(refused(connect(&preview, &token[1..], None)), 403);
    assert_eq!(refused(connect(&preview, "", None)), 403);
    // A page on another site, with the right token.
    assert_eq!(
        refused(connect(&preview, &token, Some("http://evil.example"))),
        403
    );
    assert_eq!(
        refused(connect(
            &preview,
            &token,
            Some(&format!("http://127.0.0.1:{}", port.wrapping_add(1)))
        )),
        403
    );
    // The preview's own origins, and a program without one.
    connect(&preview, &token, Some(&format!("http://127.0.0.1:{port}"))).unwrap();
    connect(&preview, &token, Some(&format!("http://localhost:{port}"))).unwrap();
    connect(&preview, &token, None).unwrap();

    // Plain requests.
    let forbidden = "HTTP/1.1 403 Forbidden";
    assert_eq!(get(&preview, "/", &[]).0, forbidden);
    assert_eq!(get(&preview, &format!("/?token={wrong}"), &[]).0, forbidden);
    assert_eq!(get(&preview, "/font/0", &[]).0, forbidden);
    assert_eq!(get(&preview, "/page/0.png", &[]).0, forbidden);
    // DNS rebinding: the right token through another host name.
    let rebound = [("Host", "evil.example")];
    assert_eq!(
        get(&preview, &format!("/?token={token}"), &rebound).0,
        forbidden
    );
    assert_eq!(
        get(
            &preview,
            &format!("/?token={token}"),
            &[("Host", addr.as_str())]
        )
        .0,
        "HTTP/1.1 200 OK"
    );
}

#[test]
fn a_broken_manifest_keeps_the_last_valid_one_with_a_banner() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);

    // A manifest that does not parse: the world keeps the last valid one,
    // whose pages stay, and says so.
    let manifest = edit(dir.path(), "folio.toml", "[project]", "[project");
    preview.changed([&manifest]);
    let broken = updated(&events, |u| u.revision > first.revision);
    assert!(broken.banner.is_some(), "{broken:?}");
    assert_eq!(broken.pages, first.pages);
    assert!(broken.lists.is_empty());
    assert!(broken.diagnostics.iter().any(|d| d.is_error()));
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    let received = receive_from(&mut ws, broken.revision);
    assert!(received.update["banner"].is_string());
    assert_eq!(received.frames.len(), first.pages);
    let errors = received.update["diagnostics"].as_array().unwrap();
    assert!(errors.iter().any(|d| d["location"]["path"] == "folio.toml"));

    // Fixed again.
    edit(dir.path(), "folio.toml", "[project", "[project]");
    preview.changed([&manifest]);
    let fixed = updated(&events, |u| u.revision > broken.revision);
    assert!(fixed.banner.is_none(), "{fixed:?}");
    assert!(fixed.diagnostics.is_empty(), "{fixed:?}");
    let received = receive_from(&mut ws, fixed.revision);
    assert!(received.update["banner"].is_null());
}

#[test]
fn when_nothing_can_be_laid_out_the_last_pages_stay_with_a_banner() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);

    // The entry file is gone.
    let entry = dir.path().join("main.fol");
    let text = std::fs::read_to_string(&entry).unwrap();
    std::fs::remove_file(&entry).unwrap();
    preview.changed([&entry]);
    let broken = updated(&events, |u| u.revision > first.revision);
    assert!(
        broken.diagnostics.iter().any(|d| d.is_error()),
        "{broken:?}"
    );
    assert!(broken.banner.is_some(), "{broken:?}");
    assert_eq!(broken.pages, first.pages, "the last pages stay");
    assert!(broken.lists.is_empty());

    std::fs::write(&entry, text).unwrap();
    preview.changed([&entry]);
    let fixed = updated(&events, |u| u.revision > broken.revision);
    assert!(fixed.banner.is_none(), "{fixed:?}");
    assert!(
        fixed.lists.is_empty(),
        "the pages are as they were: {fixed:?}"
    );
}

#[test]
fn failing_constructs_are_error_markers_on_the_pages() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);
    let file = edit(
        dir.path(),
        "main.fol",
        "= Section A",
        "= Section A #nonexistent()",
    );
    preview.changed([file]);
    let next = updated(&events, |u| u.revision > first.revision);
    assert!(
        next.banner.is_none(),
        "partial compilation lays the rest out"
    );
    assert!(next.diagnostics.iter().any(|d| d.is_error()));
    assert!(!next.lists.is_empty());
}

#[test]
fn outputs_are_written_once_the_project_is_quiet() {
    let dir = project("exam");
    let written: Arc<Mutex<Vec<(u64, usize)>>> = Arc::default();
    let sink = written.clone();
    let (_preview, events) = start(dir.path(), |o| {
        o.watch(false)
            .idle(Duration::from_millis(200))
            .on_outputs(move |outputs: &Outputs| {
                let pdfs = outputs
                    .outputs
                    .iter()
                    .filter(|(o, path)| {
                        path.is_some() && o.result.as_ref().is_ok_and(|c| c.output.is_some())
                    })
                    .count();
                sink.lock().unwrap().push((outputs.revision, pdfs));
            })
    });
    let first = updated(&events, |u| u.pages > 0);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.recv_deadline(deadline) {
            Ok(Event::Written { revision }) => {
                assert_eq!(revision, first.revision);
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("nothing was written"),
        }
    }
    // Both PDF builds; DOCX has no writer yet.
    assert_eq!(*written.lock().unwrap(), [(first.revision, 2)]);
}

#[test]
fn pages_that_look_the_same_get_new_span_maps() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    receive_from(&mut ws, first.revision);
    // The document moved to another file: the pages show the same, from
    // other sources.
    let entry = dir.path().join("main.fol");
    let chapter = dir.path().join("chapter.fol");
    std::fs::rename(&entry, &chapter).unwrap();
    std::fs::write(&entry, "#include \"chapter.fol\"\n").unwrap();
    preview.changed([&entry, &chapter]);
    let next = updated(&events, |u| u.revision > first.revision);
    assert!(next.lists.is_empty(), "{next:?}");
    assert_eq!(next.spans, (0..next.pages).collect::<Vec<_>>());
    let received = receive_from(&mut ws, next.revision);
    assert_eq!(received.frames.len(), next.pages);
    assert!(
        received
            .frames
            .iter()
            .all(|(_, m)| matches!(m, wire::Message::Spans(_)))
    );
}
