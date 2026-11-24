//! Checked values of the manifest and lockfile: names, paths, versions,
//! language tags, dates and hashes.

use std::fmt::{self, Debug, Display, Formatter};

use ecow::{EcoString, eco_format};
pub use folio_component::{ComponentName, Name, QualifiedName, Target};

use super::de::{Invalid, list};

/// The first-party components, available without declaration (SPEC §13).
pub const FIRST_PARTY_COMPONENTS: &[&str] = &["exam", "multicol", "plot", "timer", "inputs"];

/// Parses a name of a variant, numbering group or element: an identifier
/// as in content (`markscheme`, `shared-part`).
pub(super) fn parse_name(text: &str) -> Result<Name, Invalid> {
    Name::new(text).ok_or_else(|| {
        Invalid::new(eco_format!("`{text}` is not a valid name")).with_hint(
            "a name starts with a letter or `_` and continues with letters, digits, `-` or `_`",
        )
    })
}

/// Parses a name that may be qualified with a component namespace:
/// `question` or `exam.question` (SPEC §3.6).
pub(super) fn parse_qualified_name(text: &str) -> Result<QualifiedName, Invalid> {
    QualifiedName::new(text).ok_or_else(|| {
        Invalid::new(eco_format!("`{text}` is not a valid name")).with_hint(
            "write a name such as `question`, or qualify it with its component: `exam.question`",
        )
    })
}

/// Parses the name of a component: an identifier for components installed
/// by version (`circuits`), or a scoped name for packages (`@school/house`).
pub(super) fn parse_component_name(text: &str) -> Result<ComponentName, Invalid> {
    ComponentName::new(text).ok_or_else(|| {
        Invalid::new(eco_format!("`{text}` is not a valid component name"))
            .with_hint("write a name such as `circuits`, or a scoped name such as `@school/house`")
    })
}

/// Whether a component is first-party (SPEC §13).
pub fn is_first_party(name: &ComponentName) -> bool {
    FIRST_PARTY_COMPONENTS.contains(&name.as_str())
}

/// A language edition: a version of the syntax and semantics of the
/// language, pinned by the manifest (SPEC §2.4).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[non_exhaustive]
pub enum Edition {
    /// The 2027 edition, the first.
    E2027,
}

impl Edition {
    /// Every edition this compiler supports, oldest first.
    pub const ALL: &[Self] = &[Self::E2027];

    /// The edition as written in the manifest: `"2027"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::E2027 => "2027",
        }
    }

    pub(super) fn parse(text: &str) -> Result<Self, Invalid> {
        if let Some(edition) = Self::ALL.iter().find(|e| e.as_str() == text) {
            return Ok(*edition);
        }
        let names: Vec<&str> = Self::ALL.iter().map(|e| e.as_str()).collect();
        let newer = text.parse::<u32>().is_ok_and(|year| {
            Self::ALL
                .last()
                .is_some_and(|last| year > last.as_str().parse().unwrap_or(0))
        });
        let invalid = Invalid::new(eco_format!("unknown edition `{text}`")).with_hint(eco_format!(
            "this compiler supports the editions {}",
            list(&names)
        ));
        Err(if newer {
            invalid.with_hint("a newer edition needs a newer compiler")
        } else {
            invalid
        })
    }
}

impl Display for Edition {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A BCP 47 language tag, such as `en`, `pl` or `en-GB` (SPEC §4.5).
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Lang(EcoString);

impl Lang {
    /// English, the default language.
    pub fn english() -> Self {
        Self("en".into())
    }

    /// The tag as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The primary language subtag, in lower case: `en` for `en-GB`.
    pub fn language(&self) -> EcoString {
        let primary = self.0.split('-').next().unwrap_or_default();
        primary.to_ascii_lowercase().into()
    }

    pub(super) fn parse(text: &str) -> Result<Self, Invalid> {
        let mut subtags = text.split('-');
        let primary = subtags.next().unwrap_or_default();
        let valid = (2..=3).contains(&primary.len())
            && primary.bytes().all(|b| b.is_ascii_alphabetic())
            && subtags.all(|s| {
                (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
            });
        if valid {
            Ok(Self(text.into()))
        } else {
            Err(Invalid::new(eco_format!("`{text}` is not a language tag"))
                .with_hint("write a language tag such as `en`, `pl` or `en-GB`"))
        }
    }
}

/// Parses an output target as the manifest writes it: `"pdf"`.
pub(super) fn parse_target(text: &str) -> Result<Target, Invalid> {
    Target::from_name(text).ok_or_else(|| {
        let names: Vec<&str> = Target::ALL.iter().map(|t| t.as_str()).collect();
        Invalid::new(eco_format!("unknown target `{text}`"))
            .with_hint(eco_format!("the targets are {}", list(&names)))
    })
}

/// A calendar date, as TOML writes local dates: `2026-11-24`.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Date {
    /// The year.
    pub year: u16,
    /// The month, from 1.
    pub month: u8,
    /// The day of the month, from 1.
    pub day: u8,
}

impl Debug for Date {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl Display for Date {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A path to a file or directory inside the project, relative to the
/// directory that holds `folio.toml`, with `/` as separator and without `.`
/// or `..` segments: `styles/exam.fsty`.
///
/// File reads are limited to the project (SPEC §4.4), so a project path never
/// leaves it.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ProjectPath(EcoString);

impl ProjectPath {
    /// A project path from text written relative to the project directory,
    /// as `folio.toml` writes paths: `styles/../main.fol` is `main.fol`.
    /// `None` if the text is empty, absolute, contains `\` or a control
    /// character, or leaves the project.
    pub fn new(text: &str) -> Option<Self> {
        Self::parse(text).ok()
    }

    /// Resolves a path written in this file, as an import or include does:
    /// relative to this file's directory, or to the project directory when
    /// it starts with `/`. `None` if the result is not a project path, or
    /// if the path is empty or names a directory.
    pub fn join(&self, path: &str) -> Option<Self> {
        if path.is_empty() || path.ends_with('/') {
            return None;
        }
        if let Some(rooted) = path.strip_prefix('/') {
            return Self::new(rooted);
        }
        match self.0.rsplit_once('/') {
            Some((dir, _)) => Self::new(&format!("{dir}/{path}")),
            None => Self::new(path),
        }
    }

    /// The normalised path.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last segment.
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or_default()
    }

    /// The extension of the last segment, without the dot. A leading dot
    /// (`.fol`) does not start an extension.
    pub fn extension(&self) -> Option<&str> {
        match self.file_name().rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => Some(ext),
            _ => None,
        }
    }

    /// The last segment without its extension: `main` for `src/main.fol`.
    pub fn stem(&self) -> &str {
        let name = self.file_name();
        match name.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => stem,
            _ => name,
        }
    }

    pub(super) fn parse(text: &str) -> Result<Self, Invalid> {
        let segments = segments(text, "path")?;
        let mut out: Vec<&str> = Vec::new();
        for segment in segments {
            if segment == ".." {
                if out.pop().is_none() {
                    return Err(Invalid::new(eco_format!("`{text}` leaves the project"))
                        .with_hint("Folio reads files only inside the project directory"));
                }
            } else {
                out.push(segment);
            }
        }
        if out.is_empty() {
            return Err(Invalid::new(eco_format!(
                "`{text}` names the project directory itself"
            )));
        }
        Ok(Self(out.join("/").into()))
    }

    /// A project path to a file with the given extension.
    pub(super) fn parse_file(text: &str, ext: &str, what: &str) -> Result<Self, Invalid> {
        let path = Self::parse(text)?;
        if path.extension() != Some(ext) {
            return Err(Invalid::new(eco_format!("`{text}` is not {what}"))
                .with_hint(eco_format!("{what} has the extension `.{ext}`")));
        }
        Ok(path)
    }
}

/// A relative path that may leave the project, such as the path of a local
/// package component (`../house-style`). It is relative to the directory
/// that holds `folio.toml`, with `/` as separator and without `.` segments;
/// `..` segments stay at its start.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RelativePath(EcoString);

impl RelativePath {
    /// The normalised path.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(super) fn parse(text: &str) -> Result<Self, Invalid> {
        let mut out: Vec<&str> = Vec::new();
        for segment in segments(text, "path")? {
            if segment == ".." && out.last().is_some_and(|last| *last != "..") {
                out.pop();
            } else {
                out.push(segment);
            }
        }
        if out.is_empty() {
            return Err(Invalid::new(eco_format!(
                "`{text}` names the project directory itself"
            )));
        }
        Ok(Self(out.join("/").into()))
    }
}

/// A glob pattern over project paths, such as `questions/**` (SPEC §5.5).
///
/// It is checked like a [`ProjectPath`]; the wildcards `*`, `**`, `?` and
/// `[...]` are interpreted where the pattern is matched.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct PathPattern(EcoString);

impl PathPattern {
    /// The normalised pattern.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(super) fn parse(text: &str) -> Result<Self, Invalid> {
        let segments = segments(text, "pattern")?;
        if segments.contains(&"..") {
            return Err(Invalid::new(eco_format!("`{text}` leaves the project"))
                .with_hint("patterns match files inside the project directory"));
        }
        if segments.is_empty() {
            return Err(Invalid::new(eco_format!("`{text}` matches no files"))
                .with_hint("write a pattern such as `questions/**`"));
        }
        Ok(Self(segments.join("/").into()))
    }
}

/// Split a relative path into its segments, without `.` and empty segments.
fn segments<'t>(text: &'t str, what: &str) -> Result<Vec<&'t str>, Invalid> {
    if text.is_empty() {
        return Err(Invalid::new(eco_format!("the {what} is empty")));
    }
    if text.contains('\\') {
        let fixed = text.replace('\\', "/");
        return Err(Invalid::new(eco_format!("`{text}` contains `\\`"))
            .with_hint(eco_format!("separate directories with `/`: `{fixed}`")));
    }
    let bytes = text.as_bytes();
    let drive = bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes.get(2).is_none_or(|b| *b == b'/');
    if text.starts_with('/') || drive {
        return Err(Invalid::new(eco_format!("`{text}` is an absolute {what}")).with_hint(
            "write it relative to the directory that holds `folio.toml`, so the project can move",
        ));
    }
    if text.chars().any(char::is_control) {
        return Err(Invalid::new(eco_format!(
            "the {what} contains a control character"
        )));
    }
    Ok(text
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect())
}

/// A SHA-256 digest, written `sha256:` and 64 lower-case hex digits.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Sha256(pub [u8; 32]);

impl Sha256 {
    pub(super) fn parse(text: &str) -> Result<Self, Invalid> {
        let invalid = || {
            Invalid::new(eco_format!("`{text}` is not a hash"))
                .with_hint("write `sha256:` followed by 64 lower-case hex digits")
        };
        let hex = text.strip_prefix("sha256:").ok_or_else(invalid)?;
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(invalid());
        }
        let mut out = [0; 32];
        for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
            let digit = |b: u8| {
                if b.is_ascii_digit() {
                    b - b'0'
                } else {
                    b - b'a' + 10
                }
            };
            *byte = digit(pair[0]) << 4 | digit(pair[1]);
        }
        Ok(Self(out))
    }
}

impl Display for Sha256 {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl Debug for Sha256 {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

macro_rules! string_traits {
    ($($ty:ty),*) => {$(
        impl Display for $ty {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Debug for $ty {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                Debug::fmt(self.0.as_str(), f)
            }
        }
    )*};
}

string_traits!(Lang, ProjectPath, RelativePath, PathPattern);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_paths() {
        let ok = |s| ProjectPath::parse(s).unwrap().as_str().to_owned();
        assert_eq!(ok("main.fol"), "main.fol");
        assert_eq!(ok("./styles//exam.fsty"), "styles/exam.fsty");
        assert_eq!(ok("styles/../main.fol"), "main.fol");
        for bad in [
            "",
            "/etc/x",
            "C:/x",
            "..",
            "../x.fol",
            "a/../..",
            "styles\\exam.fsty",
            ".",
        ] {
            assert!(ProjectPath::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn extensions_and_stems() {
        let p = ProjectPath::parse("src/main.fol").unwrap();
        assert_eq!((p.extension(), p.stem()), (Some("fol"), "main"));
        let p = ProjectPath::parse("Makefile").unwrap();
        assert_eq!((p.extension(), p.stem()), (None, "Makefile"));
        let p = ProjectPath::parse(".fol").unwrap();
        assert_eq!((p.extension(), p.stem()), (None, ".fol"));
    }

    #[test]
    fn joined_paths() {
        let file = ProjectPath::new("questions/q1.fol").unwrap();
        let join = |p| file.join(p).map(|p| p.as_str().to_owned());
        assert_eq!(join("q2.fol").as_deref(), Some("questions/q2.fol"));
        assert_eq!(join("../data/a.csv").as_deref(), Some("data/a.csv"));
        assert_eq!(
            join("/styles/exam.fsty").as_deref(),
            Some("styles/exam.fsty")
        );
        assert_eq!(join("../../outside.fol"), None);
        assert_eq!(join("/"), None);
        assert_eq!(join(""), None);
        assert_eq!(join("sub/"), None);
        let top = ProjectPath::new("main.fol").unwrap();
        assert_eq!(top.join("./a.fol").unwrap().as_str(), "a.fol");
        assert_eq!(ProjectPath::new("a\\b"), None);
    }

    #[test]
    fn relative_paths() {
        let ok = |s| RelativePath::parse(s).unwrap().as_str().to_owned();
        assert_eq!(ok("../house-style"), "../house-style");
        assert_eq!(ok("../../a/./b/../c"), "../../a/c");
        assert_eq!(ok("a/../../b"), "../b");
        assert!(RelativePath::parse("a/..").is_err());
        assert!(RelativePath::parse("/abs").is_err());
    }

    #[test]
    fn names() {
        // folio-component checks names with its own copy of the lexer's
        // identifier rule; the two must agree.
        for text in [
            "a",
            "_",
            "_a-1",
            "a-",
            "zadanie-\u{17c}",
            "\u{17c}",
            "x\u{b7}y",
            "\u{b7}x",
            "1a",
            "-a",
            "a b",
            "a.b",
            "a/b",
            "\u{301}a",
            "a\u{301}",
            "\u{1f600}",
            "",
        ] {
            assert_eq!(
                folio_component::is_ident(text),
                folio_syntax::is_ident(text),
                "{text:?}"
            );
        }
        assert!(parse_name("markscheme").is_ok());
        assert!(parse_name("part-b_2").is_ok());
        assert!(parse_name("2nd").is_err());
        assert!(parse_name("a.b").is_err());
        let q = parse_qualified_name("exam.question").unwrap();
        assert_eq!(q.to_string(), "exam.question");
        assert!(parse_qualified_name("a.b.c").is_err());
        assert!(parse_component_name("@school/house").is_ok());
        assert!(parse_component_name("circuits").is_ok());
        assert!(parse_component_name("@school").is_err());
        assert!(parse_component_name("school/house").is_err());
    }

    #[test]
    fn langs() {
        for ok in ["en", "pl", "en-GB", "zh-Hant-TW", "fil"] {
            assert!(Lang::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["", "e", "english", "en_GB", "en-", "en-toolongsubtag"] {
            assert!(Lang::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Lang::parse("EN-gb").unwrap().language(), "en");
    }

    #[test]
    fn hashes() {
        let text = format!("sha256:{}", "0123456789abcdef".repeat(4));
        let hash = Sha256::parse(&text).unwrap();
        assert_eq!(hash.0[0], 0x01);
        assert_eq!(hash.0[31], 0xef);
        assert_eq!(hash.to_string(), text);
        assert!(Sha256::parse(&text.to_uppercase()).is_err());
        assert!(Sha256::parse("sha256:abc").is_err());
        assert!(Sha256::parse("md5:00").is_err());
    }

    #[test]
    fn editions() {
        assert_eq!(Edition::parse("2027").unwrap(), Edition::E2027);
        let newer = Edition::parse("2030").unwrap_err();
        assert!(newer.hints.iter().any(|h| h.contains("newer compiler")));
        assert!(Edition::parse("2027.1").is_err());
    }
}
