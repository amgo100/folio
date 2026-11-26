//! Converting between the compiler's terms and the protocol's: positions
//! in the encoding the client chose, file URIs and paths, and diagnostics.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use folio::diag::{self, Files, Location};
use folio::{Diagnostic, Severity};
use lsp_types as lsp;

/// How the columns of positions are counted: the client and server agree
/// on it when they connect. UTF-16 code units unless the client offers
/// another.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Encoding {
    /// Bytes of UTF-8.
    Utf8,
    /// Code units of UTF-16, the protocol's default.
    Utf16,
    /// Unicode scalar values.
    Utf32,
}

impl Encoding {
    /// The encoding to use given those the client offers, in its order of
    /// preference: the first this server knows, or UTF-16.
    pub(crate) fn choose(offered: Option<&[lsp::PositionEncodingKind]>) -> Self {
        offered
            .unwrap_or_default()
            .iter()
            .find_map(|kind| match kind.as_str() {
                "utf-8" => Some(Self::Utf8),
                "utf-16" => Some(Self::Utf16),
                "utf-32" => Some(Self::Utf32),
                _ => None,
            })
            .unwrap_or(Self::Utf16)
    }

    /// The protocol's name of the encoding.
    pub(crate) fn kind(self) -> lsp::PositionEncodingKind {
        match self {
            Self::Utf8 => lsp::PositionEncodingKind::UTF8,
            Self::Utf16 => lsp::PositionEncodingKind::UTF16,
            Self::Utf32 => lsp::PositionEncodingKind::UTF32,
        }
    }

    /// How many units a character takes.
    fn width(self, c: char) -> usize {
        match self {
            Self::Utf8 => c.len_utf8(),
            Self::Utf16 => c.len_utf16(),
            Self::Utf32 => 1,
        }
    }
}

/// The lines of a text as the protocol counts them: ended by `\n`, `\r\n`
/// or `\r`.
pub(crate) struct LineIndex<'a> {
    text: &'a str,
    /// The byte offset where each line starts.
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let bytes = text.as_bytes();
        let mut starts = vec![0];
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => starts.push(i + 1),
                b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                    starts.push(i + 2);
                    i += 1;
                }
                b'\r' => starts.push(i + 1),
                _ => {}
            }
            i += 1;
        }
        Self { text, starts }
    }

    /// The position of a byte offset. An offset past the end is the end; one
    /// inside a character is that character's start.
    pub(crate) fn position(&self, byte: usize, encoding: Encoding) -> lsp::Position {
        let mut byte = byte.min(self.text.len());
        while !self.text.is_char_boundary(byte) {
            byte -= 1;
        }
        let line = self.starts.partition_point(|&start| start <= byte) - 1;
        let start = self.starts[line];
        // Inside a line break is the end of the line before it.
        let rest = &self.text[start..];
        let byte = byte.min(start + rest.find(['\r', '\n']).unwrap_or(rest.len()));
        let column = self.text[start..byte]
            .chars()
            .map(|c| encoding.width(c))
            .sum::<usize>();
        lsp::Position::new(saturate(line), saturate(column))
    }

    /// The byte offset of a position. A line past the last is the end of
    /// the text; a column past the end of its line is the line's end, as the
    /// protocol asks; a column inside a character is that character's
    /// start.
    pub(crate) fn offset(&self, position: lsp::Position, encoding: Encoding) -> usize {
        let Some(&start) = self.starts.get(position.line as usize) else {
            return self.text.len();
        };
        let rest = &self.text[start..];
        let end = start + rest.find(['\r', '\n']).unwrap_or(rest.len());
        let mut units = 0;
        for (i, c) in self.text[start..end].char_indices() {
            if units >= position.character as usize {
                return start + i;
            }
            units += encoding.width(c);
            if units > position.character as usize {
                return start + i;
            }
        }
        end
    }

    /// The range of a byte range.
    pub(crate) fn range(&self, bytes: std::ops::Range<usize>, encoding: Encoding) -> lsp::Range {
        lsp::Range::new(
            self.position(bytes.start, encoding),
            self.position(bytes.end, encoding),
        )
    }
}

fn saturate(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The `file:` URI of an absolute path.
pub(crate) fn path_to_uri(path: &Path) -> Option<lsp::Uri> {
    let text = path.to_str()?;
    let mut uri = String::from("file://");
    if cfg!(windows) {
        uri.push('/');
    }
    for &byte in text.as_bytes() {
        match byte {
            b'\\' if cfg!(windows) => uri.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(char::from(byte));
            }
            b':' if cfg!(windows) => uri.push(':'),
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    lsp::Uri::from_str(&uri).ok()
}

/// The path of a `file:` URI on this machine, or `None` for another scheme
/// or a host other than this one.
pub(crate) fn uri_to_path(uri: &lsp::Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let scheme = text.get(..5)?;
    if !scheme.eq_ignore_ascii_case("file:") {
        return None;
    }
    let rest = text[5..].strip_prefix("//")?;
    let (authority, path) = rest.split_at(rest.find('/')?);
    if !(authority.is_empty() || authority.eq_ignore_ascii_case("localhost")) {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let decoded = percent_decode(path)?;
    if cfg!(windows) {
        // `/C:/dir` is `C:/dir`.
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            return Some(PathBuf::from(&decoded[1..]));
        }
    }
    Some(PathBuf::from(decoded))
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Where the diagnostics of one file go: the URI to publish them under.
pub(crate) struct Uris<'a> {
    /// The project directory.
    pub root: &'a Path,
    /// The URIs the client opened files under, by project path; the
    /// diagnostics of an open file go under the URI the client knows it by.
    pub open: &'a HashMap<String, lsp::Uri>,
}

impl Uris<'_> {
    pub(crate) fn get(&self, path: &str) -> Option<lsp::Uri> {
        match self.open.get(path) {
            Some(uri) => Some(uri.clone()),
            None => path_to_uri(&self.root.join(path)),
        }
    }
}

/// The diagnostics to publish, by the project path of the file they are
/// in. A diagnostic that points nowhere, such as an internal error, is
/// shown at the start of `folio.toml`. A diagnostic reported alike by
/// several builds is shown once.
pub(crate) fn diagnostics(
    files: &dyn Files,
    diagnostics: &[Diagnostic],
    uris: &Uris<'_>,
    encoding: Encoding,
) -> BTreeMap<String, Vec<lsp::Diagnostic>> {
    let mut out: BTreeMap<String, Vec<lsp::Diagnostic>> = BTreeMap::new();
    let mut seen = HashSet::new();
    let resolved: Vec<_> = diagnostics
        .iter()
        .filter(|d| seen.insert(*d))
        .map(|d| (d, diag::resolve(files, d)))
        .collect();
    // Each file's text, and then its lines, once.
    let mut texts: HashMap<folio::FileId, String> = HashMap::new();
    for (_, r) in &resolved {
        for location in r
            .location
            .iter()
            .chain(r.related.iter().map(|r| &r.location))
        {
            if !texts.contains_key(&location.id)
                && let Some(lines) = files.lines(location.id)
            {
                texts.insert(location.id, lines.text().to_owned());
            }
        }
    }
    let indexes: HashMap<folio::FileId, LineIndex<'_>> = texts
        .iter()
        .map(|(id, text)| (*id, LineIndex::new(text)))
        .collect();
    let range = |location: &Location| match indexes.get(&location.id) {
        Some(index) => index.range(location.bytes.clone(), encoding),
        None => lsp::Range::default(),
    };
    for (diagnostic, resolved) in resolved {
        let (path, range_) = match &resolved.location {
            Some(location) => (location.path.to_string(), range(location)),
            None => (folio::loader::MANIFEST.to_owned(), lsp::Range::default()),
        };
        let mut message = resolved.message.to_string();
        for hint in &resolved.hints {
            message.push_str("\nhint: ");
            message.push_str(hint);
        }
        let related: Vec<lsp::DiagnosticRelatedInformation> = resolved
            .related
            .iter()
            .filter_map(|related| {
                Some(lsp::DiagnosticRelatedInformation {
                    location: lsp::Location::new(
                        uris.get(&related.location.path)?,
                        range(&related.location),
                    ),
                    message: related.message.to_string(),
                })
            })
            .collect();
        out.entry(path).or_default().push(lsp::Diagnostic {
            range: range_,
            severity: Some(match diagnostic.severity {
                Severity::Error => lsp::DiagnosticSeverity::ERROR,
                Severity::Warning => lsp::DiagnosticSeverity::WARNING,
            }),
            code: resolved
                .code
                .as_ref()
                .map(|code| lsp::NumberOrString::String(code.to_string())),
            source: Some("folio".into()),
            message,
            related_information: (!related.is_empty()).then_some(related),
            ..lsp::Diagnostic::default()
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_in_every_encoding() {
        let text = "a\u{e9}\u{1F600}b\r\nx\ry\n";
        let index = LineIndex::new(text);
        for (encoding, b) in [
            (Encoding::Utf8, 8),
            (Encoding::Utf16, 5),
            (Encoding::Utf32, 4),
        ] {
            assert_eq!(index.position(8, encoding), lsp::Position::new(0, b));
            assert_eq!(index.offset(lsp::Position::new(0, b), encoding), 8);
            // Past the end of the line is its end, before `\r\n`.
            assert_eq!(index.offset(lsp::Position::new(0, 99), encoding), 8);
        }
        // Between `\r` and `\n`: the end of the line.
        assert_eq!(index.position(9, Encoding::Utf16), lsp::Position::new(0, 5));
        assert_eq!(
            index.position(10, Encoding::Utf16),
            lsp::Position::new(1, 0)
        );
        assert_eq!(
            index.position(12, Encoding::Utf16),
            lsp::Position::new(2, 0)
        );
        assert_eq!(index.offset(lsp::Position::new(2, 1), Encoding::Utf16), 13);
        assert_eq!(
            index.position(14, Encoding::Utf16),
            lsp::Position::new(3, 0)
        );
        assert_eq!(index.offset(lsp::Position::new(9, 0), Encoding::Utf16), 14);
        // Inside the emoji's surrogate pair: its start.
        assert_eq!(index.offset(lsp::Position::new(0, 3), Encoding::Utf16), 3);
    }

    #[test]
    fn encoding_follows_the_client() {
        let kinds = [
            lsp::PositionEncodingKind::new("utf-7"),
            lsp::PositionEncodingKind::UTF8,
        ];
        assert_eq!(Encoding::choose(Some(&kinds)), Encoding::Utf8);
        assert_eq!(Encoding::choose(None), Encoding::Utf16);
    }

    #[test]
    fn file_uris() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\exams\Fizyka 1\main.fol")
        } else {
            PathBuf::from("/exams/Fizyka 1/ząb.fol")
        };
        let uri = path_to_uri(&path).unwrap();
        assert!(uri.as_str().starts_with("file:///"));
        assert!(!uri.as_str().contains(' '));
        assert_eq!(uri_to_path(&uri).unwrap(), path);
        let other = lsp::Uri::from_str("untitled:Untitled-1").unwrap();
        assert_eq!(uri_to_path(&other), None);
        assert_eq!(percent_decode("/a%2B5").as_deref(), Some("/a+5"));
        assert_eq!(percent_decode("/a%+5"), None);
        let remote = lsp::Uri::from_str("file://server/share/a.fol").unwrap();
        assert_eq!(uri_to_path(&remote), None);
    }
}
