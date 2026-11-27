//! The font stage (SPEC §2.4, §2.5): which faces a project can use, whether
//! they pin its output, fallback, and the checks a build makes of the fonts
//! its text is set in.
//!
//! - **Faces** ([`fonts()`]): the bundled families, the project's font
//!   directory, the font cache's files for `folio.lock`, and the faces
//!   installed on the machine. Each face records what pins it ([`Pin`]):
//!   the compiler version, the project, or a lock by hash. A system face
//!   that the lockfile does not lock pins nothing.
//! - **Locking**: every `[[font]]` of `folio.lock` must be found with the
//!   hash it is locked by. A missing locked font, or one whose file has a
//!   different hash, is an error at its entry.
//! - **Lookup and fallback** ([`Fonts::select`], [`Fonts::resolve`]): a
//!   family is looked up among pinned faces first, and among unpinned
//!   system faces only when no pinned face has it. Text is split into runs
//!   of one script, and each run is set in the first font of the `font`
//!   chain that has all its characters, or character by character in the
//!   first that has each.
//! - **Checks** ([`check_fonts`]), of the segments the text stage sets, so
//!   of every character a build draws: a family that no face has is a warning;
//!   a character that no font of the chain has is an error, never a silent
//!   empty box; an unpinned system font is allowed in watch mode, a warning
//!   in a build and an error in a CI build or with strict formatting; and a
//!   font whose licence forbids embedding is an error in a PDF build.
//!
//! The fonts are inputs of the query database ([`Project`]); the files are
//! parsed once per process ([`FontFile::faces`]), so every compile and every
//! revision shares them.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Target, Value};
use folio_fonts::{
    FaceKey, Font, FontBook, FontFile, FontOrigin, FontStyle, FontVariant, SystemFont,
};
use folio_syntax::{DiagSpan, Span};
use folio_text::SegmentKind;

use crate::db::{Build, BuildMode, Db, Project};
use crate::diag::{self, Diagnostic};
use crate::pipeline::step_styled;
use crate::project::{FormattingMode, LockedFont};
use crate::style::{ComputedStyle, Properties};
use crate::text::{CollectedParagraph, collect_text};

/// What pins a face, so that a build with it gives the same output on
/// another machine (SPEC §2.4).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Pin {
    /// It is bundled with the compiler, which the project pins.
    Bundled,
    /// It is in the project's font directory.
    Project,
    /// `folio.lock` locks it by the hash of its file.
    Locked,
    /// Nothing: it is a system font that `folio.lock` does not lock.
    Unpinned,
}

/// A face a project can use.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Face {
    /// Where its file is.
    pub origin: FontOrigin,
    /// Its family, as stylesheets name it.
    pub family: EcoString,
    /// Its PostScript name.
    pub postscript_name: EcoString,
    /// What pins it.
    pub pin: Pin,
    source: Source,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
enum Source {
    Loaded(Font),
    /// A system face, read on first use.
    System(SystemFace),
}

impl Face {
    /// The face, read from its file if it is a system face that the world
    /// has not read yet. `None` when the file cannot be read.
    pub fn font(&self) -> Option<Font> {
        match &self.source {
            Source::Loaded(font) => Some(font.clone()),
            // A failed read is kept too, so that every query of a build
            // agrees on the face.
            Source::System(system) => system.read.get_or_init(|| system.font.load().ok()).clone(),
        }
    }
}

/// A system face and what reading its file gave, once it has been read.
#[derive(Debug, Clone)]
struct SystemFace {
    font: SystemFont,
    read: Arc<OnceLock<Option<Font>>>,
}

/// Equal when the faces are and they share what reading them gave, as the
/// faces of one [`SystemFaces`] do; a face of the store that replaced it is
/// not equal, so the queries that hold it are not kept.
impl PartialEq for SystemFace {
    fn eq(&self, other: &Self) -> bool {
        self.font == other.font && Arc::ptr_eq(&self.read, &other.read)
    }
}

impl Eq for SystemFace {}

impl Hash for SystemFace {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.font.hash(state);
    }
}

/// What a world has read of the system faces its documents use: each face's
/// file is read at most once, on first use, and a failed read is kept too, and every query and every
/// revision sees what that read gave, until the host sets the system fonts
/// again. A file that changes on disk meanwhile therefore cannot make two
/// queries of one build disagree; reading it is not a tracked input.
#[derive(Default)]
pub struct SystemFaces {
    read: Mutex<HashMap<SystemFont, Arc<OnceLock<Option<Font>>>>>,
}

impl SystemFaces {
    /// The cell that holds what reading a face gave.
    fn cell(&self, font: &SystemFont) -> Arc<OnceLock<Option<Font>>> {
        let mut read = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        read.entry(font.clone()).or_default().clone()
    }

    /// The number of faces that have been asked for.
    pub fn len(&self) -> usize {
        self.read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether no face has been asked for.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for SystemFaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemFaces")
            .field("faces", &self.len())
            .finish()
    }
}

/// The faces a project can use, indexed by family.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Fonts {
    faces: EcoVec<Face>,
    /// Pinned faces, by their index in the book.
    pinned: FontBook,
    pinned_faces: Vec<usize>,
    /// Unpinned system faces.
    system: FontBook,
    system_faces: Vec<usize>,
}

/// One run of text set in one face, or in none when no font of the chain
/// has its characters.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct FontRun {
    /// The run's byte range in the text.
    pub range: Range<usize>,
    /// The index of its face in [`Fonts::faces`].
    pub face: Option<usize>,
}

impl Fonts {
    /// Every face: bundled, project, locked, then unpinned system faces.
    pub fn faces(&self) -> &[Face] {
        &self.faces
    }

    /// Whether any face has a family name, compared without regard to case.
    pub fn contains_family(&self, family: &str) -> bool {
        self.pinned.contains_family(family) || self.system.contains_family(family)
    }

    /// The face of a family closest to a variant, as an index into
    /// [`Self::faces`]. Pinned faces are preferred: a system face is chosen
    /// only for a family no pinned face has.
    pub fn select(&self, family: &str, variant: FontVariant) -> Option<usize> {
        if let Some(i) = self.pinned.select(family, variant) {
            return Some(self.pinned_faces[i]);
        }
        self.system
            .select(family, variant)
            .map(|i| self.system_faces[i])
    }

    /// The family names faces have, in the case the fonts write them.
    pub fn family_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.faces.iter().map(|f| f.family.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Splits text into runs set in one face each (SPEC §2.5). The text is
    /// split into runs of one script, characters of no particular script
    /// (spaces, punctuation, combining marks) joining the run around them;
    /// a run is set in the first face of `chain` that has all its
    /// characters, or else each character in the first face that has it.
    /// `chain` holds indices into [`Self::faces`]; whitespace and invisible
    /// formatting characters need no glyph.
    pub fn resolve(&self, chain: &[usize], text: &str) -> Vec<FontRun> {
        let fonts: Vec<Option<Font>> = chain.iter().map(|&i| self.faces[i].font()).collect();
        let has = |k: usize, c: char| fonts[k].as_ref().is_some_and(|f| f.has_char(c));
        let mut out: Vec<FontRun> = Vec::new();
        let mut push = |range: Range<usize>, face: Option<usize>| match out.last_mut() {
            Some(last) if last.face == face && last.range.end == range.start => {
                last.range.end = range.end;
            }
            _ => out.push(FontRun { range, face }),
        };
        for run in script_runs(text) {
            let chars = || {
                text[run.clone()]
                    .char_indices()
                    .filter(|&(_, c)| !ignorable(c))
            };
            if let Some(k) = (0..chain.len()).find(|&k| chars().all(|(_, c)| has(k, c))) {
                push(run, Some(chain[k]));
                continue;
            }
            // Character by character; ignorable ones join the run before.
            // Ignorable characters join the run before them, or the
            // first run when they lead.
            let mut start = run.start;
            let mut face = None;
            let mut first = true;
            for (i, c) in text[run.clone()].char_indices() {
                let i = run.start + i;
                if ignorable(c) {
                    continue;
                }
                let here = (0..chain.len()).find(|&k| has(k, c)).map(|k| chain[k]);
                if !first && here != face {
                    push(start..i, face);
                    start = i;
                }
                first = false;
                face = here;
            }
            push(start..run.end, face);
        }
        out
    }
}

/// Splits text into runs of one script, as shaping does.
fn script_runs(text: &str) -> impl Iterator<Item = Range<usize>> {
    folio_text::script_runs(text)
        .into_iter()
        .map(|(range, _)| range)
}

/// Whether a character is drawn without a glyph of its own: whitespace,
/// controls and invisible formatting characters.
pub(crate) fn ignorable(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '\u{AD}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{E0100}'..='\u{E01EF}'
        )
}

/// A face found in a file, before the lockfile is checked.
struct Candidate {
    file: FontFile,
    font: Font,
    pin: Pin,
}

/// The faces a project can use. Reports the files that hold no font and
/// the fonts of `folio.lock` that are missing or do not have their locked
/// hash.
#[salsa::tracked(returns(ref))]
pub fn fonts(db: &dyn Db, project: Project) -> Fonts {
    let mut candidates: Vec<Candidate> = Vec::new();
    let files = project.bundled_fonts(db).iter().chain(project.fonts(db));
    for file in files {
        let pin = match file.origin() {
            FontOrigin::Bundled(_) => Pin::Bundled,
            FontOrigin::Project(_) => Pin::Project,
            // Pinned only once the lockfile is found to lock them.
            FontOrigin::Cache | FontOrigin::System(_) => Pin::Unpinned,
        };
        match file.faces() {
            Ok(faces) => candidates.extend(faces.iter().map(|font| Candidate {
                file: file.clone(),
                font: font.clone(),
                pin,
            })),
            Err(error) => diag::report(
                db,
                Diagnostic::warning(
                    DiagSpan::detached(),
                    eco_format!("{} is not used: {error}", file.origin()),
                ),
            ),
        }
    }

    let system = project.system_fonts(db);
    let mut taken = vec![false; system.len()];
    for locked in &project.lockfile(db).fonts {
        check_locked(db, locked, &mut candidates, system, &mut taken);
    }

    let mut out = Fonts::default();
    for candidate in candidates {
        // A cache file that no lock asks for is not the project's.
        if candidate.pin == Pin::Unpinned && candidate.file.origin() == &FontOrigin::Cache {
            continue;
        }
        let info = candidate.font.info();
        let face = Face {
            origin: candidate.file.origin().clone(),
            family: info.family.clone(),
            postscript_name: info.postscript_name.clone(),
            pin: candidate.pin,
            source: Source::Loaded(candidate.font.clone()),
        };
        let key = FaceKey::from(info);
        if face.pin == Pin::Unpinned {
            out.system.push(&key);
            out.system_faces.push(out.faces.len());
        } else {
            out.pinned.push(&key);
            out.pinned_faces.push(out.faces.len());
        }
        out.faces.push(face);
    }
    for (font, _) in system.iter().zip(&taken).filter(|(_, taken)| !**taken) {
        out.system.push(&font.key);
        out.system_faces.push(out.faces.len());
        out.faces.push(Face {
            origin: FontOrigin::System(font.path.clone()),
            family: font.key.families.first().cloned().unwrap_or_default(),
            postscript_name: font.postscript_name.clone(),
            pin: Pin::Unpinned,
            source: Source::System(SystemFace {
                font: font.clone(),
                read: db.system_faces().cell(font),
            }),
        });
    }
    out
}

/// Finds the face a `[[font]]` of `folio.lock` locks and marks it locked,
/// or reports why it is missing.
fn check_locked(
    db: &dyn Db,
    locked: &LockedFont,
    candidates: &mut Vec<Candidate>,
    system: &[SystemFont],
    taken: &mut [bool],
) {
    let name = &locked.postscript_name.v;
    let want = locked.hash.v.0;
    // Files that have the face, but with another hash.
    let mut changed: Vec<(FontOrigin, folio_fonts::FontHash)> = Vec::new();
    let mut found = false;
    for candidate in candidates.iter_mut() {
        if candidate.font.info().postscript_name != *name {
            continue;
        }
        let hash = candidate.file.hash();
        if hash.0 == want {
            found = true;
            if candidate.pin == Pin::Unpinned {
                candidate.pin = Pin::Locked;
            }
        } else {
            changed.push((candidate.file.origin().clone(), hash));
        }
    }
    if !found {
        // A locked system font: read the files of the faces of that name.
        for (i, font) in system.iter().enumerate() {
            if font.postscript_name != *name {
                continue;
            }
            let Ok(file) = font.file() else { continue };
            let hash = file.hash();
            if hash.0 != want {
                changed.push((file.origin().clone(), hash));
                continue;
            }
            if let Ok(face) = font.load() {
                taken[i] = true;
                found = true;
                candidates.push(Candidate {
                    file,
                    font: face,
                    pin: Pin::Locked,
                });
                break;
            }
        }
    }
    if found {
        return;
    }
    let diagnostic = match changed.first() {
        Some((origin, hash)) => Diagnostic::error(
            locked.hash.span,
            eco_format!("the font `{name}` does not have the hash it is locked by"),
        )
        .with_hint(eco_format!(
            "`folio.lock` locks {}, but {origin} has {hash}",
            locked.hash.v
        ))
        .with_hint(if *origin == FontOrigin::Cache {
            // The cache finds a file by the hash it is locked by, so
            // locking the other hash would not find it.
            eco_format!(
                "the font cache file `fonts/{}` in {CACHE_DIR} holds another file: \
                 replace it with the locked file, or delete it",
                folio_fonts::FontHash(want).hex()
            )
        } else {
            "the file changed since it was locked: restore the locked file, or lock the new one"
                .into()
        }),
        None => Diagnostic::error(
            locked.postscript_name.span,
            eco_format!(
                "the locked font `{name}` (family `{}`) is not available",
                locked.family.v
            ),
        )
        .with_hint(eco_format!(
            "put its file in the project's `fonts` directory, or copy it into the font cache \
             as `fonts/{}` in {CACHE_DIR}",
            folio_fonts::FontHash(locked.hash.v.0).hex()
        )),
    };
    diag::report(db, diagnostic);
}

/// Where the font cache is, for hints that say where to put a file: its
/// directory is `fonts` in this one (`folio_fonts::cache_root`).
const CACHE_DIR: &str = "Folio's cache directory (`$FOLIO_CACHE_DIR`, by default `folio` in the user's cache directory)";

/// The fonts a node's text asks for: the family chain and the variant.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct Request {
    pub(crate) families: EcoVec<EcoString>,
    pub(crate) variant: FontVariant,
}

impl Request {
    pub(crate) fn of(style: &ComputedStyle, props: &Properties) -> Self {
        let families = match style.value(props, props.builtin("font")) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| match v {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            Some(Value::Str(s)) => EcoVec::from([s.clone()]),
            _ => EcoVec::new(),
        };
        let weight = match style.value(props, props.builtin("font-weight")) {
            Some(Value::Int(n)) => u16::try_from((*n).clamp(1, 1000)).unwrap_or(400),
            Some(Value::Keyword(k)) if k.as_str() == "bold" => 700,
            _ => 400,
        };
        let style = match style.value(props, props.builtin("font-style")) {
            Some(Value::Keyword(k)) if k.as_str() == "italic" => FontStyle::Italic,
            Some(Value::Keyword(k)) if k.as_str() == "oblique" => FontStyle::Oblique,
            _ => FontStyle::Normal,
        };
        Self {
            families,
            variant: FontVariant {
                weight,
                style,
                stretch: 1000,
            },
        }
    }
}

/// Checks the fonts a build's text is set in: reports families that no
/// face has, characters that no font of their chain has, unpinned system
/// fonts as the build mode says, and fonts that forbid embedding in a PDF.
///
/// It reads the segments of the text stage ([`collect_text`]), so it sees every
/// character the build sets, whatever made it: markup's text, the text of
/// `raw`, a link's destination shown as text, the quotation marks of the
/// text's language.
#[salsa::tracked]
pub fn check_fonts(db: &dyn Db, build: Build<'_>) {
    let project = build.project(db);
    let Some(styled) = step_styled(db, build) else {
        return;
    };
    let Some(text) = collect_text(db, build) else {
        return;
    };
    let fonts = fonts(db, project);
    let props = &styled.properties;
    let strict = project.manifest(db).formatting.mode.v == FormattingMode::Strict;
    let mode = *project.mode(db);
    let pdf = build.target(db) == Target::Pdf;

    let mut chains: HashMap<Request, EcoVec<usize>> = HashMap::new();
    let mut unknown: HashSet<EcoString> = HashSet::new();
    // The requests whose families were checked, and the faces.
    let mut checked: HashSet<Request> = HashSet::new();
    let mut used: HashSet<usize> = HashSet::new();
    // Requests and faces met only at segments of spaces and invisible
    // characters so far, with where. They are checked there only if no
    // text with a glyph to draw asks for them, so a diagnostic points at
    // text rather than at a blank where it can.
    let mut blank_requests: Vec<(Request, Span)> = Vec::new();
    let mut blank_faces: Vec<(usize, Span)> = Vec::new();
    let mut check_request = |request: &Request, chain: &[usize], span: Span| {
        for family in &request.families {
            if fonts.select(family, request.variant).is_none()
                && unknown.insert(family.to_lowercase())
            {
                report_unknown(db, fonts, family, span);
            }
        }
        if chain.is_empty() {
            report_no_chain(db, request, span);
        }
    };
    for paragraph in &text.paragraphs {
        let string = paragraph.paragraph.text.as_str();
        let segments = &paragraph.paragraph.segments;
        // The characters no font has, by chain and by the source node they
        // are the text of, in order.
        let mut missing: Vec<(EcoVec<usize>, Span, Vec<char>)> = Vec::new();
        let mut groups: HashMap<(EcoVec<usize>, Span), usize> = HashMap::new();
        for (segment, info) in segments.iter().zip(&paragraph.segments) {
            if !matches!(segment.kind, SegmentKind::Text { .. }) {
                continue;
            }
            let range = segment.range.clone();
            let span = || segment_span(paragraph, string, range.clone());
            let request = Request::of(&info.style, props);
            let chain = chains
                .entry(request.clone())
                .or_insert_with(|| {
                    request
                        .families
                        .iter()
                        .filter_map(|family| fonts.select(family, request.variant))
                        .collect()
                })
                .clone();
            // Spaces need no glyph but are set in a face all the same,
            // which gives their width; without one, layout drops them.
            let visible = string[range.clone()].chars().any(|c| !ignorable(c));
            if !checked.contains(&request) {
                if visible {
                    checked.insert(request.clone());
                    check_request(&request, &chain, span());
                } else if !blank_requests.iter().any(|(r, _)| *r == request) {
                    blank_requests.push((request.clone(), span()));
                }
            }
            if chain.is_empty() {
                continue;
            }
            match info.face {
                Some(face) if !used.contains(&face) => {
                    if visible {
                        used.insert(face);
                        check_face(db, &fonts.faces[face], span(), mode, strict, pdf);
                    } else if !blank_faces.iter().any(|(f, _)| *f == face) {
                        blank_faces.push((face, span()));
                    }
                }
                Some(_) => {}
                None => {
                    for (i, c) in string[range.clone()].char_indices() {
                        if ignorable(c) {
                            continue;
                        }
                        let at = source_span(paragraph, range.start + i).unwrap_or(paragraph.span);
                        let group = *groups.entry((chain.clone(), at)).or_insert_with(|| {
                            missing.push((chain.clone(), at, Vec::new()));
                            missing.len() - 1
                        });
                        let chars = &mut missing[group].2;
                        if !chars.contains(&c) {
                            chars.push(c);
                        }
                    }
                }
            }
        }
        for (chain, span, chars) in missing {
            report_missing(db, fonts, &chain, &chars, span);
        }
    }
    for (request, span) in blank_requests {
        if !checked.contains(&request) {
            check_request(&request, &chains[&request], span);
        }
    }
    for (face, span) in blank_faces {
        if !used.contains(&face) {
            check_face(db, &fonts.faces[face], span, mode, strict, pdf);
        }
    }
    // The faces formulas are set in, each at the first formula set in it,
    // where Folio lays formulas out (SPEC §8.2): checked as the faces of
    // text are.
    if matches!(build.target(db), Target::Pdf | Target::Live) {
        for math in &text.maths {
            if let Some(face) = crate::layout::math_face(fonts, &math.settings.families)
                && used.insert(face)
            {
                check_face(db, &fonts.faces[face], math.span, mode, strict, pdf);
            }
        }
    }
}

/// The source node whose text a byte of a paragraph's text is, if any.
/// The sources are in order and do not overlap.
fn source_span(paragraph: &CollectedParagraph, at: usize) -> Option<Span> {
    let sources = &paragraph.sources;
    let i = sources.partition_point(|s| s.range.end <= at);
    sources
        .get(i)
        .filter(|s| s.range.contains(&at))
        .map(|s| s.span)
}

/// Where to report a problem with a segment of a paragraph: the source of
/// its first character that has a glyph, else of its first character, else
/// the element whose content the paragraph is.
fn segment_span(paragraph: &CollectedParagraph, text: &str, range: Range<usize>) -> Span {
    text[range.clone()]
        .char_indices()
        .find(|&(_, c)| !ignorable(c))
        .and_then(|(i, _)| source_span(paragraph, range.start + i))
        .or_else(|| source_span(paragraph, range.start))
        .unwrap_or(paragraph.span)
}

/// Reports a `font` chain of which no family is available, once per chain
/// rather than for every character set in it.
fn report_no_chain(db: &dyn Db, request: &Request, span: Span) {
    let names: Vec<String> = request.families.iter().map(|f| format!("`{f}`")).collect();
    let message = if names.is_empty() {
        EcoString::from("the text has no font: `font` names no family")
    } else {
        eco_format!(
            "no font of the `font` chain is available: {}",
            names.join(", ")
        )
    };
    diag::report(
        db,
        Diagnostic::error(span, message).with_hint(
            "name an available family in `font`, such as the bundled `New Computer Modern`",
        ),
    );
}

fn report_unknown(db: &dyn Db, fonts: &Fonts, family: &str, span: Span) {
    let mut diagnostic = Diagnostic::warning(
        span,
        eco_format!("the font family `{family}` is not available"),
    );
    let names = fonts.family_names();
    let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let refs: Vec<&str> = lower.iter().map(String::as_str).collect();
    diagnostic = match crate::project::suggest(&family.to_lowercase(), &refs) {
        Some(close) => {
            let i = refs.iter().position(|r| *r == close).unwrap_or(0);
            diagnostic.with_hint(eco_format!("did you mean `{}`?", names[i]))
        }
        None => diagnostic.with_hint(
            "put the font in the project's `fonts` directory, or use a bundled family such as `Source Serif 4`",
        ),
    };
    diag::report(db, diagnostic);
}

fn report_missing(db: &dyn Db, fonts: &Fonts, chain: &[usize], missing: &[char], span: Span) {
    const SHOWN: usize = 5;
    let mut list: Vec<String> = missing
        .iter()
        .take(SHOWN)
        .map(|c| format!("`{c}` (U+{:04X})", u32::from(*c)))
        .collect();
    if missing.len() > SHOWN {
        list.push(format!("{} more", missing.len() - SHOWN));
    }
    let tried: Vec<String> = chain
        .iter()
        .map(|&i| format!("`{}`", fonts.faces[i].postscript_name))
        .collect();
    let (glyph, has) = if missing.len() == 1 {
        ("a glyph", "it")
    } else {
        ("glyphs", "them")
    };
    diag::report(
        db,
        Diagnostic::error(
            span,
            eco_format!(
                "no font of the `font` chain has {glyph} for {}",
                list.join(", ")
            ),
        )
        .with_hint(eco_format!("the fonts tried are {}", tried.join(", ")))
        .with_hint(eco_format!(
            "add a font that has {has} to `font` in a stylesheet"
        )),
    );
}

/// Reports what a face that text is set in costs the build: an unpinned
/// system font as the mode says, a font that forbids embedding in a PDF.
/// Watch mode keeps no output, so a system font is allowed there even with
/// strict formatting; the strict build that writes output reports it.
fn check_face(db: &dyn Db, face: &Face, span: Span, mode: BuildMode, strict: bool, pdf: bool) {
    if face.pin == Pin::Unpinned && mode != BuildMode::Watch {
        let message = eco_format!(
            "`{}` is a system font, so the output depends on this machine",
            face.postscript_name
        );
        let diagnostic = if mode == BuildMode::Ci || strict {
            Diagnostic::error(span, message)
        } else {
            Diagnostic::warning(span, message)
        };
        diag::report(
            db,
            diagnostic
                .with_hint(eco_format!(
                    "{} is not locked: lock it with a `[[font]]` entry in `folio.lock` \
                     (family `{}`, postscript-name `{}` and the `sha256:` hash of its file), \
                     or put the file in the project's `fonts` directory",
                    face.origin,
                    face.family,
                    face.postscript_name
                ))
                .with_hint(eco_format!(
                    "a locked font is found among the system fonts, or, on a machine that \
                     does not have it, in the font cache as `fonts/<the hash's 64 hex digits>` \
                     in {CACHE_DIR}"
                )),
        );
    }
    if pdf
        && let Some(font) = face.font()
        && !font.info().embedding.allows_embedding()
    {
        diag::report(
            db,
            Diagnostic::error(
                span,
                eco_format!(
                    "the licence of `{}` forbids embedding it, so a PDF cannot use it",
                    face.postscript_name
                ),
            )
            .with_hint("choose another font for this text"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_scripts() {
        let text = "Abc, αβγ. Def";
        let runs: Vec<&str> = script_runs(text).map(|r| &text[r]).collect();
        assert_eq!(runs, ["Abc, ", "αβγ. ", "Def"]);
        assert_eq!(script_runs("1, 2").collect::<Vec<_>>(), vec![0..4]);
        assert_eq!(script_runs("").count(), 0);
    }
}
