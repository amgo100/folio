//! The project manifest, `folio.toml` (SPEC §4.1).

use ecow::{EcoString, eco_format};
use folio_syntax::FileId;

use super::de::{Cx, Invalid, Table, Value, list, suggest};
use super::values::{
    ComponentName, Date, Edition, FIRST_PARTY_COMPONENTS, Lang, Name, PathPattern, ProjectPath,
    QualifiedName, RelativePath, Target,
};
use super::{ProjectError, Spanned};

/// A parsed `folio.toml`: the project's entry point, pinned versions,
/// components, variants, numbering groups, formatting mode and output naming.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Manifest {
    /// `[project]`.
    pub project: Project,
    /// `[components]`.
    pub components: Components,
    /// `[variants.*]`, in declaration order.
    pub variants: Vec<Variant>,
    /// `[numbering-groups.*]`, in declaration order.
    pub numbering_groups: Vec<NumberingGroup>,
    /// `[formatting]`.
    pub formatting: Formatting,
    /// `[output]`.
    pub output: Output,
}

/// `[project]`: the entry point, pinned versions, language and stylesheets.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Project {
    /// The content file that compilation starts from.
    pub entry: Spanned<ProjectPath>,
    /// The language edition (SPEC §2.4).
    pub edition: Spanned<Edition>,
    /// The compiler version that fixes output (SPEC §2.4).
    pub compiler: Spanned<semver::Version>,
    /// The document language, which chooses locale strings (SPEC §4.5).
    /// English when not given.
    pub lang: Spanned<Lang>,
    /// The project stylesheets, applied in order to every variant.
    pub styles: Vec<Spanned<ProjectPath>>,
    /// The date that scripts see as `today`, since there is no clock
    /// (SPEC §4.4). The command line can also supply one.
    pub today: Option<Spanned<Date>>,
}

/// `[components]`: components declared by path or version, and first-party
/// components that are switched off (SPEC §3.6).
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Components {
    /// Declared components, in declaration order.
    pub declared: Vec<ComponentDecl>,
    /// First-party components listed in `disable`.
    pub disabled: Vec<Spanned<ComponentName>>,
}

impl Components {
    /// Whether a component is available: first-party and not disabled, or
    /// declared.
    pub fn is_enabled(&self, name: &str) -> bool {
        if FIRST_PARTY_COMPONENTS.contains(&name) {
            !self.disabled.iter().any(|d| d.v.as_str() == name)
        } else {
            self.declared.iter().any(|d| d.name.v.as_str() == name)
        }
    }
}

/// A component declared in `[components]`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ComponentDecl {
    /// The component's name, which is also its namespace.
    pub name: Spanned<ComponentName>,
    /// Where it comes from.
    pub source: ComponentSource,
}

/// Where a declared component comes from.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ComponentSource {
    /// A directory, relative to the project: `{ path = "../house-style" }`.
    Path(Spanned<RelativePath>),
    /// A published version: `{ version = "0.3" }`. The lockfile pins the
    /// exact version that matched.
    Version(Spanned<semver::VersionReq>),
}

/// `[variants.<name>]`: a named output of the document (SPEC §6.1).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Variant {
    /// The variant's name, as `@variant` rules and `variant` in scripts see
    /// it.
    pub name: Spanned<Name>,
    /// The targets it is built for; at least one.
    pub targets: Vec<Spanned<Target>>,
    /// Stylesheet overlays appended to the project styles, in order.
    pub styles: Vec<Spanned<ProjectPath>>,
}

/// `[numbering-groups.<name>]`: variants that agree on numbering (SPEC §6.1).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct NumberingGroup {
    /// The group's name.
    pub name: Spanned<Name>,
    /// Its variants; each is declared, and in no other group.
    pub variants: Vec<Spanned<Name>>,
    /// The counters resolved on the group's common tree.
    pub shared_counters: Vec<Spanned<QualifiedName>>,
}

/// `[formatting]`: local or strict formatting (SPEC §5.4, §5.5).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Formatting {
    /// Whether content may carry formatting. Local when not given.
    pub mode: Spanned<FormattingMode>,
    /// In strict mode, the files it applies to; all files when empty.
    pub strict_paths: Vec<Spanned<PathPattern>>,
    /// The stylesheet that holds id rules for single elements.
    pub overrides_sheet: Option<Spanned<ProjectPath>>,
    /// Whether the overrides sheet may hold id rules in strict mode. True
    /// when not given.
    pub allow_overrides_sheet: Spanned<bool>,
    /// Whether a strict project may use components and packages that rely
    /// on local overrides. False when not given.
    pub allow_local_packages: Spanned<bool>,
}

impl Default for Formatting {
    fn default() -> Self {
        Self {
            mode: Spanned::detached(FormattingMode::Local),
            strict_paths: Vec::new(),
            overrides_sheet: None,
            allow_overrides_sheet: Spanned::detached(true),
            allow_local_packages: Spanned::detached(false),
        }
    }
}

/// Whether content may carry formatting (SPEC §5.5).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum FormattingMode {
    /// Local overrides in content are allowed and tracked.
    Local,
    /// Stylesheets are the only source of formatting.
    Strict,
}

/// `[output]`: where outputs are written (SPEC §6.4).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Output {
    /// The path of each output file. `out/{stem}-{variant}.{ext}` when not
    /// given.
    pub path: Spanned<OutputPattern>,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            path: Spanned::detached(OutputPattern::default()),
        }
    }
}

/// The path of an output file, relative to the project, with the
/// placeholders `{stem}` (the entry file's name without extension),
/// `{variant}` and `{ext}` (the target's file extension).
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct OutputPattern(EcoString);

impl OutputPattern {
    const PLACEHOLDERS: &[&str] = &["stem", "variant", "ext"];

    /// The pattern as written, normalised.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the pattern uses a placeholder, given without braces.
    pub fn uses(&self, placeholder: &str) -> bool {
        self.0.contains(&format!("{{{placeholder}}}"))
    }

    /// The path of one output file.
    pub fn resolve(&self, stem: &str, variant: &str, ext: &str) -> EcoString {
        self.0
            .replace("{stem}", stem)
            .replace("{variant}", variant)
            .replace("{ext}", ext)
    }

    fn parse(text: &str) -> Result<Self, Invalid> {
        if text.ends_with('/') {
            return Err(Invalid::new("the output path names a directory")
                .with_hint("end it with a file name, such as `out/{stem}-{variant}.{ext}`"));
        }
        let mut rest = text;
        while let Some(open) = rest.find(['{', '}']) {
            let after = &rest[open + 1..];
            let close = after.find('}').filter(|_| rest[open..].starts_with('{'));
            let Some(close) = close else {
                return Err(
                    Invalid::new(eco_format!("unmatched brace in `{text}`")).with_hint(
                        eco_format!("placeholders are {}", braced(Self::PLACEHOLDERS)),
                    ),
                );
            };
            let name = &after[..close];
            if !Self::PLACEHOLDERS.contains(&name) {
                let error = Invalid::new(eco_format!("unknown placeholder `{{{name}}}`"));
                return Err(match suggest(name, Self::PLACEHOLDERS) {
                    Some(close) => error.with_hint(eco_format!("did you mean `{{{close}}}`?")),
                    None => error.with_hint(eco_format!(
                        "placeholders are {}",
                        braced(Self::PLACEHOLDERS)
                    )),
                });
            }
            rest = &after[close + 1..];
        }
        let path = ProjectPath::parse(text).map_err(|mut invalid| {
            invalid
                .hints
                .push("outputs are written inside the project directory".into());
            invalid
        })?;
        Ok(Self(path.as_str().into()))
    }
}

impl Default for OutputPattern {
    fn default() -> Self {
        Self("out/{stem}-{variant}.{ext}".into())
    }
}

impl std::fmt::Debug for OutputPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.0.as_str(), f)
    }
}

fn braced(names: &[&str]) -> EcoString {
    let names: Vec<String> = names.iter().map(|n| format!("{{{n}}}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    list(&names)
}

impl Manifest {
    /// Parse the text of a `folio.toml`. `file` is the id that the spans of
    /// the model and of errors point into.
    ///
    /// Returns every error found, in source order.
    pub fn parse(text: &str, file: FileId) -> Result<Self, Vec<ProjectError>> {
        let mut cx = Cx::new(file);
        let doc = cx.parse(text);
        let root = Table::root(&doc);
        root.check_keys(
            &mut cx,
            &[
                "project",
                "components",
                "variants",
                "numbering-groups",
                "formatting",
                "output",
            ],
        );

        let project = match root.get("project") {
            Some(value) => value
                .table(&mut cx, "`[project]`".into())
                .and_then(|table| read_project(&mut cx, &table)),
            None => {
                cx.error(0..0, "missing table `[project]`").hint(
                    "a manifest starts with `[project]`, which names at least `entry`, `edition` \
                     and `compiler`",
                );
                None
            }
        };

        let components = root
            .get("components")
            .and_then(|v| v.table(&mut cx, "`[components]`".into()))
            .map(|table| read_components(&mut cx, &table))
            .unwrap_or_default();

        let variants = root
            .get("variants")
            .and_then(|v| v.table(&mut cx, "`[variants]`".into()))
            .map(|table| read_variants(&mut cx, &table))
            .unwrap_or_default();

        let numbering_groups = root
            .get("numbering-groups")
            .and_then(|v| v.table(&mut cx, "`[numbering-groups]`".into()))
            .map(|table| read_numbering_groups(&mut cx, &table, &variants))
            .unwrap_or_default();

        let formatting = root
            .get("formatting")
            .and_then(|v| v.table(&mut cx, "`[formatting]`".into()))
            .map(|table| read_formatting(&mut cx, &table))
            .unwrap_or_default();

        let output = root
            .get("output")
            .and_then(|v| v.table(&mut cx, "`[output]`".into()))
            .map(|table| read_output(&mut cx, &table, &variants))
            .unwrap_or_default();

        let manifest = project.map(|project| Self {
            project,
            components,
            variants,
            numbering_groups,
            formatting,
            output,
        });
        cx.finish(manifest)
    }

    /// The variant with the given name.
    pub fn variant(&self, name: &str) -> Option<&Variant> {
        self.variants.iter().find(|v| v.name.v.as_str() == name)
    }

    /// The numbering group a variant belongs to, if any.
    pub fn numbering_group(&self, variant: &str) -> Option<&NumberingGroup> {
        self.numbering_groups
            .iter()
            .find(|g| g.variants.iter().any(|v| v.v.as_str() == variant))
    }
}

fn read_project(cx: &mut Cx, table: &Table<'_>) -> Option<Project> {
    table.check_keys(
        cx,
        &["entry", "edition", "compiler", "lang", "styles", "today"],
    );
    let entry = table
        .require(cx, "entry")
        .and_then(|v| v.parse(cx, |s| ProjectPath::parse_file(s, "fol", "a content file")));
    let edition = table
        .require(cx, "edition")
        .and_then(|v| v.parse(cx, Edition::parse));
    let compiler = table
        .require(cx, "compiler")
        .and_then(|v| v.parse(cx, parse_compiler));
    let lang = match table.get("lang") {
        Some(v) => v.parse(cx, Lang::parse),
        None => Some(Spanned::detached(Lang::english())),
    };
    let styles = table
        .get("styles")
        .map(|v| stylesheets(cx, v))
        .unwrap_or_default();
    let today = table.get("today").and_then(|v| {
        let (year, month, day) = v.date(cx)?;
        Some(cx.spanned(Date { year, month, day }, v.span.clone()))
    });
    Some(Project {
        entry: entry?,
        edition: edition?,
        compiler: compiler?,
        lang: lang?,
        styles,
        today,
    })
}

fn parse_compiler(text: &str) -> Result<semver::Version, Invalid> {
    semver::Version::parse(text).map_err(|_| {
        let invalid = Invalid::new(eco_format!("`{text}` is not a compiler version"));
        if text == "current" {
            invalid.with_hint(
                "the manifest pins a version; `--compiler current` builds with the running \
                 compiler for one build",
            )
        } else {
            invalid.with_hint("pin an exact version, such as `1.2.0`")
        }
    })
}

fn stylesheets(cx: &mut Cx, value: &Value<'_>) -> Vec<Spanned<ProjectPath>> {
    value.parse_list(cx, |s| ProjectPath::parse_file(s, "fsty", "a stylesheet"))
}

fn read_components(cx: &mut Cx, table: &Table<'_>) -> Components {
    let mut components = Components::default();
    for (key, value) in table.entries() {
        if key.name == "disable" {
            components.disabled = value.parse_list(cx, |s| {
                let name = ComponentName::parse(s)?;
                if !name.is_first_party() {
                    return Err(
                        Invalid::new(eco_format!("`{s}` is not a first-party component"))
                            .with_hint(eco_format!(
                                "`disable` switches off first-party components: {}",
                                list(FIRST_PARTY_COMPONENTS)
                            )),
                    );
                }
                Ok(name)
            });
            continue;
        }

        let name = match ComponentName::parse(key.name) {
            Ok(name) => name,
            Err(invalid) => {
                cx.invalid(key.span.clone(), invalid);
                continue;
            }
        };
        if name.is_first_party() {
            cx.error(
                key.span.clone(),
                eco_format!("`{name}` is a first-party component and needs no declaration"),
            )
            .hint("first-party components are on by default; `disable` switches them off");
            continue;
        }

        let Some(entry) = value.table(cx, eco_format!("component `{name}`")) else {
            continue;
        };
        entry.check_keys(cx, &["path", "version"]);
        let path = entry.get("path").map(|v| v.parse(cx, RelativePath::parse));
        let version = entry.get("version").map(|v| v.parse(cx, parse_version_req));
        let source = match (path, version) {
            (Some(path), None) => path.map(ComponentSource::Path),
            (None, Some(version)) => version.map(ComponentSource::Version),
            (Some(_), Some(_)) => {
                cx.error(
                    entry.span(),
                    eco_format!("component `{name}` has both `path` and `version`"),
                )
                .hint("a component comes either from a directory or from a published version");
                None
            }
            (None, None) => {
                cx.error(entry.span(), eco_format!("component `{name}` needs `path` or `version`"))
                    .hint(eco_format!(
                        "write `{name} = {{ path = \"../{name}\" }}` or `{name} = {{ version = \"1.0\" }}`"
                    ));
                None
            }
        };
        if let Some(source) = source {
            components.declared.push(ComponentDecl {
                name: cx.spanned(name, key.span.clone()),
                source,
            });
        }
    }
    components
}

fn parse_version_req(text: &str) -> Result<semver::VersionReq, Invalid> {
    semver::VersionReq::parse(text).map_err(|_| {
        Invalid::new(eco_format!("`{text}` is not a version requirement"))
            .with_hint("write a version such as `0.3` or `1.2.0`")
    })
}

fn read_variants(cx: &mut Cx, table: &Table<'_>) -> Vec<Variant> {
    let mut variants = Vec::new();
    for (key, value) in table.entries() {
        let name = match Name::parse(key.name) {
            Ok(name) => cx.spanned(name, key.span.clone()),
            Err(invalid) => {
                cx.invalid(key.span.clone(), invalid);
                continue;
            }
        };
        let Some(entry) = value.table(cx, eco_format!("`[variants.{}]`", key.name)) else {
            continue;
        };
        entry.check_keys(cx, &["targets", "styles"]);
        let targets = match entry.require(cx, "targets") {
            Some(v) => {
                let targets = v.parse_list(cx, Target::parse);
                if targets.is_empty() && v.is_empty_array() {
                    cx.error(
                        v.span.clone(),
                        eco_format!("variant `{}` has no targets", key.name),
                    )
                    .hint("list at least one of `pdf`, `docx`, `pptx` and `live`");
                }
                targets
            }
            None => Vec::new(),
        };
        let styles = entry
            .get("styles")
            .map(|v| stylesheets(cx, v))
            .unwrap_or_default();
        variants.push(Variant {
            name,
            targets,
            styles,
        });
    }
    variants
}

fn read_numbering_groups(
    cx: &mut Cx,
    table: &Table<'_>,
    variants: &[Variant],
) -> Vec<NumberingGroup> {
    let declared: Vec<&str> = variants.iter().map(|v| v.name.v.as_str()).collect();
    let mut groups: Vec<NumberingGroup> = Vec::new();
    for (key, value) in table.entries() {
        let name = match Name::parse(key.name) {
            Ok(name) => cx.spanned(name, key.span.clone()),
            Err(invalid) => {
                cx.invalid(key.span.clone(), invalid);
                continue;
            }
        };
        let Some(entry) = value.table(cx, eco_format!("`[numbering-groups.{}]`", key.name)) else {
            continue;
        };
        entry.check_keys(cx, &["variants", "shared-counters"]);

        let mut members = Vec::new();
        if let Some(v) = entry.require(cx, "variants") {
            let items = v.array(cx).unwrap_or_default();
            if v.is_empty_array() {
                cx.error(
                    v.span.clone(),
                    eco_format!("numbering group `{}` has no variants", key.name),
                );
            }
            for item in items {
                let Some(variant) = item.parse(cx, Name::parse) else {
                    continue;
                };
                let text = variant.v.as_str();
                if !declared.contains(&text) {
                    let error = cx.error(
                        item.span.clone(),
                        eco_format!("no variant `{text}` is declared"),
                    );
                    match suggest(text, &declared) {
                        Some(close) => error.hint(eco_format!("did you mean `{close}`?")),
                        None => error.hint(eco_format!("declare it in `[variants.{text}]`")),
                    };
                } else if members.iter().any(|m: &Spanned<Name>| m.v == variant.v) {
                    cx.error(
                        item.span.clone(),
                        eco_format!("duplicate item in `variants`"),
                    );
                } else if let Some(other) = groups
                    .iter()
                    .find(|g| g.variants.iter().any(|m| m.v == variant.v))
                {
                    cx.error(
                        item.span.clone(),
                        eco_format!(
                            "variant `{text}` is already in numbering group `{}`",
                            other.name.v
                        ),
                    )
                    .hint("a variant belongs to at most one numbering group");
                } else {
                    members.push(variant);
                }
            }
        }

        let shared_counters = entry
            .get("shared-counters")
            .map(|v| v.parse_list(cx, QualifiedName::parse))
            .unwrap_or_default();
        groups.push(NumberingGroup {
            name,
            variants: members,
            shared_counters,
        });
    }
    groups
}

fn read_formatting(cx: &mut Cx, table: &Table<'_>) -> Formatting {
    table.check_keys(
        cx,
        &[
            "mode",
            "strict-paths",
            "overrides-sheet",
            "allow-overrides-sheet",
            "allow-local-packages",
        ],
    );
    let mut formatting = Formatting::default();
    if let Some(mode) = table.get("mode").and_then(|v| {
        v.parse(cx, |s| match s {
            "local" => Ok(FormattingMode::Local),
            "strict" => Ok(FormattingMode::Strict),
            _ => Err(Invalid::new(eco_format!("unknown formatting mode `{s}`"))
                .with_hint("the modes are `local` and `strict`")),
        })
    }) {
        formatting.mode = mode;
    }
    if let Some(v) = table.get("strict-paths") {
        formatting.strict_paths = v.parse_list(cx, PathPattern::parse);
        if formatting.mode.v == FormattingMode::Local {
            cx.error(v.span.clone(), "`strict-paths` applies only in strict mode")
                .hint("set `mode = \"strict\"` to make these paths strict");
        }
    }
    formatting.overrides_sheet = table
        .get("overrides-sheet")
        .and_then(|v| v.parse(cx, |s| ProjectPath::parse_file(s, "fsty", "a stylesheet")));
    for (key, field) in [
        (
            "allow-overrides-sheet",
            &mut formatting.allow_overrides_sheet,
        ),
        ("allow-local-packages", &mut formatting.allow_local_packages),
    ] {
        if let Some(v) = table.get(key)
            && let Some(b) = v.bool(cx)
        {
            *field = cx.spanned(b, v.span.clone());
        }
    }
    formatting
}

fn read_output(cx: &mut Cx, table: &Table<'_>, variants: &[Variant]) -> Output {
    table.check_keys(cx, &["path"]);
    let Some(path) = table
        .get("path")
        .and_then(|v| v.parse(cx, OutputPattern::parse))
    else {
        return Output::default();
    };
    let Some(range) = table.get("path").map(|v| v.span.clone()) else {
        return Output::default();
    };

    // Two outputs must not share a file.
    let writes_file = |v: &Variant| v.targets.iter().any(|t| t.v.extension().is_some());
    if !path.v.uses("variant") && variants.iter().filter(|v| writes_file(v)).count() > 1 {
        cx.error(range.clone(), "every variant writes to the same file")
            .hint("add `{variant}` to the path");
    }
    let file_targets = |v: &Variant| {
        v.targets
            .iter()
            .filter(|t| t.v.extension().is_some())
            .count()
    };
    if !path.v.uses("ext")
        && let Some(variant) = variants.iter().find(|v| file_targets(v) > 1)
    {
        cx.error(
            range,
            eco_format!(
                "the targets of variant `{}` write to the same file",
                variant.name.v
            ),
        )
        .hint("add `{ext}` to the path");
    }
    Output { path }
}
