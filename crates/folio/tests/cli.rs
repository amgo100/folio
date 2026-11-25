//! The command line (SPEC §12.1): `folio build` compiles the test corpus to
//! the outputs the determinism check recorded, prints diagnostics with
//! source excerpts, exits with an error when a build has errors, and
//! rejects what is not supported yet.

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
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/determinism/digests.txt"),
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
    assert_eq!(checked, 4);
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
        &["watch"],
        &["check", "--leaks"],
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
