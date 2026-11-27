//! Page-number references (SPEC §2.3, §4.5): `#ref(<label>, form:
//! "page")` shows the page its target is on, found by a bounded fixpoint
//! of layout passes that converges in one extra pass for an ordinary
//! document, and freezes, deterministically and with a warning, for one
//! whose references move their own targets.

use folio::{Target, World};

fn manifest(lang: &str) -> String {
    format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         lang = \"{lang}\"\nstyles = [\"styles/main.fsty\"]\n"
    )
}

fn world(style: &str, text: &str) -> World {
    world_in("en", style, text)
}

fn world_in(lang: &str, style: &str, text: &str) -> World {
    let mut world = World::new(&manifest(lang)).unwrap();
    world.set_source("styles/main.fsty", style).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn paragraphs(world: &World, target: Target) -> Vec<String> {
    folio::text(world, target, None)
        .unwrap()
        .unwrap()
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().replace('\u{a0}', "~"))
        .collect()
}

const BREAKS: &str = "heading { break-before: page; }\n";

const DOC: &str = "= One <one>\n\nSee #ref(<three>, form: \"page\") and #ref(<one>, form: \"page\").\n\n= Two\n\nText.\n\n= Three <three>\n\nMore.\n";

#[test]
fn shows_the_page_of_its_target() {
    let world = world(BREAKS, DOC);
    assert_eq!(paragraphs(&world, Target::Pdf)[1], "See page~3 and page~1.");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
}

#[test]
fn entering_at_pages_gives_the_same() {
    let world = world(BREAKS, DOC);
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(pages.pages.len(), 3);
    assert_eq!(paragraphs(&world, Target::Pdf)[1], "See page~3 and page~1.");
}

#[test]
fn ordinary_converges_in_one_extra_pass() {
    let world = world(BREAKS, DOC);
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(pages.page_refs.pass, 1);
    assert!(!pages.page_refs.frozen);
}

#[test]
fn docx_measures_its_own_layout() {
    let world = world(BREAKS, DOC);
    assert_eq!(
        paragraphs(&world, Target::Docx)[1],
        "See page~3 and page~1."
    );
}

#[test]
fn incremental_matches_fresh() {
    let mut w = world(BREAKS, DOC);
    let _ = folio::compile(&w, Target::Pdf, None).unwrap();
    let edited = DOC.replace("= Two\n", "= Two\n\nInserted.\n\n= Two b\n");
    w.set_source("main.fol", &edited).unwrap();
    let a = folio::compile(&w, Target::Pdf, None).unwrap();
    let fresh = world(BREAKS, &edited);
    let b = folio::compile(&fresh, Target::Pdf, None).unwrap();
    assert_eq!(a.output, b.output);
    assert_eq!(paragraphs(&w, Target::Pdf)[1], "See page~4 and page~1.");
}

#[test]
fn polish_word_and_supplement() {
    let doc = "= Jeden <one>\n\nZob. #ref(<two>, form: \"page\"), #ref(<two>, form: \"page\", supplement: [str.]) i #ref(<two>, form: \"page\")[].\n\n= Dwa <two>\n\nTekst.\n";
    let world = world_in("pl", BREAKS, doc);
    assert_eq!(paragraphs(&world, Target::Pdf)[1], "Zob. s.~2, str.~2 i 2.");
}

#[test]
fn number_and_page_side_by_side() {
    let style = "heading { break-before: page; numbering: \"1\"; }\n";
    let doc = "= One\n\nSee @two on #ref(<two>, form: \"page\").\n\n= Two <two>\n\nText.\n";
    let world = world(style, doc);
    assert_eq!(
        paragraphs(&world, Target::Pdf)[1],
        "See Section~2 on page~2."
    );
}

#[test]
fn missing_label_warns_once() {
    let doc = "See #ref(<nowhere>, form: \"page\").\n";
    let world = world("", doc);
    assert_eq!(paragraphs(&world, Target::Pdf)[0], "See ??.");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let messages: Vec<_> = compiled
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("<nowhere>"), "{messages:?}");
}

#[test]
fn no_page_refs_no_passes() {
    let world = world(BREAKS, "= One <one>\n\nSee @one.\n");
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(pages.page_refs, Default::default());
}

const WORDS: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
    "lambda", "mu",
];

fn filler(n: usize, seed: usize) -> String {
    (0..n)
        .map(|i| WORDS[(i * 7 + seed) % WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

/// Small pages, and a section that starts on an even page.
const OSCILLATING_STYLE: &str = "heading.big { break-before: even-page; }\n\
     page { page-size: 80mm 40mm; margin-top: 5mm; margin-bottom: 5mm; \
     margin-inner: 5mm; margin-outer: 5mm; }\n";

/// A document whose two page-number references move each other's targets
/// for ever. `<t1>` is on page 9 or 10, after a paragraph that refers to
/// `<t2>`, and a page number of three digits there pushes `<t1>` to page
/// 10; `<t2>` is on page 99 or 100, after a paragraph that refers to
/// `<t1>`, and *??* or a number of two digits there pushes `<t2>` to page
/// 100, while a number of one digit leaves it on page 99. The section
/// between starts on page 12 either way. From no page known, the targets
/// go (9, 100), (10, 99), (9, 100), ... and never settle. The filler lengths were found by search: a change to
/// line or page breaking may make the document settle, and then they
/// need finding again.
fn oscillating() -> String {
    format!(
        "{}\n\n{} {} #ref(<t2>, form: \"page\")\n\n= One <t1>\n\n{}\n\n\
         #heading(class: \"big\")[B]\n\n{}\n\n{} {} #ref(<t1>, form: \"page\")\n\n\
         = Two <t2>\n\n{}\n",
        (0..5)
            .map(|i| filler(30, i))
            .collect::<Vec<_>>()
            .join("\n\n"),
        filler(3, 3),
        "x".repeat(12),
        filler(20, 2),
        (0..56)
            .map(|i| filler(30, i + 7))
            .collect::<Vec<_>>()
            .join("\n\n"),
        filler(57, 3),
        "x".repeat(13),
        filler(25, 2),
    )
}

#[test]
fn oscillating_references_freeze_with_a_warning() {
    let world = world(OSCILLATING_STYLE, &oscillating());
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    let numbers = &pages.page_refs;
    assert!(
        numbers.frozen,
        "the document no longer oscillates; find its filler lengths again: {numbers:?}"
    );
    assert_eq!(numbers.pass, 3, "three extra passes, then frozen");
    assert_eq!(numbers.unsettled.len(), 2);
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let warnings: Vec<String> = compiled
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .filter(|m| m.contains("did not settle"))
        .collect();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("<t2>") && warnings[1].contains("<t1>"));
}

#[test]
fn oscillating_output_is_deterministic() {
    let text = oscillating();
    // From a fresh world, entered at the PDF build, at the text stage and
    // at the DOCX build first.
    let first = world(OSCILLATING_STYLE, &text);
    let a = folio::compile(&first, Target::Pdf, None).unwrap();
    let second = world(OSCILLATING_STYLE, &text);
    let _ = folio::text(&second, Target::Pdf, None);
    let b = folio::compile(&second, Target::Pdf, None).unwrap();
    let third = world(OSCILLATING_STYLE, &text);
    let _ = folio::compile(&third, Target::Docx, None);
    let c = folio::compile(&third, Target::Pdf, None).unwrap();
    assert!(a.output.is_some());
    assert_eq!(a.output, b.output);
    assert_eq!(a.output, c.output);
    assert_eq!(a.diagnostics, b.diagnostics);
    // After edits that leave the document as it was, as from scratch.
    let mut edited = world(OSCILLATING_STYLE, &text);
    let _ = folio::compile(&edited, Target::Pdf, None).unwrap();
    edited
        .set_source("main.fol", &text.replace("= Two", "= Twofold"))
        .unwrap();
    let _ = folio::compile(&edited, Target::Pdf, None).unwrap();
    edited.set_source("main.fol", &text).unwrap();
    let d = folio::compile(&edited, Target::Pdf, None).unwrap();
    assert_eq!(a.output, d.output);
    assert_eq!(a.diagnostics, d.diagnostics);
}

#[test]
fn a_target_on_no_page_shows_unresolved_with_a_warning() {
    let doc = "= One\n\nA #emph[word] <word> here.\n\n= Two\n\nSee #ref(<word>, form: \"page\").\n";
    let world = world(BREAKS, doc);
    assert_eq!(paragraphs(&world, Target::Pdf)[3], "See page~??.");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let messages: Vec<String> = compiled
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect();
    assert_eq!(
        messages,
        ["`<word>` is on no page, so the reference has no page number to show"]
    );
}
