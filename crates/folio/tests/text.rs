//! The text stage (SPEC §7.2) through the whole pipeline: paragraphs from
//! the styled document, quotation marks by language, fonts by fallback,
//! shaping, break opportunities and the Polish line-end rule, and
//! memoisation across edits.

use std::sync::Arc;

use folio::{BreakKind, ItemKind, Severity, Target, Text, TextParagraph, World, mark_breaks};

fn manifest(lang: &str) -> String {
    format!(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         lang = \"{lang}\"\nstyles = [\"styles/main.fsty\"]\n"
    )
}

/// There is no built-in stylesheet yet, so the inline elements are styled
/// here.
const SERIF: &str = "doc { font: \"Source Serif 4\", \"New Computer Modern\"; }\n\
     strong { font-weight: bold; }\n\
     emph { font-style: italic; }\n\
     raw { font: \"Source Code Pro\"; }\n";

fn world(lang: &str, style: &str, text: &str) -> World {
    let mut world = World::new(&manifest(lang)).unwrap();
    world.set_source("styles/main.fsty", style).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn text(world: &World) -> Text {
    folio::text(world, Target::Pdf, None).unwrap().unwrap()
}

fn paragraph_texts(text: &Text) -> Vec<String> {
    text.paragraphs
        .iter()
        .map(|p| p.shaped.text().to_string())
        .collect()
}

fn breaks(paragraph: &TextParagraph) -> String {
    mark_breaks(paragraph.shaped.text(), &paragraph.shaped.breaks)
}

/// The text of each face's segments, as `postscript name: text`.
fn faces(world: &World, paragraph: &TextParagraph) -> Vec<String> {
    let fonts = folio::fonts(world).unwrap();
    let shaped = &paragraph.shaped;
    shaped
        .paragraph
        .segments
        .iter()
        .zip(&paragraph.segments)
        .map(|(segment, style)| {
            let name = style
                .face
                .map_or("-".into(), |f| fonts.faces()[f].postscript_name.to_string());
            format!("{name}: {}", &shaped.text()[segment.range.clone()])
        })
        .collect()
}

#[test]
fn polish_text() {
    let world = world(
        "pl",
        "doc { font: \"Source Serif 4\"; hyphenate: true; }\nstrong { font-weight: bold; }\n",
        "Uczeń powiedział: \"To jest 'proste' zadanie\" i w *Warszawie* z kolegą o tym rozmawiał, \
         a potem poszedł do biało-czerwonego domu.\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let text = text(&world);
    assert_eq!(text.paragraphs.len(), 1);
    let paragraph = &text.paragraphs[0];
    assert_eq!(
        breaks(paragraph),
        "Uczeń |po-wie-dział: |„To |jest |‚pro-ste’ |za-da-nie” |i w War-sza-wie |\
         z ko-le-gą |o tym |roz-ma-wiał, |a po-tem |po-szedł |do |bia-ło-=czer-wo-ne-go |do-mu.¶"
    );
    // A single-letter word never ends a line, also before an emphasised
    // word, which is a segment of its own.
    let shaped = &paragraph.shaped;
    for b in shaped
        .breaks
        .iter()
        .filter(|b| b.kind != BreakKind::Mandatory)
    {
        let line = shaped.text()[..b.offset].trim_end();
        let last = line.rsplit(' ').next().unwrap();
        assert!(last.chars().count() > 1, "a line may end with `{last}`");
    }
    assert!(
        faces(&world, paragraph).contains(&"SourceSerif4-Bold: Warszawie".to_string()),
        "{:?}",
        faces(&world, paragraph)
    );
}

#[test]
fn english_quotation_marks_and_apostrophes() {
    let world = world(
        "en",
        SERIF,
        "\"Don't,\" she said, 'it's the students' book.'\n",
    );
    assert_eq!(
        paragraph_texts(&text(&world)),
        ["“Don’t,” she said, ‘it’s the students’ book.’"]
    );
    // German quotes for a German document.
    let world = self::world("de", SERIF, "Er sagte: \"Ja.\"\n");
    assert_eq!(paragraph_texts(&text(&world)), ["Er sagte: „Ja.“"]);
}

#[test]
fn fallback_and_bidi() {
    let world = world("en", SERIF, "Peace is שלום in Hebrew.\n");
    let text = text(&world);
    let paragraph = &text.paragraphs[0];
    assert_eq!(
        faces(&world, paragraph),
        [
            "SourceSerif4-Regular: Peace is ",
            // The space joins the Hebrew run before it, as in fallback.
            "NewCM10-Regular: שלום ",
            "SourceSerif4-Regular: in Hebrew."
        ]
    );
    let shaped = &paragraph.shaped;
    assert_eq!(shaped.level, 0);
    let levels: Vec<u8> = shaped.items.iter().map(|i| i.level).collect();
    // The space after the Hebrew word is between left-to-right text, so it
    // is at the paragraph's level, an item of its own.
    assert_eq!(levels, [0, 1, 0, 0]);
    assert!(
        shaped
            .items
            .iter()
            .all(|i| matches!(&i.kind, ItemKind::Glyphs { glyphs, .. } if !glyphs.is_empty()))
    );
}

#[test]
fn paragraphs_of_blocks_and_inline_elements() {
    let world = world(
        "en",
        SERIF,
        "= Forces\n\nA *bold* move, `raw text`, and $x^2$.\\\nNext line.\n\n\
         - First item\n- Second item\n\n\
         #figure[Body of the figure.][A caption]\n\n\
         ```\nlet x = 1;\nlet y = 2;\n```\n",
    );
    let text = text(&world);
    assert_eq!(
        paragraph_texts(&text),
        [
            "Forces",
            "A bold move, raw text, and \u{FFFC}.\u{2028}Next line.",
            "•\u{a0}First item",
            "•\u{a0}Second item",
            "Body of the figure.",
            "A caption",
            "let x = 1;\u{2028}let y = 2;",
        ]
    );
    let paragraph = &text.paragraphs[1];
    assert_eq!(
        faces(&world, paragraph),
        [
            "SourceSerif4-Regular: A ",
            "SourceSerif4-Bold: bold",
            "SourceSerif4-Regular:  move, ",
            "SourceCodePro-Regular: raw text",
            "SourceSerif4-Regular: , and ",
            "-: \u{FFFC}",
            "SourceSerif4-Regular: .\u{2028}Next line.",
        ]
    );
    assert!(
        paragraph
            .shaped
            .items
            .iter()
            .any(|i| i.kind == ItemKind::Object)
    );
    assert_eq!(
        breaks(paragraph),
        "A |bold |move, |raw |text, |and |\u{FFFC}.⏎¶Next |line.¶"
    );
    // The text of source nodes maps back to them.
    let world_text = paragraph.shaped.text();
    let sourced: Vec<&str> = paragraph
        .sources
        .iter()
        .map(|s| &world_text[s.range.clone()])
        .collect();
    assert!(sourced.contains(&"bold"), "{sourced:?}");
    assert!(sourced.contains(&"raw text"), "{sourced:?}");
    assert!(paragraph.sources.iter().all(|s| !s.span.is_detached()));
}

#[test]
fn features_and_languages_by_style() {
    let world = world(
        "en",
        "doc { font: \"Source Serif 4\"; }\n\
         strong { features: \"-liga\", \"ss01=2\"; }\n\
         emph { lang: \"pl\"; }\n",
        "office *office* _a w domu_\n",
    );
    let text = text(&world);
    let shaped = &text.paragraphs[0].shaped;
    let glyph_counts: Vec<usize> = shaped
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Glyphs { glyphs, .. } => Some(glyphs.len()),
            _ => None,
        })
        .collect();
    // `office ` with the `ffi` ligature, then `office` without it.
    assert_eq!(glyph_counts[..2], [5, 6]);
    let langs: Vec<&str> = shaped
        .paragraph
        .segments
        .iter()
        .map(|s| s.lang.as_ref().unwrap().as_str())
        .collect();
    assert_eq!(langs.last(), Some(&"pl"));
    // Polish rules apply to the Polish segment only. `hyphenate: auto`
    // hyphenates justified text, which `justify` is by default.
    assert_eq!(breaks(&text.paragraphs[0]), "of-fice |of-fice |a w do-mu¶");
}

#[test]
fn reports_values_text_cannot_use() {
    let world = world(
        "en",
        "doc { font: \"Source Serif 4\"; features: \"liga\", \"not-a-feature\"; }\n\
         emph { lang: \"english\"; }\n\
         strong { lang: \"xx\"; hyphenate: true; }\n",
        "Plain, _emphasised_ and *strong* text. More _emphasis_.\n",
    );
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let messages: Vec<(Severity, &str)> = compiled
        .diagnostics
        .iter()
        .map(|d| (d.severity, d.message.as_str()))
        .collect();
    assert_eq!(
        messages,
        [
            (
                Severity::Warning,
                "`not-a-feature` is not an OpenType feature setting"
            ),
            (Severity::Warning, "`english` is not a language tag"),
            (
                Severity::Warning,
                "there are no hyphenation patterns for the language `xx`"
            ),
        ]
    );
    assert!(compiled.diagnostics.iter().all(|d| !d.span.is_detached()));
}

#[test]
fn an_edit_reshapes_only_its_paragraph() {
    let mut world = world(
        "en",
        SERIF,
        "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.\n",
    );
    let before = text(&world);
    world
        .set_source(
            "main.fol",
            "First paragraph.\n\nSecond paragraph, edited.\n\nThird paragraph.\n",
        )
        .unwrap();
    let after = text(&world);
    let same: Vec<bool> = before
        .paragraphs
        .iter()
        .zip(&after.paragraphs)
        .map(|(a, b)| Arc::ptr_eq(&a.shaped, &b.shaped))
        .collect();
    assert_eq!(same, [true, false, true]);
    assert_eq!(
        after.paragraphs[1].shaped.text(),
        "Second paragraph, edited."
    );

    // Every variant and target of a world shares the shaped paragraphs.
    let pdf = text(&world);
    let docx = folio::text(&world, Target::Docx, None).unwrap().unwrap();
    assert!(Arc::ptr_eq(
        &pdf.paragraphs[0].shaped,
        &docx.paragraphs[0].shaped
    ));
}

/// Shaping runs in parallel inside the text query while `compile_all` runs
/// the builds in parallel: a thread waiting for shaping must not pick up
/// another build's compilation, whose database handle salsa would refuse.
/// Each variant sets its text in another font, so that every build has
/// paragraphs of its own to shape.
#[test]
fn compile_all_shapes_every_build_in_parallel() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
        [variants.a]\ntargets = [\"pdf\", \"docx\", \"pptx\"]\nstyles = [\"a.fsty\"]\n\
        [variants.b]\ntargets = [\"pdf\", \"docx\", \"pptx\"]\nstyles = [\"b.fsty\"]\n\
        [variants.c]\ntargets = [\"pdf\", \"docx\", \"pptx\"]\nstyles = [\"c.fsty\"]\n\
        [variants.d]\ntargets = [\"pdf\", \"docx\", \"pptx\"]\nstyles = [\"d.fsty\"]\n";
    let fonts = [
        ("a.fsty", "Source Serif 4"),
        ("b.fsty", "Source Sans 3"),
        ("c.fsty", "New Computer Modern"),
        ("d.fsty", "STIX Two Text"),
    ];
    for round in 0..5 {
        let mut world = World::new(manifest).unwrap();
        for (file, font) in fonts {
            world
                .set_source(file, &format!("doc {{ font: \"{font}\"; }}\n"))
                .unwrap();
        }
        let text: String = (0..400)
            .map(|i| format!("Paragraph {i} of round {round}, with some words to shape.\n\n"))
            .collect();
        world.set_source("main.fol", &text).unwrap();
        let outputs = folio::compile_all(&world);
        assert_eq!(outputs.len(), 12);
        for output in outputs {
            let compiled = output.result.unwrap();
            assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        }
    }
}

#[test]
fn fields_are_collected_in_the_order_they_are_written() {
    let world = world(
        "en",
        SERIF,
        "#doc(title: [Mechanics])\n\nFirst paragraph.\n\n#figure[Body.][Caption.]\n",
    );
    assert_eq!(
        paragraph_texts(&text(&world)),
        ["Mechanics", "First paragraph.", "Body.", "Caption."]
    );
}
