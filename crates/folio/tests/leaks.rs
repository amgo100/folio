//! The leak check (SPEC §6.4): what a variant does not show reaches none
//! of its outputs, checked by provenance and by the text of the written
//! file. The unit tests of `folio::leaks` inject leaks into the writer's
//! input (alt text, bookmarks, metadata, boxes); these check the API on
//! whole builds, where text that a variant hides is written somewhere it
//! cannot be traced to its node.

use folio::leaks::{LEAK, LEAK_TEXT};
use folio::{Diagnostic, Name, Target, World};

const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                        compiler = \"0.0.0\"\nlang = \"en\"\n\
                        styles = [\"paper.fsty\"]\n\n\
                        [variants.exam]\ntargets = [\"pdf\"]\n\n\
                        [variants.markscheme]\ntargets = [\"pdf\"]\n";

const STYLE: &str = "figure.answer { display: none; }\n\
                     @variant markscheme { figure.answer { display: block; } }\n";

/// An answer of more than six words.
const ANSWER: &str = "The speed is nineteen point six metres per second";

fn world(main: &str) -> World {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("paper.fsty", STYLE).unwrap();
    world.set_source("main.fol", main).unwrap();
    world
}

fn leaks(world: &World, variant: &str) -> Vec<Diagnostic> {
    let variant = Name::new(variant).unwrap();
    folio::check_leaks(world, Target::Pdf, Some(&variant), None)
        .unwrap()
        .to_vec()
}

fn codes(found: &[Diagnostic]) -> Vec<&str> {
    found.iter().filter_map(|d| d.code.as_deref()).collect()
}

#[test]
fn a_paper_that_hides_its_answers_does_not_leak() {
    let world = world(&format!(
        "#doc(title: [Forces], subject: \"Kinematics\")\n\n= Question one\n\n\
         A ball is dropped from rest. Find its speed after two seconds.\n\n\
         #figure(class: \"answer\")[{ANSWER}.]\n\n\
         #if variant == \"markscheme\" [Award one mark for the equation and one for the value.]\n"
    ));
    assert_eq!(leaks(&world, "exam"), []);
    assert_eq!(leaks(&world, "markscheme"), []);
}

#[test]
fn hidden_text_in_the_metadata_is_caught() {
    // The subject repeats the answer that the exam hides.
    let world = world(&format!(
        "#doc(title: [Forces], subject: \"{ANSWER}\")\n\n\
         A ball is dropped from rest.\n\n#figure(class: \"answer\")[{ANSWER}.]\n"
    ));
    let found = leaks(&world, "exam");
    assert_eq!(codes(&found), [LEAK_TEXT], "{found:?}");
    assert!(found[0].is_error());
    assert!(found[0].message.contains("nineteen point six"), "{found:?}");
    // At the hidden answer.
    let main = world.source("main.fol").unwrap();
    let location = folio::diag::resolve(&world, &found[0]).location.unwrap();
    assert!(
        main.text()[location.bytes.clone()].starts_with("figure(class"),
        "{location:?}"
    );
    // The mark scheme shows it.
    assert_eq!(leaks(&world, "markscheme"), []);
}

#[test]
fn what_another_variant_shows_is_searched_for() {
    // The answer exists only in the mark scheme's tree, as evaluation
    // leaves it out of the exam's.
    let world = world(&format!(
        "#doc(title: [Forces], subject: \"{ANSWER}\")\n\nA ball is dropped from rest.\n\n\
         #if variant == \"markscheme\" [{ANSWER}.]\n"
    ));
    let found = leaks(&world, "exam");
    assert_eq!(codes(&found), [LEAK_TEXT], "{found:?}");
    assert_eq!(leaks(&world, "markscheme"), []);
}

#[test]
fn short_hidden_text_is_not_searched_for() {
    // Five words of fewer than 30 letters: too common to be a leak.
    let world = world(
        "#doc(title: [Forces], subject: \"It is 19.6 m/s\")\n\nA ball.\n\n\
         #figure(class: \"answer\")[It is 19.6 m/s.]\n",
    );
    assert_eq!(leaks(&world, "exam"), []);
}

#[test]
fn text_the_variant_also_shows_is_not_a_leak() {
    // The answer repeats six words of the question, which the exam shows.
    let world = world(
        "#doc(title: [Forces], subject: \"the speed of the ball after two seconds\")\n\n\
         Find the speed of the ball after two seconds.\n\n\
         #figure(class: \"answer\")[The speed of the ball after two seconds is 19.6 m/s.]\n",
    );
    assert_eq!(leaks(&world, "exam"), []);
}

#[test]
fn only_pdf_is_checked_for_now() {
    let world = world(&format!(
        "#doc(title: [Forces], subject: \"{ANSWER}\")\n\n#figure(class: \"answer\")[{ANSWER}.]\n"
    ));
    let variant = Name::new("exam").unwrap();
    for target in [Target::Docx, Target::Pptx, Target::Live] {
        let found = folio::check_leaks(&world, target, Some(&variant), None).unwrap();
        assert!(found.is_empty(), "{target:?}");
    }
    let _ = LEAK;
}
