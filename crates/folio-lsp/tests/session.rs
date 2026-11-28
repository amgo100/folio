//! Language server sessions (SPEC §12.3) driven over in-memory channels: a
//! client opens and edits files and sees diagnostics published while it
//! types; an edit cancels the check still running for the revision before
//! it, whose diagnostics are never published; a project that pins another
//! compiler version gets a notice; the errors of `folio.toml` are shown
//! until it is fixed on disk.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use folio_lsp::{Event, Options, Outcome};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::{Value, json};

/// How long to wait for any one message before failing.
const TIMEOUT: Duration = Duration::from_secs(120);

fn manifest(pin: &str) -> String {
    format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"{pin}\"\nlang = \"en-GB\"\n"
    )
}

/// A project directory with `folio.toml` and `main.fol`.
fn project(manifest: &str, main: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("folio.toml"), manifest).unwrap();
    fs::write(dir.path().join("main.fol"), main).unwrap();
    dir
}

fn uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    let text = path.to_str().unwrap().replace('\\', "/");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                uri.push(char::from(byte));
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// The client's end of a session with a server running on a thread named
/// `server`.
struct Client {
    connection: Connection,
    server: Option<JoinHandle<Result<(), folio_lsp::Error>>>,
    /// The server's events, except the world's.
    events: Receiver<Event>,
    /// Notifications the server sent, in order.
    notifications: Vec<Notification>,
    /// `window/showDocument` requests the server sent, in order.
    shown: Vec<Value>,
    next_id: i32,
    /// The revision the server checks next, from its last `Edited` event.
    latest: u64,
    /// Whether the server has changed the world since [`Self::settle`] last
    /// returned. It starts true: starting the server loads the world.
    edited: bool,
    /// The revisions whose checks have started and not yet ended.
    running: BTreeSet<u64>,
    /// The checks that ended since [`Self::settle`] last returned.
    ended: Vec<(u64, Outcome)>,
}

impl Client {
    /// Starts a server on `root` and initialises it. `hook` sees every
    /// event first.
    fn start(root: &Path, hook: impl Fn(&Event) + Send + Sync + 'static) -> Self {
        Self::start_with(root, json!({}), hook)
    }

    /// [`Self::start`] for a client with these window capabilities.
    fn start_with(
        root: &Path,
        window: Value,
        hook: impl Fn(&Event) + Send + Sync + 'static,
    ) -> Self {
        let (events_tx, events) = crossbeam_channel::unbounded();
        let options = Options::default()
            .system_fonts(false)
            .on_event(move |event| {
                hook(event);
                if !matches!(event, Event::World(_)) {
                    let _ = events_tx.send(event.clone());
                }
            });
        let (server_end, connection) = Connection::memory();
        let server = thread::Builder::new()
            .name("server".into())
            .spawn(move || folio_lsp::run(&server_end, options))
            .unwrap();
        let mut client = Self {
            connection,
            server: Some(server),
            events,
            notifications: Vec::new(),
            shown: Vec::new(),
            next_id: 0,
            latest: 0,
            edited: true,
            running: BTreeSet::new(),
            ended: Vec::new(),
        };
        let response = client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": uri(root),
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } },
                    "window": window,
                },
            }),
        );
        let result = response.response_result.unwrap();
        assert_eq!(result["capabilities"]["positionEncoding"], "utf-16");
        assert_eq!(result["serverInfo"]["name"], "folio");
        client.notify("initialized", json!({}));
        client
    }

    fn notify(&self, method: &str, params: Value) {
        self.connection
            .sender
            .send(Notification::new(method.into(), params).into())
            .unwrap();
    }

    /// Sends a request and waits for its response, keeping the
    /// notifications that come before it.
    fn request(&mut self, method: &str, params: Value) -> Response {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.connection
            .sender
            .send(Request::new(id.clone(), method.into(), params).into())
            .unwrap();
        loop {
            match self.recv() {
                Message::Response(response) if response.id == id => return response,
                _ => {}
            }
        }
    }

    /// The next message, answering the server's requests and keeping
    /// notifications.
    fn recv(&mut self) -> Message {
        let message = self
            .connection
            .receiver
            .recv_timeout(TIMEOUT)
            .expect("the server answers");
        self.take(&message);
        message
    }

    /// Answers a request of the server, or keeps a notification.
    fn take(&mut self, message: &Message) {
        match message {
            Message::Request(request) if request.method == "window/showDocument" => {
                self.shown.push(request.params.clone());
                self.connection
                    .sender
                    .send(Response::new_ok(request.id.clone(), json!({ "success": true })).into())
                    .unwrap();
            }
            Message::Request(request) => {
                assert_eq!(request.method, "client/registerCapability");
                self.connection
                    .sender
                    .send(Response::new_ok(request.id.clone(), Value::Null).into())
                    .unwrap();
            }
            Message::Notification(notification) => self.notifications.push(notification.clone()),
            Message::Response(_) => {}
        }
    }

    /// Waits for an event of the server. Every event it reads, wanted or
    /// not, is accounted for in the checks [`Self::settle`] waits for.
    fn event(&mut self, wanted: impl Fn(&Event) -> bool) -> Event {
        loop {
            let Ok(event) = self.events.recv_timeout(TIMEOUT) else {
                panic!(
                    "the server reports (checks still running: {:?})",
                    self.running
                );
            };
            match event {
                Event::Edited { revision } => {
                    self.latest = revision;
                    self.edited = true;
                }
                Event::Started { revision } => {
                    self.running.insert(revision);
                }
                Event::Checked { revision, outcome } => {
                    self.running.remove(&revision);
                    self.ended.push((revision, outcome));
                }
                _ => {}
            }
            if wanted(&event) {
                return event;
            }
        }
    }

    /// Waits until the server has changed the world since the last call, the
    /// check of the latest revision has been published and every check that
    /// started has ended, and returns the outcomes of the checks that ended
    /// since the last call.
    ///
    /// A cancelled check's thread drops its world handle, which lets the
    /// next edit through, before it sends its result to the server, so the
    /// server can emit its `Checked` after that of the check that overtook
    /// it: waiting for the latest revision alone would miss it.
    fn settle(&mut self) -> Vec<(u64, Outcome)> {
        loop {
            let published = self.ended.contains(&(self.latest, Outcome::Published));
            if self.edited && published && self.running.is_empty() {
                break;
            }
            self.event(|_| true);
        }
        // Every notification of that check is sent before the event; take
        // them in.
        while let Ok(message) = self.connection.receiver.try_recv() {
            self.take(&message);
        }
        self.edited = false;
        std::mem::take(&mut self.ended)
    }

    /// The diagnostics published last for a file, if any were.
    fn diagnostics(&self, uri: &str) -> Option<Vec<Value>> {
        self.published(uri).last().cloned()
    }

    /// Every list of diagnostics published for a file, in order.
    fn published(&self, uri: &str) -> Vec<Vec<Value>> {
        self.notifications
            .iter()
            .filter(|n| n.method == "textDocument/publishDiagnostics")
            .filter(|n| n.params["uri"] == uri)
            .map(|n| n.params["diagnostics"].as_array().unwrap().clone())
            .collect()
    }

    fn open(&self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": { "uri": uri, "languageId": "folio", "version": 1, "text": text },
            }),
        );
    }

    fn change(&self, uri: &str, version: i32, range: [u32; 4], text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{
                    "range": {
                        "start": { "line": range[0], "character": range[1] },
                        "end": { "line": range[2], "character": range[3] },
                    },
                    "text": text,
                }],
            }),
        );
    }

    /// Shuts the server down, which then returns.
    fn shut_down(mut self) {
        let response = self.request("shutdown", Value::Null);
        assert!(response.response_result.is_ok());
        self.notify("exit", Value::Null);
        self.server.take().unwrap().join().unwrap().unwrap();
    }
}

const VALID: &str = "= Forces\n\nA ball is thrown.\n";

#[test]
fn diagnostics_follow_typing() {
    let dir = project(&manifest(folio::VERSION), VALID);
    let main = uri(&dir.path().join("main.fol"));
    let mut client = Client::start(dir.path(), |_| {});
    client.settle();
    assert_eq!(client.diagnostics(&main), None, "a valid project has none");

    // The editor's buffer differs from the disk and is what is checked.
    client.open(&main, VALID);
    client.settle();
    client.change(&main, 2, [2, 17, 2, 17], "\n\n#nope\n");
    client.settle();
    let found = client
        .diagnostics(&main)
        .expect("diagnostics are published");
    assert_eq!(found.len(), 1, "{found:#?}");
    let error = &found[0];
    assert_eq!(error["severity"], 1);
    assert_eq!(error["source"], "folio");
    assert_eq!(
        error["range"]["start"],
        json!({ "line": 4, "character": 1 })
    );
    assert_eq!(error["range"]["end"], json!({ "line": 4, "character": 5 }));
    let version = client
        .notifications
        .iter()
        .rev()
        .find(|n| n.method == "textDocument/publishDiagnostics" && n.params["uri"] == main)
        .unwrap()
        .params["version"]
        .clone();
    assert_eq!(version, 2);

    // Fixing it clears them.
    client.change(&main, 3, [4, 0, 4, 5], "");
    client.settle();
    assert_eq!(client.diagnostics(&main), Some(Vec::new()));

    // Closing the file goes back to the disk, which has no error.
    client.change(&main, 4, [0, 0, 0, 0], "#nope\n");
    client.settle();
    assert_eq!(client.diagnostics(&main).unwrap().len(), 1);
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": main } }),
    );
    client.settle();
    assert_eq!(client.diagnostics(&main), Some(Vec::new()));

    // Requests other than shutdown are not supported yet.
    let response = client.request("textDocument/hover", json!({}));
    assert!(response.response_result.is_err());
    client.shut_down();
}

/// A gate that holds the first query to run after it is armed until an
/// edit asks the running queries to stop.
#[derive(Default)]
struct Gate {
    armed: AtomicBool,
    /// Whether a query is held, and whether it has been let go.
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Gate {
    fn hook(self: &Arc<Self>, held: Sender<()>) -> impl Fn(&Event) + Send + Sync + 'static {
        let gate = self.clone();
        move |event| {
            let Event::World(event) = event else { return };
            match event {
                // Queries run on the check's threads, never the server's.
                folio::Event::Execute { .. }
                    if thread::current().name() != Some("server")
                        && gate.armed.swap(false, Ordering::SeqCst) =>
                {
                    let mut state = gate.state.lock().unwrap();
                    state.0 = true;
                    held.send(()).unwrap();
                    let (state, timeout) = gate
                        .changed
                        .wait_timeout_while(state, TIMEOUT, |state| !state.1)
                        .unwrap();
                    assert!(!timeout.timed_out(), "an edit lets the query go");
                    drop(state);
                }
                folio::Event::Cancel => {
                    let mut state = gate.state.lock().unwrap();
                    if state.0 {
                        state.1 = true;
                        gate.changed.notify_all();
                    }
                }
                _ => {}
            }
        }
    }
}

#[test]
fn an_edit_cancels_the_stale_check() {
    let dir = project(&manifest(folio::VERSION), VALID);
    let main = uri(&dir.path().join("main.fol"));
    let gate = Arc::new(Gate::default());
    let (held_tx, held) = crossbeam_channel::unbounded();
    let mut client = Client::start(dir.path(), gate.hook(held_tx));
    client.open(&main, VALID);
    client.settle();

    // An error whose check is held inside a query...
    gate.armed.store(true, Ordering::SeqCst);
    client.change(&main, 2, [2, 17, 2, 17], "\n\n#nope\n");
    held.recv_timeout(TIMEOUT).expect("the check runs a query");
    let Event::Started { revision: stale } = client.event(|e| matches!(e, Event::Started { .. }))
    else {
        unreachable!()
    };

    // ...is fixed before the check ends: the edit cancels it, and its
    // diagnostics are never published.
    client.change(&main, 3, [4, 0, 4, 5], "");
    let outcomes = client.settle();
    assert!(
        outcomes.contains(&(stale, Outcome::Cancelled)),
        "{outcomes:?}"
    );
    assert!(
        client.published(&main).iter().all(Vec::is_empty),
        "{:#?}",
        client.published(&main)
    );
    client.shut_down();
}

#[test]
fn another_pin_gets_a_notice() {
    let dir = project(&manifest("9.9.9"), VALID);
    let mut client = Client::start(dir.path(), |_| {});
    client.settle();
    let notice = client
        .notifications
        .iter()
        .find(|n| n.method == "window/showMessage")
        .expect("a notice");
    assert_eq!(notice.params["type"], 2);
    let message = notice.params["message"].as_str().unwrap();
    assert!(message.contains("9.9.9"), "{message}");
    assert!(message.contains(folio::VERSION), "{message}");
    client.shut_down();
}

#[test]
fn manifest_errors_are_shown_until_fixed() {
    let dir = project("[project]\nentry = 3\n", VALID);
    let toml = uri(&dir.path().join("folio.toml"));
    let mut client = Client::start(dir.path(), |_| {});
    client.settle();
    let found = client.diagnostics(&toml).expect("the manifest's errors");
    assert!(!found.is_empty());
    assert!(found.iter().all(|d| d["severity"] == 1), "{found:#?}");

    fs::write(dir.path().join("folio.toml"), manifest(folio::VERSION)).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": toml, "type": 2 }] }),
    );
    client.settle();
    assert_eq!(client.diagnostics(&toml), Some(Vec::new()));

    // An error in a file on disk that is not open is shown too.
    let main = uri(&dir.path().join("main.fol"));
    fs::write(dir.path().join("main.fol"), "#nope\n").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": main, "type": 2 }] }),
    );
    client.settle();
    assert_eq!(client.diagnostics(&main).unwrap().len(), 1);
    client.shut_down();
}

#[test]
fn exiting_without_shutting_down_is_a_failure() {
    let dir = project(&manifest(folio::VERSION), VALID);
    let mut client = Client::start(dir.path(), |_| {});
    client.settle();
    client.notify("exit", Value::Null);
    let result = client.server.take().unwrap().join().unwrap();
    assert!(result.is_err());
}

#[test]
fn notifications_between_shutdown_and_exit_are_ignored() {
    let dir = project(&manifest(folio::VERSION), VALID);
    let main = uri(&dir.path().join("main.fol"));
    let mut client = Client::start(dir.path(), |_| {});
    client.open(&main, VALID);
    client.settle();
    let response = client.request("shutdown", Value::Null);
    assert!(response.response_result.is_ok());
    // Editors close their files as they quit.
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": main } }),
    );
    let response = client.request("textDocument/hover", json!({}));
    assert!(response.response_result.is_err());
    client.notify("exit", Value::Null);
    client.server.take().unwrap().join().unwrap().unwrap();
}

/// The notices of the pin a client was shown.
fn notices(client: &Client) -> Vec<String> {
    client
        .notifications
        .iter()
        .filter(|n| n.method == "window/showMessage")
        .map(|n| n.params["message"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_changed_pin_gets_a_notice() {
    let dir = project(&manifest(folio::VERSION), VALID);
    let toml = uri(&dir.path().join("folio.toml"));
    let mut client = Client::start(dir.path(), |_| {});
    client.settle();
    assert_eq!(notices(&client), Vec::<String>::new());

    // Through a file event...
    fs::write(dir.path().join("folio.toml"), manifest("9.9.9")).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": toml, "type": 2 }] }),
    );
    client.settle();
    let found = notices(&client);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("9.9.9"), "{found:?}");

    // ...and through a save, for a client that watches no files.
    fs::write(dir.path().join("folio.toml"), manifest("9.9.8")).unwrap();
    client.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": toml } }),
    );
    client.settle();
    let found = notices(&client);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[1].contains("9.9.8"), "{found:?}");
    client.shut_down();
}

/// Reads the preview's websocket until a text message of `kind`.
fn ws_message(
    ws: &mut tokio_tungstenite::tungstenite::WebSocket<std::net::TcpStream>,
    kind: &str,
) -> Value {
    use tokio_tungstenite::tungstenite::Message as Ws;
    loop {
        if let Ws::Text(text) = ws.read().unwrap() {
            let value: Value = serde_json::from_str(&text).unwrap();
            if value["type"] == kind {
                return value;
            }
        }
    }
}

#[test]
fn the_preview_follows_the_cursor_and_clicks_open_the_source() {
    // The cursor in `ball` on the third line.
    preview_round_trip("", VALID.find("ball").unwrap() + 2, 2, 4);
}

/// In a file that starts with a byte order mark, which the editor does not
/// show and the loader leaves out of the text, the cursor and clicks on the
/// first line land on the same character (#468).
#[test]
fn the_preview_round_trip_skips_a_byte_order_mark() {
    // The cursor in `Forces` on the first line.
    preview_round_trip("\u{FEFF}", VALID.find("Forces").unwrap() + 2, 0, 4);
}

/// Opens the preview of a project whose `main.fol` is `VALID` after
/// `prefix`, reveals the cursor at `line` and `character`, which is at byte
/// `at` of the text, in it, and clicks the glyph it marks, which must show
/// the source at the same character.
fn preview_round_trip(prefix: &str, at: usize, line: u32, character: u32) {
    use tokio_tungstenite::tungstenite::{self, Message as Ws};

    let dir = project(&manifest(folio::VERSION), &format!("{prefix}{VALID}"));
    let main = uri(&dir.path().join("main.fol"));
    let mut client = Client::start_with(
        dir.path(),
        json!({ "showDocument": { "support": true } }),
        |_| {},
    );
    client.settle();

    // Before the preview runs, the cursor goes nowhere.
    client.notify(
        "folio/reveal",
        json!({ "textDocument": { "uri": main }, "position": { "line": 2, "character": 4 } }),
    );

    let response = client.request(
        "workspace/executeCommand",
        json!({ "command": "folio.preview", "arguments": [] }),
    );
    let url = response.response_result.unwrap()["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    // Asking again answers the same preview.
    let again = client.request(
        "workspace/executeCommand",
        json!({ "command": "folio.preview" }),
    );
    assert_eq!(again.response_result.unwrap()["url"], url.as_str());
    let unknown = client.request(
        "workspace/executeCommand",
        json!({ "command": "folio.nope" }),
    );
    assert!(unknown.response_result.is_err());

    let (addr, token) = url
        .strip_prefix("http://")
        .unwrap()
        .split_once("/?token=")
        .unwrap();
    let stream = std::net::TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let (mut ws, _) =
        tungstenite::client::client(format!("ws://{addr}/ws?token={token}"), stream).unwrap();
    let revision = loop {
        let update = ws_message(&mut ws, "update");
        if update["pages"].as_u64().unwrap() > 0 {
            break update["revision"].as_u64().unwrap();
        }
    };

    // One glyph, marked in the preview, once the preview has indexed its
    // pages, which follows them.
    let mut revealed;
    let mut tries = 0;
    loop {
        client.notify(
            "folio/reveal",
            json!({ "textDocument": { "uri": main }, "position": { "line": line, "character": character } }),
        );
        revealed = client.event(|e| matches!(e, Event::Revealed { .. }));
        tries += 1;
        if !matches!(revealed, Event::Revealed { areas: 0, .. }) || tries == 100 {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        revealed,
        Event::Revealed {
            path: "main.fol".into(),
            bytes: at..at,
            areas: 1,
        }
    );
    let reveal = ws_message(&mut ws, "reveal");
    let rect: Vec<f64> = serde_json::from_value(reveal["areas"][0]["rect"].clone()).unwrap();

    // A click on that glyph opens the source at its character.
    let click = json!({
        "type": "click", "revision": revision, "page": 0,
        "x": rect[0] + rect[2] / 2.0, "y": rect[1] + rect[3] / 2.0,
    });
    ws.send(Ws::text(click.to_string())).unwrap();
    // Its bytes count in the text, without a byte order mark.
    let source = ws_message(&mut ws, "source");
    assert_eq!(source["location"]["bytes"]["start"], at, "{source}");
    client.event(|e| matches!(e, Event::ShowSource { .. }));
    while client.shown.is_empty() {
        client.recv();
    }
    let shown = &client.shown[0];
    assert_eq!(shown["uri"], main.as_str());
    assert_eq!(shown["takeFocus"], true);
    assert_eq!(
        shown["selection"],
        json!({
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character + 1 },
        })
    );
    drop(ws);
    client.shut_down();
}

/// Every version of a shuffled paper is checked (SPEC §6.4): an error that
/// only one version gives reaches the editor.
#[test]
fn every_version_is_checked() {
    let manifest = format!("{}versions = [\"A\", \"B\"]\n", manifest(folio::VERSION));
    let dir = project(&manifest, "= Forces\n\n#if version == \"B\" [#nope]\n");
    let main = uri(&dir.path().join("main.fol"));
    let mut client = Client::start(dir.path(), |_| {});
    client.settle();
    let found = client
        .diagnostics(&main)
        .expect("diagnostics are published");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0]["severity"], 1);
    assert_eq!(
        found[0]["range"]["start"],
        json!({ "line": 2, "character": 21 })
    );
    client.shut_down();
}
