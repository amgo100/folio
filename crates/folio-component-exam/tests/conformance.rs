//! Conformance of the exam component with the component interface (SPEC
//! §3.5, §14.2): it registers cleanly, alone and among the first-party
//! components; its declarations and calls are deterministic; its default
//! stylesheet and templates load without errors; and a build on a target
//! it does not map falls back rather than failing.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use folio::{Name, Registry, Target, World, WorldBuilder};
use folio_component::{
    BODY, Component, Instance, InstanceTree, Location, Part, QualifiedName, SourceRef, Value,
};
use folio_component_exam::Exam;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn it_registers_alone_and_as_a_first_party_component() {
    let mut registry = Registry::new();
    registry.register(Exam::new()).unwrap();
    let first_party = WorldBuilder::first_party();
    let exam = first_party.get("exam").expect("the exam is first-party");
    assert_eq!(exam.manifest, Exam::new().manifest());
    // Versioned with the compiler (SPEC §2.2).
    assert_eq!(exam.manifest.version.to_string(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn its_declarations_and_calls_are_deterministic() {
    let (a, b) = (Exam::new(), Exam::new());
    assert_eq!(a.declarations(), b.declarations());
    assert_eq!(hash(&a.declarations()), hash(&b.declarations()));

    let at = |n| Location {
        source: SourceRef(n),
        instance: 0,
    };
    let instance = |element: &str, n, marks: Option<i64>, children: Vec<Instance>| {
        let mut out = Instance::new(QualifiedName::qualified(name("exam"), name(element)), at(n));
        if let Some(m) = marks {
            out.fields = [(name("marks"), Value::Int(m))].into_iter().collect();
        }
        out.parts = [Part::new(name(BODY), children)].into_iter().collect();
        out
    };
    let view = InstanceTree::new([
        instance(
            "question",
            1,
            Some(5),
            vec![
                instance("part", 2, Some(2), vec![]),
                instance("part", 3, Some(2), vec![]),
            ],
        ),
        instance(
            "question",
            4,
            None,
            vec![instance("part", 5, Some(3), vec![])],
        ),
    ]);
    assert_eq!(a.aggregate("total-marks", &view), Some(Value::Int(8)));
    assert_eq!(
        a.aggregate("total-marks", &view),
        b.aggregate("total-marks", &view)
    );
    assert_eq!(a.aggregate("unknown", &view), None);
    let lints = a.lint(&view);
    assert_eq!(lints, b.lint(&view));
    assert_eq!(lints.len(), 1);
    assert_eq!(lints[0].code.as_str(), "marks-sum");
    assert_eq!(lints[0].at, at(1));
}

/// Every element in content, every template chosen and the default
/// stylesheet in both variants, on PDF and on a target the component does
/// not map: no diagnostics, and no failure.
#[test]
fn its_stylesheet_and_templates_load_without_errors() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    styles = [\"styles/main.fsty\"]\n\n\
                    [variants.exam]\ntargets = [\"pdf\", \"docx\"]\n\n\
                    [variants.markscheme]\ntargets = [\"pdf\"]\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source(
            "styles/main.fsty",
            "@variant markscheme { question.table, part.table { template: markscheme-table; } }\n",
        )
        .unwrap();
    world
        .set_source(
            "main.fol",
            "#question(marks: 2)[Q.\n  #part(marks: 2, class: \"table\")[P.\n    \
             #answer(lines: 2)[A. #mark(\"B1\") #mark(code: \"B1\")]\n    \
             #markscheme-note[N.]\n  ]\n]\n\n\
             #question(marks: 1, class: \"table\")[Q. #answer[A.]]\n\n\
             Total #exam.total-marks().\n",
        )
        .unwrap();
    for variant in ["exam", "markscheme"] {
        let compiled = folio::compile(&world, Target::Pdf, Some(&name(variant))).unwrap();
        assert!(
            compiled.diagnostics.is_empty(),
            "{variant}: {:?}",
            compiled.diagnostics
        );
        assert!(compiled.output.is_some(), "{variant}");
    }
    // DOCX has no writer yet: the build checks the document and warns
    // about the writer, not about the component.
    let compiled = folio::compile(&world, Target::Docx, Some(&name("exam"))).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
}
