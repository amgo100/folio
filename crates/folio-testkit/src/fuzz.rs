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
//! | `reparse` | a sequence of edits to markup or a stylesheet | after each, incremental reparsing equals a full parse; node identities follow the edit |
//! | `style` | content and a stylesheet | evaluation, the cascade and selector matching finish without a panic |
//! | `nesting` | wrappers and how often to apply each | a script that nests values that deeply compiles to PDF on a small stack, or reports the nesting limit, without a panic or an overflow |
//! | `document` | content markup | evaluation and the document tree finish without a panic, with no stylesheet |
//! | `compile` | content and declarations of the registry's properties | a whole build to pages and a PDF finishes without a panic, whatever the property values: huge, zero, negative or not finite |
//! | `manifest` | a `folio.toml` | reading it finishes without a panic; a valid one compiles |
//! | `lockfile` | a `folio.lock` | reading it finishes without a panic; a valid one compiles |
//! | `wire` | a display list message | decoding finishes without a panic; what decodes encodes and decodes to the same |
//! | `tex` | the body of a LaTeX equation | reading it into the math tree finishes without a panic, and its errors point into the equation |
//! | `image` | a byte saying how the figure is placed, then an image file | reading it, writing it to PDF and rendering its page finish without a panic, within libFuzzer's time and memory limits (`crate::fuzz_image`) |
//! | `differential` | a corpus project and a sequence of edits to its content and stylesheets | after each edit, every build of a world edited in place has the outputs, diagnostics and span maps of a world built afresh from the same texts |
//!
//! A target panics when a check fails; libFuzzer records the input.

use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::OnceLock;

use folio::{BuildMode, Fragment, FragmentItem, Limits, Name, Target, World, span_map};
use folio_syntax::{NodeIds, Source, SyntaxNode, parse, parse_stylesheet};

use crate::corpus;

pub use crate::fuzz_image::image;

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
    ("document", document),
    ("compile", compile),
    ("manifest", manifest),
    ("lockfile", lockfile),
    ("wire", wire),
    ("tex", tex),
    ("image", image),
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

/// An edit to a file, and the edits that follow it, as [`reparse`] reads
/// them from bytes.
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
    /// The edits made after this one, in order, each to the text the one
    /// before it left.
    pub then: Vec<Step>,
}

/// An edit after the first of an [`Edit`], placed by characters so that it
/// fits whatever text the edits before it left.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Step {
    /// The character the replaced range starts at, wrapped to the text's
    /// length in characters.
    pub at: usize,
    /// How many characters it replaces (up to 15), stopping at the end.
    pub len: usize,
    /// What replaces them.
    pub with: String,
}

/// How many [`Step`]s [`Edit::decode`] reads at most, so that one input
/// stays fast.
pub const MAX_STEPS: usize = 32;

/// The start of a [`Step`] that a character encodes: its code point less
/// one, not counting the surrogates, so that every start up to the last
/// has a character.
fn step_start(c: char) -> usize {
    let cp = c as usize;
    cp.saturating_sub(1) - if cp > 0xDFFF { 0x800 } else { 0 }
}

/// The character that encodes the start of a [`Step`] ([`step_start`]).
fn step_char(at: usize) -> Option<char> {
    let cp = at.checked_add(1)?;
    let cp = if cp >= 0xD800 {
        cp.checked_add(0x800)?
    } else {
        cp
    };
    char::from_u32(u32::try_from(cp).ok()?)
}

impl Step {
    /// The range of `text` this step replaces.
    pub fn range(&self, text: &str) -> Range<usize> {
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        let first = self.at % boundaries.len();
        let last = (first + self.len % 16).min(boundaries.len() - 1);
        boundaries[first]..boundaries[last]
    }
}

impl Edit {
    /// Reads an edit: a flags byte (bit 0: a stylesheet; bit 1: a sequence),
    /// the start of the range as two bytes (little-endian, wrapped to the
    /// text's length), the range's length in characters as one byte (up to
    /// 15), then the text, and after a NUL byte, the replacement. In a
    /// sequence, the replacement ends at the next NUL, and each part after
    /// a further NUL is a [`Step`]: a character that says where it starts
    /// (its code point less one, not counting the surrogates), a character whose code point modulo 16 is how
    /// many characters it replaces, and the replacement (parts of fewer
    /// than two characters are skipped; up to [`MAX_STEPS`]). `None` when
    /// the bytes are too few or not UTF-8.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let [flags, lo, hi, len, rest @ ..] = data else {
            return None;
        };
        let rest = std::str::from_utf8(rest).ok()?;
        let (text, with) = rest.split_once('\0').unwrap_or((rest, ""));
        let (with, then) = if flags & 2 == 2 {
            let mut parts = with.split('\0');
            let with = parts.next().unwrap_or("");
            let then = parts
                .filter_map(|part| {
                    let mut chars = part.chars();
                    let at = chars.next()?;
                    let len = chars.next()?;
                    Some(Step {
                        at: step_start(at),
                        len: len as usize % 16,
                        with: chars.as_str().to_string(),
                    })
                })
                .take(MAX_STEPS)
                .collect();
            (with, then)
        } else {
            (with, Vec::new())
        };
        let first = Step {
            at: usize::from(u16::from_le_bytes([*lo, *hi])),
            len: usize::from(*len),
            with: String::new(),
        };
        Some(Self {
            stylesheet: flags & 1 == 1,
            text: text.to_string(),
            range: first.range(text),
            with: with.to_string(),
            then,
        })
    }

    /// The bytes that [`Edit::decode`] reads as this edit, if its range is on
    /// character boundaries of a text of at most 65,535 characters, it spans
    /// at most 15 characters, the text holds no NUL, and in a sequence no
    /// replacement does either, and every step replaces at most 15
    /// characters and starts before the character 1,112,063.
    pub fn encode(&self) -> Option<Vec<u8>> {
        // A NUL ends the text, and in a sequence each replacement.
        let sequence = !self.then.is_empty();
        let mut separated = self.then.iter().map(|step| &step.with);
        if self.text.contains('\0')
            || (sequence && (self.with.contains('\0') || separated.any(|w| w.contains('\0'))))
        {
            return None;
        }
        let first = self.text.get(..self.range.start)?.chars().count();
        let len = self.text.get(self.range.clone())?.chars().count();
        let first = u16::try_from(first).ok()?;
        let len = u8::try_from(len).ok().filter(|len| *len < 16)?;
        let mut out = vec![u8::from(self.stylesheet) | (u8::from(sequence) << 1)];
        out.extend(first.to_le_bytes());
        out.push(len);
        out.extend(self.text.as_bytes());
        out.push(0);
        out.extend(self.with.as_bytes());
        for step in &self.then {
            if step.len >= 16 {
                return None;
            }
            let at = step_char(step.at)?;
            let len = char::from_u32(u32::try_from(16 + step.len).ok()?)?;
            out.push(0);
            out.extend(at.to_string().as_bytes());
            out.extend(len.to_string().as_bytes());
            out.extend(step.with.as_bytes());
        }
        Some(out)
    }

    /// Applies the edit and then its steps incrementally, and checks after
    /// each the tree against a full parse of the new text and the node
    /// identities against the edit.
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
        let mut text = self.text.clone();
        let steps = std::iter::once(None).chain(self.then.iter().map(Some));
        for (i, step) in steps.enumerate() {
            let (range, with) = match step {
                None => (self.range.clone(), self.with.as_str()),
                Some(step) => (step.range(&text), step.with.as_str()),
            };
            let old = source.root().clone();
            source.edit(range.clone(), with);
            text.replace_range(range.clone(), with);
            assert_eq!(source.text(), text);
            assert_eq!(source.root().full_text(), text);
            assert!(
                source.root().spanless_eq(&full(&text)),
                "incremental reparse differs from a full parse after edit {i}, \
                 replacing {range:?} with {with:?}"
            );
            ids.update(&old, source.root());
            assert_eq!(
                identities(source.root(), &ids),
                ids.len(),
                "ids of nodes that went remain"
            );
        }
    }
}

/// Checks that every node of a tree has an identity that leads back to it,
/// and counts the nodes.
fn identities(node: &SyntaxNode, ids: &NodeIds) -> usize {
    let span = node.span();
    assert!(!span.is_detached(), "{:?} has no span", node.kind());
    let id = ids
        .id(span)
        .unwrap_or_else(|| panic!("{:?} has no identity", node.kind()));
    assert_eq!(ids.span(id), Some(span), "an identity leads elsewhere");
    1 + node
        .children()
        .map(|child| identities(child, ids))
        .sum::<usize>()
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
/// a dictionary, in a closure, in a function's arguments, by changing a
/// place in an array or a dictionary, and in an equation. None repeats `v`, which would make
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
    "$(#v)/2$",
    "$sqrt(#v)_i$",
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
/// blocks, scripts, equations, references and labels, words, and breaks, so that
/// random edits restructure the document as well as reflow it; and footnotes
/// (one long enough to split across pages), page-number references and
/// images, so that edits reach the page cache's queue of carried notes, the
/// layout passes that settle page references, and footnote numbers per page.
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
    "$",
    "$v^2 = u^2 + 2 a s$",
    "$ x &= 1/2 a t^2 <eq:l> \\ y $",
    "(m v - m u) / t",
    "sum_(i=1)^n ",
    "&",
    "żółć ",
    "biało-czerwony ",
    "A sentence long enough to move a line break or two further down the page. ",
    "#footnote[A note.]",
    "#footnote[A note long enough to be split across two pages when its anchor falls near the foot of one: it runs on over several lines, so that the page builder places as much of it as fits below the separator, carries the rest to the next page after a continuation mark, and keeps the carried part in the key of the next page's cache entry. It goes on a little longer still, to be sure that at least one line has to move.]",
    "#ref(<l>, form: \"page\")",
    "#ref(<eq:l>, form: \"page\")",
    "#image(\"assets/wave.png\", alt: \"A wave\", width: 40%)",
    "#figure[#image(\"assets/sledge.svg\", alt: \"A sledge\", width: 50%)][A sledge.] <l>",
];

/// What [`differential`] inserts into stylesheets: rules, declarations of
/// properties that layout reads (box properties, widows and orphans, page
/// breaks, the page size and footnote numbering per page), and the
/// punctuation that opens and closes them.
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
    "figure { padding: 6pt 10pt; border: 2pt solid #000000; background: #eeeeee; }\n",
    "paragraph { border: 1pt; padding: 4pt; }\n",
    "doc { widows: 4; orphans: 4; }\n",
    "heading { break-before: page; }\n",
    "heading[level=1] { break-before: odd-page; }\n",
    "page { page-size: A5; }\n",
    "page { page-size: 120mm 160mm; }\n",
    "page { counter-reset: footnote; }\n",
    "footnote { font-size: 14pt; }\n",
];

/// The images that [`CONTENT_SNIPPETS`] name, by the corpus project that
/// holds each and its path there.
const SNIPPET_IMAGES: &[(&str, &str)] = &[
    ("figures", "assets/wave.png"),
    ("acceptance", "assets/sledge.svg"),
];

/// The corpus projects, each given the images of [`SNIPPET_IMAGES`] as data
/// files, so that an inserted image is laid out whichever project the edit
/// is made to, rather than reported missing.
fn with_snippet_images() -> Vec<corpus::Project> {
    let mut projects = corpus::projects();
    let images: Vec<(String, Vec<u8>)> = SNIPPET_IMAGES
        .iter()
        .map(|&(name, path)| {
            let bytes = projects
                .iter()
                .find(|p| p.name == name)
                .and_then(|p| p.files.get(path))
                .unwrap_or_else(|| panic!("the corpus project {name} has {path}"))
                .clone();
            (path.to_string(), bytes)
        })
        .collect();
    for project in &mut projects {
        for (path, bytes) in &images {
            project
                .files
                .entry(path.clone())
                .or_insert_with(|| bytes.clone());
        }
    }
    projects
}

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
    /// Reads a project (the first byte picks one of [`corpus::projects`],
    /// given the images the snippets name)
    /// and then up to [`MAX_EDITS`] edits of five bytes each: the source
    /// (an index into the project's sources in order of path), where the
    /// edit starts (two bytes, little-endian, wrapped to the text's
    /// length in characters), how many characters it removes (up to 15),
    /// and what it inserts (an index into [`CONTENT_SNIPPETS`] or
    /// [`SHEET_SNIPPETS`], wrapped to one more than its length, which
    /// inserts nothing). Bytes left over are ignored. `None` without a
    /// project byte and at least one edit.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let (&first, rest) = data.split_first()?;
        // Read once: the fuzzer runs the target many times a second.
        static PROJECTS: OnceLock<Vec<corpus::Project>> = OnceLock::new();
        let projects = PROJECTS.get_or_init(with_snippet_images);
        let project = projects[usize::from(first) % projects.len()].clone();
        let ops = rest
            .chunks_exact(5)
            .take(MAX_EDITS)
            .map(|op| op.try_into().expect("chunks of five"))
            .collect::<Vec<_>>();
        // Without an edit, there is nothing to compare.
        if ops.is_empty() {
            return None;
        }
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
                .get(usize::from(snippet) % (snippets.len() + 1))
                .copied()
                .unwrap_or("")
                .to_string(),
        }
    }

    /// Applies the edits one at a time to a world through
    /// [`World::edit_source`], compiling every build of the project after
    /// each, and checks each time that the outputs, the diagnostics and the
    /// span maps of laid-out pages equal those of a world built afresh
    /// from the same texts.
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
                name: self.project.name.clone(),
                manifest: self.project.manifest.clone(),
                sources: sources.clone(),
                files: self.project.files.clone(),
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
                if corpus::laid_out(*target) {
                    assert_eq!(
                        span_map(&world, variant.as_ref(), *target),
                        span_map(&fresh_world, variant.as_ref(), *target),
                        "{build}: the span map of an incremental compile differs from a fresh one after the edits {applied:#?}"
                    );
                }
            }
        }
    }
}

/// Where a box or a glyph comes from: a file and a byte range, or `None`
/// for one that names no source.
type Origin = Option<(String, Range<usize>)>;

/// Where each box and glyph of a build's pages comes from, by page, in
/// painting order: a file and a byte range, or `None` for one that names
/// no source. Pages and glyph runs reused from a cache must point at the
/// nodes of the current revision, which the output bytes do not show.
fn span_map(world: &World, variant: Option<&Name>, target: Target) -> Vec<Vec<Origin>> {
    fn walk(world: &World, fragment: &Fragment, out: &mut Vec<Origin>) {
        let located = |l: Option<folio::diag::Location>| l.map(|l| (l.path.to_string(), l.bytes));
        out.push(located(span_map::locate_fragment(world, fragment)));
        for (_, item) in &fragment.items {
            match item {
                FragmentItem::Group(group) => walk(world, group, out),
                FragmentItem::Text(run) => out.extend(
                    run.glyphs
                        .iter()
                        .map(|glyph| located(span_map::locate_glyph(world, glyph))),
                ),
                _ => {}
            }
        }
    }
    let Some(pages) = folio::pages(world, target, variant).expect("nothing cancels the compile")
    else {
        return Vec::new();
    };
    pages
        .pages
        .iter()
        .map(|page| {
            let mut out = Vec::new();
            walk(world, &page.fragment, &mut out);
            out
        })
        .collect()
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
    random_bytes(seed, 1 + 5 * MAX_EDITS)
}

/// The manifest of the worlds [`document`] and [`compile`] make: one
/// stylesheet, which [`document`] leaves empty.
const BUILD_MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
styles = [\"style.fsty\"]
";

/// A world for a fuzz target: low evaluation limits, so that one input
/// stays fast whatever its scripts do, in CI mode, so that no system font
/// is read.
fn fuzz_world(manifest: &str, lockfile: Option<&str>) -> Result<World, Vec<folio::ProjectError>> {
    World::builder()
        .limits(STYLE_LIMITS)
        .mode(BuildMode::Ci)
        .build(manifest, lockfile)
}

/// Evaluates content markup and builds its document tree, without a
/// stylesheet.
pub fn document(data: &[u8]) {
    let Ok(content) = std::str::from_utf8(data) else {
        return;
    };
    let mut world = fuzz_world(BUILD_MANIFEST, None).expect("the fuzz manifest is valid");
    world.set_source("main.fol", content).expect("a valid path");
    world.set_source("style.fsty", "").expect("a valid path");
    let _ = folio::document(&world, None);
}

/// Reads the body of a LaTeX equation (SPEC §8.1) in `$:tex …$`, and
/// checks that every error of the build points into the file.
pub fn tex(data: &[u8]) {
    let Ok(body) = std::str::from_utf8(data) else {
        return;
    };
    if body.contains('$') {
        return;
    }
    let mut world = fuzz_world(BUILD_MANIFEST, None).expect("the fuzz manifest is valid");
    world
        .set_source("main.fol", &format!("$:tex {body}$"))
        .expect("a valid path");
    world.set_source("style.fsty", "").expect("a valid path");
    let Ok(checked) = folio::check(&world, Target::Pdf, None) else {
        return;
    };
    for diagnostic in checked.diagnostics.iter() {
        if diagnostic.severity == folio::Severity::Error {
            assert!(
                folio::diag::locate(&world, diagnostic.span).is_some(),
                "{diagnostic:?} points nowhere"
            );
        }
    }
}

/// LaTeX formulas for the seed corpus of [`tex`].
const TEX_SEEDS: &[&str] = &[
    r"\frac{1}{2} m v^2",
    r"\int_0^T P(t)\,\mathrm{d}t = \sum_{i=1}^{n} x_i^2",
    r"\left( \frac{a}{b} \right)^2 \sqrt[3]{x} \vec{F} \hat{x}",
    r"\begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}",
    r"|x| = \begin{cases} x & \text{if } x \geq 0 \\ -x & \text{otherwise} \end{cases}",
    r"\begin{align} v^2 &= u^2 + 2as \label{eq:a} \\ s &= ut \end{align}",
    r"\underbrace{a+b}_{n} \bigl( x \bigr] \{a\} \langle x \rangle \not= \fracc",
];

/// What [`compile`] puts declarations on: the elements of the core, and the
/// page.
pub const SELECTORS: &[&str] = &[
    "doc",
    "page",
    "paragraph",
    "heading",
    "heading[level=1]",
    "emph",
    "strong",
    "list",
    "list-item",
    "enum",
    "enum-item",
    "terms",
    "terms-item",
    "raw",
    "figure",
    "sidenote",
];

/// Lengths for [`compile`]: ordinary, zero, negative, huge, tiny, relative
/// to a font size, at and past the limit of a length (SPEC §5.1), and not
/// finite.
pub const LENGTHS: &[&str] = &[
    "0pt",
    "1pt",
    "12pt",
    "-12pt",
    "40mm",
    "2em",
    "-3em",
    "14400pt",
    "20000pt",
    "1e6pt",
    "-1e6pt",
    "1000001pt",
    "1e5em",
    "1e12pt",
    "1e300pt",
    "-1e300pt",
    "1e-300pt",
    "1e300em",
    "1e308pt * 10",
    "-1e308pt * 10",
];

/// Numbers for [`compile`]: ordinary, zero, negative and extreme.
pub const NUMBERS: &[&str] = &[
    "0",
    "1",
    "2",
    "-1",
    "1.5",
    "1e9",
    "1e300",
    "-1e300",
    "9223372036854775807",
    "-9223372036854775807",
];

/// Ratios for [`compile`].
pub const RATIOS: &[&str] = &["0%", "50%", "100%", "-50%", "1000%", "1e300%"];

/// A value of a property type for [`compile`], picked by a byte; `None` for
/// a type [`compile`] does not generate, such as a template.
pub fn property_value(ty: &folio_component::PropertyType, pick: u8) -> Option<String> {
    use folio_component::PropertyType as T;
    let pick = usize::from(pick);
    let from = |values: &[&str]| values[pick % values.len()].to_string();
    Some(match ty {
        T::None => "none".into(),
        T::Auto => "auto".into(),
        T::Bool => from(&["true", "false"]),
        T::Int | T::Number => from(NUMBERS),
        T::Length => from(LENGTHS),
        T::Ratio => from(RATIOS),
        T::Color => from(&["#f00", "#1f5fa8", "rgb(\"000000\")"]),
        T::Duration => from(&["0s", "1s", "-1s", "1e300s"]),
        T::Fraction => from(&["1fr", "0fr", "-1fr", "1e300fr"]),
        T::Str => from(&["\"Source Serif 4\"", "\"\"", "\"x\""]),
        T::Keyword(words) if !words.is_empty() => words[pick % words.len()].to_string(),
        T::Call(names) if !names.is_empty() => format!("{}(x)", names[pick % names.len()]),
        T::List(item) => {
            let first = property_value(item, pick.to_le_bytes()[0])?;
            if pick % 2 == 0 {
                first
            } else {
                let second = property_value(item, (pick / 2).to_le_bytes()[0])?;
                format!("{first} {second}")
            }
        }
        T::AnyOf(types) if !types.is_empty() => property_value(
            &types[pick % types.len()],
            (pick / types.len()).to_le_bytes()[0],
        )?,
        _ => return None,
    })
}

/// The stylesheet [`compile`] makes of bytes: for each three, a declaration
/// with a selector of [`SELECTORS`], a property of the built-in registry
/// and a value of its type ([`property_value`]). `page-size` also takes
/// widths and heights.
pub fn compile_stylesheet(data: &[u8]) -> String {
    static PROPERTIES: OnceLock<Vec<(String, folio_component::PropertyType)>> = OnceLock::new();
    let properties = PROPERTIES.get_or_init(|| {
        folio::style::Properties::new(None, [])
            .iter()
            .map(|(_, p)| (p.name.to_string(), p.ty.clone()))
            .collect()
    });
    let mut out = String::new();
    for decl in data.chunks_exact(3) {
        let selector = SELECTORS[usize::from(decl[0]) % SELECTORS.len()];
        let (name, ty) = &properties[usize::from(decl[1]) % properties.len()];
        let value = if name == "page-size" && decl[2] % 2 == 1 {
            let width = LENGTHS[usize::from(decl[2] / 2) % LENGTHS.len()];
            let height = LENGTHS[usize::from(decl[2] / 4) % LENGTHS.len()];
            Some(format!("{width} {height}"))
        } else {
            property_value(ty, decl[2])
        };
        if let Some(value) = value {
            out.push_str(&format!("{selector} {{ {name}: {value}; }}\n"));
        }
    }
    out
}

/// Builds content with a generated stylesheet to pages and a PDF: the
/// bytes before the first NUL are the content, those after it the
/// declarations ([`compile_stylesheet`]).
pub fn compile(data: &[u8]) {
    let (content, sheet) = match data.iter().position(|b| *b == 0) {
        Some(nul) => (&data[..nul], &data[nul + 1..]),
        None => (data, &[][..]),
    };
    let Ok(content) = std::str::from_utf8(content) else {
        return;
    };
    let sheet = compile_stylesheet(sheet);
    let mut world = fuzz_world(BUILD_MANIFEST, None).expect("the fuzz manifest is valid");
    world.set_source("main.fol", content).expect("a valid path");
    world
        .set_source("style.fsty", &sheet)
        .expect("a valid path");
    let _ = folio::compile(&world, Target::Pdf, None);
}

/// Reads a manifest, and compiles every build of a valid one with a
/// small document.
pub fn manifest(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(mut world) = fuzz_world(text, None) else {
        return;
    };
    let entry = world.manifest().project.entry.v.to_string();
    if world.set_source(&entry, "= A\n\nText.\n").is_err() {
        return;
    }
    let _ = folio::compile_all(&world);
}

/// Reads a lockfile beside a fixed manifest, and compiles a valid one.
pub fn lockfile(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(mut world) = fuzz_world(NESTING_MANIFEST, Some(text)) else {
        return;
    };
    world
        .set_source("main.fol", "Text.\n")
        .expect("a valid path");
    let _ = folio::compile(&world, Target::Pdf, None);
}

/// Decodes a display list message of `folio-render`'s wire format, which
/// a preview client receives (SPEC §12.2), and checks that what decodes
/// encodes to a message that decodes to the same.
pub fn wire(data: &[u8]) {
    use folio_render::wire::{self, Message};
    let Ok(message) = wire::decode(data) else {
        return;
    };
    let bytes = match &message {
        Message::List(list) => wire::encode(list),
        Message::Spans(spans) => wire::encode_spans(spans),
    };
    assert_eq!(
        wire::decode(&bytes),
        Ok(message),
        "a decoded message encodes differently"
    );
}

/// The seeds of [`wire`]: the page of [`crate::golden::boxes`] and its
/// span map, and a list with a dashed path.
fn wire_seeds() -> Vec<(&'static str, String, Vec<u8>)> {
    use folio_component::{Abs, Color, Dash, Path, Point, Shape, Size, SourceRef, Stroke};
    use folio_render::{DisplayList, wire};
    let (_, pages) = crate::golden::boxes();
    let mut out = Vec::new();
    for (i, list) in crate::golden::display_lists(&pages).iter().enumerate() {
        out.push(("wire", format!("boxes-{}.bin", i + 1), wire::encode(list)));
        out.push((
            "wire",
            format!("boxes-{}.spans.bin", i + 1),
            wire::encode_spans(&list.spans()),
        ));
    }
    let mut stroke = Stroke::solid(Color::rgba(0, 0, 255, 128), Abs::pt(1.5));
    stroke.dash = Some(Dash {
        array: [Abs::pt(2.0), Abs::pt(1.0)].into_iter().collect(),
        phase: Abs::pt(0.5),
    });
    let mut path = Path::rect(Abs::pt(10.0), Abs::pt(5.0));
    path.cubic_to(
        Point::new(Abs::pt(1.0), Abs::pt(2.0)),
        Point::new(Abs::pt(3.0), Abs::pt(4.0)),
        Point::new(Abs::pt(5.0), Abs::pt(6.0)),
    );
    let mut fragment = Fragment::new(Size::new(Abs::pt(20.0), Abs::pt(20.0)));
    fragment.push(
        Point::new(Abs::pt(2.0), Abs::pt(3.0)),
        FragmentItem::Shape {
            shape: Shape::stroked(path, stroke),
            source: SourceRef(5),
        },
    );
    let list = DisplayList::from_fragment(&fragment);
    out.push(("wire", "path.bin".into(), wire::encode(&list)));
    out
}

/// How many random inputs of [`compile`] the seed corpus holds per corpus
/// project.
pub const COMPILE_SEEDS: u64 = 4;

/// The three bytes of [`compile_stylesheet`] that declare a property of the
/// built-in registry on a selector of [`SELECTORS`] with a value byte.
///
/// # Panics
///
/// When the selector or the property is unknown.
pub fn compile_decl(selector: &str, property: &str, value: u8) -> [u8; 3] {
    let selector = SELECTORS
        .iter()
        .position(|s| *s == selector)
        .expect("a selector of SELECTORS");
    let property = folio::style::Properties::new(None, [])
        .iter()
        .position(|(_, p)| p.name.to_string() == property)
        .expect("a built-in property");
    [
        u8::try_from(selector).expect("few selectors"),
        u8::try_from(property).expect("fewer than 256 built-in properties"),
        value,
    ]
}

/// Bytes from a seed, the same on every platform (SplitMix64).
fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        out.extend((z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}

/// How many random inputs of [`differential`] the seed corpus holds.
pub const DIFFERENTIAL_SEEDS: u64 = 4;

/// Sequences of edits to a file for the seed corpus of [`reparse`], by kind:
/// for each pair of delimiters, the opening one typed a third of the way in,
/// the closing one typed at the middle, and both deleted again; and the same
/// file made dense with syntax errors (an unbalanced delimiter every forty
/// characters), with a binding typed into its middle a character at a time
/// and deleted again. Authors pass through such broken states on every
/// keystroke, and they are where a reparse is most likely to go wrong
/// (#334).
pub fn edit_sequences(text: &str, stylesheet: bool) -> Vec<(String, Edit)> {
    let (pairs, scatter, typed): (&[(&str, &str)], &[&str], &str) = if stylesheet {
        (
            &[("{", "}"), ("(", ")"), ("[", "]"), ("\"", "\"")],
            &["{", "}", "(", "\"", ";", ":", "[", "@variant v {", "/*"],
            "--x: 4pt{;",
        )
    } else {
        (
            &[
                ("{", "}"),
                ("[", "]"),
                ("\"", "\""),
                ("$", "$"),
                ("#(", ")"),
            ],
            &[
                "{",
                "[",
                "\"",
                "(",
                "*",
                "$",
                "}",
                "]",
                ")",
                "#",
                "_",
                "`",
                "#let x = ",
            ],
            "#let x = \"[",
        )
    };
    let n = text.chars().count();
    let at = |i: usize| text.char_indices().nth(i).map_or(text.len(), |(b, _)| b);
    let mut out = Vec::new();
    for (i, (open, close)) in pairs.iter().enumerate() {
        let (a, b) = (n / 3, n / 2 + open.chars().count());
        let then = vec![
            Step {
                at: b,
                len: 0,
                with: close.to_string(),
            },
            Step {
                at: b,
                len: close.chars().count(),
                with: String::new(),
            },
            Step {
                at: a,
                len: open.chars().count(),
                with: String::new(),
            },
        ];
        let edit = Edit {
            stylesheet,
            text: text.to_string(),
            range: at(a)..at(a),
            with: open.to_string(),
            then,
        };
        out.push((format!("pair-{i}"), edit));
    }
    let mut dense = String::new();
    for (i, c) in text.chars().enumerate() {
        if i % 40 == 39 {
            dense.push_str(scatter[i / 40 % scatter.len()]);
        }
        dense.push(c);
    }
    let middle = dense.chars().count() / 2;
    let start = dense
        .char_indices()
        .nth(middle)
        .map_or(dense.len(), |(b, _)| b);
    let mut chars = typed.chars();
    let first = chars.next().expect("a typed text").to_string();
    let mut then: Vec<Step> = chars
        .enumerate()
        .map(|(i, c)| Step {
            at: middle + 1 + i,
            len: 0,
            with: c.to_string(),
        })
        .collect();
    then.extend((0..typed.chars().count()).rev().map(|i| Step {
        at: middle + i,
        len: 1,
        with: String::new(),
    }));
    out.push((
        "dense-typed".to_string(),
        Edit {
            stylesheet,
            text: dense,
            range: start..start,
            with: first,
            then,
        },
    ));
    out
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
    // Each wrapper past the nesting limit, and all of them in turn.
    for (i, _) in WRAPPERS.iter().enumerate() {
        let wrapper = u8::try_from(i).expect("few wrappers");
        out.push(("nesting", format!("wrapper-{i}.bin"), vec![wrapper, 255]));
    }
    let mixed: Vec<u8> = (0..WRAPPERS.len())
        .flat_map(|i| [u8::try_from(i).expect("few wrappers"), 7])
        .collect();
    out.push(("nesting", "mixed.bin".into(), mixed));
    // Content alone, for `document`; content with declarations for
    // `compile`: random ones, and the page and font sizes at the extremes.
    for (name, text) in &contents {
        out.push(("document", name.clone(), text.as_bytes().to_vec()));
    }
    for project in corpus::projects() {
        let entry = project.sources.get("main.fol").cloned().unwrap_or_default();
        for seed in 0..COMPILE_SEEDS {
            let mut bytes = entry.clone().into_bytes();
            bytes.push(0);
            bytes.extend(random_bytes(seed, 60));
            out.push((
                "compile",
                format!("{}-random-{seed}.bin", project.name),
                bytes,
            ));
        }
        let extremes: Vec<u8> = (0..LENGTHS.len())
            .flat_map(|i| {
                let i = u8::try_from(i).expect("few lengths");
                // `page-size` as a width and a height, and `font-size`.
                [
                    compile_decl("page", "page-size", 2 * i + 1),
                    compile_decl("doc", "font-size", i),
                    compile_decl("paragraph", "indent", i),
                ]
            })
            .flatten()
            .collect();
        let mut bytes = entry.into_bytes();
        bytes.push(0);
        bytes.extend(extremes);
        out.push(("compile", format!("{}-extremes.bin", project.name), bytes));
    }
    for project in corpus::projects() {
        out.push((
            "manifest",
            format!("{}.toml", project.name),
            project.manifest.into_bytes(),
        ));
    }
    out.push(("manifest", "minimal.toml".into(), NESTING_MANIFEST.into()));
    for (i, formula) in TEX_SEEDS.iter().enumerate() {
        out.push((
            "tex",
            format!("formula-{i}.tex"),
            formula.as_bytes().to_vec(),
        ));
    }
    out.push(("lockfile", "empty.lock".into(), b"version = 1\n".to_vec()));
    out.push((
        "lockfile",
        "font.lock".into(),
        b"version = 1\n\n[[font]]\nfamily = \"Source Serif 4\"\n\
          postscript-name = \"SourceSerif4-Regular\"\n\
          hash = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n"
            .to_vec(),
    ));
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
                    then: Vec::new(),
                };
                if let Some(bytes) = edit.encode() {
                    out.push(("reparse", format!("{name}.{kind}.bin"), bytes));
                }
            }
            for (kind, edit) in edit_sequences(text, stylesheet) {
                if let Some(bytes) = edit.encode() {
                    out.push(("reparse", format!("{name}.{kind}.bin"), bytes));
                }
            }
        }
    }
    out.extend(wire_seeds());
    out.extend(
        crate::fuzz_image::seeds()
            .into_iter()
            .map(|(name, bytes)| ("image", name, bytes)),
    );
    out
}
