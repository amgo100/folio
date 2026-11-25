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
//! | `nesting` | wrappers and how often to apply each | a script that nests values that deeply compiles to PDF on a small stack, or reports the nesting limit, without a panic or an overflow |
//! | `differential` | a corpus project and a sequence of edits to its content and stylesheets | after each edit, every build of a world edited in place has the outputs and diagnostics of a world built afresh from the same texts |
//!
//! A target panics when a check fails; libFuzzer records the input.

use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::OnceLock;

use folio::{BuildMode, Limits, Name, Target, World};
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
    ("nesting", nesting),
    ("differential", differential),
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
    /// character boundaries of a text of at most 65,535 characters without
    /// a NUL, and it spans at most 15 characters.
    pub fn encode(&self) -> Option<Vec<u8>> {
        if self.text.contains('\0') {
            return None;
        }
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

/// The ways [`nesting`] wraps a value `v` once: in content, in an array or
/// a dictionary, in a closure, in a function's arguments, and by changing
/// a place in an array or a dictionary. None repeats `v`, which would make
/// the value grow exponentially rather than nest.
pub const WRAPPERS: &[&str] = &[
    "[*#v*]",
    "[_#v _]",
    "[- #v]",
    "[+ #v]",
    "[/ t: #v]",
    "figure([#v])",
    "strong(override: (space-before: v))[x]",
    "(v,)",
    "(x: v)",
    "() => v",
    "strong.with(v)",
    "sink(v)",
    "{ let a = (); a.push(v); a }",
    "{ let d = (x: none); d.x = v; d }",
    "{ let a = (none,); a.at(0) = v; a }",
];

/// The script [`nesting`] compiles: for each pair of bytes, a wrapper of
/// [`WRAPPERS`] (the first byte) applied in a loop as many times as the
/// second byte says, so that a few bytes nest a value past the limit; then
/// the value shown.
pub fn nesting_script(data: &[u8]) -> String {
    let mut script = String::from("#let sink(..a) = a\n#let v = [x]\n");
    for pair in data.chunks_exact(2) {
        let wrapper = WRAPPERS[usize::from(pair[0]) % WRAPPERS.len()];
        let times = pair[1];
        script.push_str(&format!("#for i in range({times}) {{ v = {wrapper} }}\n"));
    }
    script.push_str("#v\n");
    script
}

/// Compiles a script that nests values deeply ([`nesting_script`]) to a
/// PDF, on a thread with the stack of the threads that compile builds in
/// parallel. Any nesting is either within the limit, and goes through
/// every stage, or an error where it is made.
pub fn nesting(data: &[u8]) {
    let script = nesting_script(data);
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let mut world = World::builder()
                .limits(STYLE_LIMITS)
                .build(NESTING_MANIFEST, None)
                .expect("the fuzz manifest is valid");
            world.set_source("main.fol", &script).expect("a valid path");
            let _ = folio::compile(&world, Target::Pdf, None);
        })
        .expect("a thread starts")
        .join()
        .expect("compiling does not panic");
}

/// The manifest of the world [`nesting`] builds.
const NESTING_MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
";

/// What [`differential`] inserts into content: markup that opens and closes
/// blocks, scripts, references and labels, words, and breaks, so that
/// random edits restructure the document as well as reflow it.
pub const CONTENT_SNIPPETS: &[&str] = &[
    "x",
    "word ",
    " ",
    "\n",
    "\n\n",
    "*",
    "_",
    "= ",
    "== ",
    "- ",
    "+ ",
    "/ term: ",
    "#",
    "#[",
    "[",
    "]",
    "{",
    "}",
    "(",
    ")",
    "\"",
    "`",
    "\\",
    "<l>",
    "@l",
    "#strong[bold]",
    "#figure[A figure.]",
    "#let v = 1\n",
    "#v",
    "#if true [yes] else [no]",
    "żółć ",
    "biało-czerwony ",
    "A sentence long enough to move a line break or two further down the page. ",
];

/// What [`differential`] inserts into stylesheets: rules, declarations of
/// properties that layout reads, and the punctuation that opens and
/// closes them.
pub const SHEET_SNIPPETS: &[&str] = &[
    "{",
    "}",
    ";",
    ":",
    " ",
    "\n",
    "1",
    "pt",
    "heading",
    "paragraph",
    "strong",
    "figure { space-before: 20pt; }\n",
    "doc { font-size: 14pt; }\n",
    "doc { justify: false; }\n",
    "doc { leading: 1.5em; }\n",
    "paragraph { indent: 2em; }\n",
    "heading { keep-with-next: false; }\n",
    "heading[level=1] { font-size: 20pt; }\n",
    "heading + paragraph { indent: 0pt; }\n",
    "emph { color: #f00; }\n",
    "page { margin-top: 60mm; }\n",
    "font-size: 9pt;",
    "space-after: 30pt;",
];

/// How many edits [`differential`] applies at most, so that one input stays
/// within a few seconds.
pub const MAX_EDITS: usize = 6;

/// An edit of [`differential`], as [`Edits::decode`] reads it.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceEdit {
    /// The source edited, by path in the project.
    pub path: String,
    /// The bytes replaced, on character boundaries.
    pub range: Range<usize>,
    /// What replaces them.
    pub with: String,
}

/// A corpus project and a sequence of edits to its sources, which
/// [`differential`] applies one at a time.
#[derive(Debug, Clone)]
pub struct Edits {
    /// The project.
    pub project: corpus::Project,
    /// The bytes each edit is decoded from, five per edit.
    pub ops: Vec<[u8; 5]>,
}

impl Edits {
    /// Reads a project (the first byte picks one of [`corpus::projects`])
    /// and then up to [`MAX_EDITS`] edits of five bytes each: the source
    /// (an index into the project's sources in order of path), where the
    /// edit starts (two bytes, little-endian, wrapped to the text's
    /// length in characters), how many characters it removes (up to 15),
    /// and what it inserts (an index into [`CONTENT_SNIPPETS`] or
    /// [`SHEET_SNIPPETS`]; past its end, nothing). Bytes left over are
    /// ignored. `None` without a project byte.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let (&first, rest) = data.split_first()?;
        // Read once: the fuzzer runs the target many times a second.
        static PROJECTS: OnceLock<Vec<corpus::Project>> = OnceLock::new();
        let projects = PROJECTS.get_or_init(corpus::projects);
        let project = projects[usize::from(first) % projects.len()].clone();
        let ops = rest
            .chunks_exact(5)
            .take(MAX_EDITS)
            .map(|op| op.try_into().expect("chunks of five"))
            .collect();
        Some(Self { project, ops })
    }

    /// The edit an op makes to the current texts of the project's sources.
    pub fn edit(sources: &BTreeMap<String, String>, op: [u8; 5]) -> SourceEdit {
        let [file, lo, hi, len, snippet] = op;
        let (path, text) = sources
            .iter()
            .nth(usize::from(file) % sources.len())
            .expect("a corpus project has sources");
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        let start = usize::from(u16::from_le_bytes([lo, hi])) % boundaries.len();
        let end = (start + usize::from(len % 16)).min(boundaries.len() - 1);
        let snippets = if path.ends_with(".fsty") {
            SHEET_SNIPPETS
        } else {
            CONTENT_SNIPPETS
        };
        SourceEdit {
            path: path.clone(),
            range: boundaries[start]..boundaries[end],
            with: snippets
                .get(usize::from(snippet))
                .copied()
                .unwrap_or("")
                .to_string(),
        }
    }

    /// Applies the edits one at a time to a world through
    /// [`World::edit_source`], compiling every build of the project after
    /// each, and checks each time that the outputs and the diagnostics
    /// equal those of a world built afresh from the same texts.
    ///
    /// The incremental world is compiled before the first edit too, so
    /// that its queries and its shaping, line-break and page caches hold
    /// results from before the edit. Diagnostics are compared with their
    /// spans resolved to file byte ranges, as the command line reports
    /// them: span numbers differ between an incremental reparse and a
    /// fresh parse.
    ///
    /// # Panics
    ///
    /// When a build differs, naming the project, the build and the edits
    /// applied so far.
    pub fn check(&self) {
        let mut sources = self.project.sources.clone();
        let mut world = self.project.world(BuildMode::Ci);
        let builds = world.builds();
        compile_builds(&world, &builds);
        let mut applied = Vec::new();
        for &op in &self.ops {
            let edit = Self::edit(&sources, op);
            world
                .edit_source(&edit.path, edit.range.clone(), &edit.with)
                .expect("the edit's range is on character boundaries");
            sources
                .get_mut(&edit.path)
                .expect("the edit's source")
                .replace_range(edit.range.clone(), &edit.with);
            applied.push(edit);
            let incremental = compile_builds(&world, &builds);
            let fresh_project = corpus::Project {
                sources: sources.clone(),
                ..self.project.clone()
            };
            let fresh_world = fresh_project.world(BuildMode::Ci);
            let fresh = compile_builds(&fresh_world, &builds);
            for (((variant, target), a), b) in builds.iter().zip(&incremental).zip(&fresh) {
                let build = format!(
                    "{} {} {}",
                    self.project.name,
                    variant.as_ref().map_or("-", |v| v.as_str()),
                    target.as_str()
                );
                let diags = |world: &World, (_, diagnostics): &Built| {
                    folio::diag::to_json(world, diagnostics)
                };
                assert_eq!(
                    diags(&world, a),
                    diags(&fresh_world, b),
                    "{build}: the diagnostics of an incremental compile differ from a fresh one after the edits {applied:#?}"
                );
                assert!(
                    a.0 == b.0,
                    "{build}: the output of an incremental compile differs from a fresh one after the edits {applied:#?}"
                );
            }
        }
    }
}

/// A build's output and diagnostics.
type Built = (Option<folio::Bytes>, Vec<folio::Diagnostic>);

/// Compiles builds of a world in parallel, as `folio build` does.
fn compile_builds(world: &World, builds: &[(Option<Name>, Target)]) -> Vec<Built> {
    folio::compile_builds(world, builds.iter().cloned())
        .into_iter()
        .map(|output| {
            let compiled = output.result.expect("nothing cancels the compile");
            (compiled.output, compiled.diagnostics.to_vec())
        })
        .collect()
}

/// Edits a corpus project in place ([`Edits::decode`] reads the bytes) and
/// checks every build after each edit against a fresh compile
/// ([`Edits::check`]).
pub fn differential(data: &[u8]) {
    if let Some(edits) = Edits::decode(data) {
        edits.check();
    }
}

/// Random inputs of [`differential`], from a seed: a project and
/// [`MAX_EDITS`] edits, for the property test and the seed corpus. The
/// same seed gives the same bytes on every platform.
pub fn differential_input(seed: u64) -> Vec<u8> {
    // SplitMix64: small, and its output depends on nothing but the seed.
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut out = Vec::with_capacity(1 + 5 * MAX_EDITS);
    out.extend(next().to_le_bytes().into_iter().take(1));
    for _ in 0..MAX_EDITS {
        out.extend(next().to_le_bytes().into_iter().take(5));
    }
    out
}

/// How many random inputs of [`differential`] the seed corpus holds.
pub const DIFFERENTIAL_SEEDS: u64 = 4;

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
    // Each wrapper past the nesting limit, and all of them in turn.
    for (i, _) in WRAPPERS.iter().enumerate() {
        let wrapper = u8::try_from(i).expect("few wrappers");
        out.push(("nesting", format!("wrapper-{i}.bin"), vec![wrapper, 255]));
    }
    let mixed: Vec<u8> = (0..WRAPPERS.len())
        .flat_map(|i| [u8::try_from(i).expect("few wrappers"), 7])
        .collect();
    out.push(("nesting", "mixed.bin".into(), mixed));
    for seed in 0..DIFFERENTIAL_SEEDS {
        out.push((
            "differential",
            format!("random-{seed}.bin"),
            differential_input(seed),
        ));
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
