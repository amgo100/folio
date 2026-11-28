//! Property tests of the formatter (`folio_syntax::fmt`, SPEC §12.1) over the
//! corpus: the parser's own (`crates/folio-syntax/tests/corpus`) and the
//! projects of `tests/corpus`. Each case takes a corpus file, changes its
//! whitespace at random places (spaces, tabs, line breaks, blank lines,
//! `\r\n`, removals), keeps the change only if the file still parses to an
//! equivalent tree, and checks that formatting it succeeds, keeps what it
//! means, and is idempotent: `fmt(fmt(x)) == fmt(x)`.
//!
//! The formatter lives in `folio-syntax`; the test is here because this crate
//! already has `proptest`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use folio_syntax::fmt::{self, FormatError};
use folio_syntax::{SyntaxNode, parse, parse_stylesheet};
use proptest::prelude::*;

/// A corpus file: whether it is a stylesheet, and its text.
struct File {
    path: PathBuf,
    stylesheet: bool,
    text: String,
}

impl File {
    fn parse(&self, text: &str) -> SyntaxNode {
        if self.stylesheet {
            parse_stylesheet(text)
        } else {
            parse(text)
        }
    }
}

/// Every `.fol` and `.fsty` file under `dir` whose name does not start with
/// `broken-`, sorted.
fn collect(dir: &Path, out: &mut Vec<File>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            collect(&path, out);
        } else if !name.starts_with("broken-")
            && let Some(ext @ ("fol" | "fsty")) = path.extension().and_then(|e| e.to_str())
        {
            let text = fs::read_to_string(&path).unwrap();
            out.push(File {
                stylesheet: ext == "fsty",
                path,
                text,
            });
        }
    }
}

static CORPUS: LazyLock<Vec<File>> = LazyLock::new(|| {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    collect(&manifest.join("../folio-syntax/tests/corpus"), &mut out);
    collect(&manifest.join("../../tests/corpus"), &mut out);
    assert!(out.len() > 10, "the corpus is missing");
    out
});

/// A change of whitespace at a place given as a fraction of the text.
#[derive(Debug, Clone)]
enum Change {
    /// Inserts whitespace.
    Insert(f64, &'static str),
    /// Removes the whitespace character at or after the place.
    Remove(f64),
}

fn change() -> impl Strategy<Value = Change> {
    let place = 0.0..1.0f64;
    prop_oneof![
        (
            place.clone(),
            prop::sample::select(vec![
                " ", "  ", "\t", "\n", "\n\n", "\n\n\n", "\r\n", " \n  "
            ])
        )
            .prop_map(|(at, text)| Change::Insert(at, text)),
        place.prop_map(Change::Remove),
    ]
}

/// The byte at `fraction` of `text`, moved back to a character boundary.
fn place(text: &str, fraction: f64) -> usize {
    // A fraction of a corpus file's length fits in a usize.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut at = (fraction * text.len() as f64) as usize;
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The file's text with a change made, if it keeps the file's meaning.
fn apply(file: &File, text: &str, change: &Change) -> Option<String> {
    let mut new = text.to_owned();
    match change {
        Change::Insert(at, ws) => new.insert_str(place(text, *at), ws),
        Change::Remove(at) => {
            let start = place(text, *at);
            let (offset, c) = text[start..]
                .char_indices()
                .find(|(_, c)| c.is_whitespace())?;
            new.replace_range(start + offset..start + offset + c.len_utf8(), "");
        }
    }
    let before = file.parse(text);
    let after = file.parse(&new);
    (!after.diagnosis().errors && fmt::equivalent(&before, &after)).then_some(new)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn formatting_keeps_meaning_and_is_idempotent(
        index in any::<prop::sample::Index>(),
        changes in prop::collection::vec(change(), 0..12),
    ) {
        let file = &CORPUS[index.index(CORPUS.len())];
        let mut text = file.text.clone();
        for change in &changes {
            if let Some(new) = apply(file, &text, change) {
                text = new;
            }
        }
        let root = file.parse(&text);
        let edits = match fmt::format(&root) {
            Ok(edits) => edits,
            Err(FormatError::Syntax) => unreachable!("the changes keep the file parsing"),
            Err(error) => panic!("{}: {error}:\n{text}", file.path.display()),
        };
        let once = edits.apply(&text);
        let reparsed = file.parse(&once);
        prop_assert!(!reparsed.diagnosis().errors, "{}:\n{}", file.path.display(), once);
        prop_assert!(fmt::equivalent(&root, &reparsed), "{}:\n{}", file.path.display(), once);
        let twice = fmt::format(&reparsed).unwrap().apply(&once);
        prop_assert_eq!(&twice, &once, "{}: not idempotent", file.path.display());
        prop_assert!(!once.contains('\r'));
        if file.stylesheet {
            prop_assert!(once.ends_with('\n') && !once.ends_with("\n\n"));
        } else {
            // Content keeps the kind of whitespace it ends with: none, a
            // space (a line break or one space) or a paragraph break.
            let end = &once[once.trim_end().len()..];
            prop_assert!(matches!(end, "" | " " | "\n" | "\n\n"), "{:?}", end);
        }
    }
}

#[test]
fn formatting_text_files_with_syntax_errors_leaves_them() {
    let root = parse("#f(\n");
    assert_eq!(fmt::format(&root), Err(FormatError::Syntax));
}
