//! The command line (SPEC §12.1): `folio build` compiles the test corpus to
//! the outputs the determinism check recorded, prints diagnostics with
//! source excerpts, exits with an error when a build has errors, and
//! rejects what is not supported yet; `folio check` reports the same
//! diagnostics and writes nothing; `folio fmt` formats a project's sources
//! without changing what they mean.

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
        // The determinism check names outputs
        // `{project}-{variant}-{version}.{ext}`; `folio build` names them
        // `out/{stem}-{variant}-{version}.{ext}`.
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
    assert_eq!(checked, 19);
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

/// SPEC §12.1: a reader that stops early, as `head` does, closes the pipe
/// that standard error writes to. The command stops writing and exits with
/// the status of the check, 1 for errors, not 101 for a crash (#343).
#[test]
fn a_closed_pipe_ends_the_report_quietly() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let entry: Vec<String> = (0..3000).map(|i| format!("#nothing{i}")).collect();
    let dir = project(&[("folio.toml", MANIFEST), ("main.fol", &entry.join("\n\n"))]);
    for format in ["human", "json"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_folio"))
            .args(["check", "--diagnostics", format])
            .current_dir(dir.path())
            .env_remove("FOLIO_CACHE_DIR")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stderr.take().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        assert!(!first.is_empty(), "{format}");
        // The report is far larger than a pipe holds, so writing the rest
        // finds the pipe closed.
        drop(reader);
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(1), "{format}");
    }
}

/// A source that is not UTF-8 is an error where it is used, which says
/// why it was not read, not that it is missing (#343).
#[test]
fn reports_why_a_source_was_not_read() {
    let dir = project(&[("folio.toml", MANIFEST)]);
    // Latin-1, as an old editor might save it.
    fs::write(dir.path().join("main.fol"), b"= Si\xb3y\n").unwrap();
    let output = folio(dir.path(), &["check"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains("error: cannot read `main.fol`: it is not UTF-8 text"),
        "{text}"
    );
    assert!(text.contains("folio.toml:2:9"), "{text}");
    assert!(!text.contains("not found"), "{text}");

    let output = folio(dir.path(), &["check", "--diagnostics", "json"]);
    let document = json(&output);
    let diagnostics = document["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{document}");
    assert_eq!(
        diagnostics[0]["message"],
        "cannot read `main.fol`: it is not UTF-8 text"
    );
    assert_eq!(diagnostics[0]["location"]["path"], "folio.toml");

    // `folio fmt` builds nothing, so it says itself why it left the file.
    let output = folio(dir.path(), &["fmt", "--check"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stderr(&output);
    assert!(
        text.contains("warning: cannot read `main.fol`: it is not UTF-8 text"),
        "{text}"
    );
}

/// A `.fol` link that leads out of the project is an error where it is
/// imported, which says so (#343).
#[cfg(unix)]
#[test]
fn reports_a_source_link_out_of_the_project() {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("q1.fol"), "= Question\n").unwrap();
    let dir = project(&[
        ("folio.toml", MANIFEST),
        ("main.fol", "#include \"q1.fol\"\n"),
    ]);
    std::os::unix::fs::symlink(outside.path().join("q1.fol"), dir.path().join("q1.fol")).unwrap();
    let output = folio(dir.path(), &["check"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains("error: cannot read `q1.fol`: it is a link that leads out of the project"),
        "{text}"
    );
    assert!(text.contains("main.fol:1:10"), "{text}");

    // So is reading it as text.
    fs::write(dir.path().join("main.fol"), "#read(\"q1.fol\")\n").unwrap();
    let text = stderr(&folio(dir.path(), &["check"]));
    assert!(
        text.contains("error: cannot read `q1.fol`: it is a link that leads out of the project"),
        "{text}"
    );
    assert!(text.contains("main.fol:1:7"), "{text}");
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
        &["promote"],
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
            {"variant": null, "version": null, "target": "pdf", "path": "out/main.pdf", "status": "skipped"}
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
                {"variant": null, "version": null, "target": "pdf", "path": "out/main.pdf", "status": "written"}
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
            {"variant": "exam", "version": null, "target": "pdf", "path": null, "status": "checked"},
            {"variant": "exam", "version": null, "target": "docx", "path": null, "status": "checked"}
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

/// An output never replaces a file of the project: the manifest, the
/// lockfile, a source or a hidden file is refused by the manifest, and a
/// file the build reads by `folio build` and `folio check`.
#[test]
fn outputs_never_replace_project_files() {
    let lockfile = "version = 1\n";
    for (output, expected) in [
        (
            "folio.toml",
            "the output `folio.toml` would replace the manifest",
        ),
        (
            "folio.lock",
            "the output `folio.lock` would replace the lockfile",
        ),
        ("notes.fol", "the output `notes.fol` is a content file"),
        (".git/HEAD", "the output `.git/HEAD` is a hidden path"),
        ("main.fol", "the output `main.fol` is the entry file"),
        ("{stem}.fol", "the output `main.fol` is the entry file"),
        (
            "keep.txt",
            "cannot write `keep.txt`: a build reads `keep.txt` as a data file",
        ),
    ] {
        let manifest = format!("{MANIFEST}\n[output]\npath = \"{output}\"\n");
        let files = [
            ("folio.toml", manifest.as_str()),
            ("folio.lock", lockfile),
            ("main.fol", "Text #read(\"keep.txt\").\n"),
            ("notes.fol", "Notes.\n"),
            ("keep.txt", "kept"),
            (".git/HEAD", "ref: refs/heads/main\n"),
        ];
        let dir = project(&files);
        for command in ["check", "build"] {
            let output = folio(dir.path(), &[command]);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{output:?}: {}",
                stderr(&output)
            );
            assert!(
                stderr(&output).contains(expected),
                "{command} {output:?}: {}",
                stderr(&output)
            );
            assert!(!stderr(&output).contains("wrote"), "{}", stderr(&output));
        }
        for (path, text) in files {
            assert_eq!(
                fs::read_to_string(dir.path().join(path)).unwrap(),
                text,
                "{path}"
            );
        }
    }
}

/// Manifest keys that have no effect yet are reported, not ignored: strict
/// mode and numbering groups warn, and a declared component that cannot be
/// loaded fails the build (#330).
#[test]
fn reports_manifest_keys_that_have_no_effect_yet() {
    let strict = format!("{MANIFEST}\n[formatting]\nmode = \"strict\"\n");
    let dir = project(&[("folio.toml", &strict), ("main.fol", "Text.\n")]);
    for command in ["check", "build"] {
        let output = folio(dir.path(), &[command]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(
            stderr(&output).contains("strict mode is not implemented yet"),
            "{}",
            stderr(&output)
        );
    }

    let declared = format!("{MANIFEST}\n[components]\ncircuits = {{ version = \"0.3\" }}\n");
    let dir = project(&[("folio.toml", &declared), ("main.fol", "Text.\n")]);
    for command in ["check", "build"] {
        let output = folio(dir.path(), &[command]);
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        assert!(
            stderr(&output).contains("component `circuits` cannot be loaded"),
            "{}",
            stderr(&output)
        );
    }
}

/// The `.fol` and `.fsty` files under `dir`, by path relative to it.
fn sources(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type().unwrap().is_dir() {
            sources(&entry.path(), &format!("{name}/"), out);
        } else if name.ends_with(".fol") || name.ends_with(".fsty") {
            out.push((name, fs::read_to_string(entry.path()).unwrap()));
        }
    }
}

/// Changes whitespace in a source without changing what it means: trailing
/// spaces, blank lines, `\r\n` line ends, and spacing in stylesheets; raw
/// text keeps its whitespace and line ends.
fn untidy(path: &str, text: &str) -> String {
    let mut out = String::new();
    let mut raw = false;
    for line in text.lines() {
        // Raw text keeps its whitespace.
        if line.trim_start().starts_with("```") {
            raw = !raw;
        }
        // Outside comments and strings.
        if path.ends_with(".fsty") && !line.trim_start().starts_with("//") && !line.contains('"') {
            out.push_str(&line.replace("; ", ";   ").replace(": ", " :"));
        } else {
            out.push_str(line);
        }
        out.push_str(match (raw, line.is_empty()) {
            (true, _) => "\n",
            (false, true) => "  \r\n\n",
            (false, false) => "  \r\n",
        });
    }
    out
}

#[test]
fn formats_the_corpus_without_changing_its_outputs() {
    for name in ["exam", "notes", "polish"] {
        let tidy = tempfile::tempdir().unwrap();
        let messy = tempfile::tempdir().unwrap();
        copy_dir(&corpus().join(name), tidy.path());
        copy_dir(&corpus().join(name), messy.path());
        let mut files = Vec::new();
        sources(messy.path(), "", &mut files);
        for (path, text) in &files {
            fs::write(messy.path().join(path), untidy(path, text)).unwrap();
        }

        // `--check` reports every untidy file, with the edits that tidy it,
        // and changes nothing.
        let output = folio(messy.path(), &["fmt", "--check", "--diagnostics", "json"]);
        assert_eq!(output.status.code(), Some(1), "{name}: {}", stderr(&output));
        let document = json(&output);
        let diagnostics = document["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), files.len(), "{name}: {document:#}");
        for diagnostic in diagnostics {
            assert_eq!(diagnostic["code"], "unformatted");
            assert!(
                !diagnostic["fixes"][0]["edits"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        for (path, text) in &files {
            let now = fs::read_to_string(messy.path().join(path)).unwrap();
            assert_eq!(now, untidy(path, text), "{name}/{path} changed");
        }

        // Formatting gives what formatting the corpus gives, and the same
        // outputs.
        for dir in [tidy.path(), messy.path()] {
            let output = folio(dir, &["fmt"]);
            assert!(output.status.success(), "{name}: {}", stderr(&output));
            let output = folio(dir, &["fmt", "--check"]);
            assert!(output.status.success(), "{name}: {}", stderr(&output));
            assert!(stderr(&output).is_empty(), "{name}: {}", stderr(&output));
        }
        let (mut a, mut b) = (Vec::new(), Vec::new());
        sources(tidy.path(), "", &mut a);
        sources(messy.path(), "", &mut b);
        assert_eq!(a, b, "{name}");

        let built = |dir: &Path| {
            let output = folio(dir, &["build", "--diagnostics", "json"]);
            assert!(output.status.success(), "{name}: {}", stderr(&output));
            let mut digests = Vec::new();
            for build in json(&output)["builds"].as_array().unwrap() {
                if build["status"] == "written"
                    && let Some(path) = build["path"].as_str()
                {
                    let bytes = fs::read(dir.join(path)).unwrap();
                    digests.push((path.to_owned(), Sha256::digest(bytes).to_vec()));
                }
            }
            digests
        };
        let original = tempfile::tempdir().unwrap();
        copy_dir(&corpus().join(name), original.path());
        assert_eq!(built(messy.path()), built(original.path()), "{name}");
    }
}

#[test]
fn formatting_leaves_files_with_syntax_errors() {
    let broken = "= Forces\n#box(  \n";
    let dir = project(&[
        ("folio.toml", MANIFEST),
        ("main.fol", "#include \"part.fol\"   \n"),
        ("part.fol", broken),
        ("style.fsty", "a{b:c}"),
    ]);
    let output = folio(dir.path(), &["fmt", "--color", "never"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("part.fol:2:5"), "{text}");
    assert!(text.contains("leaves part.fol as it is"), "{text}");
    assert!(text.contains("formatted main.fol"), "{text}");
    assert!(text.contains("formatted style.fsty"), "{text}");
    let read = |path| fs::read_to_string(dir.path().join(path)).unwrap();
    assert_eq!(read("part.fol"), broken);
    assert_eq!(read("main.fol"), "#include \"part.fol\"\n");
    assert_eq!(read("style.fsty"), "a {\n  b: c\n}\n");
}

#[cfg(unix)]
#[test]
fn formatting_formats_a_linked_file_once() {
    let dir = project(&[
        ("folio.toml", MANIFEST),
        ("main.fol", "#include \"parts/a.fol\"\n"),
        ("parts/a.fol", "#f(x:1)\n"),
    ]);
    std::os::unix::fs::symlink("parts", dir.path().join("chapters")).unwrap();
    std::os::unix::fs::symlink("a.fol", dir.path().join("parts/b.fol")).unwrap();
    let output = folio(dir.path(), &["fmt", "--color", "never"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stderr(&output).matches("formatted").count(),
        1,
        "{}",
        stderr(&output)
    );
    let text = fs::read_to_string(dir.path().join("parts/a.fol")).unwrap();
    assert_eq!(text, "#f(x: 1)\n");
    assert!(
        fs::symlink_metadata(dir.path().join("parts/b.fol"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn formatting_keeps_byte_order_marks_and_permissions() {
    let dir = project(&[
        ("folio.toml", MANIFEST),
        ("main.fol", "\u{FEFF}= Forces  \n"),
        ("style.fsty", "a{b:c}"),
        ("locked.fsty", "a{b:c}"),
    ]);
    let path = |name: &str| dir.path().join(name);
    let mut locked = fs::metadata(path("locked.fsty")).unwrap().permissions();
    locked.set_readonly(true);
    fs::set_permissions(path("locked.fsty"), locked).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path("style.fsty"), fs::Permissions::from_mode(0o600)).unwrap();
    }

    let output = folio(dir.path(), &["fmt", "--color", "never"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("cannot write locked.fsty: it is read-only"));
    let read = |name: &str| fs::read_to_string(path(name)).unwrap();
    assert_eq!(read("main.fol"), "\u{FEFF}= Forces\n");

    // A file with a byte order mark gets no fix, whose byte ranges would
    // not count the mark.
    fs::write(path("main.fol"), "\u{FEFF}= Forces  \n").unwrap();
    let output = folio(dir.path(), &["fmt", "--check", "--diagnostics", "json"]);
    let document = json(&output);
    let unformatted: Vec<_> = document["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "unformatted" && d["location"]["path"] == "main.fol")
        .collect();
    assert_eq!(unformatted.len(), 1, "{document:#}");
    assert!(unformatted[0]["fixes"].as_array().unwrap().is_empty());
    assert_eq!(read("style.fsty"), "a {\n  b: c\n}\n");
    assert_eq!(read("locked.fsty"), "a{b:c}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path("style.fsty"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
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
