//! `folio check --leaks` (SPEC §6.4, §12.1): a leak is an error with its
//! code, and a project in strict mode is checked by `folio build` and
//! `folio check` without asking, so a build that leaks writes nothing.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

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

/// A paper whose subject repeats the answer that the exam hides, in a
/// temporary directory, with `formatting` appended to its manifest.
fn leaking(formatting: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let manifest = format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         styles = [\"paper.fsty\"]\n\n[variants.exam]\ntargets = [\"pdf\"]\n\n\
         [variants.markscheme]\ntargets = [\"pdf\"]\n{formatting}"
    );
    let answer = "The speed is nineteen point six metres per second";
    let files = [
        ("folio.toml", manifest),
        (
            "paper.fsty",
            "figure.answer { display: none; }\n\
             @variant markscheme { figure.answer { display: block; } }\n"
                .into(),
        ),
        (
            "main.fol",
            format!(
                "#doc(title: [Forces], subject: \"{answer}\")\n\nA ball is dropped.\n\n\
                 #figure(class: \"answer\")[{answer}.]\n"
            ),
        ),
    ];
    for (path, text) in files {
        fs::write(dir.path().join(path), text).unwrap();
    }
    dir
}

#[test]
fn check_leaks_reports_leaks_as_errors() {
    let dir = leaking("");
    // Without the flag, the leak is not looked for.
    let output = folio(dir.path(), &["check"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let output = folio(dir.path(), &["check", "--leaks"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(text.contains("error[leak-text]"), "{text}");
    assert!(text.contains("variant `exam` does not show"), "{text}");
    assert!(text.contains("main.fol"), "{text}");
    // Only the exam leaks.
    assert!(!text.contains("variant `markscheme`"), "{text}");

    let output = folio(dir.path(), &["check", "--leaks", "--variant", "markscheme"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let output = folio(dir.path(), &["check", "--leaks", "--diagnostics", "json"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    let codes: Vec<&str> = json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert_eq!(codes, ["leak-text"], "{json}");
    assert!(!dir.path().join("out").exists());
}

#[test]
fn strict_mode_checks_every_build_for_leaks() {
    let dir = leaking("\n[formatting]\nmode = \"strict\"\n");
    let output = folio(dir.path(), &["check"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("error[leak-text]"));

    let output = folio(dir.path(), &["build"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("error[leak-text]"));
    // The exam leaks and is not written; the mark scheme is.
    assert!(!dir.path().join("out/main-exam.pdf").exists());
    assert!(dir.path().join("out/main-markscheme.pdf").exists());
}

#[test]
fn the_corpus_passes_the_leak_check() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    for project in ["variants", "groups", "versions"] {
        let output = folio(&corpus.join(project), &["check", "--leaks"]);
        assert!(output.status.success(), "{project}: {}", stderr(&output));
    }
}
