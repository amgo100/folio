//! The component registry: the components a compiler hosts, checked when
//! they are registered (SPEC §2.2, §3).

use std::fmt::{self, Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use ecow::{EcoString, EcoVec, eco_format};

use crate::aggregate::{AggregateDecl, LintReads};
use crate::component::{
    Component, ComponentManifest, Declarations, INTERFACE_VERSION, SUPPORTED_INTERFACES,
};
use crate::element::{BODY, Body, ElementSchema, LocaleString, UNIVERSAL_FIELDS};
use crate::instance::InstanceTree;
use crate::lint::LintDiagnostic;
use crate::names::{ComponentName, Name};
use crate::property::PropertyDecl;
use crate::template::TemplateDecl;
use crate::value::{FieldType, PropertyType, Value};

/// The components a compiler hosts, in registration order.
///
/// Registration checks each component's manifest and declarations, so the
/// core can rely on what it finds here: names are unique within a component,
/// defaults fit their types, and every template renders an element of its
/// own component. Which registered components a project uses, and how
/// unqualified names resolve, is the core's decision (SPEC §3.6).
#[derive(Debug, Clone, Default, Eq, PartialEq, Hash)]
pub struct Registry {
    components: Vec<Arc<Registered>>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks a component and adds it, or returns every problem found.
    ///
    /// The registry keeps the component, so that the core can call the
    /// extension points that run code, such as its lints.
    pub fn register(
        &mut self,
        component: impl Component + 'static,
    ) -> Result<(), Vec<RegistryError>> {
        let manifest = component.manifest();
        let declarations = component.declarations();
        let errors = self.check(&manifest, &declarations);
        if !errors.is_empty() {
            return Err(errors);
        }
        self.components.push(Arc::new(Registered {
            manifest,
            declarations,
            component: Arc::new(component),
        }));
        Ok(())
    }

    /// The registered components, in registration order.
    pub fn components(&self) -> impl ExactSizeIterator<Item = &Registered> {
        self.components.iter().map(Arc::as_ref)
    }

    /// The component with a name.
    pub fn get(&self, name: &str) -> Option<&Registered> {
        self.components().find(|c| c.manifest.name.as_str() == name)
    }

    /// The component with a namespace.
    pub fn by_namespace(&self, namespace: &str) -> Option<&Registered> {
        self.components()
            .find(|c| c.manifest.namespace().as_str() == namespace)
    }

    fn check(&self, manifest: &ComponentManifest, decls: &Declarations) -> Vec<RegistryError> {
        let mut cx = Checker {
            component: manifest.name.clone(),
            errors: Vec::new(),
        };
        check_manifest(&mut cx, manifest);
        if self.get(manifest.name.as_str()).is_some() {
            cx.error(eco_format!(
                "a component named `{}` is already registered",
                manifest.name
            ));
        } else if let Some(other) = self.by_namespace(manifest.namespace().as_str()) {
            cx.error(eco_format!(
                "namespace `{}` is already used by component `{}`",
                manifest.namespace(),
                other.manifest.name
            ));
        }
        check_locale_strings(&mut cx, &decls.locale_strings);
        check_elements(&mut cx, &decls.elements, &decls.locale_strings);
        check_properties(&mut cx, &decls.properties);
        check_templates(&mut cx, &decls.templates, &decls.elements);
        check_aggregates(&mut cx, decls);
        cx.errors
    }
}

/// A registered component: its manifest, its checked declarations, and the
/// component itself, for the extension points that run code.
///
/// Registered components are equal, and hash alike, when their manifests
/// and declarations are: a component's calls are pure (SPEC §3.5), and its
/// version changes whenever its code does, so the core may keep results
/// made by an equal component registered earlier.
#[derive(Clone)]
pub struct Registered {
    /// The component's manifest.
    pub manifest: ComponentManifest,
    /// What it adds through the extension points.
    pub declarations: Declarations,
    component: Arc<dyn Component>,
}

impl Debug for Registered {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registered")
            .field("manifest", &self.manifest)
            .field("declarations", &self.declarations)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Registered {
    fn eq(&self, other: &Self) -> bool {
        self.manifest == other.manifest && self.declarations == other.declarations
    }
}

impl Eq for Registered {}

impl Hash for Registered {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.manifest.hash(state);
        self.declarations.hash(state);
    }
}

impl Registered {
    /// Runs the component's lints over the instances of its elements
    /// ([`Component::lint`]).
    pub fn lint(&self, view: &InstanceTree) -> EcoVec<LintDiagnostic> {
        self.component.lint(view)
    }

    /// The value of one of the component's aggregates over the instances
    /// of its elements ([`Component::aggregate`]); `None` if it gives
    /// none.
    pub fn aggregate(&self, name: &str, view: &InstanceTree) -> Option<Value> {
        self.component.aggregate(name, view)
    }

    /// The aggregate with a name, within the component's namespace.
    pub fn aggregate_decl(&self, name: &str) -> Option<&AggregateDecl> {
        self.declarations
            .aggregates
            .iter()
            .find(|a| a.name.as_str() == name)
    }

    /// The component's name.
    pub fn name(&self) -> &ComponentName {
        &self.manifest.name
    }

    /// The element with a name, within the component's namespace.
    pub fn element(&self, name: &str) -> Option<&ElementSchema> {
        self.declarations
            .elements
            .iter()
            .find(|e| e.name.as_str() == name)
    }

    /// The property with a name, within the component's namespace.
    pub fn property(&self, name: &str) -> Option<&PropertyDecl> {
        self.declarations
            .properties
            .iter()
            .find(|p| p.name.as_str() == name)
    }

    /// The templates for an element: its default template and its named
    /// ones, in declaration order.
    pub fn templates_for<'a>(
        &'a self,
        element: &'a str,
    ) -> impl Iterator<Item = &'a TemplateDecl> + 'a {
        self.declarations
            .templates
            .iter()
            .filter(move |t| t.element.as_str() == element)
    }

    /// A word in a language, or `None` if the component has no string for
    /// that key in that language. Choosing a fallback language is the
    /// core's decision.
    pub fn locale_string(&self, lang: &str, key: &str) -> Option<&str> {
        self.declarations
            .locale_strings
            .iter()
            .find(|s| s.lang.eq_ignore_ascii_case(lang) && s.key.as_str() == key)
            .map(|s| s.text.as_str())
    }
}

/// A problem with a component's manifest or declarations, found when it is
/// registered.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct RegistryError {
    /// The component at fault.
    pub component: ComponentName,
    /// What is wrong.
    pub message: EcoString,
}

impl Display for RegistryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "component `{}`: {}", self.component, self.message)
    }
}

impl std::error::Error for RegistryError {}

/// Collects the errors of one component.
struct Checker {
    component: ComponentName,
    errors: Vec<RegistryError>,
}

impl Checker {
    fn error(&mut self, message: EcoString) {
        self.errors.push(RegistryError {
            component: self.component.clone(),
            message,
        });
    }
}

/// Reports every item that appears more than once in `items`, once each.
fn check_unique<T: PartialEq>(cx: &mut Checker, items: &[T], message: impl Fn(&T) -> EcoString) {
    for (i, item) in items.iter().enumerate() {
        let first = items.iter().position(|other| other == item) == Some(i);
        if first && items[i + 1..].contains(item) {
            cx.error(message(item));
        }
    }
}

fn check_manifest(cx: &mut Checker, manifest: &ComponentManifest) {
    if !SUPPORTED_INTERFACES.contains(&manifest.interface) {
        cx.error(eco_format!(
            "it targets interface version {}, but this compiler supports version {}",
            manifest.interface,
            INTERFACE_VERSION
        ));
    }
    let targets: Vec<_> = manifest.targets.iter().map(|t| t.as_str()).collect();
    check_unique(cx, &targets, |t| {
        eco_format!("target `{t}` is listed more than once")
    });
}

fn check_locale_strings(cx: &mut Checker, strings: &[LocaleString]) {
    for string in strings {
        if !is_language_tag(&string.lang) {
            cx.error(eco_format!(
                "locale string `{}` has an invalid language tag `{}`",
                string.key,
                string.lang
            ));
        }
    }
    let keys: Vec<_> = strings
        .iter()
        .map(|s| (s.lang.to_ascii_lowercase(), &s.key))
        .collect();
    check_unique(cx, &keys, |(lang, key)| {
        eco_format!("locale string `{key}` is declared more than once for `{lang}`")
    });
}

/// Whether `tag` looks like a BCP 47 language tag: a primary language of two
/// to eight letters, then subtags of one to eight letters or digits.
fn is_language_tag(tag: &str) -> bool {
    let mut subtags = tag.split('-');
    let primary = subtags.next().unwrap_or_default();
    (2..=8).contains(&primary.len())
        && primary.chars().all(|c| c.is_ascii_alphabetic())
        && subtags
            .all(|s| (1..=8).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn check_elements(cx: &mut Checker, elements: &[ElementSchema], strings: &[LocaleString]) {
    let names: Vec<_> = elements.iter().map(|e| &e.name).collect();
    check_unique(cx, &names, |n| {
        eco_format!("element `{n}` is declared more than once")
    });
    for element in elements {
        check_element(cx, element, strings);
    }
}

/// Whether a field type holds content inside an array or dictionary, at any
/// depth, which the core does not locate or style (SPEC §3.2).
fn holds_collected_content(ty: &FieldType) -> bool {
    fn holds(ty: &FieldType) -> bool {
        match ty {
            FieldType::Content => true,
            FieldType::Array(item) | FieldType::Dict(item) => holds(item),
            _ => false,
        }
    }
    matches!(ty, FieldType::Array(item) | FieldType::Dict(item) if holds(item))
}

fn check_element(cx: &mut Checker, element: &ElementSchema, strings: &[LocaleString]) {
    let name = &element.name;
    let fields: Vec<_> = element.fields.iter().map(|f| &f.name).collect();
    check_unique(cx, &fields, |f| {
        eco_format!("element `{name}` declares field `{f}` more than once")
    });

    for field in &element.fields {
        let field_name = &field.name;
        if UNIVERSAL_FIELDS.contains(&field_name.as_str()) {
            cx.error(eco_format!(
                "element `{name}` declares field `{field_name}`, which every element already has"
            ));
        }
        if field_name.as_str() == BODY {
            cx.error(eco_format!(
                "element `{name}` declares field `body`, which names the body"
            ));
        }
        if let FieldType::Choice(options) = &field.ty {
            if options.is_empty() {
                cx.error(eco_format!(
                    "field `{field_name}` of element `{name}` is a choice without options"
                ));
            }
            if options.iter().any(|o| o.is_empty()) {
                cx.error(eco_format!(
                    "field `{field_name}` of element `{name}` has an empty option"
                ));
            }
            check_unique(cx, options, |o| {
                eco_format!(
                    "field `{field_name}` of element `{name}` lists option {o:?} more than once"
                )
            });
        }
        if holds_collected_content(&field.ty) {
            cx.error(eco_format!(
                "field `{field_name}` of element `{name}` has type `{}`: arrays and dictionaries \
                 of content are not supported; hold several pieces of content as child elements \
                 of the body",
                field.ty
            ));
        }
        if field.ty.is_presentational() && !field.layout_data {
            cx.error(eco_format!(
                "field `{field_name}` of element `{name}` has type `{}`, which is formatting \
                 unless the field is marked as layout data",
                field.ty
            ));
        } else if field.layout_data && !field.ty.is_presentational() {
            cx.error(eco_format!(
                "field `{field_name}` of element `{name}` is marked as layout data, \
                 but only lengths and colours can be"
            ));
        }
        if let Some(default) = &field.default
            && !field.ty.accepts(default)
        {
            cx.error(eco_format!(
                "the default of field `{field_name}` of element `{name}` has type `{}`, \
                 but the field has type `{}`",
                default.type_name(),
                field.ty
            ));
        }
    }

    if let Body::Slots(slots) = &element.body {
        if slots.is_empty() {
            cx.error(eco_format!(
                "element `{name}` has a body of slots, but no slots"
            ));
        }
        let slot_names: Vec<_> = slots.iter().map(|s| &s.name).collect();
        check_unique(cx, &slot_names, |s| {
            eco_format!("element `{name}` declares slot `{s}` more than once")
        });
        for slot in slots {
            let slot = slot.name.as_str();
            if slot == BODY || UNIVERSAL_FIELDS.contains(&slot) || element.field(slot).is_some() {
                cx.error(eco_format!(
                    "slot `{slot}` of element `{name}` has the name of a field; \
                     an instance stores the slot's content under that name"
                ));
            }
        }
    }

    check_unique(cx, &element.positional, |p| {
        eco_format!("element `{name}` lists positional field `{p}` more than once")
    });
    for positional in &element.positional {
        let ok = match positional.as_str() {
            BODY => element.body != Body::None,
            field => element.field(field).is_some(),
        };
        if !ok {
            cx.error(eco_format!(
                "element `{name}` lists `{positional}` as positional, but has no such field"
            ));
        }
    }

    if let Some(key) = &element.ref_key
        && !strings
            .iter()
            .any(|s| &s.key == key && s.lang.eq_ignore_ascii_case("en"))
    {
        cx.error(eco_format!(
            "element `{name}` has the reference key `{key}`, but no English locale string for it"
        ));
    }
}

fn check_properties(cx: &mut Checker, properties: &[PropertyDecl]) {
    let names: Vec<_> = properties.iter().map(|p| &p.name).collect();
    check_unique(cx, &names, |n| {
        eco_format!("property `{n}` is declared more than once")
    });
    for property in properties {
        if has_empty_keywords(&property.ty) {
            cx.error(eco_format!(
                "property `{}` has a keyword type without keywords",
                property.name
            ));
        }
        if let Some(word) = reserved_keyword(&property.ty) {
            cx.error(eco_format!(
                "property `{}` lists `{word}` as a keyword, but stylesheets write it as a value; \
                 use its own type instead",
                property.name
            ));
        }
        if !property.ty.accepts(&property.initial) {
            cx.error(eco_format!(
                "the initial value of property `{}` has type `{}`, but the property takes `{}`",
                property.name,
                property.initial.type_name(),
                property.ty
            ));
        }
    }
}

fn has_empty_keywords(ty: &PropertyType) -> bool {
    match ty {
        PropertyType::Keyword(words) | PropertyType::Call(words) => words.is_empty(),
        PropertyType::List(item) => has_empty_keywords(item),
        PropertyType::AnyOf(types) => types.is_empty() || types.iter().any(has_empty_keywords),
        _ => false,
    }
}

/// Words that stylesheets read as values, so a keyword of that name could
/// never be written.
const RESERVED_WORDS: &[&str] = &["none", "auto", "true", "false"];

fn reserved_keyword(ty: &PropertyType) -> Option<&Name> {
    match ty {
        PropertyType::Keyword(words) => words.iter().find(|w| RESERVED_WORDS.contains(&w.as_str())),
        PropertyType::List(item) => reserved_keyword(item),
        PropertyType::AnyOf(types) => types.iter().find_map(reserved_keyword),
        _ => None,
    }
}

fn check_templates(cx: &mut Checker, templates: &[TemplateDecl], elements: &[ElementSchema]) {
    let ids: Vec<_> = templates.iter().map(template_id).collect();
    check_unique(cx, &ids, |id| {
        eco_format!("template `{id}` is declared more than once")
    });
    for (template, id) in templates.iter().zip(&ids) {
        if !elements.iter().any(|e| e.name == template.element) {
            cx.error(eco_format!(
                "template `{id}` renders `{}`, which is not an element of this component",
                template.element
            ));
        }
        if template.parts.trim().is_empty() {
            cx.error(eco_format!("template `{id}` has no parts"));
        }
    }
}

/// How a template is named in messages: `example` for a default template,
/// `example as compact` for a named one.
fn template_id(template: &TemplateDecl) -> EcoString {
    match &template.name {
        Some(name) => eco_format!("{} as {name}", template.element),
        None => eco_format!("{}", template.element),
    }
}

#[cfg(test)]
mod tests {
    use ecow::EcoVec;

    use super::*;
    use crate::element::{ContentKind, ElementKind, FieldDecl, SlotDecl};
    use crate::geom::Abs;
    use crate::names::Target;
    use crate::value::Value;

    fn name(text: &str) -> Name {
        Name::new(text).unwrap()
    }

    struct Test(ComponentManifest, Declarations);

    impl Component for Test {
        fn manifest(&self) -> ComponentManifest {
            self.0.clone()
        }

        fn declarations(&self) -> Declarations {
            self.1.clone()
        }
    }

    fn manifest(name: &str) -> ComponentManifest {
        ComponentManifest::new(
            ComponentName::new(name).unwrap(),
            semver::Version::new(0, 1, 0),
            [Target::Pdf],
        )
    }

    fn example() -> Declarations {
        let mut element = ElementSchema::new(name("example"), ElementKind::Block);
        element.fields = EcoVec::from([
            FieldDecl::with_default(name("title"), FieldType::Content, Value::None),
            FieldDecl::with_default(
                name("difficulty"),
                FieldType::Choice(EcoVec::from(["core".into(), "extension".into()])),
                Value::Str("core".into()),
            ),
        ]);
        element.body = Body::Slots(EcoVec::from([
            SlotDecl::new(name("statement"), ContentKind::Blocks),
            SlotDecl::new(name("solution"), ContentKind::Blocks),
        ]));
        element.ref_key = Some(name("example"));
        let mut declarations = Declarations::new();
        declarations.elements = EcoVec::from([element]);
        declarations.properties = EcoVec::from([PropertyDecl::new(
            name("example-gap"),
            PropertyType::Length,
            Value::Length(Abs::pt(6.0)),
        )]);
        declarations.templates = EcoVec::from([TemplateDecl::new(
            name("example"),
            "body: slot(statement) rule(0.4pt) slot(solution);",
        )]);
        declarations.locale_strings =
            EcoVec::from([LocaleString::new("en", name("example"), "Example")]);
        declarations
    }

    fn messages(result: Result<(), Vec<RegistryError>>) -> Vec<String> {
        result
            .err()
            .unwrap_or_default()
            .iter()
            .map(|e| e.message.to_string())
            .collect()
    }

    #[test]
    fn registers_and_looks_up() {
        let mut registry = Registry::new();
        registry
            .register(Test(manifest("@school/blocks"), example()))
            .unwrap();
        let component = registry.by_namespace("blocks").unwrap();
        assert_eq!(component.name().as_str(), "@school/blocks");
        assert_eq!(component.element("example").unwrap().fields.len(), 2);
        assert!(component.property("example-gap").is_some());
        assert_eq!(component.templates_for("example").count(), 1);
        assert_eq!(component.locale_string("EN", "example"), Some("Example"));
        assert_eq!(component.locale_string("pl", "example"), None);
    }

    #[test]
    fn rejects_clashing_components() {
        let mut registry = Registry::new();
        registry
            .register(Test(manifest("blocks"), example()))
            .unwrap();
        let again = registry.register(Test(manifest("blocks"), Declarations::default()));
        assert_eq!(
            messages(again),
            ["a component named `blocks` is already registered"]
        );
        let same_namespace =
            registry.register(Test(manifest("@school/blocks"), Declarations::default()));
        assert_eq!(
            messages(same_namespace),
            ["namespace `blocks` is already used by component `blocks`"]
        );
    }

    #[test]
    fn rejects_content_in_arrays_and_dictionaries() {
        let mut decls = example();
        let element = &mut decls.elements.make_mut()[0];
        element.fields.push(FieldDecl::with_default(
            name("options"),
            FieldType::Array(Box::new(FieldType::Content)),
            Value::None,
        ));
        element.fields.push(FieldDecl::with_default(
            name("cells"),
            FieldType::Dict(Box::new(FieldType::Array(Box::new(FieldType::Content)))),
            Value::None,
        ));
        element.fields.push(FieldDecl::with_default(
            name("tags"),
            FieldType::Array(Box::new(FieldType::Str)),
            Value::None,
        ));
        let result = Registry::new().register(Test(manifest("blocks"), decls));
        assert_eq!(
            messages(result),
            [
                "field `options` of element `example` has type `array(content)`: arrays and \
                 dictionaries of content are not supported; hold several pieces of content as \
                 child elements of the body",
                "field `cells` of element `example` has type `dict(array(content))`: arrays and \
                 dictionaries of content are not supported; hold several pieces of content as \
                 child elements of the body",
            ]
        );
    }

    #[test]
    fn rejects_bad_declarations() {
        let mut decls = example();
        let element = &mut decls.elements.make_mut()[0];
        element
            .fields
            .push(FieldDecl::required(name("class"), FieldType::Str));
        element
            .fields
            .push(FieldDecl::required(name("gap"), FieldType::Length));
        element.fields.push(FieldDecl {
            layout_data: true,
            ..FieldDecl::required(name("count"), FieldType::Int)
        });
        element.fields.push(FieldDecl::with_default(
            name("count"),
            FieldType::Int,
            Value::Bool(true),
        ));
        element.ref_key = Some(name("sample"));
        element
            .fields
            .push(FieldDecl::required(name("body"), FieldType::Content));
        element
            .fields
            .push(FieldDecl::required(name("statement"), FieldType::Content));
        element.positional = EcoVec::from([name("title"), name("caption"), name("title")]);
        element.fields.push(FieldDecl::with_default(
            name("level"),
            FieldType::Choice(EcoVec::from(["a".into(), "a".into(), "".into()])),
            Value::None,
        ));
        decls.properties.make_mut()[0].initial = Value::Int(1);
        decls.properties.push(PropertyDecl::new(
            name("example-fill"),
            PropertyType::Keyword(EcoVec::from([name("balance"), name("auto")])),
            Value::Keyword(name("balance")),
        ));
        decls
            .templates
            .push(TemplateDecl::named(name("figure"), name("wide"), " "));
        decls
            .locale_strings
            .push(LocaleString::new("EN", name("example"), "Sample"));
        let mut manifest = manifest("blocks");
        manifest.interface = 2;
        manifest.targets.push(Target::Pdf);

        let result = Registry::new().register(Test(manifest, decls));
        assert_eq!(
            messages(result),
            [
                "it targets interface version 2, but this compiler supports version 1",
                "target `pdf` is listed more than once",
                "locale string `example` is declared more than once for `en`",
                "element `example` declares field `count` more than once",
                "element `example` declares field `class`, which every element already has",
                "field `gap` of element `example` has type `length`, which is formatting \
                 unless the field is marked as layout data",
                "field `count` of element `example` is marked as layout data, \
                 but only lengths and colours can be",
                "the default of field `count` of element `example` has type `bool`, \
                 but the field has type `int`",
                "element `example` declares field `body`, which names the body",
                "field `level` of element `example` has an empty option",
                "field `level` of element `example` lists option \"a\" more than once",
                "slot `statement` of element `example` has the name of a field; \
                 an instance stores the slot's content under that name",
                "element `example` lists positional field `title` more than once",
                "element `example` lists `caption` as positional, but has no such field",
                "element `example` has the reference key `sample`, \
                 but no English locale string for it",
                "the initial value of property `example-gap` has type `int`, \
                 but the property takes `length`",
                "property `example-fill` lists `auto` as a keyword, but stylesheets write it \
                 as a value; use its own type instead",
                "template `figure as wide` renders `figure`, \
                 which is not an element of this component",
                "template `figure as wide` has no parts",
            ]
        );
    }
}

/// Checks the aggregates: each named once, and not like one of the
/// component's elements, whose names share the namespace; each reading
/// fields of the component's elements, each once. The fields lints read
/// are checked the same way.
fn check_aggregates(cx: &mut Checker, decls: &Declarations) {
    let names: Vec<&Name> = decls.aggregates.iter().map(|a| &a.name).collect();
    check_unique(cx, &names, |name| {
        eco_format!("aggregate `{name}` is declared twice")
    });
    let fields = |field: &Name| {
        decls
            .elements
            .iter()
            .any(|e| e.fields.iter().any(|f| f.name == *field))
    };
    for aggregate in &decls.aggregates {
        if decls.elements.iter().any(|e| e.name == aggregate.name) {
            cx.error(eco_format!(
                "aggregate `{}` has the name of one of the component's elements",
                aggregate.name
            ));
        }
        let reads: Vec<&Name> = aggregate.reads.iter().collect();
        check_unique(cx, &reads, |field| {
            eco_format!("aggregate `{}` reads `{field}` twice", aggregate.name)
        });
        for field in &aggregate.reads {
            if !fields(field) {
                cx.error(eco_format!(
                    "aggregate `{}` reads `{field}`, which no element of the component has",
                    aggregate.name
                ));
            }
        }
    }
    if let LintReads::Fields(reads) = &decls.lint_reads {
        for field in reads {
            if !fields(field) {
                cx.error(eco_format!(
                    "the lints read `{field}`, which no element of the component has"
                ));
            }
        }
    }
}
