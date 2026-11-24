//! Names shared by the core and components: identifiers, qualified names,
//! component names and output targets (SPEC §1.5, §3.6).
//!
//! The project manifest and lockfile use the same types, so a name means the
//! same thing in `folio.toml`, in `folio.lock` and in a component manifest.

use std::fmt::{self, Debug, Display, Formatter};

use ecow::EcoString;

/// An identifier as in content: the name of an element, field, slot,
/// property, template, variant or numbering group (`question`,
/// `markscheme-box`, `shared-part`).
///
/// It starts with a letter or `_` and continues with letters, digits, `-`
/// or `_`, with "letter" and "digit" as Unicode's `XID_Start` and
/// `XID_Continue` define them.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Name(EcoString);

impl Name {
    /// The name, if `text` is a valid identifier.
    pub fn new(text: &str) -> Option<Self> {
        is_ident(text).then(|| Self(text.into()))
    }

    /// The name as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether `text` is an identifier (see [`Name`]).
pub fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| unicode_ident::is_xid_start(c) || c == '_')
        && chars.all(|c| unicode_ident::is_xid_continue(c) || c == '_' || c == '-')
}

/// A name that may be qualified with a component namespace: `question` or
/// `exam.question` (SPEC §3.6).
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct QualifiedName {
    /// The component namespace, if the name is qualified.
    pub namespace: Option<Name>,
    /// The name within the namespace.
    pub name: Name,
}

impl QualifiedName {
    /// Parses `question` or `exam.question`.
    pub fn new(text: &str) -> Option<Self> {
        let (namespace, name) = match text.split_once('.') {
            Some((namespace, name)) => (Some(Name::new(namespace)?), name),
            None => (None, text),
        };
        Some(Self {
            namespace,
            name: Name::new(name)?,
        })
    }

    /// A qualified name: `namespace.name`.
    pub fn qualified(namespace: Name, name: Name) -> Self {
        Self {
            namespace: Some(namespace),
            name,
        }
    }

    /// An unqualified name.
    pub fn unqualified(name: Name) -> Self {
        Self {
            namespace: None,
            name,
        }
    }
}

/// The name of a component: an identifier for components installed by
/// version or compiled in (`exam`, `circuits`), or a scoped name for
/// packages (`@school/house`).
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ComponentName(EcoString);

impl ComponentName {
    /// The component name, if `text` is an identifier or a scoped name
    /// `@scope/name` made of two identifiers.
    pub fn new(text: &str) -> Option<Self> {
        let valid = match text.strip_prefix('@') {
            Some(rest) => rest
                .split_once('/')
                .is_some_and(|(scope, name)| is_ident(scope) && is_ident(name)),
            None => is_ident(text),
        };
        valid.then(|| Self(text.into()))
    }

    /// The name as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The namespace that qualifies the component's elements and properties
    /// (SPEC §3.6): the name itself (`exam`), or the part after the scope for
    /// a scoped name (`house` for `@school/house`).
    pub fn namespace(&self) -> Name {
        let name = match self.0.strip_prefix('@') {
            Some(rest) => rest.split_once('/').map_or(rest, |(_, name)| name),
            None => &self.0,
        };
        Name(name.into())
    }
}

/// An output target (SPEC §1.5, §11).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Target {
    /// PDF (§11.1).
    Pdf,
    /// Word (§11.2).
    Docx,
    /// PowerPoint (§11.3).
    Pptx,
    /// The live presentation (§11.4), which writes no file.
    Live,
}

impl Target {
    /// Every target.
    pub const ALL: &[Self] = &[Self::Pdf, Self::Docx, Self::Pptx, Self::Live];

    /// The target as written in the manifest: `"pdf"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Pptx => "pptx",
            Self::Live => "live",
        }
    }

    /// The target written as `text`, if there is one.
    pub fn from_name(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.as_str() == text)
    }

    /// The extension of the file the target writes, if it writes one.
    pub fn extension(self) -> Option<&'static str> {
        match self {
            Self::Pdf => Some("pdf"),
            Self::Docx => Some("docx"),
            Self::Pptx => Some("pptx"),
            Self::Live => None,
        }
    }
}

impl Display for Target {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

string_traits!(Name, ComponentName);

impl Display for QualifiedName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if let Some(namespace) = &self.namespace {
            write!(f, "{namespace}.")?;
        }
        write!(f, "{}", self.name)
    }
}

impl Debug for QualifiedName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(self.to_string().as_str(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(Name::new("markscheme").is_some());
        assert!(Name::new("part-b_2").is_some());
        assert!(Name::new("_x").is_some());
        assert!(Name::new("zadanie-ż").is_some());
        for bad in ["", "2nd", "-a", "a.b", "a b", "@a"] {
            assert!(Name::new(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn qualified_names() {
        let q = QualifiedName::new("exam.question").unwrap();
        assert_eq!(q.namespace.unwrap().as_str(), "exam");
        assert_eq!(q.name.as_str(), "question");
        assert_eq!(QualifiedName::new("question").unwrap().namespace, None);
        for bad in ["a.b.c", ".a", "a.", ""] {
            assert!(QualifiedName::new(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn component_names() {
        assert_eq!(
            ComponentName::new("exam").unwrap().namespace().as_str(),
            "exam"
        );
        let house = ComponentName::new("@school/house").unwrap();
        assert_eq!(house.namespace().as_str(), "house");
        for bad in ["@school", "school/house", "@/x", "@a/b/c", ""] {
            assert!(ComponentName::new(bad).is_none(), "{bad}");
        }
    }
}
