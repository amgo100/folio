//! Conformance of a test component with the M2 interface additions (SPEC
//! §3.2, #337): a default stylesheet, a lint over the instance view, and
//! the lint diagnostics it returns.

use ecow::EcoVec;
use folio_component::{
    BODY, Body, Component, ComponentManifest, ComponentName, ContentKind, Declarations,
    ElementKind, ElementSchema, FieldDecl, FieldType, Instance, InstanceTree, LintDiagnostic,
    Location, Name, Part, QualifiedName, Registry, Severity, SourceRef, Target, Value,
};

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

/// A small exam component: questions with parts, both with marks, a
/// default stylesheet, and the lint that a question's marks are the sum of
/// its parts' (SPEC §13.1).
struct Marks;

impl Component for Marks {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest::new(
            ComponentName::new("exam").unwrap(),
            semver::Version::new(0, 0, 0),
            [Target::Pdf],
        )
    }

    fn declarations(&self) -> Declarations {
        let element = |n: &str| {
            let mut schema = ElementSchema::new(name(n), ElementKind::Block);
            schema.fields = EcoVec::from([FieldDecl::required(name("marks"), FieldType::Int)]);
            schema.body = Body::Content(ContentKind::Blocks);
            schema
        };
        let mut declarations = Declarations::new();
        declarations.elements = EcoVec::from([element("question"), element("part")]);
        declarations.stylesheet = Some(
            "question { space-before: 1em; }\n\
             @variant exam { part { indent: 1em; } }\n"
                .into(),
        );
        declarations
    }

    fn lint(&self, view: &InstanceTree) -> EcoVec<LintDiagnostic> {
        let marks = |i: &Instance| match i.field("marks") {
            Some(Value::Int(m)) => *m,
            _ => 0,
        };
        let mut out = EcoVec::new();
        for question in view.iter().filter(|i| i.name.name.as_str() == "question") {
            let parts: Vec<&Instance> = question
                .children()
                .filter(|c| c.name.name.as_str() == "part")
                .collect();
            if parts.is_empty() {
                continue;
            }
            let sum: i64 = parts.iter().map(|p| marks(p)).sum();
            if sum != marks(question) {
                let mut diagnostic = LintDiagnostic::warning(
                    name("marks-sum"),
                    question.location,
                    format!(
                        "the question has {} marks, but its parts have {sum}",
                        marks(question)
                    ),
                )
                .with_field(name("marks"))
                .with_hint(format!("set `marks: {sum}`"));
                for part in parts {
                    diagnostic =
                        diagnostic.with_related(part.location, format!("{} marks", marks(part)));
                }
                out.push(diagnostic);
            }
        }
        out
    }
}

fn location(source: u64) -> Location {
    Location {
        source: SourceRef(source),
        instance: 0,
    }
}

fn instance(element: &str, source: u64, marks: i64, parts: Vec<Instance>) -> Instance {
    let mut out = Instance::new(
        QualifiedName::qualified(name("exam"), name(element)),
        location(source),
    );
    out.fields = EcoVec::from([(name("marks"), Value::Int(marks))]);
    out.parts = EcoVec::from([Part::new(name(BODY), parts)]);
    out
}

#[test]
fn a_component_with_a_default_stylesheet_registers() {
    let mut registry = Registry::new();
    registry.register(&Marks).unwrap();
    let registered = registry.components().next().unwrap();
    assert!(registered.declarations.stylesheet.is_some());
}

#[test]
fn a_lint_reads_the_instance_view() {
    let view = InstanceTree::new([
        // 3 = 1 + 2: no diagnostic.
        instance(
            "question",
            1,
            3,
            vec![
                instance("part", 2, 1, vec![]),
                instance("part", 3, 2, vec![]),
            ],
        ),
        // 5 ≠ 2 + 2.
        instance(
            "question",
            4,
            5,
            vec![
                instance("part", 5, 2, vec![]),
                instance("part", 6, 2, vec![]),
            ],
        ),
        // No parts: nothing to check.
        instance("question", 7, 4, vec![]),
    ]);

    // Document order: an instance before the instances in its parts.
    let order: Vec<u64> = view.iter().map(|i| i.location.source.0).collect();
    assert_eq!(order, [1, 2, 3, 4, 5, 6, 7]);
    assert!(view.roots[2].part(BODY).unwrap().empty);

    let diagnostics = Marks.lint(&view);
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.code.as_str(), "marks-sum");
    assert_eq!(diagnostic.severity, Severity::Warning);
    assert_eq!(diagnostic.at, location(4));
    assert_eq!(diagnostic.field, Some(name("marks")));
    assert_eq!(diagnostic.related.len(), 2);
    assert_eq!(diagnostic.related[0].at, location(5));
    assert_eq!(diagnostic.hints.as_slice(), ["set `marks: 4`"]);
}

#[test]
fn lints_default_to_none() {
    struct Plain;
    impl Component for Plain {
        fn manifest(&self) -> ComponentManifest {
            Marks.manifest()
        }
        fn declarations(&self) -> Declarations {
            Declarations::default()
        }
    }
    assert!(Plain.lint(&InstanceTree::default()).is_empty());
}
