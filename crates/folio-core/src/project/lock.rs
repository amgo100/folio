//! The lockfile, `folio.lock` (SPEC §2.4, §3.6).

use ecow::{EcoString, eco_format};
use folio_syntax::FileId;

use super::de::{Cx, Invalid, Table, suggest};
use super::values::{ComponentName, Name, Sha256};
use super::{ProjectError, Spanned};

/// A parsed `folio.lock`: fonts locked by hash, third-party components and
/// packages with their versions and hashes, and the components that
/// unqualified names were bound to (SPEC §2.4, §3.6).
///
/// Folio writes the lockfile; a build fails when a locked font, component or
/// package is not available.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Lockfile {
    /// `[[font]]`: fonts locked by hash.
    pub fonts: Vec<LockedFont>,
    /// `[[component]]`: the components declared in the manifest.
    pub components: Vec<LockedPackage>,
    /// `[[package]]`: packages imported by content and stylesheets.
    pub packages: Vec<LockedPackage>,
    /// `[bindings]`: the component each unqualified name resolves to.
    pub bindings: Bindings,
}

/// A font face locked by the hash of its file.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct LockedFont {
    /// The family name, as stylesheets write it.
    pub family: Spanned<EcoString>,
    /// The PostScript name, which identifies the face within its family.
    pub postscript_name: Spanned<EcoString>,
    /// The hash of the font file.
    pub hash: Spanned<Sha256>,
}

/// A component or package locked at an exact version.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct LockedPackage {
    /// The name, as the manifest or an import writes it.
    pub name: Spanned<ComponentName>,
    /// The exact version that was resolved.
    pub version: Spanned<semver::Version>,
    /// The hash of its contents.
    pub hash: Spanned<Sha256>,
}

/// The recorded bindings of unqualified names to component namespaces
/// (SPEC §3.6): `question` written without a namespace means the element
/// `question` of the component recorded here, even if a later version of
/// another component adds an element of that name.
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Bindings {
    /// `[bindings.elements]`: element names.
    pub elements: Vec<Binding>,
    /// `[bindings.properties]`: stylesheet property names.
    pub properties: Vec<Binding>,
}

/// One recorded binding of an unqualified name.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Binding {
    /// The unqualified name.
    pub name: Spanned<Name>,
    /// The component whose namespace it resolves in.
    pub component: Spanned<ComponentName>,
}

impl Bindings {
    /// The component an unqualified element name is bound to.
    pub fn element(&self, name: &str) -> Option<&ComponentName> {
        find(&self.elements, name)
    }

    /// The component an unqualified property name is bound to.
    pub fn property(&self, name: &str) -> Option<&ComponentName> {
        find(&self.properties, name)
    }
}

fn find<'a>(bindings: &'a [Binding], name: &str) -> Option<&'a ComponentName> {
    bindings
        .iter()
        .find(|b| b.name.v.as_str() == name)
        .map(|b| &b.component.v)
}

impl Lockfile {
    /// The lockfile format this compiler reads and writes.
    pub const FORMAT: i64 = 1;

    /// Parse the text of a `folio.lock`. `file` is the id that the spans of
    /// the model and of errors point into.
    ///
    /// Returns every error found, in source order.
    pub fn parse(text: &str, file: FileId) -> Result<Self, Vec<ProjectError>> {
        let mut cx = Cx::new(file);
        let doc = cx.parse(text);
        let root = Table::root(&doc);
        root.check_keys(
            &mut cx,
            &["version", "font", "component", "package", "bindings"],
        );

        let format = match root.get("version") {
            Some(v) => v.integer(&mut cx).map(|n| (n, v.span.clone())),
            None => {
                cx.error(0..0, "missing key `version` in the lockfile")
                    .hint(eco_format!(
                        "this compiler writes `version = {}`",
                        Self::FORMAT
                    ));
                None
            }
        };
        if let Some((n, range)) = &format
            && *n != Self::FORMAT
        {
            let error = cx.error(range.clone(), eco_format!("unknown lockfile format {n}"));
            if *n > Self::FORMAT {
                error.hint("the lockfile was written by a newer Folio compiler");
            }
            error.hint(eco_format!("this compiler reads format {}", Self::FORMAT));
        }

        let fonts = root
            .get("font")
            .map(|v| {
                let tables = v.tables(&mut cx, "font");
                read_fonts(&mut cx, &tables)
            })
            .unwrap_or_default();
        let components = root
            .get("component")
            .map(|v| {
                let tables = v.tables(&mut cx, "component");
                read_packages(&mut cx, &tables, "component")
            })
            .unwrap_or_default();
        let packages = root
            .get("package")
            .map(|v| {
                let tables = v.tables(&mut cx, "package");
                read_packages(&mut cx, &tables, "package")
            })
            .unwrap_or_default();
        let bindings = root
            .get("bindings")
            .and_then(|v| v.table(&mut cx, "`[bindings]`".into()))
            .map(|table| read_bindings(&mut cx, &table, &components))
            .unwrap_or_default();

        let lockfile = format.map(|_| Self {
            fonts,
            components,
            packages,
            bindings,
        });
        cx.finish(lockfile)
    }

    /// The locked version of a component.
    pub fn component(&self, name: &str) -> Option<&LockedPackage> {
        self.components.iter().find(|c| c.name.v.as_str() == name)
    }

    /// The locked version of a package.
    pub fn package(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.name.v.as_str() == name)
    }
}

fn read_fonts(cx: &mut Cx, tables: &[Table<'_>]) -> Vec<LockedFont> {
    let mut fonts: Vec<LockedFont> = Vec::new();
    for table in tables {
        table.check_keys(cx, &["family", "postscript-name", "hash"]);
        let family = table
            .require(cx, "family")
            .and_then(|v| v.parse(cx, non_empty));
        let postscript_name = table
            .require(cx, "postscript-name")
            .and_then(|v| v.parse(cx, non_empty));
        let hash = table
            .require(cx, "hash")
            .and_then(|v| v.parse(cx, Sha256::parse));
        let (Some(family), Some(postscript_name), Some(hash)) = (family, postscript_name, hash)
        else {
            continue;
        };
        if fonts
            .iter()
            .any(|f| f.postscript_name.v == postscript_name.v)
        {
            let range = table
                .get("postscript-name")
                .map(|v| v.span.clone())
                .unwrap_or_default();
            cx.error(
                range,
                eco_format!("the font `{}` is locked twice", postscript_name.v),
            );
            continue;
        }
        fonts.push(LockedFont {
            family,
            postscript_name,
            hash,
        });
    }
    fonts
}

fn read_packages(cx: &mut Cx, tables: &[Table<'_>], kind: &str) -> Vec<LockedPackage> {
    let mut out: Vec<LockedPackage> = Vec::new();
    for table in tables {
        table.check_keys(cx, &["name", "version", "hash"]);
        let name = table
            .require(cx, "name")
            .and_then(|v| v.parse(cx, ComponentName::parse));
        let version = table.require(cx, "version").and_then(|v| {
            v.parse(cx, |s| {
                semver::Version::parse(s).map_err(|_| {
                    Invalid::new(eco_format!("`{s}` is not an exact version")).with_hint(
                        "the lockfile records the version that was resolved, such as `0.3.2`",
                    )
                })
            })
        });
        let hash = table
            .require(cx, "hash")
            .and_then(|v| v.parse(cx, Sha256::parse));
        let (Some(name), Some(version), Some(hash)) = (name, version, hash) else {
            continue;
        };
        if out.iter().any(|p| p.name.v == name.v) {
            let range = table
                .get("name")
                .map(|v| v.span.clone())
                .unwrap_or_default();
            cx.error(
                range,
                eco_format!("the {kind} `{}` is locked twice", name.v),
            );
            continue;
        }
        out.push(LockedPackage {
            name,
            version,
            hash,
        });
    }
    out
}

fn read_bindings(cx: &mut Cx, table: &Table<'_>, components: &[LockedPackage]) -> Bindings {
    table.check_keys(cx, &["elements", "properties"]);
    let mut known: Vec<&str> = super::FIRST_PARTY_COMPONENTS.to_vec();
    known.extend(components.iter().map(|c| c.name.v.as_str()));
    let mut read = |key: &str| {
        let Some(table) = table
            .get(key)
            .and_then(|v| v.table(cx, eco_format!("`[bindings.{key}]`")))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (key, value) in table.entries() {
            let name = match Name::parse(key.name) {
                Ok(name) => cx.spanned(name, key.span.clone()),
                Err(invalid) => {
                    cx.invalid(key.span.clone(), invalid);
                    continue;
                }
            };
            let Some(component) = value.parse(cx, ComponentName::parse) else {
                continue;
            };
            let text = component.v.as_str();
            if !known.contains(&text) {
                let error = cx.error(
                    value.span.clone(),
                    eco_format!(
                        "`{name}` is bound to `{text}`, which is not locked",
                        name = name.v
                    ),
                );
                match suggest(text, &known) {
                    Some(close) => error.hint(eco_format!("did you mean `{close}`?")),
                    None => {
                        error.hint("bind names to first-party components or to a `[[component]]`")
                    }
                };
                continue;
            }
            out.push(Binding { name, component });
        }
        out
    };
    let elements = read("elements");
    let properties = read("properties");
    Bindings {
        elements,
        properties,
    }
}

fn non_empty(text: &str) -> Result<EcoString, Invalid> {
    if text.trim().is_empty() {
        Err(Invalid::new("the name is empty"))
    } else {
        Ok(text.into())
    }
}
