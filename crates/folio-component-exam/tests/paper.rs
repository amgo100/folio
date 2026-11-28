//! A question paper and its mark scheme from one source (SPEC §13.1, the M2
//! exit criterion): built through `folio` with the first-party components,
//! numbered alike, with the same totals, the answers only in the mark
//! scheme, and the exam's lints.

use folio::{Fragment, FragmentItem, Name, Target, World};
use folio_component::Value;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
lang = "en-GB"
styles = ["styles/main.fsty"]

[variants.exam]
targets = ["pdf"]

[variants.markscheme]
targets = ["pdf"]

[numbering-groups.paper]
variants = ["exam", "markscheme"]
shared-counters = ["question", "part"]
"#;

/// The project's stylesheet: the mark scheme leaves out the question with
/// the class `exam-only`, which the shared counters still count.
const STYLES: &str = "@variant markscheme { question.exam-only { display: none; } }\n";

const PAPER: &str = r#"#doc(title: [Test])

Total: #exam.total-marks() marks.

#question[
  A ball is thrown upwards.
  #part(marks: 2)[
    Calculate the greatest height.
    #answer(lines: 3)[The height is 7.3 m. #mark("M1") #mark("A1")]
  ]
  #part(marks: 3)[
    Calculate the time of flight.
    #answer(lines: 4)[
      The time is 2.4 s. #mark("M1")
      #markscheme-note[Accept 2.5 s.]
    ]
  ]
] <q:ball>

#question(marks: 4, class: "exam-only")[
  A trolley is pushed.
  #part(marks: 4)[
    Find the force.
    #part(marks: 2)[Its size. #answer(lines: 2)[Size 0.75 N.]]
    #part(marks: 2)[Its direction. #answer(lines: 2)[Forwards.]]
  ]
]

#exam.question(marks: 1)[
  State Newton's third law.
  #exam.answer[Equal and opposite forces.]
]

See @q:ball.
"#;

fn world(manifest: &str, styles: &str, text: &str) -> World {
    let mut world = World::new(manifest).unwrap();
    world.set_source("styles/main.fsty", styles).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

/// The text of every paragraph a variant sets.
fn paragraphs(world: &World, variant: &str) -> Vec<String> {
    folio::text(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap()
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().to_string())
        .collect()
}

/// All the text a variant's pages show: what reaches its outputs, unlike
/// the text stage's paragraphs, which also hold answers measured for their
/// space and then dropped (SPEC §6.3).
fn shown(world: &World, variant: &str) -> String {
    fn walk(fragment: &Fragment, out: &mut String) {
        for (_, item) in &fragment.items {
            match item {
                FragmentItem::Group(group) => walk(group, out),
                FragmentItem::Text(run) => {
                    for glyph in &run.glyphs {
                        out.push_str(
                            &run.text[glyph.range.start as usize..glyph.range.end as usize],
                        );
                    }
                    out.push(' ');
                }
                _ => {}
            }
        }
    }
    let pages = folio::pages(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .unwrap();
    let mut out = String::new();
    for page in &pages.pages {
        walk(&page.fragment, &mut out);
    }
    out
}

/// The numbers of questions and parts a variant shows, in order.
fn numbers(paragraphs: &[String]) -> Vec<&str> {
    paragraphs
        .iter()
        .map(String::as_str)
        .filter(|p| {
            !p.is_empty()
                && (p.chars().all(|c| c.is_ascii_digit())
                    || (p.starts_with('(')
                        && p.ends_with(')')
                        && p[1..p.len() - 1].chars().all(|c| c.is_ascii_lowercase())))
        })
        .collect()
}

/// The diagnostics of a build.
fn diagnostics(world: &World, variant: &str) -> Vec<folio::Diagnostic> {
    folio::compile(world, Target::Pdf, Some(&name(variant)))
        .unwrap()
        .diagnostics
        .to_vec()
}

#[test]
fn exam_and_mark_scheme_number_alike_and_total_alike() {
    let world = world(MANIFEST, STYLES, PAPER);
    for variant in ["exam", "markscheme"] {
        let diagnostics = diagnostics(&world, variant);
        assert!(diagnostics.is_empty(), "{variant}: {diagnostics:?}");
    }
    let exam = paragraphs(&world, "exam");
    let scheme = paragraphs(&world, "markscheme");

    // Questions count through the paper and parts nest; the mark scheme
    // leaves out question 2 but numbers question 3 as the exam does.
    assert_eq!(
        numbers(&exam),
        ["1", "(a)", "(b)", "2", "(a)", "(i)", "(ii)", "3"]
    );
    assert_eq!(numbers(&scheme), ["1", "(a)", "(b)", "3"]);

    // The total counts each question's marks once: question 1's parts,
    // question 2's own marks and question 3's, from the group's common
    // tree in both.
    for paragraphs in [&exam, &scheme] {
        assert_eq!(paragraphs[1], "Total: 10 marks.");
        assert!(paragraphs.iter().any(|p| p == "See Question\u{a0}1."));
    }
    for variant in ["exam", "markscheme"] {
        let total = folio::aggregate(
            &world,
            Target::Pdf,
            Some(&name(variant)),
            "exam.total-marks",
        )
        .unwrap();
        assert_eq!(total, Some(Value::Int(10)), "{variant}");
    }

    // A part's marks stand beside it in both; a question's trailer shows
    // the marks it gives.
    for paragraphs in [&exam, &scheme] {
        assert!(paragraphs.iter().any(|p| p == "[2]"));
        assert!(paragraphs.iter().any(|p| p == "[3]"));
        assert!(
            paragraphs
                .iter()
                .any(|p| p == "(Total for Question 3: 1 mark)")
        );
    }

    // Answers, mark codes and notes are in the mark scheme only.
    let (exam, scheme) = (shown(&world, "exam"), shown(&world, "markscheme"));
    assert!(exam.contains("Calculate"), "{exam}");
    for text in ["7.3", "M1", "Accept", "Equal"] {
        assert!(!exam.contains(text), "the exam shows {text:?}: {exam}");
        assert!(
            scheme.contains(text),
            "the mark scheme lacks {text:?}: {scheme}"
        );
    }
}

#[test]
fn marks_that_do_not_add_up_are_a_warning() {
    let text = "#question(marks: 5)[\n  #part(marks: 2)[A.]\n  #part(marks: 2)[B.]\n]\n\n\
                #question(marks: 4)[\n  #part(marks: 2)[A.]\n  #part(marks: 2)[B.]\n]\n\n\
                #question[\n  #part(marks: 3)[\n    #part(marks: 1)[A.]\n    #part[B.]\n  ]\n]\n";
    let world = world(MANIFEST, "", text);
    let found: Vec<_> = diagnostics(&world, "exam")
        .into_iter()
        .filter(|d| d.code.as_deref() == Some("exam.marks-sum"))
        .collect();
    let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "the question has 5 marks, but its parts have 4 marks (field `marks`)",
            "the part has 3 marks, but its parts have 1 mark (field `marks`)",
        ]
    );
    assert!(
        found[0].hints.iter().any(|h| h.contains("`marks: 4`")),
        "{:?}",
        found[0]
    );
    assert_eq!(found[0].related.len(), 2);
    // The total takes a question's own marks over its parts'.
    let total =
        folio::aggregate(&world, Target::Pdf, Some(&name("exam")), "exam.total-marks").unwrap();
    assert_eq!(total, Some(Value::Int(12)));
}

#[test]
fn half_marks_add_up() {
    let text = "Total: #exam.total-marks().\n\n\
                #question(marks: 1.5)[\n  #part(marks: 0.5)[A.]\n  #part(marks: 1)[B.]\n]\n";
    let world = world(MANIFEST, "", text);
    assert!(diagnostics(&world, "exam").is_empty());
    assert_eq!(paragraphs(&world, "exam")[0], "Total: 1.5.");
}

#[test]
fn an_answer_with_too_many_lines_is_a_warning() {
    let text = "#question[#answer(lines: 400)[A.]]\n\n#question[#answer(lines: 4)[B.]]\n";
    let world = world(MANIFEST, "", text);
    let codes: Vec<_> = diagnostics(&world, "exam")
        .into_iter()
        .filter_map(|d| d.code)
        .collect();
    assert_eq!(codes, ["exam.answer-lines"]);
}

#[test]
fn a_project_can_disable_the_exam() {
    let manifest = format!("{MANIFEST}\n[components]\ndisable = [\"exam\"]\n");
    let world = world(&manifest, "", "#question[A.]\n");
    let diagnostics = diagnostics(&world, "exam");
    assert!(
        diagnostics.iter().any(|d| d.message.contains("question")),
        "{diagnostics:?}"
    );
}

#[test]
fn fractions_of_marks_show_rounded() {
    let text = "Total: #exam.total-marks().\n\n\
                #question(marks: 1)[\n  #part(marks: 0.1)[A.]\n  #part(marks: 0.2)[B.]\n]\n\n\
                #question[#part(marks: 0.1)[C.] #part(marks: 0.2)[D.]]\n";
    let world = world(MANIFEST, "", text);
    let messages: Vec<String> = diagnostics(&world, "exam")
        .into_iter()
        .map(|d| d.message.to_string())
        .collect();
    assert_eq!(
        messages,
        ["the question has 1 mark, but its parts have 0.3 marks (field `marks`)"]
    );
    assert_eq!(paragraphs(&world, "exam")[0], "Total: 1.3.");
}
