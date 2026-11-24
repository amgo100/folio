//! Reading spanned TOML into typed values, with errors on keys and values.

use std::ops::Range;

use ecow::{EcoString, EcoVec, eco_format};
use folio_syntax::{DiagSpan, DiagSpanKind, FileId};
use toml::de::{DeString, DeTable, DeValue};

use super::{ProjectError, Spanned};

/// Collects the errors of one file while it is read.
pub(super) struct Cx {
    file: FileId,
    errors: Vec<ProjectError>,
    /// The ranges of TOML syntax errors. The parser recovers with a
    /// placeholder value, and errors about that value would only repeat them.
    syntax: Vec<Range<usize>>,
    /// How many errors at the start of `errors` are syntax errors.
    syntax_errors: usize,
}

impl Cx {
    pub(super) fn new(file: FileId) -> Self {
        Self {
            file,
            errors: Vec::new(),
            syntax: Vec::new(),
            syntax_errors: 0,
        }
    }

    /// Parse the TOML syntax of a document, recording syntax errors. Read
    /// the result with [`Table::root`]. Called first, so syntax errors are
    /// the first errors recorded.
    pub(super) fn parse<'i>(&mut self, text: &'i str) -> Document<'i> {
        let (root, errors) = DeTable::parse_recoverable(text);
        for error in errors {
            let range = error.span().unwrap_or(0..0);
            self.error(range.clone(), error.message().trim_end().to_owned());
            if error.span().is_some() {
                self.syntax.push(range);
            }
        }
        self.syntax_errors = self.errors.len();
        Document(root.into_inner())
    }

    pub(super) fn span(&self, range: Range<usize>) -> DiagSpan {
        DiagSpan::from_range(self.file, range)
    }

    pub(super) fn spanned<T>(&self, v: T, range: Range<usize>) -> Spanned<T> {
        Spanned::new(v, self.span(range))
    }

    pub(super) fn error(
        &mut self,
        range: Range<usize>,
        message: impl Into<EcoString>,
    ) -> &mut ProjectError {
        let span = self.span(range);
        self.errors.push(ProjectError {
            span,
            message: message.into(),
            hints: EcoVec::new(),
        });
        self.errors.last_mut().unwrap()
    }

    /// Add a hint to the last error.
    pub(super) fn hint(&mut self, hint: impl Into<EcoString>) {
        if let Some(error) = self.errors.last_mut() {
            error.hint(hint);
        }
    }

    /// Record an error from a value check.
    pub(super) fn invalid(&mut self, range: Range<usize>, invalid: Invalid) {
        let error = self.error(range, invalid.message);
        for hint in invalid.hints {
            error.hint(hint);
        }
    }

    /// The model, or every error in source order. `value` is `None` only
    /// when an error was recorded.
    pub(super) fn finish<T>(mut self, value: Option<T>) -> Result<T, Vec<ProjectError>> {
        match value {
            Some(value) if self.errors.is_empty() => return Ok(value),
            None if self.errors.is_empty() => {
                self.error(0..0, "the file could not be read");
            }
            _ => {}
        }
        let range = |error: &ProjectError| match error.span.get() {
            DiagSpanKind::Range { range, .. } => range,
            _ => 0..0,
        };
        let syntax = self.syntax;
        let syntax_errors = self.syntax_errors;
        let mut errors: Vec<ProjectError> = self
            .errors
            .into_iter()
            .enumerate()
            .filter(|(i, error)| {
                let r = range(error);
                *i < syntax_errors || !syntax.iter().any(|s| s.start <= r.start && r.end <= s.end)
            })
            .map(|(_, error)| error)
            .collect();
        errors.sort_by_key(|error| {
            let r = range(error);
            (r.start, r.end)
        });
        Err(errors)
    }
}

/// A value that failed a check, before its span is known.
#[derive(Debug)]
pub(super) struct Invalid {
    pub(super) message: EcoString,
    pub(super) hints: Vec<EcoString>,
}

impl Invalid {
    pub(super) fn new(message: impl Into<EcoString>) -> Self {
        Self {
            message: message.into(),
            hints: Vec::new(),
        }
    }

    pub(super) fn with_hint(mut self, hint: impl Into<EcoString>) -> Self {
        self.hints.push(hint.into());
        self
    }
}

/// A parsed TOML document.
pub(super) struct Document<'i>(DeTable<'i>);

/// A TOML table being read.
pub(super) struct Table<'a> {
    /// How messages name the table: "`[project]`", "component `circuits`".
    name: EcoString,
    /// The table's header, or its key for implicit and inline tables.
    span: Range<usize>,
    /// The entries in source order.
    entries: Vec<(Key<'a>, Value<'a>)>,
}

/// A key in a table.
#[derive(Clone)]
pub(super) struct Key<'a> {
    pub(super) name: &'a str,
    pub(super) span: Range<usize>,
}

impl<'a> Table<'a> {
    /// The top-level table of a document.
    pub(super) fn root(doc: &'a Document<'_>) -> Self {
        Self::new(&doc.0, "the top level".into(), 0..0)
    }

    fn new(table: &'a DeTable<'_>, name: EcoString, span: Range<usize>) -> Self {
        let mut entries: Vec<_> = table
            .iter()
            .map(|(key, value)| {
                let key = Key {
                    name: as_str(key.get_ref()),
                    span: key.span(),
                };
                let value = Value {
                    value: value.get_ref(),
                    span: value.span(),
                    name: eco_format!("`{}`", key.name),
                };
                (key, value)
            })
            .collect();
        entries.sort_by_key(|(key, _)| key.span.start);
        Self {
            name,
            span,
            entries,
        }
    }

    /// The span that errors about the whole table point at.
    pub(super) fn span(&self) -> Range<usize> {
        self.span.clone()
    }

    /// The entries in source order.
    pub(super) fn entries(&self) -> &[(Key<'a>, Value<'a>)] {
        &self.entries
    }

    /// The value of a key, if present.
    pub(super) fn get(&self, key: &str) -> Option<&Value<'a>> {
        self.entries
            .iter()
            .find(|(k, _)| k.name == key)
            .map(|(_, v)| v)
    }

    /// The value of a key that must be present.
    pub(super) fn require(&self, cx: &mut Cx, key: &str) -> Option<&Value<'a>> {
        let value = self.get(key);
        if value.is_none() {
            cx.error(
                self.span(),
                eco_format!("missing key `{key}` in {}", self.name),
            );
        }
        value
    }

    /// Report every key that is not in `allowed`.
    pub(super) fn check_keys(&self, cx: &mut Cx, allowed: &[&str]) {
        for (key, _) in &self.entries {
            if !allowed.contains(&key.name) {
                let error = cx.error(
                    key.span.clone(),
                    eco_format!("unknown key `{}` in {}", key.name, self.name),
                );
                match suggest(key.name, allowed) {
                    Some(close) => error.hint(eco_format!("did you mean `{close}`?")),
                    None if allowed.is_empty() => error,
                    None => error.hint(eco_format!("expected {}", list(allowed))),
                };
            }
        }
    }
}

/// A TOML value being read.
pub(super) struct Value<'a> {
    value: &'a DeValue<'a>,
    pub(super) span: Range<usize>,
    /// How messages name the value: "`entry`", "each item of `styles`".
    name: EcoString,
}

impl<'a> Value<'a> {
    fn mismatch(&self, cx: &mut Cx, expected: &str) {
        let found = match self.value {
            DeValue::String(_) => "a string",
            DeValue::Integer(_) => "an integer",
            DeValue::Float(_) => "a float",
            DeValue::Boolean(_) => "a boolean",
            DeValue::Datetime(_) => "a date or time",
            DeValue::Array(_) => "an array",
            DeValue::Table(_) => "a table",
        };
        cx.error(
            self.span.clone(),
            eco_format!("{} must be {expected}, not {found}", self.name),
        );
    }

    pub(super) fn str(&self, cx: &mut Cx) -> Option<&'a str> {
        match self.value {
            DeValue::String(s) => Some(as_str(s)),
            DeValue::Integer(i) => {
                self.mismatch(cx, "a string");
                cx.hint(eco_format!("write it in quotes: `\"{}\"`", i.as_str()));
                None
            }
            DeValue::Float(f) => {
                self.mismatch(cx, "a string");
                cx.hint(eco_format!("write it in quotes: `\"{}\"`", f.as_str()));
                None
            }
            _ => {
                self.mismatch(cx, "a string");
                None
            }
        }
    }

    pub(super) fn bool(&self, cx: &mut Cx) -> Option<bool> {
        match self.value {
            DeValue::Boolean(b) => Some(*b),
            _ => {
                self.mismatch(cx, "`true` or `false`");
                None
            }
        }
    }

    pub(super) fn integer(&self, cx: &mut Cx) -> Option<i64> {
        match self.value {
            DeValue::Integer(i) => {
                let value = i64::from_str_radix(i.as_str(), i.radix()).ok();
                if value.is_none() {
                    cx.error(self.span.clone(), eco_format!("{} is too large", self.name));
                }
                value
            }
            _ => {
                self.mismatch(cx, "an integer");
                None
            }
        }
    }

    /// A local date such as `2026-11-24`, without time or offset.
    pub(super) fn date(&self, cx: &mut Cx) -> Option<(u16, u8, u8)> {
        match self.value {
            DeValue::Datetime(dt) if dt.time.is_none() && dt.offset.is_none() => {
                dt.date.map(|d| (d.year, d.month, d.day))
            }
            DeValue::String(_) => {
                self.mismatch(cx, "a date such as `2026-11-24`");
                cx.hint("write the date without quotes");
                None
            }
            _ => {
                self.mismatch(cx, "a date such as `2026-11-24`, without a time");
                None
            }
        }
    }

    /// Whether the value is an empty array.
    pub(super) fn is_empty_array(&self) -> bool {
        matches!(self.value, DeValue::Array(items) if items.is_empty())
    }

    /// The items of an array, each named "each item of `key`".
    pub(super) fn array(&self, cx: &mut Cx) -> Option<Vec<Value<'a>>> {
        match self.value {
            DeValue::Array(items) => Some(
                items
                    .iter()
                    .map(|item| Value {
                        value: item.get_ref(),
                        span: item.span(),
                        name: eco_format!("each item of {}", self.name),
                    })
                    .collect(),
            ),
            _ => {
                self.mismatch(cx, "an array");
                None
            }
        }
    }

    /// The value as a table, which messages call `name`.
    pub(super) fn table(&self, cx: &mut Cx, name: EcoString) -> Option<Table<'a>> {
        match self.value {
            DeValue::Table(table) => Some(Table::new(table, name, self.span.clone())),
            _ => {
                self.mismatch(cx, "a table");
                None
            }
        }
    }

    /// The items of an array of tables, such as `[[font]]`, each called `name`.
    pub(super) fn tables(&self, cx: &mut Cx, name: &str) -> Vec<Table<'a>> {
        let Some(items) = self.array(cx) else {
            return Vec::new();
        };
        items
            .into_iter()
            .filter_map(|item| item.table(cx, eco_format!("`[[{name}]]`")))
            .collect()
    }

    /// A string checked and converted by `check`.
    pub(super) fn parse<T>(
        &self,
        cx: &mut Cx,
        check: impl FnOnce(&str) -> Result<T, Invalid>,
    ) -> Option<Spanned<T>> {
        let text = self.str(cx)?;
        match check(text) {
            Ok(v) => Some(cx.spanned(v, self.span.clone())),
            Err(invalid) => {
                cx.invalid(self.span.clone(), invalid);
                None
            }
        }
    }

    /// An array of strings, each checked by `check`; duplicates are errors.
    pub(super) fn parse_list<T: PartialEq>(
        &self,
        cx: &mut Cx,
        mut check: impl FnMut(&str) -> Result<T, Invalid>,
    ) -> Vec<Spanned<T>> {
        let mut out: Vec<Spanned<T>> = Vec::new();
        for item in self.array(cx).unwrap_or_default() {
            let Some(v) = item.parse(cx, &mut check) else {
                continue;
            };
            if out.iter().any(|other| other.v == v.v) {
                cx.error(
                    item.span.clone(),
                    eco_format!("duplicate item in {}", self.name),
                );
                continue;
            }
            out.push(v);
        }
        out
    }
}

fn as_str<'a>(s: &'a DeString<'_>) -> &'a str {
    s
}

/// "`a`, `b` or `c`".
pub(super) fn list(names: &[&str]) -> EcoString {
    let mut out = EcoString::new();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push_str(if i + 1 == names.len() { " or " } else { ", " });
        }
        out.push_str(&eco_format!("`{name}`"));
    }
    out
}

/// The candidate closest to `word`, if it is close enough to be a likely typo.
pub(super) fn suggest<'c>(word: &str, candidates: &[&'c str]) -> Option<&'c str> {
    let limit = word.chars().count().div_ceil(3).max(1);
    candidates
        .iter()
        .map(|c| (distance(word, c), *c))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Levenshtein distance, case-insensitive, with `_` and `-` equal.
fn distance(a: &str, b: &str) -> usize {
    let norm = |c: char| {
        if c == '_' {
            '-'
        } else {
            c.to_ascii_lowercase()
        }
    };
    let a: Vec<char> = a.chars().map(norm).collect();
    let b: Vec<char> = b.chars().map(norm).collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut row = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            row.push((prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1));
        }
        prev = row;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions() {
        assert_eq!(suggest("entyr", &["entry", "edition"]), Some("entry"));
        assert_eq!(
            suggest("strict_paths", &["strict-paths"]),
            Some("strict-paths")
        );
        assert_eq!(suggest("colour", &["entry", "edition"]), None);
    }

    #[test]
    fn lists() {
        assert_eq!(list(&["a"]), "`a`");
        assert_eq!(list(&["a", "b", "c"]), "`a`, `b` or `c`");
    }
}
