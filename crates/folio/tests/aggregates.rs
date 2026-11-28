//! Components' aggregates and lints (SPEC §3.2, §3.5): query functions and
//! checks over a view of their own element instances, memoised by the view,
//! which holds only the fields they declare they read.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ecow::EcoVec;
use folio::{Name, Registry, Target, World};
use folio_component::{
    AggregateDecl, Body, Component, ComponentManifest, ComponentName, ContentKind, Declarations,
    ElementKind, ElementSchema, FieldDecl, FieldType, InstanceTree, LintDiagnostic, LintReads,
    Value,
};

fn manifest(group: bool) -> String {
    let group = if group {
        "\n[numbering-groups.paper]\nvariants = [\"exam\", \"markscheme\"]\nshared-counters = []\n"
    } else {
        ""
    };
    format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         styles = [\"styles/main.fsty\"]\n\n\
         [variants.exam]\ntargets = [\"pdf\"]\n\n\
         [variants.markscheme]\ntargets = [\"pdf\"]\n{group}"
    )
}

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

/// How often the component's calls ran.
#[derive(Default)]
struct Calls {
    aggregate: AtomicUsize,
    lint: AtomicUsize,
}

/// A test component: questions with marks and a topic, an aggregate of
/// the total marks that reads only `marks`, and a lint that a question
/// has marks, reading only `marks`.
struct Quiz(Arc<Calls>);

impl Component for Quiz {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest::new(
            ComponentName::new("exam").unwrap(),
            semver::Version::new(0, 0, 0),
            [Target::Pdf],
        )
    }

    fn declarations(&self) -> Declarations {
        let mut question = ElementSchema::new(name("question"), ElementKind::Block);
        question.fields = EcoVec::from([
            FieldDecl::with_default(name("marks"), FieldType::Int, Value::Int(0)),
            FieldDecl::with_default(name("topic"), FieldType::Str, Value::Str("".into())),
        ]);
        question.body = Body::Content(ContentKind::Blocks);
        let mut declarations = Declarations::new();
        declarations.elements = EcoVec::from([question]);
        declarations.aggregates =
            EcoVec::from([AggregateDecl::new(name("total-marks")).reading([name("marks")])]);
        declarations.lint_reads = LintReads::Fields(EcoVec::from([name("marks")]));
        declarations
    }

    fn aggregate(&self, aggregate: &str, view: &InstanceTree) -> Option<Value> {
        self.0.aggregate.fetch_add(1, Ordering::SeqCst);
        if aggregate != "total-marks" {
            return None;
        }
        // The view holds only the fields the aggregate reads.
        assert!(view.iter().all(|q| q.field("topic").is_none()));
        let total = view
            .iter()
            .map(|q| match q.field("marks") {
                Some(Value::Int(m)) => *m,
                _ => 0,
            })
            .sum();
        Some(Value::Int(total))
    }

    fn lint(&self, view: &InstanceTree) -> EcoVec<LintDiagnostic> {
        self.0.lint.fetch_add(1, Ordering::SeqCst);
        view.iter()
            .filter(|q| matches!(q.field("marks"), Some(Value::Int(0))))
            .map(|q| {
                LintDiagnostic::warning(name("no-marks"), q.location, "the question has no marks")
            })
            .collect()
    }
}

fn world(group: bool, text: &str) -> (World, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let mut registry = Registry::new();
    registry.register(Quiz(calls.clone())).unwrap();
    let mut world = World::builder()
        .components(registry)
        .build(&manifest(group), None)
        .unwrap();
    world
        .set_source(
            "styles/main.fsty",
            ".scheme-only { display: none; }\n@variant markscheme { .scheme-only { display: auto; } }\n",
        )
        .unwrap();
    world.set_source("main.fol", text).unwrap();
    (world, calls)
}

fn total(world: &World, variant: &str) -> Option<Value> {
    folio::aggregate(world, Target::Pdf, Some(&name(variant)), "exam.total-marks").unwrap()
}

const PAPER: &str = "#exam.question(marks: 3, topic: \"forces\")[A.]\n\n\
                     #exam.question(marks: 4, class: \"scheme-only\")[B.]\n\n\
                     #exam.question(marks: 2)[C.]\n";

/// An aggregate counts what the variant shows, or, in a numbering group,
/// what the group's common tree holds, the same in every variant.
#[test]
fn an_aggregate_counts_the_variant_or_the_group() {
    let (alone, _) = world(false, PAPER);
    assert_eq!(total(&alone, "exam"), Some(Value::Int(5)));
    assert_eq!(total(&alone, "markscheme"), Some(Value::Int(9)));
    let (grouped, _) = world(true, PAPER);
    assert_eq!(total(&grouped, "exam"), Some(Value::Int(9)));
    assert_eq!(total(&grouped, "markscheme"), Some(Value::Int(9)));
    assert_eq!(
        folio::aggregate(&grouped, Target::Pdf, Some(&name("exam")), "exam.nothing").unwrap(),
        None
    );
}

/// The component runs again only when the instances or the fields it
/// reads change. Text elsewhere, and a field it does not read whose value
/// comes from a binding elsewhere, reuse its value and its lint
/// diagnostics; the marks it reads run it again. (An edit inside a
/// question's own markup renumbers its spans, so its location, and the
/// view, change.)
#[test]
fn aggregates_and_lints_run_again_only_for_what_they_read() {
    const SOURCE: &str = "#let topic = \"forces\"\n#let first = 3\n\n\
                          Introduction.\n\n\
                          #exam.question(marks: first, topic: topic)[A.]\n\n\
                          #exam.question(marks: 2)[C.]\n";
    let (mut world, calls) = world(false, SOURCE);
    let compile = |world: &World| {
        folio::compile(world, Target::Pdf, Some(&name("exam"))).unwrap();
        total(world, "exam")
    };
    let counts = || {
        (
            calls.aggregate.load(Ordering::SeqCst),
            calls.lint.load(Ordering::SeqCst),
        )
    };
    assert_eq!(compile(&world), Some(Value::Int(5)));
    assert_eq!(counts(), (1, 1));

    // Text outside the questions, and the topic, which neither reads.
    let edited = SOURCE
        .replace("Introduction.", "An introduction.")
        .replace("\"forces\"", "\"energy\"");
    world.set_source("main.fol", &edited).unwrap();
    assert_eq!(compile(&world), Some(Value::Int(5)));
    assert_eq!(counts(), (1, 1), "the aggregate and the lint were reused");

    // The marks they read.
    let edited = edited.replace("first = 3", "first = 6");
    world.set_source("main.fol", &edited).unwrap();
    assert_eq!(compile(&world), Some(Value::Int(8)));
    assert_eq!(counts(), (2, 2));
}

/// A lint's diagnostics carry its namespaced code.
#[test]
fn a_lint_reports_with_its_code() {
    let (world, _) = world(false, "#exam.question[No marks.]\n");
    let compiled = folio::compile(&world, Target::Pdf, Some(&name("exam"))).unwrap();
    let found: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.code.as_deref() == Some("exam.no-marks"))
        .collect();
    assert_eq!(found.len(), 1, "{:?}", compiled.diagnostics);
    assert_eq!(found[0].message, "the question has no marks");
}
