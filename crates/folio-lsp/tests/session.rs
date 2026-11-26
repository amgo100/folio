//! Language server sessions (SPEC §12.3) driven over in-memory channels: a
//! client opens and edits files and sees diagnostics published while it
//! types; an edit cancels the check still running for the revision before
//! it, whose diagnostics are never published; a project that pins another
//! compiler version gets a notice; the errors of `folio.toml` are shown
//! until it is fixed on disk.

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
    next_id: i32,
}

impl Client {
    /// Starts a server on `root` and initialises it. `hook` sees every
    /// event first.
    fn start(root: &Path, hook: impl Fn(&Event) + Send + Sync + 'static) -> Self {
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
            next_id: 0,
        };
        let response = client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": uri(root),
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } },
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
        match &message {
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
        message
    }

    /// Waits for an event of the server.
    fn event(&mut self, wanted: impl Fn(&Event) -> bool) -> Event {
        loop {
            let event = self
                .events
                .recv_timeout(TIMEOUT)
                .expect("the server reports");
            if wanted(&event) {
                return event;
            }
        }
    }

    /// Waits until the check of the latest revision has been published, and
    /// returns the outcomes of every check that ended meanwhile.
    fn settle(&mut self) -> Vec<(u64, Outcome)> {
        let mut latest = 0;
        let mut outcomes = Vec::new();
        loop {
            match self.event(|_| true) {
                Event::Edited { revision } => latest = revision,
                Event::Checked { revision, outcome } => {
                    outcomes.push((revision, outcome));
                    if revision == latest && outcome == Outcome::Published {
                        break;
                    }
                }
                _ => {}
            }
        }
        // Every notification of that check is sent before the event; take
        // them in.
        while let Ok(message) = self.connection.receiver.try_recv() {
            if let Message::Notification(notification) = message {
                self.notifications.push(notification);
            }
        }
        outcomes
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
