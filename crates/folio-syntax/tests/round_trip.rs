//! Lossless round trip and incremental reparsing over the test corpus in
//! `tests/corpus`: content (`.fol`) and stylesheets (`.fsty`). Files whose
//! names start with `broken-` must contain syntax errors; all other files must
//! parse without any.

use std::fs;
use std::path::Path;

use folio_syntax::{LinkedNode, Source, SyntaxKind, SyntaxNode, parse, parse_stylesheet};

/// The kind of file in the corpus.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Lang {
    /// Content markup, `.fol`.
    Content,
    /// A stylesheet, `.fsty`.
    Stylesheet,
}

impl Lang {
    /// The file extension.
    fn ext(self) -> &'static str {
        match self {
            Self::Content => "fol",
            Self::Stylesheet => "fsty",
        }
    }

    /// Parses text as this kind of file.
    fn parse(self, text: &str) -> SyntaxNode {
        match self {
            Self::Content => parse(text),
            Self::Stylesheet => parse_stylesheet(text),
        }
    }

    /// Creates a detached source of this kind.
    fn source(self, text: String) -> Source {
        match self {
            Self::Content => Source::detached(text),
            Self::Stylesheet => Source::detached_stylesheet(text),
        }
    }

    /// The kind of the root node.
    fn root(self) -> SyntaxKind {
        match self {
            Self::Content => SyntaxKind::Markup,
            Self::Stylesheet => SyntaxKind::Stylesheet,
        }
    }
}

/// Every file of the corpus, as (file name, language, text), sorted by name.
fn corpus() -> Vec<(String, Lang, String)> {
    let mut files: Vec<_> = [Lang::Content, Lang::Stylesheet]
        .into_iter()
        .flat_map(|lang| corpus_of(lang).into_iter().map(move |(n, t)| (n, lang, t)))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// The files of the corpus in one language, as (file name, text).
fn corpus_of(lang: Lang) -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("corpus directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == lang.ext()))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(&path).expect("UTF-8 corpus file");
            (name, text)
        })
        .collect();
    files.sort();
    assert!(files.len() >= 5, "the corpus should not be empty");
    files
}

/// Checks that every inner node is exactly as long as its children, so the
/// tree covers the text without gaps or overlaps.
fn check_lengths(node: &SyntaxNode) {
    if node.children().len() > 0 {
        let sum: usize = node.children().map(SyntaxNode::len).sum();
        assert_eq!(
            sum,
            node.len(),
            "children of {:?} do not add up",
            node.kind()
        );
        node.children().for_each(check_lengths);
    }
}

#[test]
fn print_of_parse_is_source() {
    for (name, lang, text) in corpus() {
        let root = lang.parse(&text);
        assert_eq!(root.full_text(), text, "round trip failed for {name}");
        assert_eq!(root.kind(), lang.root());
        check_lengths(&root);

        let (errors, _warnings) = lang.source(text.clone()).root().errors_and_warnings();
        if name.starts_with("broken-") {
            assert!(!errors.is_empty(), "{name} should have syntax errors");
        } else {
            let messages: Vec<_> = errors.iter().map(|error| &error.message).collect();
            assert!(
                messages.is_empty(),
                "{name} has syntax errors: {messages:?}"
            );
        }
    }
}

/// Text inserted by the reparse test: delimiters, sigils and the starts of
/// Folio's own constructs, which change the tree the most.
const INSERTIONS: &[&str] = &[
    "", "x", " ", "\n", "\n\n", "[", "]", "{", "}", "(", ")", "$", "#", "*", "_", "<", ">", "@",
    ":", "-", ".", "`", "```", "\"", "/", "//", "/*", "\\", "= ", "- ", "rule ", "element ",
    ":tex ", "<a:b>", "@fig:x:", ";",
];

/// Text inserted by the reparse test in stylesheets: delimiters, combinators
/// and the starts of stylesheet constructs.
const STYLE_INSERTIONS: &[&str] = &[
    "x",
    " ",
    "\n",
    "{",
    "}",
    "[",
    "]",
    "(",
    ")",
    ";",
    ":",
    ",",
    ".",
    ">",
    "+",
    "~",
    "#",
    "$",
    "@",
    "\"",
    "//",
    "/*",
    "*/",
    "#{a:b}",
    "16:9",
    "if ",
    "else",
    "render ",
    "as ",
    "@variant ",
    "@import ",
    ":has(",
    ":step(2-",
    "!important",
    "slide-master \"m\" ",
];

/// Text that replaces a deleted range in the reparse test.
const REPLACEMENTS: &[&str] = &["", "x", "#", "\n", "]"];

#[test]
fn incremental_reparse_matches_full_parse() {
    for (name, lang, text) in corpus() {
        let original = lang.source(text.clone());
        let insertions = match lang {
            Lang::Content => INSERTIONS,
            Lang::Stylesheet => STYLE_INSERTIONS,
        };
        // Step through the text at char boundaries: insert everything of
        // `INSERTIONS`, and delete one and four characters.
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        for (k, &start) in boundaries.iter().enumerate().step_by(3) {
            for with in insertions.iter().filter(|with| !with.is_empty()) {
                check_edit(&name, lang, &original, start..start, with);
            }
            for len in [1, 4] {
                let Some(&end) = boundaries.get(k + len) else {
                    continue;
                };
                for with in REPLACEMENTS {
                    check_edit(&name, lang, &original, start..end, with);
                }
            }
        }
    }
}

/// Applies one edit incrementally and compares the tree with a full parse of
/// the new text.
#[track_caller]
fn check_edit(
    name: &str,
    lang: Lang,
    original: &Source,
    range: std::ops::Range<usize>,
    with: &str,
) {
    let mut source = original.clone();
    source.edit(range.clone(), with);
    let mut new_text = original.text().to_string();
    new_text.replace_range(range.clone(), with);
    assert_eq!(source.text(), new_text);
    assert_eq!(source.root().full_text(), new_text);
    if !source.root().spanless_eq(&lang.parse(&new_text)) {
        panic!(
            "incremental reparse differs from a full parse in {name} \
             after replacing {range:?} with {with:?}:\n{new_text}"
        );
    }
}

#[test]
fn reparse_is_local_and_reuses_untouched_nodes() {
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/spec-4-2-markup.fol"),
    )
    .unwrap();
    let mut source = Source::detached(text.clone());

    // The span of the last element call, far from the edit.
    let question = |source: &Source| {
        let start = source.text().find("#question").unwrap() + 1;
        let leaf = LinkedNode::new(source.root())
            .leaf_at(start + 1, folio_syntax::Side::After)
            .unwrap();
        let call = leaf.parent().unwrap().clone();
        assert_eq!(call.kind(), SyntaxKind::FuncCall);
        call.get().span()
    };
    let before = question(&source);

    let at = text.find("negligible").unwrap() + "negligible".len();
    let reparsed = source.edit(at..at, " here");
    assert!(
        reparsed.len() < text.len() / 4,
        "an edit in a paragraph reparsed {reparsed:?} of {} bytes",
        text.len()
    );
    assert!(reparsed.end <= source.text().find("#figure").unwrap());
    assert_eq!(
        question(&source),
        before,
        "an untouched node was renumbered"
    );

    // An edit inside a content block reparses only that block.
    let at = source.text().find("Calculate").unwrap();
    let reparsed = source.edit(at..at, "Now ");
    assert!(source.text()[reparsed.clone()].contains("Now Calculate"));
    let block_start = source.text().find("#question(marks: 3)[").unwrap();
    assert!(reparsed.start > block_start, "reparsed {reparsed:?}");
    assert!(reparsed.end < source.text().find("#answer").unwrap());
}
