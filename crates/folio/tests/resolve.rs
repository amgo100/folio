//! The Resolve stage (SPEC §2.1, §4.5) through the whole pipeline:
//! counters, numbering patterns, markers, cross-references and their
//! words in English and Polish, and the diagnostics of references that do
//! not resolve.

use folio::{Compiled, Severity, Target, World, diag};

fn manifest(lang: &str) -> String {
    format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         lang = \"{lang}\"\nstyles = [\"styles/main.fsty\"]\n"
    )
}

fn world(lang: &str, style: &str, text: &str) -> World {
    let mut world = World::new(&manifest(lang)).unwrap();
    world.set_source("styles/main.fsty", style).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

/// The text of every paragraph, with no-break spaces written `~`.
fn paragraphs(world: &World) -> Vec<String> {
    folio::text(world, Target::Pdf, None)
        .unwrap()
        .unwrap()
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().replace('\u{a0}', "~"))
        .collect()
}

fn compile(world: &World) -> Compiled {
    folio::compile(world, Target::Pdf, None).unwrap()
}

/// Each diagnostic's severity, message and the source text it points at.
fn located(world: &World, compiled: &Compiled) -> Vec<(Severity, String, String)> {
    compiled
        .diagnostics
        .iter()
        .map(|d| {
            let at = diag::locate(world, d.span).map_or(String::new(), |l| {
                let source = world.source(&l.path).unwrap();
                source.text()[l.bytes].to_string()
            });
            (d.severity, d.message.to_string(), at)
        })
        .collect()
}

fn messages(world: &World) -> Vec<String> {
    compile(world)
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect()
}

const NUMBERED: &str = "heading { numbering: \"1.1\"; }\n\
                        figure { numbering: \"1\"; }\n\
                        math { numbering: \"(1)\"; }\n";

#[test]
fn nested_headings_are_numbered_by_level() {
    let world = world(
        "en",
        "heading { numbering: \"1.\"; }\n",
        "= Kinematics\n\n== Speed\n\n== Acceleration\n\n=== Uniform\n\n\
         = Dynamics\n\n=== Skipped\n\n== Forces\n",
    );
    assert_eq!(
        paragraphs(&world),
        [
            "1.~Kinematics",
            "1.1.~Speed",
            "1.2.~Acceleration",
            "1.2.1.~Uniform",
            "2.~Dynamics",
            // A level that was skipped counts as 0.
            "2.0.1.~Skipped",
            "2.1.~Forces",
        ]
    );
    assert!(messages(&world).is_empty(), "{:?}", messages(&world));
}

#[test]
fn patterns_by_level() {
    let world = world(
        "en",
        "heading[level=1] { numbering: \"I.\"; }\n\
         heading[level=2] { numbering: \"1.a)\"; }\n",
        "= One\n\n== Part\n\n== Part\n\n= Two\n",
    );
    assert_eq!(
        paragraphs(&world),
        ["I.~One", "1.a)~Part", "1.b)~Part", "II.~Two"]
    );
}

#[test]
fn nothing_is_numbered_by_default() {
    // The base stylesheet numbers nothing (SPEC §5.1), and no warning.
    let world = world(
        "en",
        "",
        "= Forces\n\n+ First\n+ Second\n\n- Bullet\n\n#figure[Body][A caption.]\n",
    );
    assert_eq!(
        paragraphs(&world),
        ["Forces", "First", "Second", "Bullet", "Body", "A caption."]
    );
    assert!(messages(&world).is_empty(), "{:?}", messages(&world));
}

#[test]
fn enumerated_lists_and_their_markers() {
    let world = world(
        "en",
        "enum { numbering: \"1.\"; }\n\
         enum enum { numbering: \"(a)\"; }\n\
         list-item { marker: \"•\"; }\n",
        "+ First\n+ Second\n  + Inner\n  + Inner too\n+ Third\n\n\
         Between.\n\n\
         + Again\n\n\
         - Bullet\n",
    );
    assert_eq!(
        paragraphs(&world),
        [
            "1.~First",
            "2.~Second",
            "(a)~Inner",
            "(b)~Inner too",
            "3.~Third",
            "Between.",
            // A new list counts from 1.
            "1.~Again",
            "•~Bullet",
        ]
    );
}

#[test]
fn an_item_pattern_wins_over_its_list_and_markers_can_be_hidden() {
    let world = world(
        "en",
        "enum { numbering: \"1.\"; }\n\
         enum-item:nth(2) { numbering: \"i)\"; }\n\
         enum-item:nth(3) { marker: none; }\n",
        "+ One\n+ Two\n+ Three\n+ Four\n",
    );
    assert_eq!(
        paragraphs(&world),
        ["1.~One", "ii)~Two", "Three", "4.~Four"]
    );
}

#[test]
fn figure_captions_and_references_in_english() {
    let world = world(
        "en",
        NUMBERED,
        "= Motion <sec:motion>\n\n\
         #figure[Ball][Path of the ball.] <fig:ball>\n\n\
         #figure[Cliff][The cliff.] <fig:cliff>\n\n\
         See @fig:cliff and @fig:ball in @sec:motion.\n",
    );
    assert_eq!(
        paragraphs(&world),
        [
            "1~Motion",
            "Ball",
            "Figure~1: Path of the ball.",
            "Cliff",
            "Figure~2: The cliff.",
            "See Figure~2 and Figure~1 in Section~1.",
        ]
    );
    assert!(messages(&world).is_empty(), "{:?}", messages(&world));
}

#[test]
fn references_in_polish() {
    let world = world(
        "pl",
        NUMBERED,
        "= Ruch <sec:ruch>\n\n\
         #figure[Piłka][Tor piłki.] <fig:pilka>\n\n\
         $ v = u + a t <eq:v> $\n\n\
         Zob. @fig:pilka, @sec:ruch i @eq:v.\n",
    );
    assert_eq!(
        paragraphs(&world),
        [
            "1~Ruch",
            "Piłka",
            // A caption starts a sentence; references do not inflect.
            "Rys.~1: Tor piłki.",
            "Zob. rys.~1, rozdz.~1 i wzór~(1).",
        ]
    );
}

#[test]
fn the_language_of_the_reference_chooses_the_word() {
    // The word is in the language of the text the reference stands in.
    let world = world(
        "en",
        "figure { numbering: \"1\"; }\n.pl { lang: \"pl\"; }\n",
        "#figure[Ball][Path.] <fig:ball>\n\n\
         See @fig:ball.\n\n\
         #emph(class: \"pl\")[Zob. @fig:ball.]\n",
    );
    let texts = paragraphs(&world);
    assert_eq!(texts[2], "See Figure~1.");
    assert_eq!(texts[3], "Zob. rys.~1.");
}

#[test]
fn supplements_and_ref_format() {
    let world = world(
        "en",
        "heading { numbering: \"1.\"; ref-format: number; }\n\
         figure { numbering: \"1\"; }\n",
        "= Forces <forces>\n\n\
         #figure[Ball][Path.] <fig:ball>\n\n\
         See @fig:ball[Fig.], @fig:ball[] and section @forces.\n",
    );
    assert_eq!(
        paragraphs(&world).last().unwrap(),
        "See Fig.~1, 1 and section 1."
    );
}

#[test]
fn equations_are_numbered_once_or_by_labelled_line() {
    let world = world(
        "en",
        NUMBERED,
        "$ E = m c^2 $ <eq:energy>\n\n\
         $ v = u + a t <eq:v> \\ s = u t \\ v^2 = u^2 + 2 a s <eq:v2> $\n\n\
         $ x = 1 $\n\n\
         See @eq:energy, @eq:v and @eq:v2.\n",
    );
    // The first is 1; the labelled lines of the second are 2 and 3; the
    // third is 4. References trim the pattern's brackets.
    assert_eq!(
        paragraphs(&world).last().unwrap(),
        "See Equation~1, Equation~2 and Equation~3."
    );
}

#[test]
fn counters_within_and_reset() {
    let world = world(
        "en",
        "heading { numbering: \"1.\"; }\n\
         figure { numbering: \"1\"; }\n\
         heading[level=1] { counter-reset: figure; }\n",
        "= One\n\n#figure[A][First.]\n\n#figure[B][Second.]\n\n\
         = Two\n\n#figure[C][Third.]\n\n== Sub\n\n#figure[D][Fourth.]\n",
    );
    let captions: Vec<String> = paragraphs(&world)
        .into_iter()
        .filter(|t| t.starts_with("Figure"))
        .collect();
    assert_eq!(
        captions,
        [
            "Figure~1: First.",
            "Figure~2: Second.",
            "Figure~1: Third.",
            "Figure~2: Fourth.",
        ]
    );
}

#[test]
fn a_counter_reset_of_an_unknown_counter_is_reported() {
    let world = world(
        "en",
        "heading { counter-reset: figur; }\n",
        "= One\n\n= Two\n",
    );
    assert_eq!(
        messages(&world),
        ["there is no counter `figur`, so `counter-reset` does not restart it"]
    );
}

#[test]
fn numbering_without_a_counter_is_reported() {
    let world = world("en", "paragraph { numbering: \"1.\"; }\n", "One.\n\nTwo.\n");
    assert_eq!(
        messages(&world),
        ["`paragraph` has no counter, so its `numbering` has no effect"]
    );
    assert_eq!(paragraphs(&world), ["One.", "Two."]);
}

#[test]
fn references_that_do_not_resolve() {
    let world = world(
        "en",
        "",
        "= Forces <forces>\n\n== Speed <speed>\n\n_Note_ <note>\n\n\
         See @forces, @speed, @note and @force.\n",
    );
    let compiled = compile(&world);
    let warning = |message: &str, at: &str| (Severity::Warning, message.into(), at.into());
    assert_eq!(
        located(&world, &compiled),
        [
            warning(
                "`<note>` names an `emph`, which is not numbered, so the reference has no \
                 number to show",
                "@note"
            ),
            warning(
                "no element has the label `<force>`, so the reference has nothing to refer to",
                "@force"
            ),
        ]
    );
    assert_eq!(
        paragraphs(&world).last().unwrap(),
        "See Section~1, Section~1.1, ?? and ??."
    );
    // A heading without `numbering` still counts, so the reference to it
    // shows its number without a warning.
}

#[test]
fn references_resolve_in_every_target() {
    let world = world(
        "en",
        NUMBERED,
        "#figure[Ball][Path.] <fig:ball>\n\nSee @fig:ball.\n",
    );
    let docx = folio::compile(&world, Target::Docx, None).unwrap();
    assert!(
        docx.diagnostics
            .iter()
            .all(|d| !d.message.contains("label")),
        "{:?}",
        docx.diagnostics
    );
}

#[test]
fn an_edit_renumbers_and_rewrites_references() {
    let mut world = world(
        "en",
        NUMBERED,
        "#figure[A][First.] <fig:a>\n\n#figure[B][Second.] <fig:b>\n\nSee @fig:b.\n",
    );
    assert_eq!(paragraphs(&world).last().unwrap(), "See Figure~2.");
    world
        .set_source(
            "main.fol",
            "#figure[B][Second.] <fig:b>\n\n#figure[A][First.] <fig:a>\n\nSee @fig:b.\n",
        )
        .unwrap();
    assert_eq!(paragraphs(&world).last().unwrap(), "See Figure~1.");
}

#[test]
fn hanging_markers_are_not_laid_out_yet() {
    let world = world(
        "en",
        "enum { numbering: \"1.\"; }\n\
         enum-item { marker-position: hanging 8mm; }\n\
         list-item { marker-position: inside; }\n",
        "+ One\n\n- Two\n",
    );
    assert_eq!(
        messages(&world),
        ["`marker-position` is not implemented yet, so this declaration has no effect"]
    );
    // The marker stays in the line.
    assert_eq!(paragraphs(&world), ["1.~One", "Two"]);
}
