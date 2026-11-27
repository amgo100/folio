//! The component host (SPEC §3, §3.6): which registered components a project
//! uses, how element and property names resolve to them, and their render
//! templates, parsed and checked.
//!
//! The core sees components only through the [`Registry`] of
//! `folio-component`; it never names a component's crate. A project uses
//! every registered first-party component it does not disable, and every
//! other registered component it declares in `[components]`.

use std::fmt::{self, Display, Formatter};
use std::ops::Range;

use ecow::{EcoString, eco_format};
use folio_component::{
    ComponentName, ElementSchema, Name, PropertyDecl, QualifiedName, Registered, Registry, Target,
    TemplateDecl,
};
use folio_syntax::ast::{self, AstNode, StyleItem};
use folio_syntax::{DiagSpanKind, Source, Span};

use crate::diag::Diagnostic;
use crate::project::{Bindings, ComponentSource, Components};

/// Errors for the components a project declares in `[components]` that the
/// compiler cannot host, at each one's key: only registered components can
/// be, and loading a component from a path or a published version is not
/// implemented yet (SPEC §3.6). A build fails, as for a locked version
/// that is not available (SPEC §2.4), rather than at the first use of one
/// of its names.
pub fn unhosted(registry: &Registry, components: &Components) -> Vec<Diagnostic> {
    components
        .declared
        .iter()
        .filter(|decl| registry.get(decl.name.v.as_str()).is_none())
        .map(|decl| {
            let from = match decl.source {
                ComponentSource::Path(_) => "from a path",
                ComponentSource::Version(_) => "by version",
            };
            Diagnostic::error(
                decl.name.span,
                eco_format!("component `{}` cannot be loaded", decl.name.v),
            )
            .with_hint(eco_format!(
                "loading components {from} is not implemented yet, so its elements and \
                 properties are not available"
            ))
        })
        .collect()
}

/// The components a project uses, with their templates parsed.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ComponentHost {
    components: Vec<Hosted>,
    bindings: Bindings,
}

/// A component in use.
#[derive(Debug, Clone, Eq, PartialEq)]
struct Hosted {
    registered: Registered,
    templates: Vec<Template>,
    stylesheet: Option<DefaultSheet>,
}

impl ComponentHost {
    /// Takes the registered components that a project's `[components]`
    /// enables, parses their templates, and resolves unqualified names with
    /// the bindings of the project's lockfile.
    ///
    /// Fails with every error in the components' templates. A component
    /// that registered cleanly but has a template that does not parse is a
    /// bug in that component.
    pub fn new(
        registry: &Registry,
        components: &Components,
        bindings: &Bindings,
    ) -> Result<Self, Vec<TemplateError>> {
        let mut errors = Vec::new();
        let mut hosted = Vec::new();
        for registered in registry.components() {
            if !components.is_enabled(registered.name().as_str()) {
                continue;
            }
            let namespace = registered.manifest.namespace();
            let mut templates = Vec::new();
            for decl in &registered.declarations.templates {
                match Template::parse(registered.name(), &namespace, decl) {
                    Ok(template) => templates.push(template),
                    Err(errs) => errors.extend(errs),
                }
            }
            let stylesheet = registered
                .declarations
                .stylesheet
                .as_ref()
                .map(|text| DefaultSheet::parse(registered.name(), text));
            hosted.push(Hosted {
                registered: registered.clone(),
                templates,
                stylesheet,
            });
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Self {
            components: hosted,
            bindings: bindings.clone(),
        })
    }

    /// The components in use, in registration order.
    pub fn components(&self) -> impl Iterator<Item = &Registered> {
        self.components.iter().map(|c| &c.registered)
    }

    /// The default stylesheets of the components in use, parsed, in
    /// registration order (SPEC §3.2).
    pub fn stylesheets(&self) -> impl Iterator<Item = (&Registered, &DefaultSheet)> {
        self.components
            .iter()
            .filter_map(|c| Some((&c.registered, c.stylesheet.as_ref()?)))
    }

    /// Resolves an element name, qualified (`exam.question`) or not
    /// (`question`).
    pub fn element(
        &self,
        name: &QualifiedName,
    ) -> Result<Resolved<'_, ElementSchema>, ResolveError> {
        self.resolve(name, Kind::Element, Registered::element, |b, n| {
            b.element(n)
        })
    }

    /// Resolves a stylesheet property name, qualified or not.
    pub fn property(
        &self,
        name: &QualifiedName,
    ) -> Result<Resolved<'_, PropertyDecl>, ResolveError> {
        self.resolve(name, Kind::Property, Registered::property, |b, n| {
            b.property(n)
        })
    }

    /// The template for an element: its default template, or the template
    /// with a name. The element is one a component provides, as resolved by
    /// [`Self::element`].
    pub fn template(
        &self,
        element: &Resolved<'_, ElementSchema>,
        name: Option<&str>,
    ) -> Option<&Template> {
        self.hosted(element.component.name())?
            .templates
            .iter()
            .find(|t| {
                t.decl.element == element.item.name
                    && t.decl.name.as_ref().map(Name::as_str) == name
            })
    }

    /// Every template of the components in use.
    pub fn templates(&self) -> impl Iterator<Item = &Template> {
        self.components.iter().flat_map(|c| &c.templates)
    }

    fn hosted(&self, name: &ComponentName) -> Option<&Hosted> {
        self.components.iter().find(|c| c.registered.name() == name)
    }

    fn resolve<'a, T>(
        &'a self,
        name: &QualifiedName,
        kind: Kind,
        lookup: impl Fn(&'a Registered, &str) -> Option<&'a T>,
        binding: impl Fn(&'a Bindings, &str) -> Option<&'a ComponentName>,
    ) -> Result<Resolved<'a, T>, ResolveError> {
        let error = |reason| ResolveError {
            kind,
            name: name.clone(),
            reason,
        };
        let plain = name.name.as_str();

        if let Some(namespace) = &name.namespace {
            let component = self
                .components()
                .find(|c| &c.manifest.namespace() == namespace)
                .ok_or_else(|| error(Reason::UnknownNamespace))?;
            let item = lookup(component, plain).ok_or_else(|| error(Reason::NotInNamespace))?;
            return Ok(Resolved {
                component,
                item,
                via: Via::Qualified,
                clashes: Vec::new(),
            });
        }

        let candidates: Vec<(&Registered, &T)> = self
            .components()
            .filter_map(|c| lookup(c, plain).map(|item| (c, item)))
            .collect();

        if let Some(bound) = binding(&self.bindings, plain) {
            let (component, item) = candidates
                .iter()
                .find(|(c, _)| c.name() == bound)
                .copied()
                .ok_or_else(|| error(Reason::StaleBinding(bound.clone())))?;
            let clashes = candidates
                .iter()
                .filter(|(c, _)| c.name() != bound)
                .map(|(c, _)| c.name().clone())
                .collect();
            return Ok(Resolved {
                component,
                item,
                via: Via::Lockfile,
                clashes,
            });
        }

        match candidates[..] {
            [] => Err(error(Reason::Unknown)),
            [(component, item)] => Ok(Resolved {
                component,
                item,
                via: Via::Unique,
                clashes: Vec::new(),
            }),
            _ => Err(error(Reason::Ambiguous(
                candidates.iter().map(|(c, _)| c.name().clone()).collect(),
            ))),
        }
    }
}

/// An element or property that a name resolved to.
#[derive(Debug)]
pub struct Resolved<'a, T> {
    /// The component that provides it.
    pub component: &'a Registered,
    /// Its declaration.
    pub item: &'a T,
    /// How the name resolved.
    pub via: Via,
    /// Other components in use that provide an element or property of the
    /// same unqualified name. When the name resolved through the lockfile,
    /// these are new clashes since the binding was recorded, which the core
    /// reports as a warning; the binding stays (SPEC §3.6).
    pub clashes: Vec<ComponentName>,
}

/// How a name resolved (SPEC §3.6).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Via {
    /// It was qualified with the component's namespace.
    Qualified,
    /// It was unqualified, and the lockfile binds it to the component.
    Lockfile,
    /// It was unqualified and not yet bound, and exactly one component in
    /// use provides it. Folio records the binding when it writes the
    /// lockfile.
    Unique,
}

/// What kind of name failed to resolve.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Kind {
    /// An element name.
    Element,
    /// A stylesheet property name.
    Property,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Element => "element",
            Self::Property => "property",
        }
    }
}

/// A name that did not resolve to a component's element or property.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ResolveError {
    /// What kind of name it is.
    pub kind: Kind,
    /// The name as written.
    pub name: QualifiedName,
    /// Why it did not resolve.
    pub reason: Reason,
}

/// Why a name did not resolve.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Reason {
    /// No component in use has the name's namespace.
    UnknownNamespace,
    /// The namespace's component has no element or property of that name.
    NotInNamespace,
    /// No component in use provides the unqualified name.
    Unknown,
    /// Several components in use provide the unqualified name, and the
    /// lockfile binds it to none of them.
    Ambiguous(Vec<ComponentName>),
    /// The lockfile binds the name to a component that is not in use or no
    /// longer provides it.
    StaleBinding(ComponentName),
}

impl Display for ResolveError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let kind = self.kind.as_str();
        let name = &self.name;
        match &self.reason {
            Reason::UnknownNamespace => {
                let namespace = name.namespace.as_ref().map_or("", Name::as_str);
                write!(
                    f,
                    "unknown {kind} `{name}`: no component in use has the namespace `{namespace}`"
                )
            }
            Reason::NotInNamespace => {
                let namespace = name.namespace.as_ref().map_or("", Name::as_str);
                write!(
                    f,
                    "unknown {kind} `{name}`: namespace `{namespace}` has no {kind} `{}`",
                    name.name
                )
            }
            Reason::Unknown => write!(f, "unknown {kind} `{name}`"),
            Reason::Ambiguous(components) => {
                write!(f, "{kind} `{name}` is ambiguous: ")?;
                for (i, component) in components.iter().enumerate() {
                    let sep = match i {
                        0 => "",
                        _ if i + 1 == components.len() => " and ",
                        _ => ", ",
                    };
                    write!(f, "{sep}`{component}`")?;
                }
                write!(f, " provide it; qualify it with a namespace")
            }
            Reason::StaleBinding(component) => write!(
                f,
                "{kind} `{name}` is bound to component `{component}` in folio.lock, \
                 but that component is not in use or no longer provides it"
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

/// A component's default stylesheet (SPEC §3.2), parsed when the component
/// is hosted, with what is wrong with it.
#[derive(Debug, Clone)]
pub struct DefaultSheet {
    /// The parsed stylesheet. Its spans are detached: it is in no project.
    pub source: Source,
    /// Its errors, each naming the component and, where it has one, the
    /// byte range in the sheet. A sheet with errors is left out of every
    /// cascade; a component that registered cleanly but has one is a bug
    /// in that component.
    pub errors: Vec<EcoString>,
}

/// Default sheets are equal when their text is: the rest is parsed from it.
impl PartialEq for DefaultSheet {
    fn eq(&self, other: &Self) -> bool {
        self.source.text() == other.source.text()
    }
}

impl Eq for DefaultSheet {}

impl DefaultSheet {
    fn parse(component: &ComponentName, text: &str) -> Self {
        let source = Source::detached_stylesheet(text);
        let error = |message: &str, range: Option<Range<usize>>| match range {
            Some(r) => eco_format!(
                "component `{component}`: default stylesheet, bytes {}..{}: {message}",
                r.start,
                r.end
            ),
            None => eco_format!("component `{component}`: default stylesheet: {message}"),
        };
        let (diagnostics, _) = source.root().errors_and_warnings();
        let mut errors: Vec<EcoString> = diagnostics
            .into_iter()
            .map(|diag| {
                let range = match diag.span.get() {
                    DiagSpanKind::Number { num, sub_range, .. } => source.range(num, sub_range),
                    DiagSpanKind::Range { range, .. } => Some(range),
                    DiagSpanKind::Detached => None,
                };
                error(&diag.message, range)
            })
            .collect();
        if let Some(sheet) = source.root().cast::<ast::Stylesheet<'_>>() {
            let mut found = Vec::new();
            check_items(sheet.items(), &mut found);
            for (span, message) in found {
                let range = source.find(span).map(|n| n.range());
                errors.push(error(&message, range));
            }
        }
        Self { source, errors }
    }
}

/// What the host rejects in a default stylesheet's items, inside overlay
/// blocks too: imports, and targets that do not exist. Variants a project
/// does not declare are allowed: their blocks do not match.
fn check_items<'a>(items: impl Iterator<Item = StyleItem<'a>>, out: &mut Vec<(Span, EcoString)>) {
    for item in items {
        match item {
            StyleItem::Import(import) => out.push((
                import.to_untyped().span(),
                "a component's default stylesheet cannot import another".into(),
            )),
            StyleItem::Overlay(overlay) => {
                for scope in overlay.scopes() {
                    if scope.kind() == "target" {
                        for name in scope.names() {
                            if Target::from_name(name.get()).is_none() {
                                out.push((
                                    name.span(),
                                    eco_format!("unknown target `{}`", name.get()),
                                ));
                            }
                        }
                    }
                }
                check_items(overlay.items(), out);
            }
            _ => {}
        }
    }
}

/// A component's render template, parsed as a stylesheet with one `render`
/// block (SPEC §5.3).
#[derive(Debug, Clone)]
pub struct Template {
    /// The component that provides the template.
    pub component: ComponentName,
    /// The declaration it was parsed from.
    pub decl: TemplateDecl,
    /// The parsed template: `render <namespace>.<element> [as <name>] {
    /// <parts> }`.
    pub source: Source,
}

/// Templates are equal when their declarations are: the parsed source is
/// made from the declaration and the component's namespace alone.
impl PartialEq for Template {
    fn eq(&self, other: &Self) -> bool {
        self.component == other.component && self.decl == other.decl
    }
}

impl Eq for Template {}

impl Template {
    /// The parsed `render` block.
    pub fn render_block(&self) -> ast::RenderBlock<'_> {
        self.source
            .root()
            .children()
            .find_map(ast::RenderBlock::from_untyped)
            .expect("a template holds one render block")
    }

    fn parse(
        component: &ComponentName,
        namespace: &Name,
        decl: &TemplateDecl,
    ) -> Result<Self, Vec<TemplateError>> {
        let as_name = decl
            .name
            .as_ref()
            .map(|name| eco_format!(" as {name}"))
            .unwrap_or_default();
        let header = format!("render {namespace}.{}{as_name} {{\n", decl.element);
        let text = format!("{header}{}\n}}\n", decl.parts);
        let source = Source::detached_stylesheet(text);
        let error = |message: EcoString, range: Option<Range<usize>>| TemplateError {
            component: component.clone(),
            element: decl.element.clone(),
            name: decl.name.clone(),
            message,
            range,
        };

        let (diagnostics, _) = source.root().errors_and_warnings();
        let errors: Vec<_> = diagnostics
            .into_iter()
            .map(|diag| {
                let range = match diag.span.get() {
                    DiagSpanKind::Number { num, sub_range, .. } => source.range(num, sub_range),
                    DiagSpanKind::Range { range, .. } => Some(range),
                    DiagSpanKind::Detached => None,
                };
                // Report positions within `parts`; errors in the generated
                // braces have none.
                let range = range.and_then(|r| {
                    let start = r.start.checked_sub(header.len())?;
                    let end = r.end.checked_sub(header.len())?;
                    (end <= decl.parts.len()).then_some(start..end)
                });
                error(diag.message, range)
            })
            .collect();
        if !errors.is_empty() {
            return Err(errors);
        }

        // Parts that close the block early would add items after it.
        let items: Vec<_> = source
            .root()
            .cast::<ast::Stylesheet<'_>>()
            .map(|sheet| sheet.items().collect())
            .unwrap_or_default();
        if !matches!(items[..], [StyleItem::Render(_)]) {
            return Err(vec![error(
                "a template's parts must be declarations, such as `body: slot(statement);`".into(),
                None,
            )]);
        }

        Ok(Self {
            component: component.clone(),
            decl: decl.clone(),
            source,
        })
    }
}

/// An error in a component's render template.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TemplateError {
    /// The component that provides the template.
    pub component: ComponentName,
    /// The element the template renders.
    pub element: Name,
    /// The template's name, or `None` for a default template.
    pub name: Option<Name>,
    /// What is wrong.
    pub message: EcoString,
    /// The byte range in the template's parts, if the error has one.
    pub range: Option<Range<usize>>,
}

impl Display for TemplateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "component `{}`: template for `{}`",
            self.component, self.element
        )?;
        if let Some(name) = &self.name {
            write!(f, " as `{name}`")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for TemplateError {}
