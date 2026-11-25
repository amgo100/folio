//! The bodies of the fuzz targets (SPEC §14.2) in `fuzz/`, each taking the
//! raw bytes libFuzzer generates.
//!
//! They live here, not in the fuzz crate, so that the regression inputs in
//! `fuzz/regressions/<target>/` (inputs that once failed, kept after the
//! fix) are replayed by `cargo test` on stable Rust, and so that the seed
//! corpus is written by the same code that reads it ([`seeds`]).
//!
//! | Target | Input | Checks |
//! | --- | --- | --- |
//! | `markup` | content markup | the parse is lossless |
//! | `stylesheet` | a stylesheet | the parse is lossless |
//! | `reparse` | an edit to markup or a stylesheet | incremental reparsing equals a full parse; node identities follow the edit |
//! | `style` | content and a stylesheet | evaluation, the cascade and selector matching finish without a panic |
//!
//! A target panics when a check fails; libFuzzer records the input.

use std::fs;
use std::ops::Range;
use std::path::PathBuf;

use folio::{Limits, Name, Target, World};
use folio_syntax::{NodeIds, Source, SyntaxNode, parse, parse_stylesheet};

use crate::corpus;

/// The body of a fuzz target.
pub type Body = fn(&[u8]);

/// The fuzz targets, by name: the names of `fuzz/fuzz_targets/*.rs`.
pub const TARGETS: &[(&str, Body)] = &[
    ("markup", markup),
    ("stylesheet", stylesheet),
    ("reparse", reparse),
    ("style", style),
];

/// The regression inputs of a target: `fuzz/regressions/<target>/`.
pub fn regressions_dir(target: &str) -> PathBuf {
    crate::root().join("fuzz/regressions").join(target)
}

/// Parses content markup and checks that the tree holds the text exactly.
pub fn markup(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    check_lossless(&parse(text), text);
    let _ = Source::detached(text).root().errors_and_warnings();
}

/// Parses a stylesheet and checks that the tree holds the text exactly.
pub fn stylesheet(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    check_lossless(&parse_stylesheet(text), text);
    let _ = Source::detached_stylesheet(text)
        .root()
        .errors_and_warnings();
}

/// Checks that a tree's text is the text it was parsed from, and that every
/// inner node is as long as its children.
fn check_lossless(root: &SyntaxNode, text: &str) {
    assert_eq!(root.full_text(), text, "the parse lost text");
    fn lengths(node: &SyntaxNode) {
        if node.children().len() > 0 {
            let sum: usize = node.children().map(SyntaxNode::len).sum();
            assert_eq!(
                sum,
                node.len(),
                "children of {:?} do not add up",
                node.kind()
            );
            node.children().for_each(lengths);
        }
    }
    lengths(root);
}

/// An edit to a file, as [`reparse`] reads it from bytes.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Edit {
    /// Whether the file is a stylesheet rather than content.
    pub stylesheet: bool,
    /// The text before the edit.
    pub text: String,
    /// The bytes replaced. [`Edit::decode`] snaps them to character
    /// boundaries.
    pub range: Range<usize>,
    /// What replaces them.
    pub with: String,
}

impl Edit {
    /// Reads an edit: a flags byte (bit 0: a stylesheet), the start of the
    /// range as two bytes (little-endian, wrapped to the text's length),
    /// the range's length in characters as one byte (up to 15), then the
    /// text, and after a NUL byte, the replacement. `None` when the bytes
    /// are too few or not UTF-8.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let [flags, lo, hi, len, rest @ ..] = data else {
            return None;
        };
        let rest = std::str::from_utf8(rest).ok()?;
        let (text, with) = rest.split_once('\0').unwrap_or((rest, ""));
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        let first = usize::from(u16::from_le_bytes([*lo, *hi])) % boundaries.len();
        let last = (first + usize::from(len % 16)).min(boundaries.len() - 1);
        Some(Self {
            stylesheet: flags & 1 == 1,
            text: text.to_string(),
            range: boundaries[first]..boundaries[last],
            with: with.to_string(),
        })
    }

    /// The bytes that [`Edit::decode`] reads as this edit, if its range is on
    /// character boundaries of a text of at most 65,535 characters and it
    /// spans at most 15 characters.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let first = self.text.get(..self.range.start)?.chars().count();
        let len = self.text.get(self.range.clone())?.chars().count();
        let first = u16::try_from(first).ok()?;
        let len = u8::try_from(len).ok().filter(|len| *len < 16)?;
        let mut out = vec![u8::from(self.stylesheet)];
        out.extend(first.to_le_bytes());
        out.push(len);
        out.extend(self.text.as_bytes());
        out.push(0);
        out.extend(self.with.as_bytes());
        Some(out)
    }

    /// Applies the edit incrementally, and checks the tree against a full
    /// parse of the new text and the node identities against the edit.
    pub fn check(&self) {
        let (mut source, full): (Source, fn(&str) -> SyntaxNode) = if self.stylesheet {
            (
                Source::detached_stylesheet(self.text.clone()),
                parse_stylesheet,
            )
        } else {
            (Source::detached(self.text.clone()), parse)
        };
        let mut ids = NodeIds::new();
        ids.insert(source.root());
        let old = source.root().clone();
        source.edit(self.range.clone(), &self.with);
        let mut new_text = self.text.clone();
        new_text.replace_range(self.range.clone(), &self.with);
        assert_eq!(source.text(), new_text);
        assert_eq!(source.root().full_text(), new_text);
        assert!(
            source.root().spanless_eq(&full(&new_text)),
            "incremental reparse differs from a full parse after replacing {:?} with {:?}",
            self.range,
            self.with
        );
        ids.update(&old, source.root());
        // Every node of the new tree has an identity, and it leads back to
        // the node.
        fn walk(node: &SyntaxNode, ids: &NodeIds) -> usize {
            let span = node.span();
            assert!(!span.is_detached(), "{:?} has no span", node.kind());
            let id = ids
                .id(span)
                .unwrap_or_else(|| panic!("{:?} has no identity", node.kind()));
            assert_eq!(ids.span(id), Some(span), "an identity leads elsewhere");
            1 + node.children().map(|child| walk(child, ids)).sum::<usize>()
        }
        assert_eq!(
            walk(source.root(), &ids),
            ids.len(),
            "ids of nodes that went remain"
        );
    }
}

/// Edits a file incrementally ([`Edit::decode`] reads the bytes) and checks
/// the result ([`Edit::check`]).
pub fn reparse(data: &[u8]) {
    if let Some(edit) = Edit::decode(data) {
        edit.check();
    }
}

/// The manifest of the world [`style`] builds: one variant, so that the
/// stylesheet can address it with `@variant`.
const STYLE_MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
styles = [\"style.fsty\"]

[variants.paper]
targets = [\"pdf\"]
";

/// The limits of the evaluator in [`style`]: low, so that one input stays
/// fast whatever its scripts do.
const STYLE_LIMITS: Limits = Limits {
    steps: 100_000,
    memory: 16 * 1024 * 1024,
    depth: 32,
};

/// Splits bytes at the first NUL into content and a stylesheet; `None` when
/// they are not UTF-8.
pub fn split_style(data: &[u8]) -> Option<(&str, &str)> {
    let text = std::str::from_utf8(data).ok()?;
    Some(text.split_once('\0').unwrap_or((text, "")))
}

/// Styles content with a stylesheet ([`split_style`] splits the bytes):
/// evaluation, the cascade and selector matching, for the PDF target.
pub fn style(data: &[u8]) {
    let Some((content, sheet)) = split_style(data) else {
        return;
    };
    let mut world = World::builder()
        .limits(STYLE_LIMITS)
        .build(STYLE_MANIFEST, None)
        .expect("the fuzz manifest is valid");
    world.set_source("main.fol", content).expect("a valid path");
    world.set_source("style.fsty", sheet).expect("a valid path");
    let variant = Name::new("paper").expect("a valid name");
    let _ = folio::styles(&world, Target::Pdf, Some(&variant));
}

/// The seed corpus of every target, as (target, file name, bytes): the
/// documents and stylesheets of `tests/corpus` and of the parser's corpus
/// in `crates/folio-syntax/tests/corpus`, encoded for each target.
pub fn seeds() -> Vec<(&'static str, String, Vec<u8>)> {
    let mut contents = Vec::new();
    let mut sheets = Vec::new();
    let syntax = crate::root().join("crates/folio-syntax/tests/corpus");
    let mut files: Vec<PathBuf> = fs::read_dir(&syntax)
        .map(|entries| entries.filter_map(|e| Some(e.ok()?.path())).collect())
        .unwrap_or_default();
    files.sort();
    for path in files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        match path.extension().and_then(|e| e.to_str()) {
            Some("fol") => contents.push((format!("syntax-{name}"), text)),
            Some("fsty") => sheets.push((format!("syntax-{name}"), text)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for project in corpus::projects() {
        let mut project_sheets = String::new();
        for (path, text) in &project.sources {
            let name = format!("{}-{}", project.name, path.replace('/', "-"));
            if path.ends_with(".fsty") {
                project_sheets.push_str(text);
                project_sheets.push('\n');
                sheets.push((name, text.clone()));
            } else {
                contents.push((name, text.clone()));
            }
        }
        let entry = project.sources.get("main.fol").cloned().unwrap_or_default();
        let mut bytes = entry.into_bytes();
        bytes.push(0);
        bytes.extend(project_sheets.into_bytes());
        out.push(("style", format!("{}.bin", project.name), bytes));
    }
    for (name, text) in &contents {
        out.push(("markup", name.clone(), text.as_bytes().to_vec()));
    }
    for (name, text) in &sheets {
        out.push(("stylesheet", name.clone(), text.as_bytes().to_vec()));
    }
    // Edits in the middle of each file: an insertion and a deletion.
    for (stylesheet, files) in [(false, &contents), (true, &sheets)] {
        for (name, text) in files {
            let middle = (0..=text.len() / 2)
                .rev()
                .find(|i| text.is_char_boundary(*i))
                .unwrap_or(0);
            let end = text[middle..]
                .char_indices()
                .nth(4)
                .map_or(text.len(), |(i, _)| middle + i);
            for (kind, range, with) in [
                (
                    "insert",
                    middle..middle,
                    if stylesheet { "{" } else { "#[" },
                ),
                ("delete", middle..end, ""),
            ] {
                let edit = Edit {
                    stylesheet,
                    text: text.clone(),
                    range,
                    with: with.to_string(),
                };
                if let Some(bytes) = edit.encode() {
                    out.push(("reparse", format!("{name}.{kind}.bin"), bytes));
                }
            }
        }
    }
    out
}
