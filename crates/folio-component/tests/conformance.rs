//! Conformance of a test component with the M2 interface additions (SPEC
//! §3.2, #337, #72): a default stylesheet, a lint over the instance view,
//! the lint diagnostics it returns, and aggregates with the fields they
//! read.

use ecow::EcoVec;
use folio_component::{
    AggregateDecl, BODY, Body, Component, ComponentManifest, ComponentName, ContentKind,
    Declarations, ElementKind, ElementSchema, FieldDecl, FieldType, Instance, InstanceTree,
    LintDiagnostic, LintReads, Location, Name, Part, QualifiedName, Registry, Severity, SourceRef,
    Target, Value,
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
    registry.register(Marks).unwrap();
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

/// A component with the aggregate of total marks, reading `marks`.
struct Totals {
    aggregates: EcoVec<AggregateDecl>,
    lint_reads: LintReads,
}

impl Component for Totals {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest::new(
            ComponentName::new("exam").unwrap(),
            semver::Version::new(0, 0, 0),
            [Target::Pdf],
        )
    }

    fn declarations(&self) -> Declarations {
        let mut declarations = Marks.declarations();
        declarations.aggregates = self.aggregates.clone();
        declarations.lint_reads = self.lint_reads.clone();
        declarations
    }

    fn aggregate(&self, name: &str, view: &InstanceTree) -> Option<Value> {
        (name == "total-marks").then(|| {
            Value::Int(
                view.iter()
                    .filter(|i| i.name.name.as_str() == "question")
                    .map(|i| match i.field("marks") {
                        Some(Value::Int(m)) => *m,
                        _ => 0,
                    })
                    .sum(),
            )
        })
    }
}

fn totals(aggregates: impl IntoIterator<Item = AggregateDecl>) -> Totals {
    Totals {
        aggregates: aggregates.into_iter().collect(),
        lint_reads: LintReads::All,
    }
}

#[test]
fn an_aggregate_reads_the_instance_view() {
    let mut registry = Registry::new();
    registry
        .register(totals([
            AggregateDecl::new(name("total-marks")).reading([name("marks")])
        ]))
        .unwrap();
    let registered = registry.components().next().unwrap();
    let view = InstanceTree::new([
        instance("question", 1, 3, vec![instance("part", 2, 3, vec![])]),
        instance("question", 3, 4, vec![]),
    ]);
    assert_eq!(
        registered.aggregate("total-marks", &view),
        Some(Value::Int(7))
    );
    // The same view gives the same value (SPEC §3.5).
    assert_eq!(
        registered.aggregate("total-marks", &view),
        Some(Value::Int(7))
    );
    assert_eq!(registered.aggregate("unknown", &view), None);
    assert!(registered.aggregate_decl("total-marks").is_some());
}

#[test]
fn aggregates_default_to_none() {
    let mut registry = Registry::new();
    registry.register(Marks).unwrap();
    let registered = registry.components().next().unwrap();
    assert!(registered.declarations.aggregates.is_empty());
    assert_eq!(registered.declarations.lint_reads, LintReads::All);
    assert_eq!(
        registered.aggregate("total-marks", &InstanceTree::default()),
        None
    );
}

#[test]
fn aggregate_declarations_are_checked() {
    let errors = |component: Totals| {
        let mut registry = Registry::new();
        registry
            .register(component)
            .unwrap_err()
            .into_iter()
            .map(|e| e.message.to_string())
            .collect::<Vec<_>>()
    };
    let total = || AggregateDecl::new(name("total-marks")).reading([name("marks")]);
    assert_eq!(
        errors(totals([total(), total()])),
        ["aggregate `total-marks` is declared twice"]
    );
    assert_eq!(
        errors(totals([AggregateDecl::new(name("question"))])),
        ["aggregate `question` has the name of one of the component's elements"]
    );
    assert_eq!(
        errors(totals([
            AggregateDecl::new(name("total-marks")).reading([name("marks"), name("marks")])
        ])),
        ["aggregate `total-marks` reads `marks` twice"]
    );
    assert_eq!(
        errors(totals([
            AggregateDecl::new(name("total-marks")).reading([name("score")])
        ])),
        ["aggregate `total-marks` reads `score`, which no element of the component has"]
    );
    let mut lints = totals([]);
    lints.lint_reads = LintReads::Fields(EcoVec::from([name("score")]));
    assert_eq!(
        errors(lints),
        ["the lints read `score`, which no element of the component has"]
    );
    let mut twice = totals([]);
    twice.lint_reads = LintReads::Fields(EcoVec::from([name("marks"), name("marks")]));
    assert_eq!(errors(twice), ["the lints read `marks` twice"]);
    // The universal fields are every element's.
    let mut ids = totals([AggregateDecl::new(name("total-marks")).reading([name("id")])]);
    ids.lint_reads = LintReads::Fields(EcoVec::from([name("class")]));
    Registry::new().register(ids).unwrap();
}
