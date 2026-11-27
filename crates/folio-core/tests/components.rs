//! The component host: test components register an element, a property and a
//! template through `folio-component`'s registry, and the core finds them
//! only through that registry, by qualified and unqualified names (SPEC §3,
//! §3.6).

use std::num::NonZeroU16;

use ecow::EcoVec;
use folio_component::{
    Abs, Body, Component, ComponentManifest, ComponentName, ContentKind, CounterDecl, Declarations,
    Effect, ElementKind, ElementSchema, FieldDecl, FieldType, INTERFACE_VERSION, LocaleString,
    Name, PropertyDecl, PropertyType, QualifiedName, Registry, SlotDecl, Target, TemplateDecl,
    Value,
};
use folio_core::component::{ComponentHost, Kind, Reason, Via, unhosted};
use folio_core::diag::Severity;
use folio_core::project::{Lockfile, Manifest};
use folio_syntax::DiagSpanKind;
use folio_syntax::FileId;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

fn qualified(text: &str) -> QualifiedName {
    QualifiedName::new(text).unwrap()
}

/// A component built from plain data, as a WebAssembly component would hand
/// it over.
struct TestComponent {
    name: &'static str,
    element: &'static str,
    template: &'static str,
}

impl Component for TestComponent {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest {
            name: ComponentName::new(self.name).unwrap(),
            version: semver::Version::new(0, 3, 2),
            interface: INTERFACE_VERSION,
            targets: EcoVec::from([Target::Pdf, Target::Docx]),
        }
    }

    fn declarations(&self) -> Declarations {
        let mut element = ElementSchema::new(name(self.element), ElementKind::Block);
        element.fields = EcoVec::from([FieldDecl::with_default(
            name("marks"),
            FieldType::Int,
            Value::Int(1),
        )]);
        element.body = Body::Slots(EcoVec::from([
            SlotDecl {
                name: name("stem"),
                content: ContentKind::Blocks,
            },
            SlotDecl {
                name: name("answer"),
                content: ContentKind::Blocks,
            },
        ]));
        element.counter = Some(CounterDecl::default());
        element.ref_key = Some(name(self.element));
        Declarations {
            elements: EcoVec::from([element]),
            properties: EcoVec::from([PropertyDecl {
                name: name("marks-gap"),
                ty: PropertyType::Length,
                initial: Value::Length(Abs::mm(4.0)),
                inherited: false,
                effect: Effect::Layout,
            }]),
            templates: EcoVec::from([TemplateDecl {
                element: name(self.element),
                name: None,
                parts: self.template.into(),
            }]),
            locale_strings: EcoVec::from([LocaleString {
                lang: "en".into(),
                key: name(self.element),
                text: "Question".into(),
            }]),
        }
    }
}

const EXAM: TestComponent = TestComponent {
    name: "exam",
    element: "question",
    template: "body: slot(stem) slot(answer);\ntrailer: \"[\" field(marks) \" marks]\";",
};

const CIRCUITS: TestComponent = TestComponent {
    name: "circuits",
    element: "question",
    template: "body: slot(stem);",
};

fn registry(components: &[&TestComponent]) -> Registry {
    let mut registry = Registry::new();
    for component in components {
        registry.register(*component).unwrap();
    }
    registry
}

fn manifest(components: &str) -> Manifest {
    let text = format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"1.2.0\"\n\n\
         [components]\n{components}"
    );
    Manifest::parse(&text, file()).unwrap()
}

fn lockfile(text: &str) -> Lockfile {
    Lockfile::parse(&format!("version = 1\n{text}"), file()).unwrap()
}

fn file() -> FileId {
    FileId::from_raw(NonZeroU16::new(1).unwrap())
}

#[test]
fn core_sees_a_component_through_the_registry() {
    let registry = registry(&[&EXAM]);
    let manifest = manifest("");
    let host = ComponentHost::new(&registry, &manifest.components, &Default::default()).unwrap();

    // The element, qualified and not.
    let question = host.element(&qualified("exam.question")).unwrap();
    assert_eq!(question.component.name().as_str(), "exam");
    assert_eq!(question.via, Via::Qualified);
    assert_eq!(
        question.item.field("marks").unwrap().default,
        Some(Value::Int(1))
    );
    let unqualified = host.element(&qualified("question")).unwrap();
    assert_eq!(unqualified.via, Via::Unique);
    assert_eq!(unqualified.item, question.item);

    // The property.
    let gap = host.property(&qualified("exam.marks-gap")).unwrap();
    assert_eq!(gap.item.initial, Value::Length(Abs::mm(4.0)));
    assert_eq!(gap.item.effect, Effect::Layout);

    // The template, parsed as a render block for the qualified element.
    let template = host.template(&question, None).unwrap();
    let block = template.render_block();
    let element: Vec<_> = block.element().map(|n| n.as_str().to_owned()).collect();
    assert_eq!(element, ["exam", "question"]);
    let parts: Vec<_> = block
        .parts()
        .map(|p| p.property().as_str().to_owned())
        .collect();
    assert_eq!(parts, ["body", "trailer"]);
    assert!(host.template(&question, Some("compact")).is_none());
    assert_eq!(host.templates().count(), 1);

    // The locale string.
    assert_eq!(
        question.component.locale_string("en", "question"),
        Some("Question")
    );
}

#[test]
fn unknown_names() {
    let registry = registry(&[&EXAM]);
    let host =
        ComponentHost::new(&registry, &manifest("").components, &Default::default()).unwrap();
    let reason = |text| host.element(&qualified(text)).unwrap_err().reason;
    assert_eq!(reason("plot.question"), Reason::UnknownNamespace);
    assert_eq!(reason("exam.part"), Reason::NotInNamespace);
    assert_eq!(
        host.element(&qualified("exam.part"))
            .unwrap_err()
            .to_string(),
        "unknown element `exam.part`: namespace `exam` has no element `part`"
    );
    assert_eq!(reason("part"), Reason::Unknown);
    let error = host.property(&qualified("columns")).unwrap_err();
    assert_eq!(error.kind, Kind::Property);
    assert_eq!(error.to_string(), "unknown property `columns`");
    assert_eq!(
        host.element(&qualified("plot.question"))
            .unwrap_err()
            .to_string(),
        "unknown element `plot.question`: no component in use has the namespace `plot`"
    );
}

#[test]
fn disabled_and_undeclared_components_are_not_in_use() {
    let registry = registry(&[&EXAM, &CIRCUITS]);
    let host = ComponentHost::new(
        &registry,
        &manifest("disable = [\"exam\"]\n").components,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(host.components().count(), 0);
    assert_eq!(
        host.element(&qualified("exam.question"))
            .unwrap_err()
            .reason,
        Reason::UnknownNamespace
    );
}

#[test]
fn qualified_names_always_work_and_bindings_decide_the_rest() {
    let registry = registry(&[&EXAM, &CIRCUITS]);
    let manifest = manifest("circuits = { version = \"0.3\" }\n");
    let components = &manifest.components;

    // Without a binding, the unqualified name is ambiguous, but qualified
    // names resolve.
    let host = ComponentHost::new(&registry, components, &Default::default()).unwrap();
    let error = host.element(&qualified("question")).unwrap_err();
    assert_eq!(
        error.reason,
        Reason::Ambiguous(vec![
            ComponentName::new("exam").unwrap(),
            ComponentName::new("circuits").unwrap(),
        ])
    );
    assert_eq!(
        error.to_string(),
        "element `question` is ambiguous: `exam` and `circuits` provide it; \
         qualify it with a namespace"
    );
    for text in ["exam.question", "circuits.question"] {
        let resolved = host.element(&qualified(text)).unwrap();
        assert_eq!(resolved.via, Via::Qualified);
        assert!(resolved.clashes.is_empty());
    }

    // The lockfile's binding decides, and the other provider is reported as
    // a clash.
    let lock = lockfile(
        "[[component]]\nname = \"circuits\"\nversion = \"0.3.2\"\n\
         hash = \"sha256:e5895263db42699c3f2fcc16368c140c48e05d2b7617880e0c78e5c994081e68\"\n\n\
         [bindings.elements]\nquestion = \"exam\"\n\n\
         [bindings.properties]\nmarks-gap = \"circuits\"\n",
    );
    let host = ComponentHost::new(&registry, components, &lock.bindings).unwrap();
    let question = host.element(&qualified("question")).unwrap();
    assert_eq!(question.component.name().as_str(), "exam");
    assert_eq!(question.via, Via::Lockfile);
    assert_eq!(question.clashes, [ComponentName::new("circuits").unwrap()]);
    let gap = host.property(&qualified("marks-gap")).unwrap();
    assert_eq!(gap.component.name().as_str(), "circuits");

    // A binding to a component that is no longer in use is an error.
    let manifest = self::manifest("circuits = { version = \"0.3\" }\ndisable = [\"exam\"]\n");
    let host = ComponentHost::new(&registry, &manifest.components, &lock.bindings).unwrap();
    assert_eq!(
        host.element(&qualified("question")).unwrap_err().reason,
        Reason::StaleBinding(ComponentName::new("exam").unwrap())
    );
}

#[test]
fn template_errors() {
    const BROKEN: TestComponent = TestComponent {
        name: "exam",
        element: "question",
        template: "body: slot(stem;",
    };
    const ESCAPING: TestComponent = TestComponent {
        name: "exam",
        element: "question",
        template: "body: slot(stem); } question { color: red;",
    };

    let manifest = manifest("");
    let errors = ComponentHost::new(
        &registry(&[&BROKEN]),
        &manifest.components,
        &Default::default(),
    )
    .unwrap_err();
    assert!(!errors.is_empty());
    let error = &errors[0];
    assert_eq!(error.element.as_str(), "question");
    let range = error.range.clone().expect("the error is inside the parts");
    assert!(range.end <= BROKEN.template.len(), "{range:?}");
    assert!(
        error
            .to_string()
            .starts_with("component `exam`: template for `question`: ")
    );

    let errors = ComponentHost::new(
        &registry(&[&ESCAPING]),
        &manifest.components,
        &Default::default(),
    )
    .unwrap_err();
    assert_eq!(
        errors[0].message,
        "a template's parts must be declarations, such as `body: slot(statement);`"
    );
}

/// A declared component the compiler cannot host is an error at its key,
/// not a silent gap until one of its names is used (#330).
#[test]
fn declared_components_that_cannot_be_hosted() {
    let registry = registry(&[&EXAM, &CIRCUITS]);
    let declared = "circuits = { version = \"0.3\" }\n\
                    \"@school/house\" = { path = \"../house-style\" }\n\
                    tables = { version = \"1\" }\n";
    let manifest = manifest(declared);
    let errors = unhosted(&registry, &manifest.components);
    let found: Vec<_> = errors
        .iter()
        .map(|d| {
            assert_eq!(d.severity, Severity::Error);
            assert!(matches!(d.span.get(), DiagSpanKind::Range { .. }));
            (d.message.as_str(), d.hints[0].as_str())
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                "component `@school/house` cannot be loaded",
                "loading components from a path is not implemented yet, so its elements and \
                 properties are not available"
            ),
            (
                "component `tables` cannot be loaded",
                "loading components by version is not implemented yet, so its elements and \
                 properties are not available"
            ),
        ]
    );
    assert!(unhosted(&registry, &self::manifest("").components).is_empty());
}
