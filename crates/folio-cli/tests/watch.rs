//! `folio watch` as a process: it serves the preview, refuses requests
//! without the session token, and writes the outputs once the project is
//! quiet.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Stops the process when the test ends, passing or not.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The status line of a GET request.
fn status(addr: &str, target: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(stream, "GET {target} HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response.lines().next().unwrap_or_default().to_owned()
}

#[test]
fn serves_the_preview_and_writes_outputs() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/exam");
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&corpus, dir.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_folio"))
        .args(["watch", "--root"])
        .arg(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let _running = Running(child);
    let mut lines = BufReader::new(stdout).lines();
    let first = lines.next().unwrap().unwrap();
    // The address holds the token, which is not printed here.
    let url = first
        .strip_prefix("Preview at http://")
        .expect("the preview's address first");
    let (addr, query) = url.split_once("/?").unwrap();
    let token = query.strip_prefix("token=").unwrap();
    assert_eq!(token.len(), 64);
    assert!(addr.starts_with("127.0.0.1:"));

    assert_eq!(status(addr, &format!("/?{query}")), "HTTP/1.1 200 OK");
    assert_eq!(status(addr, "/"), "HTTP/1.1 403 Forbidden");
    assert_eq!(status(addr, "/?token=0"), "HTTP/1.1 403 Forbidden");

    let pdf = dir.path().join("out/main-exam.pdf");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !pdf.is_file() {
        assert!(Instant::now() < deadline, "no output was written");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF-"));
}
