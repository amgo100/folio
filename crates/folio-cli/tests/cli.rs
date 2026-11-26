//! The command line (SPEC §12.1): `folio build` compiles the test corpus to
//! the outputs the determinism check recorded, prints diagnostics with
//! source excerpts, exits with an error when a build has errors, and
//! rejects what is not supported yet; `folio check` reports the same
//! diagnostics and writes nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

/// The corpus directory.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus")
}

/// Copies a directory and everything in it.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Runs `folio` in a directory.
fn folio(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_folio"))
        .args(args)
        .current_dir(dir)
        .env_remove("FOLIO_CACHE_DIR")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A project in a temporary directory, from the files given.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    dir
}

const MANIFEST: &str =
    "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";

#[test]
fn builds_the_corpus() {
    let digests = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../folio/tests/determinism/digests.txt"),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&corpus(), dir.path());
    let mut checked = 0;
    for line in digests.lines() {
        let (digest, name) = line.split_once("  ").unwrap();
        // The determinism check names outputs `{project}-{variant}.{ext}`;
        // `folio build` names them `out/{stem}-{variant}.{ext}`.
        let (project, rest) = name.split_once(['-', '.']).unwrap();
        let built = match name[project.len()..].starts_with('-') {
            true => format!("out/main-{rest}"),
            false => format!("out/main.{rest}"),
        };
        let root = dir.path().join(project);
        if !root.join(&built).exists() {
            let output = folio(&root, &["build"]);
            assert!(output.status.success(), "{project}: {}", stderr(&output));
            assert!(stderr(&output).contains(&format!("wrote {built}")));
        }
        let bytes = fs::read(root.join(&built)).unwrap();
        let actual: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(actual, digest, "{built} of {project}");
        checked += 1;
    }
    assert_eq!(checked, 6);
}

#[test]
fn reports_broken_files_and_writes_nothing() {
    let dir = project(&[
        ("folio.toml", MANIFEST),
        ("main.fol", "= Forces\n\n#box(\n"),
    ]);
    let output = folio(dir.path(), &["build"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("error: unclosed delimiter"), "{text}");
    assert!(text.contains("┌─ main.fol:3:5"), "{text}");
    assert!(text.contains("did not write out/main.pdf"), "{text}");
    assert!(!dir.path().join("out").exists());
}

#[test]
fn reports_a_missing_entry_file() {
    let dir = project(&[("folio.toml", MANIFEST)]);
    let output = folio(dir.path(), &["build"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("error: file `main.fol` not found"), "{text}");
    assert!(text.contains("folio.toml:2:9"), "{text}");
}

#[test]
fn reports_manifest_errors() {
    let dir = project(&[("folio.toml", "[project]\nentry = \"main.fol\"\n")]);
    let output = folio(dir.path(), &["build"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("folio.toml:1:1"), "{text}");
    assert!(text.contains("`edition`"), "{text}");
}

#[test]
fn today_comes_from_the_command_line() {
    let dir = project(&[("folio.toml", MANIFEST), ("main.fol", "#today.display()\n")]);
    let output = folio(dir.path(), &["build"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no date is set for `today`"));

    let output = folio(dir.path(), &["build", "--today", "2026-11-24"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(dir.path().join("out/main.pdf").exists());

    let output = folio(dir.path(), &["build", "--today", "2026-02-30"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn builds_the_chosen_variants_into_the_manifest_path() {
    let manifest = format!(
        "{MANIFEST}\n[variants.exam]\ntargets = [\"pdf\"]\n\n\
         [variants.markscheme]\ntargets = [\"pdf\"]\n\n\
         [output]\npath = \"build/{{variant}}/{{stem}}.{{ext}}\"\n"
    );
    let dir = project(&[
        ("folio.toml", &manifest),
        ("main.fol", "= Forces\n\nA ball is thrown.\n"),
        ("sub/dir/keep.txt", ""),
    ]);
    // From a subdirectory, which finds the project above it.
    let output = folio(&dir.path().join("sub/dir"), &["build", "--variant", "exam"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(dir.path().join("build/exam/main.pdf").exists());
    assert!(!dir.path().join("build/markscheme").exists());

    let output = folio(dir.path(), &["build", "--variant", "slides"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("declares no variant `slides`"));
}

#[test]
fn rejects_what_is_not_supported_yet() {
    let dir = project(&[("folio.toml", MANIFEST), ("main.fol", "Text.\n")]);
    for args in [
        &["build", "--strict"][..],
        &["build", "--strict=report"],
        &["build", "--strict-a11y"],
        &["present"],
        &["check", "--leaks"],
        &["check", "--report", "overrides"],
        &["fmt"],
    ] {
        let output = folio(dir.path(), args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).contains("is not yet supported"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert!(!dir.path().join("out").exists());

    let output = folio(dir.path(), &["--version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("folio {}", folio::VERSION)
    );
}

/// The JSON document that `folio build --diagnostics json` writes to
/// standard error, which must be all that it writes there.
fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|e| panic!("{e}: {}", stderr(output)))
}

#[test]
fn writes_diagnostics_as_json() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&corpus().join("notes"), dir.path());
    let append = |path: &str, text: &str| {
        let path = dir.path().join(path);
        let mut source = fs::read_to_string(&path).unwrap();
        source.push_str(text);
        fs::write(path, source).unwrap();
    };
    append("main.fol", "\n#box(\n");
    append("styles/notes.fsty", "\nparagraph:bogus { indent: 0pt; }\n");
    let main = fs::read_to_string(dir.path().join("main.fol")).unwrap();
    let lines = main.lines().count();

    let output = folio(dir.path(), &["build", "--diagnostics", "json"]);
    assert_eq!(output.status.code(), Some(1));
    let document = json(&output);
    assert_eq!(document["version"], 1);

    let diagnostics = document["diagnostics"].as_array().unwrap();
    let error = diagnostics
        .iter()
        .find(|d| d["severity"] == "error")
        .unwrap_or_else(|| panic!("{document:#}"));
    assert_eq!(error["message"], "unclosed delimiter");
    assert_eq!(error["code"], serde_json::Value::Null);
    let location = &error["location"];
    assert_eq!(location["path"], "main.fol");
    assert_eq!(location["start"]["line"], lines);
    assert_eq!(location["start"]["column"], 5);
    let start = location["bytes"]["start"].as_u64().unwrap() as usize;
    assert_eq!(&main[start..start + 1], "(");

    let warning = diagnostics
        .iter()
        .find(|d| d["severity"] == "warning")
        .unwrap_or_else(|| panic!("{document:#}"));
    assert_eq!(
        warning["message"],
        "unknown pseudo-class `:bogus`; the rule is ignored"
    );
    assert_eq!(warning["location"]["path"], "styles/notes.fsty");
    assert!(warning["hints"][0].as_str().unwrap().contains(":first"));

    assert_eq!(
        document["builds"],
        serde_json::json!([
            {"variant": null, "target": "pdf", "path": "out/main.pdf", "status": "skipped"}
        ])
    );
    assert!(!dir.path().join("out").exists());
}

#[test]
fn writes_json_for_builds_without_errors_and_without_a_project() {
    let dir = project(&[("folio.toml", MANIFEST), ("main.fol", "Text.\n")]);
    let output = folio(dir.path(), &["build", "--diagnostics", "json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        json(&output),
        serde_json::json!({
            "version": 1,
            "diagnostics": [],
            "builds": [
                {"variant": null, "target": "pdf", "path": "out/main.pdf", "status": "written"}
            ]
        })
    );

    // An error before there is a project points at no file.
    let empty = tempfile::tempdir().unwrap();
    let output = folio(empty.path(), &["build", "--diagnostics", "json"]);
    assert_eq!(output.status.code(), Some(1));
    let document = json(&output);
    assert_eq!(document["diagnostics"][0]["severity"], "error");
    assert_eq!(
        document["diagnostics"][0]["location"],
        serde_json::Value::Null
    );
    assert_eq!(document["builds"], serde_json::json!([]));

    let output = folio(dir.path(), &["build", "--diagnostics", "xml"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn checks_report_what_builds_report_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&corpus().join("exam"), dir.path());
    let check = folio(dir.path(), &["check"]);
    assert!(check.status.success(), "{}", stderr(&check));
    assert!(!dir.path().join("out").exists());
    let build = folio(dir.path(), &["build"]);
    assert!(build.status.success(), "{}", stderr(&build));
    // The same diagnostics, without the lines that say what was written.
    let built: String = stderr(&build)
        .lines()
        .filter(|line| !line.starts_with("wrote "))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(stderr(&check), built);

    // With errors, a check fails as a build does.
    let broken = project(&[("folio.toml", MANIFEST), ("main.fol", "A ball #box(\n")]);
    let check = folio(broken.path(), &["check"]);
    assert_eq!(check.status.code(), Some(1));
    assert!(
        stderr(&check).contains("unclosed delimiter"),
        "{}",
        stderr(&check)
    );
    let build = folio(broken.path(), &["build"]);
    assert_eq!(build.status.code(), Some(1));
    assert_eq!(
        stderr(&check),
        stderr(&build).replace(
            "did not write out/main.pdf: the build of the document (pdf) has errors\n",
            ""
        )
    );
    assert!(!broken.path().join("out").exists());
}

#[test]
fn checks_write_diagnostics_as_json() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&corpus().join("exam"), dir.path());
    let check = folio(
        dir.path(),
        &["check", "--variant", "exam", "--diagnostics", "json"],
    );
    assert!(check.status.success(), "{}", stderr(&check));
    let checked = json(&check);
    assert_eq!(checked["version"], 1);
    assert_eq!(
        checked["builds"],
        serde_json::json!([
            {"variant": "exam", "target": "pdf", "path": null, "status": "checked"},
            {"variant": "exam", "target": "docx", "path": null, "status": "checked"}
        ])
    );
    assert!(!dir.path().join("out").exists());
    let build = folio(
        dir.path(),
        &["build", "--variant", "exam", "--diagnostics", "json"],
    );
    assert_eq!(json(&build)["diagnostics"], checked["diagnostics"]);
}

/// What would stop a build from writing its output stops a check too,
/// though the check writes nothing.
#[test]
fn checks_report_outputs_that_cannot_be_written() {
    let outside = format!("{MANIFEST}\n[output]\npath = \"../{{stem}}.{{ext}}\"\n");
    let not_dir = format!("{MANIFEST}\n[output]\npath = \"out/main.pdf/x.pdf\"\n");
    for (manifest, expected) in [
        (&outside, "leaves the project"),
        (&not_dir, "is not a directory"),
    ] {
        let dir = project(&[("folio.toml", manifest), ("main.fol", "Text.\n")]);
        if manifest == &not_dir {
            fs::create_dir(dir.path().join("out")).unwrap();
            fs::write(dir.path().join("out/main.pdf"), "").unwrap();
        }
        let check = folio(dir.path(), &["check"]);
        assert_eq!(check.status.code(), Some(1), "{}", stderr(&check));
        assert!(stderr(&check).contains(expected), "{}", stderr(&check));
        let build = folio(dir.path(), &["build"]);
        assert_eq!(build.status.code(), Some(1), "{}", stderr(&build));
        assert_eq!(stderr(&check), stderr(&build));
    }
    // A check creates no directory on the way.
    let nested = format!("{MANIFEST}\n[output]\npath = \"a/b/{{stem}}.{{ext}}\"\n");
    let dir = project(&[("folio.toml", &nested), ("main.fol", "Text.\n")]);
    let check = folio(dir.path(), &["check"]);
    assert!(check.status.success(), "{}", stderr(&check));
    assert!(!dir.path().join("a").exists());
}

/// Writes a message of the language server protocol.
fn send(to: &mut impl std::io::Write, message: &serde_json::Value) {
    let body = message.to_string();
    write!(to, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    to.flush().unwrap();
}

/// Reads a message of the language server protocol.
fn receive(from: &mut impl std::io::BufRead) -> serde_json::Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(from.read_line(&mut line).unwrap() > 0, "the server hung up");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length: ") {
            length = Some(n.parse().unwrap());
        }
    }
    let mut body = vec![0; length.expect("a length")];
    from.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn serves_the_language_server_over_standard_io() {
    use std::io::BufReader;
    use std::process::Stdio;

    let dir = project(&[
        ("folio.toml", MANIFEST),
        ("main.fol", "= Forces\n\n#nope\n"),
    ]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_folio"))
        .args(["lsp", "--stdio", "--root"])
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .env_remove("FOLIO_CACHE_DIR")
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let request = |id: i32, method: &str, params: serde_json::Value| serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let notification = |method: &str, params: serde_json::Value| serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params });

    send(
        &mut stdin,
        &request(1, "initialize", serde_json::json!({ "capabilities": {} })),
    );
    let initialized = receive(&mut stdout);
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "folio");
    send(
        &mut stdin,
        &notification("initialized", serde_json::json!({})),
    );

    // The file on disk is checked without being opened.
    loop {
        let message = receive(&mut stdout);
        if message["method"] == "textDocument/publishDiagnostics" {
            let uri = message["params"]["uri"].as_str().unwrap();
            assert!(
                uri.starts_with("file://") && uri.ends_with("/main.fol"),
                "{uri}"
            );
            let message = &message["params"]["diagnostics"][0]["message"];
            assert_eq!(message, "unknown variable: nope");
            break;
        }
    }

    send(&mut stdin, &request(2, "shutdown", serde_json::Value::Null));
    while receive(&mut stdout)["id"] != 2 {}
    send(&mut stdin, &notification("exit", serde_json::Value::Null));
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

/// `folio check` shows the serious layout diagnostics, `folio check
/// --layout` all of them; `folio build` shows what plain `folio check`
/// does.
#[test]
fn folio_check_layout_shows_every_layout_diagnostic() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../folio/tests/check/layout");
    let dir = tempfile::tempdir().unwrap();
    for file in ["folio.toml", "main.fol", "styles/main.fsty"] {
        let to = dir.path().join(file);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(fixture.join(file), to).unwrap();
    }
    let codes = |args: &[&str]| -> Vec<String> {
        let output = Command::new(env!("CARGO_BIN_EXE_folio"))
            .args(args)
            .args(["--diagnostics", "json"])
            .current_dir(dir.path())
            .env_remove("FOLIO_CACHE_DIR")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        json["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d["code"].as_str().map(str::to_string))
            .collect()
    };
    let plain = codes(&["check"]);
    assert_eq!(plain, ["page-overflow", "overfull-line"]);
    let all = codes(&["check", "--layout"]);
    for code in folio::layout::DETAIL_CODES {
        assert!(all.iter().any(|c| c == code), "{code}: {all:?}");
    }
    assert_eq!(all.len(), plain.len() + folio::layout::DETAIL_CODES.len());
    assert_eq!(codes(&["build"]), plain);
}
