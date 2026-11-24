//! The rules of a build (SPEC §5.1): the stylesheets it reads, with their
//! origins, tokens, compiled selectors and evaluated declarations, in
//! cascade order.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};
use folio_component::{Body, FieldType, Name, PropertyType, QualifiedName, Target};
use folio_syntax::ast::{self, AstNode, SimpleSelector, StyleItem};
use folio_syntax::{Span, SyntaxNode};

use super::props::{PropId, Properties, confusable};
use super::selector::{AttrTest, Compound, Needs, NodeName, Pseudo, Selector, Specificity};
use super::value::{Declared, TokenDef, Tokens, Values, ValuesCx, check};
use crate::component::{ComponentHost, Reason};
use crate::db::{Build, Db, Project, component_host, source_file};
use crate::diag::{self, Diagnostic};
use crate::eval;
use crate::model::{builtin, builtins};
use crate::project::{Manifest, ProjectPath};

/// Where a rule comes from. Later origins win over earlier ones, whatever
/// their specificity (SPEC §5.1).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Origin {
    /// The initial values of built-in and component properties.
    Default,
    /// A stylesheet another stylesheet imports.
    Theme,
    /// A project stylesheet (`project.styles`).
    Project,
    /// A variant's overlay: its stylesheets, and `@variant` blocks.
    Variant,
    /// An `@target` block.
    Target,
    /// The overrides stylesheet (`formatting.overrides-sheet`, SPEC §5.4).
    Overrides,
    /// Local overrides in content: scoped rules, `override` fields and
    /// layout hints (SPEC §5.4).
    Local,
}

/// A rule's place in the cascade: origin, then specificity, then source
/// order. A rule with a higher priority wins.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Priority {
    /// Where it comes from.
    pub origin: Origin,
    /// Its selector's specificity.
    pub specificity: Specificity,
    /// Its position in source order, across the stylesheets of a build.
    pub order: u32,
}

/// A compiled rule with one selector. A rule with a selector list becomes
/// one rule per selector.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Rule {
    /// What it styles.
    pub selector: Selector,
    /// Its place in the cascade.
    pub priority: Priority,
    /// Its declarations, in source order.
    pub declarations: EcoVec<Decl>,
    /// The rule in its stylesheet.
    pub span: Span,
}

/// A declaration: a property and its value.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Decl {
    /// The property.
    pub prop: PropId,
    /// The value.
    pub value: DeclValue,
    /// The declaration in its stylesheet.
    pub span: Span,
}

/// The value of a declaration.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum DeclValue {
    /// A value, computed per element.
    Value(Declared),
    /// A render template (SPEC §5.3): the declaration, which the template
    /// stage expands per element.
    Template(SyntaxNode),
}

/// A rule with a `:step(..)` pseudo-class: collected by the style stage and
/// applied per step by step styling (SPEC §2.1, §10.2).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct StepRule {
    /// The rule, with the step pseudo-class removed from its selector.
    pub rule: Rule,
    /// The steps: first and last, `None` where a range is open.
    pub steps: Steps,
}

/// The steps of a `:step(..)` pseudo-class: first and last, `None` where a
/// range is open.
pub type Steps = EcoVec<(Option<u64>, Option<u64>)>;

/// The rules of a build, in cascade order.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Cascade {
    /// The properties the build knows.
    pub properties: Arc<Properties>,
    /// The rules that apply, lowest priority first.
    pub rules: EcoVec<Rule>,
    /// The `:step` rules, collected for step styling.
    pub step_rules: EcoVec<StepRule>,
    /// What the rules need to see of nodes.
    pub needs: Needs,
    /// The token definitions, in cascade order.
    tokens: EcoVec<(EcoString, TokenDef)>,
    /// The rules by the key of their subject compound.
    index: BTreeMap<IndexKey, Vec<u32>>,
    /// The names of slots and content fields, which selectors can name.
    parts: BTreeSet<Name>,
}

/// What a subject compound requires most selectively.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
enum IndexKey {
    Label(EcoString),
    Class(EcoString),
    Name(NodeName),
    Any,
}

impl IndexKey {
    fn of(compound: &Compound) -> Self {
        if let Some(label) = &compound.label {
            Self::Label(label.clone())
        } else if let Some(class) = compound.classes.first() {
            Self::Class(class.clone())
        } else if let Some(name) = &compound.name {
            Self::Name(name.clone())
        } else {
            Self::Any
        }
    }
}

impl Cascade {
    /// The rules that may match a node with a name, classes and label, by
    /// index, lowest priority first.
    pub fn candidates(
        &self,
        name: &NodeName,
        classes: &[EcoString],
        label: Option<&EcoString>,
    ) -> Vec<u32> {
        let mut out = Vec::new();
        let mut take = |key: IndexKey| {
            if let Some(rules) = self.index.get(&key) {
                out.extend_from_slice(rules);
            }
        };
        take(IndexKey::Any);
        take(IndexKey::Name(name.clone()));
        for class in classes {
            take(IndexKey::Class(class.clone()));
        }
        if let Some(label) = label {
            take(IndexKey::Label(label.clone()));
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The token table, for rules compiled later: scoped rules in content.
    pub(super) fn tokens(&self) -> Tokens {
        Tokens::new(self.tokens.iter().cloned())
    }
}

/// The rules of a build: those of its project stylesheets and their
/// imports, of its variant's overlays, and of the overrides sheet, with the
/// `@variant` and `@target` blocks that apply to it. Reports errors in the
/// rules of every block, whether or not it applies.
///
/// Reads no content file, so an edit to content does not re-run it.
#[salsa::tracked(returns(ref))]
pub fn cascade(db: &dyn Db, build: Build<'_>) -> Cascade {
    let project = build.project(db);
    let manifest = project.manifest(db);
    let host = component_host(db, project).as_ref();
    let variant = build.variant(db).clone();
    let target = build.target(db);

    // Gather the items of every stylesheet in cascade order.
    let mut gather = Gather {
        db,
        project,
        manifest,
        variant,
        target,
        seen: HashSet::new(),
        rules: Vec::new(),
        tokens: Vec::new(),
        office: BTreeSet::new(),
        order: 0,
    };
    for path in &manifest.project.styles {
        gather.file(&path.v, Origin::Project);
    }
    if let Some(variant) = gather.variant.clone()
        && let Some(decl) = manifest.variants.iter().find(|v| v.name.v == variant)
    {
        for path in &decl.styles {
            gather.file(&path.v, Origin::Variant);
        }
    }
    if let Some(path) = &manifest.formatting.overrides_sheet {
        gather.file(&path.v, Origin::Overrides);
    }

    let properties = Arc::new(Properties::new(host, gather.office.iter().cloned()));
    let tokens = Tokens::new(gather.tokens.iter().cloned());
    let parts = part_names(host);
    let cx = Compiler {
        db,
        host,
        properties: &properties,
        parts: &parts,
        values: ValuesCx {
            db,
            project,
            tokens: &tokens,
        },
    };

    let mut rules = Vec::new();
    let mut step_rules = Vec::new();
    for pending in &gather.rules {
        let rule = pending
            .node
            .cast::<ast::StyleRule<'_>>()
            .expect("a style rule");
        let values = Values::new(cx.values, pending.file.clone());
        let compiled = cx.rule(
            &values,
            rule.selectors(),
            rule.declarations(),
            pending.origin,
            pending.order,
            rule.span(),
        );
        if pending.applies {
            rules.extend(compiled.rules);
            step_rules.extend(compiled.steps);
        }
    }
    rules.sort_by_key(|r| r.priority);

    let mut needs = Needs::default();
    let mut index: BTreeMap<IndexKey, Vec<u32>> = BTreeMap::new();
    for (i, rule) in rules.iter().enumerate() {
        needs.add(&rule.selector);
        index
            .entry(IndexKey::of(rule.selector.subject()))
            .or_default()
            .push(i as u32);
    }

    Cascade {
        properties,
        rules: rules.into_iter().collect(),
        step_rules: step_rules.into_iter().collect(),
        needs,
        tokens: gather.tokens.into_iter().collect(),
        index,
        parts,
    }
}

/// A rule found while gathering, not yet compiled.
struct Pending {
    node: SyntaxNode,
    file: ProjectPath,
    origin: Origin,
    order: u32,
    /// Whether it is in no block or in blocks that apply to the build.
    applies: bool,
}

struct Gather<'a> {
    db: &'a dyn Db,
    project: Project,
    manifest: &'a Manifest,
    variant: Option<Name>,
    target: Target,
    seen: HashSet<ProjectPath>,
    rules: Vec<Pending>,
    tokens: Vec<(EcoString, TokenDef)>,
    office: BTreeSet<Name>,
    order: u32,
}

impl Gather<'_> {
    /// Gathers a stylesheet, once per build. A missing file is reported by
    /// the search for the build's files.
    fn file(&mut self, path: &ProjectPath, origin: Origin) {
        if !self.seen.insert(path.clone()) {
            return;
        }
        let Some(file) = source_file(self.db, self.project, path.clone()) else {
            return;
        };
        let root = file.source(self.db).root();
        let Some(sheet) = root.cast::<ast::Stylesheet<'_>>() else {
            diag::report(
                self.db,
                Diagnostic::error(
                    root.span(),
                    eco_format!("`{}` is not a stylesheet", path.as_str()),
                )
                .with_hint("stylesheets are `.fsty` files"),
            );
            return;
        };
        let items: Vec<_> = sheet.items().collect();
        self.items(path, &items, origin, true);
    }

    fn items(
        &mut self,
        path: &ProjectPath,
        items: &[StyleItem<'_>],
        origin: Origin,
        applies: bool,
    ) {
        for item in items {
            match *item {
                StyleItem::Import(import) => {
                    if let Some(target) = import.path().map(|p| p.get())
                        && !target.starts_with('@')
                        && let Some(imported) = path.join(&target)
                    {
                        // Imports bring in themes, whatever imports them.
                        self.file(&imported, Origin::Theme);
                    }
                }
                StyleItem::Token(token) => {
                    if applies {
                        self.tokens.push((
                            token.name().get().into(),
                            TokenDef {
                                node: token.to_untyped().clone(),
                                file: path.clone(),
                            },
                        ));
                    }
                }
                StyleItem::Rule(rule) => {
                    for decl in rule.declarations() {
                        let name = decl.property();
                        if decl.namespace().is_none()
                            && Properties::is_office(name.get())
                            && let Some(name) = Name::new(name.get())
                        {
                            self.office.insert(name);
                        }
                    }
                    self.order += 1;
                    self.rules.push(Pending {
                        node: rule.to_untyped().clone(),
                        file: path.clone(),
                        origin,
                        order: self.order,
                        applies,
                    });
                }
                StyleItem::Overlay(overlay) => {
                    let (inner, matches) = self.overlay(overlay, origin);
                    let inner_items: Vec<_> = overlay.items().collect();
                    self.items(path, &inner_items, inner, applies && matches);
                }
                // Read by the stages they belong to: Office export
                // (SPEC §11.2), render templates (§5.3) and slide masters
                // (§11.3).
                StyleItem::Export(_)
                | StyleItem::Render(_)
                | StyleItem::SlideMaster(_)
                | StyleItem::SlideLayout(_) => {}
            }
        }
    }

    /// The origin of an overlay's rules, and whether the overlay applies to
    /// the build. Reports scopes that name no declared variant or no
    /// target.
    fn overlay(&self, overlay: ast::Overlay<'_>, outer: Origin) -> (Origin, bool) {
        let mut origin = outer;
        let mut matches = true;
        for scope in overlay.scopes() {
            let names: Vec<_> = scope.names().collect();
            match scope.kind() {
                "variant" => {
                    origin = origin.max(Origin::Variant);
                    let mut any = false;
                    for name in names {
                        let declared = self
                            .manifest
                            .variants
                            .iter()
                            .any(|v| v.name.v.as_str() == name.get());
                        if !declared {
                            diag::report(
                                self.db,
                                Diagnostic::error(
                                    name.span(),
                                    eco_format!("no variant `{}` is declared", name.get()),
                                )
                                .with_hint(
                                    "variants are declared as `[variants.<name>]` in folio.toml",
                                ),
                            );
                        }
                        any |= self
                            .variant
                            .as_ref()
                            .is_some_and(|v| v.as_str() == name.get());
                    }
                    matches &= any;
                }
                "target" => {
                    origin = origin.max(Origin::Target);
                    let mut any = false;
                    for name in names {
                        match Target::from_name(name.get()) {
                            Some(target) => any |= target == self.target,
                            None => diag::report(
                                self.db,
                                Diagnostic::error(
                                    name.span(),
                                    eco_format!("unknown target `{}`", name.get()),
                                )
                                .with_hint("the targets are `pdf`, `docx`, `pptx` and `live`"),
                            ),
                        }
                    }
                    matches &= any;
                }
                // A syntax error, reported by the parser.
                _ => matches = false,
            }
        }
        (origin, matches)
    }
}

/// The names of slots and content fields of the elements in use: a
/// selector can name them as parts (`example > solution`).
fn part_names(host: Option<&ComponentHost>) -> BTreeSet<Name> {
    let mut out = BTreeSet::new();
    let schemas = builtins().iter().map(|b| &b.schema).chain(
        host.into_iter()
            .flat_map(|h| h.components())
            .flat_map(|c| &c.declarations.elements),
    );
    for schema in schemas {
        for field in &schema.fields {
            if field.ty == FieldType::Content {
                out.insert(field.name.clone());
            }
        }
        if let Body::Slots(slots) = &schema.body {
            out.extend(slots.iter().map(|s| s.name.clone()));
        }
    }
    out
}

/// What compiling one rule gives.
#[derive(Default)]
pub(super) struct Compiled {
    pub(super) rules: Vec<Rule>,
    pub(super) steps: Vec<StepRule>,
}

/// Compiles rules.
pub(super) struct Compiler<'a> {
    pub(super) db: &'a dyn Db,
    pub(super) host: Option<&'a ComponentHost>,
    pub(super) properties: &'a Properties,
    pub(super) parts: &'a BTreeSet<Name>,
    pub(super) values: ValuesCx<'a>,
}

impl<'a> Compiler<'a> {
    /// A compiler for rules compiled after the cascade: scoped rules in
    /// content.
    pub(super) fn for_cascade(
        db: &'a dyn Db,
        project: Project,
        cascade: &'a Cascade,
        tokens: &'a Tokens,
    ) -> Self {
        Self {
            db,
            host: component_host(db, project).as_ref(),
            properties: &cascade.properties,
            parts: &cascade.parts,
            values: ValuesCx {
                db,
                project,
                tokens,
            },
        }
    }

    /// Compiles a rule: one rule per selector, sharing the declarations.
    /// A selector with an error is left out, after it is reported.
    pub(super) fn rule<'s>(
        &self,
        values: &Values<'_>,
        selectors: ast::SelectorList<'s>,
        declarations: impl Iterator<Item = ast::Declaration<'s>>,
        origin: Origin,
        order: u32,
        span: Span,
    ) -> Compiled {
        let declarations = self.declarations(values, declarations);
        let mut out = Compiled::default();
        for selector in selectors.iter() {
            if selector.to_untyped().diagnosis().errors {
                continue;
            }
            let Some((selector, steps)) = self.selector(values, selector) else {
                continue;
            };
            let rule = Rule {
                priority: Priority {
                    origin,
                    specificity: selector.specificity(),
                    order,
                },
                selector,
                declarations: declarations.clone(),
                span,
            };
            match steps {
                Some(steps) => out.steps.push(StepRule { rule, steps }),
                None => out.rules.push(rule),
            }
        }
        out
    }

    /// Compiles declarations, leaving out those with errors.
    pub(super) fn declarations<'s>(
        &self,
        values: &Values<'_>,
        declarations: impl Iterator<Item = ast::Declaration<'s>>,
    ) -> EcoVec<Decl> {
        let mut out = EcoVec::new();
        for decl in declarations {
            let span = decl.to_untyped().span();
            // A syntax error is reported by the parser already.
            if decl.to_untyped().diagnosis().errors {
                continue;
            }
            let Some(prop) = self.property(decl) else {
                continue;
            };
            let property = self.properties.get(prop);
            let value = if property.ty == PropertyType::Template && !is_keyword_value(decl) {
                DeclValue::Template(decl.to_untyped().clone())
            } else {
                let Some(value) = values.declaration(decl) else {
                    continue;
                };
                if let Err(message) = check(&value, &property.ty, false) {
                    let mut diagnostic = Diagnostic::error(
                        span,
                        eco_format!("wrong value for `{}`: {message}", property.name),
                    );
                    if property.name.name.as_str() == "numbering" {
                        diagnostic = diagnostic.with_hint(
                            "`numbering` takes a pattern string, such as \"1.\", \"(a)\" or \"i\"",
                        );
                    }
                    diag::report(self.db, diagnostic);
                    continue;
                }
                DeclValue::Value(value)
            };
            out.push(Decl { prop, value, span });
        }
        out
    }

    /// Resolves the property of a declaration, reporting an unknown one.
    fn property(&self, decl: ast::Declaration<'_>) -> Option<PropId> {
        let ident = decl.property();
        let namespace = decl.namespace().map(|n| n.get().clone());
        self.resolve(namespace.as_deref(), ident.get(), ident.span())
    }

    /// Resolves a property named in content, as a key of an `override`
    /// field or a layout hint: `space-before`, `multicol.columns`.
    pub(super) fn property_named(&self, text: &str, span: Span) -> Option<PropId> {
        match text.split_once('.') {
            Some((namespace, name)) => self.resolve(Some(namespace), name, span),
            None => self.resolve(None, text, span),
        }
    }

    /// Resolves a property name: built-in and office properties first,
    /// then those of components, through the lockfile's bindings for an
    /// unqualified name (SPEC §3.6). Reports a name that does not resolve.
    fn resolve(&self, namespace: Option<&str>, name: &str, span: Span) -> Option<PropId> {
        let written = match namespace {
            Some(namespace) => eco_format!("{namespace}.{name}"),
            None => name.into(),
        };
        let unknown = |hint: Option<EcoString>| {
            let mut diagnostic =
                Diagnostic::error(span, eco_format!("unknown property `{written}`"));
            if let Some(hint) = hint {
                diagnostic = diagnostic.with_hint(hint);
            }
            diag::report(self.db, diagnostic);
            None
        };
        let (Some(plain), namespace) = (Name::new(name), namespace.map(Name::new)) else {
            return unknown(None);
        };
        let namespace = match namespace {
            Some(Some(namespace)) => Some(namespace),
            Some(None) => return unknown(None),
            None => None,
        };
        if namespace.is_none()
            && let Some(id) = self
                .properties
                .find(&QualifiedName::unqualified(plain.clone()))
        {
            return Some(id);
        }
        let qualified = QualifiedName {
            namespace,
            name: plain.clone(),
        };
        match self.host.map(|h| h.property(&qualified)) {
            Some(Ok(resolved)) => {
                let full = QualifiedName::qualified(resolved.component.manifest.namespace(), plain);
                self.properties.find(&full)
            }
            Some(Err(error)) if error.reason != Reason::Unknown => {
                diag::report(self.db, Diagnostic::error(span, error.to_string()));
                None
            }
            _ => unknown(confusable(name)),
        }
    }

    /// Compiles a selector, and the steps of a `:step(..)` pseudo-class on
    /// its subject.
    fn selector(
        &self,
        values: &Values<'_>,
        selector: ast::Selector<'_>,
    ) -> Option<(Selector, Option<Steps>)> {
        let steps_ast: Vec<_> = selector.steps().collect();
        let mut steps = EcoVec::new();
        let mut step_spec = None;
        let last = steps_ast.len().checked_sub(1)?;
        for (i, (combinator, compound)) in steps_ast.into_iter().enumerate() {
            let (compound, spec) = self.compound(values, compound, i == last)?;
            if spec.is_some() {
                step_spec = spec;
            }
            steps.push((combinator, compound));
        }
        Some((Selector { steps }, step_spec))
    }

    fn compound(
        &self,
        values: &Values<'_>,
        compound: ast::CompoundSelector<'_>,
        subject: bool,
    ) -> Option<(Compound, Option<Steps>)> {
        let mut parts: Vec<SimpleSelector<'_>> = compound.parts().collect();
        let mut out = Compound::default();
        let mut steps = None;

        if let Some(ident) = compound.element() {
            let qualified = match parts.first() {
                Some(SimpleSelector::Class(class)) if self.is_namespace(ident.get()) => {
                    let class = class.name();
                    // SPEC §5.1: `a.b` is a qualified name when `a` is the
                    // namespace of a component in use.
                    self.lint_namespace(ident.get(), ident.span());
                    Some(QualifiedName {
                        namespace: Name::new(ident.get()),
                        name: Name::new(class.get())?,
                    })
                }
                _ => None,
            };
            if qualified.is_some() {
                parts.remove(0);
            }
            out.name = Some(self.name(ident, qualified)?);
        }

        for part in parts {
            match part {
                SimpleSelector::Class(class) => out.classes.push(class.name().get().clone()),
                SimpleSelector::Id(id) => out.label = Some(id.label().into()),
                SimpleSelector::Attr(attr) => out.attrs.push(self.attr(values, attr)?),
                SimpleSelector::Pseudo(pseudo) => {
                    let name = pseudo.name();
                    let span = pseudo.span();
                    let position = |what: &str| -> Option<u32> {
                        match pseudo.position().and_then(|n| u32::try_from(n).ok()) {
                            Some(n) if n >= 1 => Some(n),
                            _ => {
                                self.error(span, eco_format!("`:{what}(n)` counts from 1"));
                                None
                            }
                        }
                    };
                    match name.get().as_str() {
                        "first" => out.pseudos.push(Pseudo::First),
                        "last" => out.pseudos.push(Pseudo::Last),
                        "nth" => out.pseudos.push(Pseudo::Nth(position("nth")?)),
                        "nth-col" => {
                            out.pseudos.push(Pseudo::NthCol(position("nth-col")?));
                            self.warn(
                                span,
                                "`:nth-col(n)` styles table cells, and there are no tables yet; \
                                 the rule matches nothing",
                            );
                        }
                        "has" => {
                            let Some(inner) = pseudo.has() else {
                                self.error(
                                    span,
                                    "`:has(..)` names a child, such as `:has(answer)`",
                                );
                                return None;
                            };
                            let (inner, _) = self.compound(values, inner, false)?;
                            if !inner.pseudos.is_empty() {
                                self.error(
                                    span,
                                    "the child in `:has(..)` cannot have pseudo-classes",
                                );
                                return None;
                            }
                            out.pseudos.push(Pseudo::Has(Box::new(inner)));
                        }
                        "step" => {
                            if !subject {
                                self.error(
                                    span,
                                    "`:step(..)` belongs on the last compound of a selector",
                                );
                                return None;
                            }
                            let Some(spec) = pseudo.steps() else {
                                self.error(span, "`:step(..)` names steps, such as `:step(2-)`");
                                return None;
                            };
                            steps = Some(spec.ranges().map(|r| r.bounds()).collect());
                        }
                        other => {
                            diag::report(
                                self.db,
                                Diagnostic::warning(span, eco_format!("unknown pseudo-class `:{other}`; the rule is ignored"))
                                    .with_hint("the pseudo-classes are `:first`, `:last`, `:nth(n)`, `:nth-col(n)`, `:has(..)` and `:step(..)`"),
                            );
                            return None;
                        }
                    }
                }
            }
        }
        Some((out, steps))
    }

    /// Whether a name is the namespace of a component in use.
    fn is_namespace(&self, name: &str) -> bool {
        self.host.is_some_and(|h| {
            h.components()
                .any(|c| c.manifest.namespace().as_str() == name)
        })
    }

    /// Warns when a namespace written in a selector is also an element's
    /// name (SPEC §5.1).
    fn lint_namespace(&self, name: &str, span: Span) {
        let is_element = builtin(name).is_some()
            || Name::new(name).is_some_and(|n| {
                self.host
                    .is_some_and(|h| h.element(&QualifiedName::unqualified(n)).is_ok())
            });
        if is_element {
            diag::report(
                self.db,
                Diagnostic::warning(
                    span,
                    eco_format!("`{name}` is a component namespace and an element, so `{name}.` reads as the namespace"),
                )
                .with_code("namespace-element")
                .with_hint(eco_format!("to style `{name}` elements with a class, rename the class or the element")),
            );
        }
    }

    /// Resolves the name of a compound.
    fn name(&self, ident: ast::Ident<'_>, qualified: Option<QualifiedName>) -> Option<NodeName> {
        let span = ident.span();
        if let Some(qualified) = qualified {
            return match self.host.map(|h| h.element(&qualified)) {
                Some(Ok(_)) => Some(NodeName::Element(qualified)),
                Some(Err(error)) => {
                    self.warn(span, eco_format!("{error}; the rule is ignored"));
                    None
                }
                None => None,
            };
        }
        let text = ident.get();
        if text == "page" {
            return Some(NodeName::Page);
        }
        let name = Name::new(text)?;
        if builtin(text).is_some() {
            return Some(NodeName::Element(QualifiedName::unqualified(name)));
        }
        match self
            .host
            .map(|h| h.element(&QualifiedName::unqualified(name.clone())))
        {
            Some(Ok(resolved)) => {
                return Some(NodeName::Element(QualifiedName::qualified(
                    resolved.component.manifest.namespace(),
                    name,
                )));
            }
            Some(Err(error)) if error.reason != Reason::Unknown => {
                self.warn(span, eco_format!("{error}; the rule is ignored"));
                return None;
            }
            _ => {}
        }
        if !self.parts.contains(&name) {
            self.warn(
                span,
                eco_format!("unknown element `{name}`; the rule matches nothing"),
            );
        }
        Some(NodeName::Part(name))
    }

    fn attr(&self, values: &Values<'_>, attr: ast::AttrSelector<'_>) -> Option<AttrTest> {
        let field = attr.field().get().clone();
        let test = match (attr.op(), attr.value()) {
            (Some(op), Some(expr)) => {
                let value = match values.single(expr)? {
                    Declared::Value(v) => v,
                    // `[difficulty=extension]` compares with a string.
                    Declared::Keyword(word) => eval::Value::Str(word.as_str().into()),
                    _ => {
                        self.error(expr.span(), "an attribute test compares with one value");
                        return None;
                    }
                };
                Some((op, value))
            }
            _ => None,
        };
        Some(AttrTest { field, test })
    }

    fn error(&self, span: Span, message: impl Into<EcoString>) {
        diag::report(self.db, Diagnostic::error(span, message));
    }

    fn warn(&self, span: Span, message: impl Into<EcoString>) {
        diag::report(self.db, Diagnostic::warning(span, message));
    }
}

/// Whether a declaration's value is a single `none` or `auto`, which a
/// template property takes as a value rather than a template.
fn is_keyword_value(decl: ast::Declaration<'_>) -> bool {
    let mut values = decl.values();
    matches!(
        (values.next(), values.next()),
        (Some(ast::Expr::None(_) | ast::Expr::Auto(_)), None)
    )
}
