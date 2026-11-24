//! Showing diagnostics: resolving their spans to files, lines and columns,
//! rendering them for a terminal with source excerpts, and serialising them
//! to JSON for other front ends (SPEC §12.1, §12.3).
//!
//! The compiler reports [`Diagnostic`]s with span numbers, which stay valid
//! while the text around them moves. A front end resolves them against the
//! files of the same revision, through [`Files`], which [`World`] implements:
//!
//! ```
//! use folio::{Target, World, diag};
//!
//! let mut world = World::new(
//!     "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n",
//! )
//! .unwrap();
//! world.set_source("main.fol", "= Forces\n\n#box(\n").unwrap();
//! let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
//!
//! let text = diag::render(&world, &compiled.diagnostics, false);
//! assert!(text.contains("┌─ main.fol:3:5"));
//!
//! let json = diag::to_json(&world, &compiled.diagnostics);
//! assert!(json.contains(r#""path": "main.fol""#));
//! ```
//!
//! [`World`]: crate::World

use std::collections::HashMap;
use std::ops::Range;

use codespan_reporting::diagnostic as cs;
use codespan_reporting::files::Error as FilesError;
use codespan_reporting::term;
use ecow::EcoString;
use serde::Serialize;

use folio_core::diag::{Diagnostic, Severity};
use folio_syntax::{DiagSpan, DiagSpanKind, FileId, Lines, Source};

/// The files that diagnostics point into.
pub trait Files {
    /// The path of a file as diagnostics show it, relative to the project
    /// directory.
    fn path(&self, id: FileId) -> Option<EcoString>;

    /// A `.fol` or `.fsty` source, whose syntax tree resolves span numbers.
    fn source(&self, id: FileId) -> Option<Source>;

    /// The text of any file, by lines. By default, that of its source.
    fn lines(&self, id: FileId) -> Option<Lines<String>> {
        self.source(id).map(|source| source.lines().clone())
    }
}

/// Files given by their text, which spans can point into only by byte range.
/// For diagnostics that come before there is a world, such as the errors of
/// a manifest that does not parse.
#[derive(Debug, Clone, Default)]
pub struct TextFiles {
    files: HashMap<FileId, (EcoString, Lines<String>)>,
}

impl TextFiles {
    /// No files.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file, or replaces the file with the same id.
    pub fn insert(&mut self, id: FileId, path: impl Into<EcoString>, text: impl Into<String>) {
        self.files
            .insert(id, (path.into(), Lines::new(text.into())));
    }
}

impl Files for TextFiles {
    fn path(&self, id: FileId) -> Option<EcoString> {
        self.files.get(&id).map(|(path, _)| path.clone())
    }

    fn source(&self, _: FileId) -> Option<Source> {
        None
    }

    fn lines(&self, id: FileId) -> Option<Lines<String>> {
        self.files.get(&id).map(|(_, lines)| lines.clone())
    }
}

/// A part of a file that a diagnostic points at.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize)]
pub struct Location {
    /// The file's id.
    #[serde(skip)]
    pub id: FileId,
    /// The file's path, relative to the project directory.
    pub path: EcoString,
    /// The byte range in the file's text.
    pub bytes: Range<usize>,
    /// Where the range starts.
    pub start: Position,
    /// Where the range ends.
    pub end: Position,
}

/// A line and column in a file, both counted from 1. Columns count Unicode
/// scalar values (`char`s), not bytes or UTF-16 code units; the byte range
/// of a [`Location`] is exact for any other measure.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct Position {
    /// The line.
    pub line: usize,
    /// The column.
    pub column: usize,
}

/// Resolves a span to a location, or `None` when it is detached, its file is
/// not among `files`, or it does not point into the file's current text (it
/// is from another revision).
pub fn locate(files: &dyn Files, span: DiagSpan) -> Option<Location> {
    let (id, bytes) = match span.get() {
        DiagSpanKind::Detached => return None,
        DiagSpanKind::Range { id, range } => (id, range),
        DiagSpanKind::Number { id, num, sub_range } => {
            let source = files.source(id)?;
            let node = source.range(num, None)?;
            let bytes = match sub_range {
                Some(sub) => {
                    let bytes = sub.to_absolute(node.start);
                    (bytes.end <= node.end).then_some(bytes)?
                }
                None => node,
            };
            (id, bytes)
        }
    };
    let path = files.path(id)?;
    let lines = files.lines(id)?;
    let position = |byte| {
        let (line, column) = lines.byte_to_line_column(byte)?;
        Some(Position {
            line: line + 1,
            column: column + 1,
        })
    };
    if bytes.start > bytes.end {
        return None;
    }
    Some(Location {
        id,
        path,
        start: position(bytes.start)?,
        end: position(bytes.end)?,
        bytes,
    })
}

/// A diagnostic with its spans resolved, as front ends other than the
/// terminal receive it. Related spans and fixes that do not resolve are left
/// out; so is a fix with any such edit, since applying the rest would be
/// wrong.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize)]
pub struct Resolved {
    /// `"error"` or `"warning"`.
    pub severity: &'static str,
    /// The lint or check that reported it, such as `override-literal`.
    pub code: Option<EcoString>,
    /// What is wrong.
    pub message: EcoString,
    /// The source it is about, or `None` when it has none.
    pub location: Option<Location>,
    /// Other source that explains it.
    pub related: Vec<ResolvedRelated>,
    /// How to fix it.
    pub hints: Vec<EcoString>,
    /// Edits that fix it.
    pub fixes: Vec<ResolvedFix>,
}

/// A related span of a [`Resolved`] diagnostic.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize)]
pub struct ResolvedRelated {
    /// The source.
    pub location: Location,
    /// What it has to do with the diagnostic.
    pub message: EcoString,
}

/// A quick fix of a [`Resolved`] diagnostic.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize)]
pub struct ResolvedFix {
    /// What the fix does.
    pub title: EcoString,
    /// The edits, which do not overlap.
    pub edits: Vec<ResolvedEdit>,
}

/// One edit of a [`ResolvedFix`].
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize)]
pub struct ResolvedEdit {
    /// The source to replace.
    pub location: Location,
    /// What to replace it with.
    pub replacement: EcoString,
}

/// Resolves the spans of a diagnostic.
pub fn resolve(files: &dyn Files, diagnostic: &Diagnostic) -> Resolved {
    Resolved {
        severity: diagnostic.severity.name(),
        code: diagnostic.code.clone(),
        message: diagnostic.message.clone(),
        location: locate(files, diagnostic.span),
        related: diagnostic
            .related
            .iter()
            .filter_map(|related| {
                Some(ResolvedRelated {
                    location: locate(files, related.span)?,
                    message: related.message.clone(),
                })
            })
            .collect(),
        hints: diagnostic.hints.iter().cloned().collect(),
        fixes: diagnostic
            .fixes
            .iter()
            .filter_map(|fix| {
                let edits = fix
                    .edits
                    .iter()
                    .map(|edit| {
                        Some(ResolvedEdit {
                            location: locate(files, edit.span)?,
                            replacement: edit.replacement.clone(),
                        })
                    })
                    .collect::<Option<_>>()?;
                Some(ResolvedFix {
                    title: fix.title.clone(),
                    edits,
                })
            })
            .collect(),
    }
}

/// Serialises diagnostics, with their spans resolved, to a pretty-printed
/// JSON array of [`Resolved`] objects.
pub fn to_json(files: &dyn Files, diagnostics: &[Diagnostic]) -> String {
    let resolved: Vec<Resolved> = diagnostics.iter().map(|d| resolve(files, d)).collect();
    serde_json::to_string_pretty(&resolved).expect("diagnostics serialise to JSON")
}

/// Renders diagnostics for a terminal, each with excerpts of the source it
/// points at, in the order given. `color` adds ANSI colour codes.
///
/// Hints and the titles of fixes follow the excerpts. A span that does not
/// resolve is left out; a diagnostic with none still shows its message.
pub fn render(files: &dyn Files, diagnostics: &[Diagnostic], color: bool) -> String {
    let resolved = ResolvedFiles::new(files);
    let config = term::Config::default();
    let mut ansi = term::termcolor::Buffer::ansi();
    let mut plain = String::new();
    let mut emit = |diagnostic: &cs::Diagnostic<FileId>| {
        if color {
            term::emit_to_write_style(&mut ansi, &config, &resolved, diagnostic)
        } else {
            term::emit_to_string(&mut plain, &config, &resolved, diagnostic)
        }
    };
    for diagnostic in diagnostics {
        let diagnostic = to_codespan(&resolved, files, diagnostic);
        // Every label's range was checked against its file's lines, so this
        // should not fail; if it does, the message and notes still show.
        if emit(&diagnostic).is_err() {
            let mut bare = diagnostic;
            bare.labels.clear();
            emit(&bare).expect("a diagnostic without labels renders");
        }
    }
    if color {
        String::from_utf8(ansi.into_inner()).expect("rendered diagnostics are UTF-8")
    } else {
        plain
    }
}

/// Converts a diagnostic to codespan's, with spans resolved.
fn to_codespan(
    resolved: &ResolvedFiles<'_>,
    files: &dyn Files,
    diagnostic: &Diagnostic,
) -> cs::Diagnostic<FileId> {
    let severity = match diagnostic.severity {
        Severity::Error => cs::Severity::Error,
        Severity::Warning => cs::Severity::Warning,
    };
    let mut out = cs::Diagnostic::new(severity).with_message(diagnostic.message.as_str());
    if let Some(code) = &diagnostic.code {
        out = out.with_code(code.as_str());
    }
    let label = |style, span, message: &str| {
        let location = locate(files, span)?;
        resolved.cache(location.id);
        Some(cs::Label::new(style, location.id, location.bytes).with_message(message))
    };
    let mut labels = Vec::new();
    labels.extend(label(cs::LabelStyle::Primary, diagnostic.span, ""));
    for secondary in &diagnostic.related {
        labels.extend(label(
            cs::LabelStyle::Secondary,
            secondary.span,
            &secondary.message,
        ));
    }
    let notes = diagnostic
        .hints
        .iter()
        .map(|hint| format!("hint: {hint}"))
        .chain(
            diagnostic
                .fixes
                .iter()
                .map(|fix| format!("fix: {}", fix.title)),
        )
        .collect();
    out.with_labels(labels).with_notes(notes)
}

/// The text of a file, cheap to clone.
struct Text(Lines<String>);

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        self.0.text()
    }
}

/// The files of the labels being rendered, for codespan.
struct ResolvedFiles<'a> {
    files: &'a dyn Files,
    cache: std::cell::RefCell<HashMap<FileId, (EcoString, Lines<String>)>>,
}

impl<'a> ResolvedFiles<'a> {
    fn new(files: &'a dyn Files) -> Self {
        Self {
            files,
            cache: Default::default(),
        }
    }

    /// Looks up a file the first time a label points into it.
    fn cache(&self, id: FileId) {
        let mut cache = self.cache.borrow_mut();
        if cache.contains_key(&id) {
            return;
        }
        if let (Some(path), Some(lines)) = (self.files.path(id), self.files.lines(id)) {
            cache.insert(id, (path, lines));
        }
    }

    fn get<T>(
        &self,
        id: FileId,
        f: impl FnOnce(&EcoString, &Lines<String>) -> Option<T>,
    ) -> Result<T, FilesError> {
        let cache = self.cache.borrow();
        let (path, lines) = cache.get(&id).ok_or(FilesError::FileMissing)?;
        f(path, lines).ok_or(FilesError::FileMissing)
    }
}

impl<'a> codespan_reporting::files::Files<'a> for ResolvedFiles<'_> {
    type FileId = FileId;
    type Name = EcoString;
    type Source = Text;

    fn name(&'a self, id: FileId) -> Result<EcoString, FilesError> {
        self.get(id, |path, _| Some(path.clone()))
    }

    fn source(&'a self, id: FileId) -> Result<Text, FilesError> {
        self.get(id, |_, lines| Some(Text(lines.clone())))
    }

    fn line_index(&'a self, id: FileId, byte_index: usize) -> Result<usize, FilesError> {
        self.get(id, |_, lines| lines.byte_to_line(byte_index))
    }

    fn line_range(&'a self, id: FileId, line_index: usize) -> Result<Range<usize>, FilesError> {
        self.get(id, |_, lines| lines.line_to_range(line_index))
    }
}
