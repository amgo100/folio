//! The formatter over the corpus: the parser's own (`tests/corpus`) and the
//! projects of `tests/corpus` at the repository root. Formatting a file
//! without syntax errors succeeds, gives a file that parses to an equivalent
//! tree, and is idempotent; a file with syntax errors is left alone.

use std::fs;
use std::path::{Path, PathBuf};

use folio_syntax::fmt::{self, FormatError};
use folio_syntax::{SyntaxNode, parse, parse_stylesheet};

/// Every `.fol` and `.fsty` file under `dir`, sorted.
fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("fol" | "fsty")
        ) {
            out.push(path);
        }
    }
}

/// The corpus files.
fn corpus() -> Vec<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    files(&manifest.join("tests/corpus"), &mut out);
    files(&manifest.join("../../tests/corpus"), &mut out);
    assert!(out.len() > 10, "the corpus is missing");
    out
}

/// Parses a file by its extension.
fn parse_file(path: &Path, text: &str) -> SyntaxNode {
    match path.extension().and_then(|e| e.to_str()) {
        Some("fsty") => parse_stylesheet(text),
        _ => parse(text),
    }
}

#[test]
fn formatting_is_idempotent_and_keeps_meaning() {
    for path in corpus() {
        let text = fs::read_to_string(&path).unwrap();
        let root = parse_file(&path, &text);
        let broken = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("broken-"));
        let result = fmt::format(&root);
        if broken {
            assert_eq!(result, Err(FormatError::Syntax), "{}", path.display());
            continue;
        }
        let edits = result.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let once = edits.apply(&text);
        let reparsed = parse_file(&path, &once);
        assert!(
            !reparsed.diagnosis().errors,
            "{}: formatting gave syntax errors",
            path.display()
        );
        assert!(
            fmt::equivalent(&root, &reparsed),
            "{}: formatting changed the tree",
            path.display()
        );
        let again = fmt::format(&reparsed).unwrap();
        assert!(
            again.is_empty(),
            "{}: formatting is not idempotent:\n{}",
            path.display(),
            again.apply(&once)
        );
    }
}
