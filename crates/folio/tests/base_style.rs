//! The base stylesheet (SPEC §5.1): the lowest origin of every build's
//! cascade, which gives the built-in elements a readable look in a project
//! without stylesheets, and which every project stylesheet wins over.

use folio::style::{BASE_STYLESHEET, NodeName, Styled, StyledNode};
use folio::{Name, Source, Target, World};
use folio_component::{Abs, Value};

/// A document with the built-in elements that the base stylesheet styles.
const MAIN: &str = "\
#doc(title: [Forces], subject: \"Notes\")

= Newton's laws

A force is a *push* or a _pull_. Code such as `F = m a` is set apart.

== The first law

- an object at rest stays at rest;
- an object in motion stays in motion.

+ measure the mass;
+ measure the acceleration.

/ Mass: how much matter.

=== Inertia

#figure[A trolley on a track.][A trolley]

Two paragraphs.
";

fn world(manifest_styles: Option<&str>, sheet: &str) -> World {
    let mut manifest =
        String::from("[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n");
    if let Some(path) = manifest_styles {
        manifest.push_str(&format!("styles = [\"{path}\"]\n"));
    }
    let mut world = World::builder().build(&manifest, None).unwrap();
    world.set_source("main.fol", MAIN).unwrap();
    if let Some(path) = manifest_styles {
        world.set_source(path, sheet).unwrap();
    }
    world
}

fn styled(world: &World) -> Styled {
    folio::styles(world, Target::Pdf, None).unwrap().unwrap()
}

fn all<'a>(styled: &'a Styled, element: &str) -> Vec<&'a StyledNode> {
    styled
        .nodes()
        .into_iter()
        .filter(|n| match &n.name {
            NodeName::Element(qualified) => qualified.to_string() == element,
            NodeName::Part(part) => part.as_str() == element,
            NodeName::Page => element == "page",
        })
        .collect()
}

#[track_caller]
fn first<'a>(styled: &'a Styled, element: &str) -> &'a StyledNode {
    all(styled, element)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no `{element}`"))
}

fn get(styled: &Styled, node: &StyledNode, property: &str) -> Value {
    styled.value(node, property).cloned().unwrap_or(Value::None)
}

fn pt(value: f64) -> Value {
    Value::Length(Abs::pt(value))
}

fn kw(word: &str) -> Value {
    Value::Keyword(Name::new(word).unwrap())
}

#[test]
fn the_base_stylesheet_parses_cleanly() {
    let source = Source::detached_stylesheet(BASE_STYLESHEET);
    let (errors, warnings) = source.root().errors_and_warnings();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_project_without_stylesheets_builds_without_diagnostics() {
    let world = world(None, "");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
}

#[test]
fn built_in_elements_have_a_readable_look() {
    let world = world(None, "");
    let styled = styled(&world);

    // `strong` and `emph` stay semantic; the base stylesheet sets them.
    assert_eq!(
        get(&styled, first(&styled, "strong"), "font-weight"),
        kw("bold")
    );
    assert_eq!(
        get(&styled, first(&styled, "emph"), "font-style"),
        kw("italic")
    );

    // Headings are bold, sized by level (of the 11pt body), and kept with
    // what follows.
    let headings = all(&styled, "heading");
    assert_eq!(headings.len(), 3);
    for heading in &headings {
        assert_eq!(get(&styled, heading, "font-weight"), kw("bold"));
        assert_eq!(get(&styled, heading, "keep-with-next"), Value::Bool(true));
    }
    let size = |node| match get(&styled, node, "font-size") {
        Value::Length(size) => (size.to_pt() * 100.0).round() / 100.0,
        other => panic!("{other:?}"),
    };
    assert_eq!(size(headings[0]), 15.4);
    assert_eq!(size(headings[1]), 13.2);
    assert_eq!(size(headings[2]), 11.0);
    assert_eq!(size(first(&styled, "title")), 17.6);
    assert_eq!(
        get(&styled, first(&styled, "title"), "font-weight"),
        kw("bold")
    );

    // Paragraphs are spaced, list items closer together, lists indented.
    let paragraph = first(&styled, "paragraph");
    assert_eq!(get(&styled, paragraph, "space-before"), pt(5.5));
    assert_eq!(get(&styled, paragraph, "space-after"), pt(5.5));
    let item = first(&styled, "list-item");
    let in_item = item
        .children
        .iter()
        .find(|c| matches!(&c.name, NodeName::Element(q) if q.to_string() == "paragraph"))
        .expect("a list item holds a paragraph");
    assert_eq!(get(&styled, in_item, "space-before"), pt(2.2));
    assert_eq!(get(&styled, in_item, "space-after"), pt(2.2));
    for list in ["list", "enum", "terms"] {
        assert_eq!(
            get(&styled, first(&styled, list), "padding"),
            Value::Array([pt(0.0), pt(0.0), pt(0.0), pt(16.5)].into_iter().collect()),
            "{list}"
        );
    }
    assert_eq!(
        get(&styled, first(&styled, "term"), "font-weight"),
        kw("bold")
    );
    assert_eq!(
        get(&styled, first(&styled, "figure"), "space-before"),
        pt(11.0)
    );

    // Code is monospaced.
    let raw = first(&styled, "raw");
    assert_eq!(
        get(&styled, raw, "font"),
        Value::Array(
            [
                Value::Str("Source Code Pro".into()),
                Value::Str("New Computer Modern".into())
            ]
            .into_iter()
            .collect()
        )
    );
}

#[test]
fn project_stylesheets_win_whatever_their_specificity() {
    // A plain `paragraph` rule outranks the base sheet's `list-item >
    // paragraph`, and `strong` can be made regular again.
    let world = world(
        Some("styles/main.fsty"),
        "paragraph { space-before: 1pt; }\nstrong { font-weight: normal; }\n\
         heading { keep-with-next: false; }\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let styled = styled(&world);
    for paragraph in all(&styled, "paragraph") {
        assert_eq!(get(&styled, paragraph, "space-before"), pt(1.0));
        // What the project does not set still comes from the base sheet.
        assert_ne!(get(&styled, paragraph, "space-after"), pt(0.0));
    }
    assert_eq!(
        get(&styled, first(&styled, "strong"), "font-weight"),
        kw("normal")
    );
    assert_eq!(
        get(&styled, first(&styled, "heading"), "keep-with-next"),
        Value::Bool(false)
    );
}

#[test]
fn notes_take_the_documents_style_and_markers_their_anchors() {
    // A note looks the same wherever its anchor is (SPEC §7.8, D-327-1 =
    // A), and the marker at the anchor scales with it (D-327-3 = B).
    let mut world = World::builder()
        .build(
            "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n",
            None,
        )
        .unwrap();
    world
        .set_source(
            "main.fol",
            "#doc(title: [Forces#footnote[In the title.]])\n\n\
             = Laws#footnote[In a heading.]\n\n\
             Text.#footnote[In body text.]\n\n\
             #figure[Body#footnote[In a figure.]][Caption]\n",
        )
        .unwrap();
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let styled = styled(&world);
    let size = |node| match get(&styled, node, "font-size") {
        Value::Length(size) => (size.to_pt() * 100.0).round() / 100.0,
        other => panic!("{other:?}"),
    };
    let doc_align = get(&styled, &styled.root, "align");
    let notes = all(&styled, "footnote");
    assert_eq!(notes.len(), 4);
    let mut markers = Vec::new();
    for note in notes {
        markers.push(size(note));
        let part = note.note_style().expect("a footnote has a note part");
        assert_eq!(size(part), 7.7, "0.7 of the 11pt body");
        assert_eq!(get(&styled, part, "align"), doc_align);
        let paragraph = note
            .children
            .iter()
            .find(|c| matches!(&c.name, NodeName::Element(n) if n.to_string() == "paragraph"))
            .expect("a note holds a paragraph");
        assert_eq!(size(paragraph), 8.8, "0.8 of the 11pt body");
        assert_eq!(get(&styled, paragraph, "align"), doc_align);
        // Not centred like the figure a note is anchored in.
        assert_ne!(get(&styled, paragraph, "align"), kw("center"));
        assert_eq!(get(&styled, paragraph, "font-weight"), kw("normal"));
    }
    // The markers at the anchors: 0.7 of the title's, the heading's, the
    // body's and the figure's size.
    assert_eq!(markers, [12.32, 10.78, 7.7, 7.7]);
}

#[test]
fn notes_keep_the_language_of_their_anchors_and_the_font_of_another() {
    // A note anchored in a Polish passage of an English document is
    // Polish, in the passage's font, but sized like every other note
    // (SPEC §7.8, D-327-4 = B).
    let world = world(
        Some("styles/main.fsty"),
        ".pl { lang: \"pl\"; font: \"Source Serif 4\"; }\n",
    );
    let mut world = world;
    world
        .set_source(
            "main.fol",
            "English text.#footnote[An \"English\" note.]\n\n\
             #emph(class: \"pl\")[Polski tekst.#footnote[Zob. \"cytat\" i przypis.]]\n",
        )
        .unwrap();
    let styled = styled(&world);
    let notes: Vec<_> = all(&styled, "footnote")
        .into_iter()
        .map(|n| n.note_style().expect("a note part"))
        .collect();
    assert_eq!(
        get(&styled, notes[0], "lang"),
        get(&styled, &styled.root, "lang")
    );
    assert_eq!(get(&styled, notes[1], "lang"), Value::Str("pl".into()));
    assert_eq!(
        get(&styled, notes[1], "font"),
        get(&styled, first(&styled, "emph"), "font")
    );
    assert_ne!(
        get(&styled, notes[0], "font"),
        get(&styled, notes[1], "font")
    );
    // Size and style still come from the document.
    assert_eq!(
        get(&styled, notes[0], "font-size"),
        get(&styled, notes[1], "font-size")
    );
    assert_eq!(get(&styled, notes[1], "font-style"), kw("normal"));

    let text = folio::text(&world, Target::Pdf, None).unwrap().unwrap();
    let texts: Vec<String> = text
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().replace('\u{a0}', "~"))
        .collect();
    assert!(
        texts.iter().any(|t| t == "1~An “English” note."),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "2~Zob. „cytat” i przypis."),
        "{texts:?}"
    );
    // A note's continuation is marked in its own language.
    let marks: Vec<&str> = text
        .continued
        .iter()
        .map(|m| text.paragraphs[m.expect("a mark")].shaped.text())
        .collect();
    assert_eq!(marks, ["(cont.)", "(cd.)"]);
    // Polish hyphenation and line-end rules: the note's segments are
    // Polish.
    let note = text
        .paragraphs
        .iter()
        .find(|p| p.shaped.text().contains("cytat"))
        .unwrap();
    assert!(
        note.shaped
            .paragraph
            .segments
            .iter()
            .all(|s| s.lang.as_ref().map(|l| l.as_str()) == Some("pl"))
    );
}

#[test]
fn notes_take_the_anchors_font_only_in_another_language() {
    // A note in a sans heading of the document's language is set in the
    // document's font; one in a Polish passage in the passage's font
    // (SPEC §7.8, D-327-5 = B).
    let mut world = world(
        Some("styles/main.fsty"),
        "heading { font: \"Source Sans 3\"; }\n\
         .pl { lang: \"pl\"; font: \"Source Serif 4\"; }\n\
         .en { lang: \"en-GB\"; font: \"Source Sans 3\"; }\n",
    );
    world
        .set_source(
            "main.fol",
            "= Forces#footnote[In a sans heading.]\n\n\
             Text.#footnote[In body text.]\n\n\
             #emph(class: \"pl\")[Tekst.#footnote[W polskim fragmencie.]]\n\n\
             #emph(class: \"en\")[British.#footnote[In British English.]]\n",
        )
        .unwrap();
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let styled = styled(&world);
    let font = |node| get(&styled, node, "font");
    let notes: Vec<_> = all(&styled, "footnote")
        .into_iter()
        .map(|n| n.note_style().expect("a note part"))
        .collect();
    let document = font(&styled.root);
    assert_ne!(font(first(&styled, "heading")), document);
    assert_eq!(font(notes[0]), document, "a note in a heading");
    assert_eq!(font(notes[1]), document, "a note in body text");
    assert_eq!(
        font(notes[2]),
        font(all(&styled, "emph")[0]),
        "a note in a Polish passage"
    );
    assert_ne!(font(notes[2]), document);
    // English in another region is the same language: the document's font,
    // the passage's tag.
    assert_eq!(font(notes[3]), document, "a note in a British passage");
    assert_eq!(get(&styled, notes[3], "lang"), Value::Str("en-GB".into()));
}

#[test]
fn another_script_is_another_language_for_notes() {
    // `sr-Latn` in a `sr-Cyrl` document: the same language in another
    // script, whose font the note keeps.
    let mut world = world(
        Some("styles/main.fsty"),
        "doc { lang: \"sr-Cyrl\"; }\n.latn { lang: \"sr-Latn\"; font: \"Source Sans 3\"; }\n",
    );
    world
        .set_source(
            "main.fol",
            "Text.#footnote[A.]\n\n#emph(class: \"latn\")[Tekst.#footnote[B.]]\n",
        )
        .unwrap();
    let styled = styled(&world);
    let notes: Vec<_> = all(&styled, "footnote")
        .into_iter()
        .map(|n| n.note_style().expect("a note part"))
        .collect();
    let font = |node| get(&styled, node, "font");
    assert_eq!(font(notes[0]), font(&styled.root));
    assert_eq!(font(notes[1]), font(all(&styled, "emph")[0]));
    assert_ne!(font(notes[1]), font(&styled.root));
}
