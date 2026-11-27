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

/// Waits until the pages of a revision are indexed for source sync.
fn indexed(events: &Receiver<Event>, revision: u64) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.recv_deadline(deadline) {
            Ok(Event::Indexed { revision: r }) if r >= revision => return,
            Ok(_) => {}
            Err(_) => panic!("revision {revision} not indexed in time"),
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
    let (head, body) = request(preview, target, headers);
    (head.lines().next().unwrap().to_owned(), body)
}

/// Sends a raw HTTP request and returns the response's head and body.
fn request(preview: &Preview, target: &str, headers: &[(&str, &str)]) -> (String, Vec<u8>) {
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
    (head, response[end + 4..].to_vec())
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

/// The value of a header in a response head, matching its name in any case.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

#[test]
fn the_viewer_may_be_framed() {
    // Editors show the preview beside the source in an iframe (decision
    // D-332-1): the viewer's policy names no frame ancestors, and nothing
    // else forbids framing.
    let dir = project("exam");
    let (preview, _events) = start(dir.path(), |o| o.watch(false));
    let (head, _) = request(&preview, &format!("/?token={}", preview.token()), &[]);
    assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
    let policy = header(&head, "Content-Security-Policy").expect("a policy");
    assert!(!policy.contains("frame-ancestors"), "{policy}");
    assert!(policy.contains("default-src 'none'"), "{policy}");
    assert_eq!(header(&head, "X-Frame-Options"), None, "{head}");
}

/// Whether Node.js can load Playwright and Playwright has its Chromium,
/// which the browser test drives.
fn playwright() -> bool {
    let probe = "process.exit(require('fs').existsSync(\
                 require('playwright').chromium.executablePath()) ? 0 : 1)";
    // From the directory the script runs in, so that both find the same
    // Playwright.
    std::process::Command::new("node")
        .current_dir(tests_dir())
        .args(["-e", probe])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn the_viewer_draws_pages_inside_an_iframe() {
    // A page on another origin frames the viewer as VS Code's Simple
    // Browser does (`tests/iframe.cjs`), and Chromium must draw its pages.
    // The test needs Node.js with Playwright and its Chromium; without
    // them it says so and passes, and `the_viewer_may_be_framed` still
    // checks the headers.
    if !playwright() {
        eprintln!("skipped: Node.js with Playwright is not installed");
        return;
    }
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    updated(&events, |u| u.pages > 0);
    node("iframe.cjs", &preview.url());
}

#[test]
fn the_viewer_draws_images() {
    // Every image of the acceptance exam (SVG, PNG) and of the figures
    // corpus (every format, PDF among them) is drawn on its page in
    // Chromium (`tests/images.cjs`). Needs Node.js with Playwright, as
    // above; `figures_are_served_as_svg` checks the server's part.
    if !playwright() {
        eprintln!("skipped: Node.js with Playwright is not installed");
        return;
    }
    for name in ["acceptance", "figures"] {
        let dir = project(name);
        let (preview, events) = start(dir.path(), |o| o.watch(false));
        updated(&events, |u| u.pages > 0);
        node("images.cjs", &preview.url());
    }
}

/// Runs a script of this crate's `tests` directory with Node.js and fails
/// with its output unless it succeeds.
fn node(script: &str, url: &str) {
    let mut child = std::process::Command::new("node")
        .current_dir(tests_dir())
        .arg(script)
        .arg(url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // The script's own waits end after 30 s; a browser that hangs on
    // launch or close is killed here.
    let deadline = Instant::now() + 2 * TIMEOUT;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{script}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_viewer_hashes_images_as_the_server_does() {
    // The viewer names figures to `/figure/` by `Image::content_hash`,
    // which `contentHash` in `viewer.js` computes again. Needs Node.js only.
    let has_node = std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !has_node {
        eprintln!("skipped: Node.js is not installed");
        return;
    }
    let script = include_str!("../src/viewer/viewer.js");
    let start = script.find("function contentHash(").expect("contentHash");
    let end = start + script[start..].find("\n}\n").expect("its end") + 3;
    let mut inputs: Vec<Vec<u8>> = [0, 1, 7, 8, 9, 15, 16, 17, 1000]
        .iter()
        .map(|&n| (0..n).map(|i: usize| (i * 151 + 7) as u8).collect())
        .collect();
    inputs.push(vec![0xff; 64]);
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    for file in ["acceptance/assets/sledge.svg", "figures/assets/field.pdf"] {
        inputs.push(std::fs::read(corpus.join(file)).unwrap());
    }
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let program = format!(
        "{}\nfor (const line of require('fs').readFileSync(0, 'utf8').split('\\n')) {{\n\
         if (line === '') continue;\n\
         console.log(contentHash(Uint8Array.from(Buffer.from(line.slice(1), 'hex'))));\n}}",
        &script[start..end]
    );
    let mut child = std::process::Command::new("node")
        .args(["-e", &program])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdin: String = inputs.iter().map(|i| format!("x{}\n", hex(i))).collect();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let got: Vec<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let want: Vec<String> = inputs
        .iter()
        .map(|bytes| {
            let image = folio_component::Image::new(
                bytes.as_slice().into(),
                folio_component::ImageFormat::Png,
            );
            format!("{:016x}", image.content_hash())
        })
        .collect();
    assert_eq!(got, want);
}

#[test]
fn figures_are_served_as_svg() {
    use folio_component::ImageFormat;
    let dir = project("figures");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    let received = receive_from(&mut ws, first.revision);
    let mut images = Vec::new();
    for (_, message) in &received.frames {
        if let wire::Message::List(list) = message {
            for item in &list.items {
                if let folio_render::Command::Image(image) = &item.command {
                    images.push(image.image.clone());
                }
            }
        }
    }
    let format = |f: ImageFormat| images.iter().filter(|i| i.format() == f).count();
    assert!(
        format(ImageFormat::Svg) > 0 && format(ImageFormat::Pdf) > 0,
        "{images:?}"
    );

    let token = preview.token().to_owned();
    let path = |image: &folio_component::Image| {
        format!("/figure/{:016x}.svg?token={token}", image.content_hash())
    };
    for image in &images {
        let (head, body) = request(&preview, &path(image), &[]);
        if matches!(image.format(), ImageFormat::Svg | ImageFormat::Pdf) {
            assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
            assert_eq!(header(&head, "Content-Type"), Some("image/svg+xml"));
            let policy = header(&head, "Content-Security-Policy").expect("a policy");
            assert!(policy.contains("default-src 'none'"), "{policy}");
            let svg = String::from_utf8(body).unwrap();
            // Stretched to any box that holds the figure, as in the PDF.
            assert!(
                svg.starts_with("<svg preserveAspectRatio=\"none\" "),
                "{svg:.100}"
            );
            // The figure itself, embedded: a PDF figure as the SVG it
            // converts to.
            assert!(
                svg.contains("href=\"data:image/svg+xml;base64,"),
                "{svg:.300}"
            );
        } else {
            // Raster images are drawn from the list's own bytes.
            assert_eq!(get(&preview, &path(image), &[]).0, "HTTP/1.1 404 Not Found");
        }
    }
    let svg = images
        .iter()
        .find(|i| i.format() == ImageFormat::Svg)
        .unwrap();
    let without_token = format!("/figure/{:016x}.svg", svg.content_hash());
    assert_eq!(
        get(&preview, &without_token, &[]).0,
        "HTTP/1.1 403 Forbidden"
    );
    // The hash only as 16 lowercase digits: in capitals, or with a sign
    // for its first digit, it names nothing.
    let lower = format!("{:016x}", svg.content_hash());
    let mut bad = vec![
        "/figure/0123.svg".to_owned(),
        "/figure/zzzzzzzzzzzzzzzz.svg".to_owned(),
        "/figure/0000000000000000.svg".to_owned(),
    ];
    if lower.bytes().any(|b| b.is_ascii_alphabetic()) {
        bad.push(format!("/figure/{}.svg", lower.to_uppercase()));
    }
    if let Some(rest) = lower.strip_prefix('0') {
        bad.push(format!("/figure/+{rest}.svg"));
    }
    for bad in &bad {
        let target = format!("{bad}?token={token}");
        assert_eq!(
            get(&preview, &target, &[]).0,
            "HTTP/1.1 404 Not Found",
            "{bad}"
        );
    }
}

/// This crate's `tests` directory, which holds the scripts `node` runs.
fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
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
fn nothing_is_written_while_the_manifest_has_errors() {
    let dir = project("exam");
    let written: Arc<Mutex<Vec<u64>>> = Arc::default();
    let sink = written.clone();
    let idle = Duration::from_millis(200);
    let (preview, events) = start(dir.path(), |o| {
        o.watch(false)
            .idle(idle)
            .on_outputs(move |outputs: &Outputs| sink.lock().unwrap().push(outputs.revision))
    });
    let first = updated(&events, |u| u.pages > 0);
    let wait_written = |after: u64| {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match events.recv_deadline(deadline) {
                Ok(Event::Written { revision }) if revision > after => return revision,
                Ok(_) => {}
                Err(_) => panic!("nothing was written"),
            }
        }
    };
    wait_written(0);

    // A broken manifest, then an edit to the content: the preview shows
    // the edit under the last valid manifest, with its banner, but writes
    // nothing, as `folio build` would refuse the project.
    let manifest = edit(dir.path(), "folio.toml", "[project]", "[project");
    preview.changed([&manifest]);
    let broken = updated(&events, |u| u.revision > first.revision);
    assert!(broken.banner.is_some(), "{broken:?}");
    let file = edit(dir.path(), "main.fol", "= Section A", "= Section A edited");
    preview.changed([file]);
    let edited = updated(&events, |u| u.revision > broken.revision);
    assert!(edited.banner.is_some(), "{edited:?}");
    assert!(!edited.lists.is_empty(), "the edit is shown: {edited:?}");
    let quiet = Instant::now() + idle * 5;
    while let Ok(event) = events.recv_deadline(quiet) {
        assert!(
            !matches!(event, Event::Written { .. }),
            "written while the manifest has errors: {event:?}"
        );
    }
    assert_eq!(*written.lock().unwrap(), [first.revision]);

    // Fixed again: the edit is written.
    edit(dir.path(), "folio.toml", "[project", "[project]");
    preview.changed([&manifest]);
    let fixed = updated(&events, |u| u.revision > edited.revision);
    assert!(fixed.banner.is_none(), "{fixed:?}");
    assert_eq!(wait_written(edited.revision), fixed.revision);
    assert_eq!(*written.lock().unwrap(), [first.revision, fixed.revision]);
}

#[test]
fn nothing_is_written_while_the_manifest_is_missing() {
    let dir = project("exam");
    let written: Arc<Mutex<Vec<u64>>> = Arc::default();
    let sink = written.clone();
    let idle = Duration::from_millis(200);
    let (preview, events) = start(dir.path(), |o| {
        o.watch(false)
            .idle(idle)
            .on_outputs(move |outputs: &Outputs| sink.lock().unwrap().push(outputs.revision))
    });
    let first = updated(&events, |u| u.pages > 0);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.recv_deadline(deadline) {
            Ok(Event::Written { revision }) if revision == first.revision => break,
            Ok(_) => {}
            Err(_) => panic!("nothing was written"),
        }
    }

    // The manifest is gone: the world keeps it, but `folio build` would
    // find no project, so nothing is written.
    let manifest = dir.path().join("folio.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::remove_file(&manifest).unwrap();
    preview.changed([&manifest]);
    let gone = updated(&events, |u| u.revision > first.revision);
    let quiet = Instant::now() + idle * 5;
    while let Ok(event) = events.recv_deadline(quiet) {
        assert!(
            !matches!(event, Event::Written { .. }),
            "written while the manifest is missing: {event:?}"
        );
    }
    assert_eq!(*written.lock().unwrap(), [first.revision]);

    std::fs::write(&manifest, text).unwrap();
    preview.changed([&manifest]);
    let back = updated(&events, |u| u.revision > gone.revision);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.recv_deadline(deadline) {
            Ok(Event::Written { revision }) => {
                assert_eq!(revision, back.revision);
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("nothing was written"),
        }
    }
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

#[test]
fn a_font_change_sends_every_page_again() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    let before = receive_from(&mut ws, first.revision);
    // A font file in the project's font directory, of a family the exam
    // does not use: the pages' hashes stay, but every page is drawn again.
    let font =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../folio-fonts/bundled/NewCM10-Regular.otf");
    let fonts = dir.path().join("fonts");
    std::fs::create_dir(&fonts).unwrap();
    std::fs::copy(&font, fonts.join(font.file_name().unwrap())).unwrap();
    preview.changed([&fonts]);
    let next = updated(&events, |u| u.revision > first.revision);
    assert_eq!(next.lists, (0..next.pages).collect::<Vec<_>>());
    let received = receive_from(&mut ws, next.revision);
    assert_eq!(
        received.frames.len(),
        next.pages,
        "the client gets every page"
    );
    assert_ne!(
        received.update["fonts"], before.update["fonts"],
        "the client drops the glyphs it has"
    );
    assert!(
        received
            .frames
            .iter()
            .all(|(_, m)| matches!(m, wire::Message::List(_)))
    );
}

/// Reads text messages until one of `kind`, skipping revisions.
fn message_of(ws: &mut WebSocket<TcpStream>, kind: &str) -> Value {
    loop {
        match ws.read().unwrap() {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["type"] == kind {
                    return value;
                }
            }
            Message::Binary(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn the_cursor_reveals_pages_and_a_click_finds_the_source() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let laid_out = updated(&events, |u| u.pages > 0);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    let first = receive_from(&mut ws, laid_out.revision);
    let revision = first.update["revision"].as_u64().unwrap();
    indexed(&events, revision);
    let text = std::fs::read_to_string(dir.path().join("main.fol")).unwrap();
    let at = text.find("Answer *all*").unwrap();

    // The editor's cursor, inside the word: one glyph.
    assert_eq!(preview.reveal("main.fol", at + 2..at + 2), 1);
    let reveal = message_of(&mut ws, "reveal");
    assert_eq!(reveal["revision"], revision);
    let area = &reveal["areas"][0];
    assert_eq!(area["page"], 0);
    let rect: Vec<f64> = serde_json::from_value(area["rect"].clone()).unwrap();
    assert!(rect[2] > 0.0 && rect[3] > 0.0);
    // A range: the whole word, one area.
    assert_eq!(preview.reveal("main.fol", at..at + 6), 1);
    let word = message_of(&mut ws, "reveal");
    let wide: Vec<f64> = serde_json::from_value(word["areas"][0]["rect"].clone()).unwrap();
    assert!(wide[2] > rect[2]);
    // Nowhere: nothing is sent.
    assert_eq!(preview.reveal("nothing.fol", 0..0), 0);

    // A click in the middle of that glyph finds its character.
    let click = serde_json::json!({
        "type": "click", "revision": revision, "page": 0,
        "x": rect[0] + rect[2] / 2.0, "y": rect[1] + rect[3] / 2.0,
    });
    ws.send(Message::text(click.to_string())).unwrap();
    let source = message_of(&mut ws, "source");
    assert_eq!(source["location"]["path"], "main.fol");
    assert_eq!(
        source["location"]["bytes"],
        serde_json::json!({"start": at + 2, "end": at + 3})
    );
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.recv_deadline(deadline) {
            Ok(Event::Clicked(clicked)) => {
                assert_eq!(clicked.location.bytes, at + 2..at + 3);
                assert_eq!(clicked.location.start.line, 5);
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("no click event"),
        }
    }

    // A click on another revision, or off every item, finds nothing; a
    // message of a later version is ignored.
    ws.send(Message::text(r#"{"type":"later","x":1}"#)).unwrap();
    let stale = serde_json::json!({
        "type": "click", "revision": revision + 100, "page": 0,
        "x": rect[0] + 1.0, "y": rect[1] + 1.0,
    });
    ws.send(Message::text(stale.to_string())).unwrap();
    assert!(message_of(&mut ws, "source")["location"].is_null());
    let off = serde_json::json!({
        "type": "click", "revision": revision, "page": 99, "x": 1.0, "y": 1.0,
    });
    ws.send(Message::text(off.to_string())).unwrap();
    assert!(message_of(&mut ws, "source")["location"].is_null());
}

#[test]
fn diagnostics_are_marked_on_the_pages() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let laid_out = updated(&events, |u| u.pages > 0);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    let first = receive_from(&mut ws, laid_out.revision);
    let revision = first.update["revision"].as_u64().unwrap();
    let file = edit(
        dir.path(),
        "main.fol",
        "= Section A",
        "= Section A #nonexistent",
    );
    preview.changed([file]);
    let edited = updated(&events, |u| u.revision > revision);
    // The overlays follow the pages, in an update of the same revision.
    let mut next = receive_from(&mut ws, edited.revision);
    while next.update["overlays"].as_array().unwrap().is_empty() {
        next = receive(&mut ws);
    }
    assert_eq!(next.update["revision"], edited.revision);
    let diagnostics = next.update["diagnostics"].as_array().unwrap();
    let error = diagnostics
        .iter()
        .position(|d| d["severity"] == "error")
        .expect("an error");
    let overlays = next.update["overlays"].as_array().unwrap();
    let marked = overlays
        .iter()
        .find(|o| o["diagnostic"] == error)
        .expect("the error is marked on a page");
    assert!(!marked["rects"].as_array().unwrap().is_empty());
}

#[test]
fn glyphs_are_rasterised_for_clients_that_draw() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let laid_out = updated(&events, |u| u.pages > 0);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    let first = receive_from(&mut ws, laid_out.revision);
    let (font, id) = first
        .frames
        .iter()
        .find_map(|(_, message)| match message {
            wire::Message::List(list) => list.items.iter().find_map(|item| match &item.command {
                folio_render::Command::Glyphs(run) => Some((run.font.0, run.glyphs[0].id)),
                _ => None,
            }),
            _ => None,
        })
        .expect("a glyph run");
    let token = preview.token();
    let (status, body) = get(
        &preview,
        &format!("/glyphs/{font}?token={token}&size=24&g={id}.0,{id}.2,3"),
        &[],
    );
    assert!(status.contains("200"), "{status}");
    let u32_at = |at: usize| u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
    assert_eq!(u32_at(0), 3);
    let mut at = 4;
    let mut glyphs = Vec::new();
    for _ in 0..3 {
        let gid = u16::from_le_bytes(body[at..at + 2].try_into().unwrap());
        let sub = body[at + 2];
        let (width, height) = (u32_at(at + 12), u32_at(at + 16));
        at += 20 + (width * height) as usize;
        glyphs.push((gid, sub, width, height));
    }
    assert_eq!(at, body.len());
    assert_eq!((glyphs[0].0, glyphs[0].1), (id, 0));
    assert_eq!((glyphs[1].0, glyphs[1].1), (id, 2));
    assert!(glyphs[0].2 > 0 && glyphs[0].3 > 0);

    for bad in [
        format!("/glyphs/{font}?token={token}&size=0&g=1"),
        format!("/glyphs/{font}?token={token}&size=24&g=1.4"),
        format!("/glyphs/{font}?token={token}&size=99999&g=1"),
        format!("/glyphs/x?token={token}&size=24&g=1"),
    ] {
        assert!(get(&preview, &bad, &[]).0.contains("400"), "{bad}");
    }
    let (missing, _) = get(
        &preview,
        &format!("/glyphs/999?token={token}&size=24&g=1"),
        &[],
    );
    assert!(missing.contains("404"));
    let (refused, _) = get(&preview, &format!("/glyphs/{font}?size=24&g=1"), &[]);
    assert!(refused.contains("403"));
}

#[test]
fn pages_kept_from_an_earlier_revision_find_no_source() {
    let dir = project("exam");
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);
    indexed(&events, first.revision);
    let text = std::fs::read_to_string(dir.path().join("main.fol")).unwrap();
    let at = text.find("Answer").unwrap();
    assert_eq!(preview.reveal("main.fol", at..at), 1);

    // The entry file is gone: the last pages stay, under a banner.
    let entry = dir.path().join("main.fol");
    std::fs::remove_file(&entry).unwrap();
    preview.changed([&entry]);
    let broken = updated(&events, |u| u.revision > first.revision);
    assert!(broken.banner.is_some());
    assert_eq!(preview.reveal("main.fol", at..at), 0);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    receive_from(&mut ws, broken.revision);
    let click = serde_json::json!({
        "type": "click", "revision": broken.revision, "page": 0, "x": 100.0, "y": 100.0,
    });
    ws.send(Message::text(click.to_string())).unwrap();
    assert!(message_of(&mut ws, "source")["location"].is_null());
}

/// A project of `sections` sections of prose, many pages long, in a
/// temporary directory.
fn long_project(sections: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("folio.toml"),
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         lang = \"en\"\n",
    )
    .unwrap();
    let mut text = String::new();
    for s in 0..sections {
        text.push_str(&format!("= Section {s}\n\n"));
        for p in 0..4 {
            for w in 0..15 {
                text.push_str(&format!("Sentence {w} of paragraph {p} in section {s}. "));
            }
            text.push_str("\n\n");
        }
    }
    std::fs::write(dir.path().join("main.fol"), text).unwrap();
    dir
}

#[test]
fn the_visible_pages_are_laid_out_first() {
    let dir = long_project(30);
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);
    assert!(first.pages >= 10, "{} pages", first.pages);
    let mut ws = connect(&preview, preview.token(), None).unwrap();
    receive_from(&mut ws, first.revision);
    ws.send(Message::text(r#"{"type":"visible","pages":[0,1]}"#))
        .unwrap();
    // A click is answered after the visible pages are recorded.
    let click = serde_json::json!({
        "type": "click", "revision": first.revision, "page": 0, "x": 100.0, "y": 100.0,
    });
    ws.send(Message::text(click.to_string())).unwrap();
    message_of(&mut ws, "source");

    // A sentence added at the top reflows every page.
    let file = edit(
        dir.path(),
        "main.fol",
        "Sentence 0 of paragraph 0 in section 0.",
        "Sentence 0 of paragraph 0 in section 0, said again at length.",
    );
    preview.changed([file]);
    let deadline = Instant::now() + TIMEOUT;
    let (revision, laid, early) = loop {
        match events.recv_deadline(deadline) {
            Ok(Event::FirstPages {
                revision,
                laid,
                pages,
                lists,
                spans,
            }) => {
                assert!(revision > first.revision);
                assert_eq!(pages, first.pages, "the pages after stay");
                assert!(spans.is_empty(), "{spans:?}");
                break (revision, laid, lists);
            }
            Ok(Event::Updated(update)) if update.revision > first.revision => {
                panic!(
                    "checked before the first pages were sent: {:?}",
                    update.lists
                )
            }
            Ok(_) => {}
            Err(_) => panic!("no first pages in time"),
        }
    };
    // Through the last visible page, and not the whole document.
    assert!(
        (2..first.pages).contains(&laid),
        "{laid} pages laid out first"
    );
    assert!(!early.is_empty());
    assert!(early.iter().all(|&i| i < laid), "{early:?}");
    let next = updated(&events, |u| u.revision == revision);
    assert!(next.banner.is_none());
    // The rest follows; the pages laid out first are not sent again.
    assert!(next.lists.iter().all(|&i| i >= laid), "{:?}", next.lists);
    assert!(next.lists.len() > first.pages / 2, "{:?}", next.lists);
    indexed(&events, revision);

    // The client receives each page that changed once, the first pages
    // first, whether in an update of their own or, when the server sends
    // only the latest, together with the rest.
    let mut sent = Vec::new();
    while sent.len() < early.len() + next.lists.len() {
        let received = receive_from(&mut ws, revision);
        assert_eq!(received.update["revision"], revision);
        let frames: Vec<usize> = received.frames.iter().map(|(i, _)| *i as usize).collect();
        if received.update["partial"] == true {
            assert!(frames.iter().all(|&i| i < laid), "{frames:?}");
        } else {
            assert_eq!(received.update["partial"], false);
        }
        sent.extend(frames);
    }
    let mut expected = early.clone();
    expected.extend(&next.lists);
    assert_eq!(sent.len(), expected.len(), "{sent:?}");
    assert_eq!(&sent[..2], &[0, 1], "{sent:?}");
    sent.sort_unstable();
    expected.sort_unstable();
    assert_eq!(sent, expected);
    preview.stop();
}

#[test]
fn without_a_visible_page_the_revision_is_sent_at_once() {
    let dir = long_project(30);
    let (preview, events) = start(dir.path(), |o| o.watch(false));
    let first = updated(&events, |u| u.pages > 0);
    let file = edit(
        dir.path(),
        "main.fol",
        "Sentence 0 of paragraph 0 in section 0.",
        "Sentence 0 of paragraph 0 in section 0, said again at length.",
    );
    preview.changed([file]);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.recv_deadline(deadline) {
            Ok(Event::FirstPages { .. }) => panic!("no client said what it shows"),
            Ok(Event::Updated(update)) if update.revision > first.revision => {
                assert!(update.lists.contains(&0), "{:?}", update.lists);
                break;
            }
            Ok(_) => {}
            Err(_) => panic!("no update in time"),
        }
    }
    preview.stop();
}
