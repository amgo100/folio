//! The style stage (SPEC §5.1, §5.2): selector matching, the cascade,
//! inheritance, tokens and values, computed property groups, and
//! incremental restyling.

use std::sync::{Arc, Mutex};

use ecow::EcoVec;
use folio::style::{NodeName, Styled, StyledNode};
use folio::{Event, Name, Registry, Target, World};
use folio_component::{
    Abs, Body, Color, Component, ComponentManifest, ComponentName, ContentKind, Declarations,
    Effect, ElementKind, ElementSchema, FieldDecl, FieldType, INTERFACE_VERSION, PropertyDecl,
    PropertyType, SlotDecl, Value,
};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
styles = ["styles/main.fsty"]

[variants.exam]
targets = ["pdf", "pptx"]

[variants.markscheme]
targets = ["pdf"]
styles = ["styles/markscheme.fsty"]

[formatting]
overrides-sheet = "styles/overrides.fsty"
"#;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

/// A stand-in for the exam component: an element with slots, and a
/// property of its own.
struct Exam;

impl Component for Exam {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest {
            name: ComponentName::new("exam").unwrap(),
            version: semver::Version::new(0, 0, 0),
            interface: INTERFACE_VERSION,
            targets: EcoVec::from([Target::Pdf]),
        }
    }

    fn declarations(&self) -> Declarations {
        let mut question = ElementSchema::new(name("question"), ElementKind::Block);
        question.fields = EcoVec::from([FieldDecl::required(name("marks"), FieldType::Int)]);
        question.body = Body::Content(ContentKind::Blocks);
        let mut example = ElementSchema::new(name("example"), ElementKind::Block);
        example.body = Body::Slots(EcoVec::from([
            SlotDecl {
                name: name("statement"),
                content: ContentKind::Blocks,
            },
            SlotDecl {
                name: name("solution"),
                content: ContentKind::Blocks,
            },
        ]));
        Declarations {
            elements: EcoVec::from([question, example]),
            properties: EcoVec::from([PropertyDecl {
                name: name("mark-gap"),
                ty: PropertyType::Length,
                initial: Value::Length(Abs::pt(4.0)),
                inherited: true,
                effect: Effect::Layout,
            }]),
            ..Declarations::default()
        }
    }
}

/// A world with the exam component, a content file and stylesheets.
fn world(main: &str, sheets: &[(&str, &str)]) -> World {
    let mut registry = Registry::new();
    registry.register(&Exam).unwrap();
    let mut world = World::builder()
        .components(registry)
        .build(MANIFEST, None)
        .unwrap();
    world.set_source("main.fol", main).unwrap();
    for path in [
        "styles/main.fsty",
        "styles/markscheme.fsty",
        "styles/overrides.fsty",
    ] {
        world.set_source(path, "").unwrap();
    }
    for (path, text) in sheets {
        world.set_source(path, text).unwrap();
    }
    world
}

/// The styles of a build, which must have no errors.
#[track_caller]
fn styled(world: &World, variant: &str, target: Target) -> Styled {
    let variant = name(variant);
    let compiled = folio::compile(world, target, Some(&variant)).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    folio::styles(world, target, Some(&variant))
        .unwrap()
        .unwrap()
}

/// The diagnostics of the exam build, as messages.
fn messages(world: &World) -> Vec<String> {
    folio::compile(world, Target::Pdf, Some(&name("exam")))
        .unwrap()
        .diagnostics
        .iter()
        .map(|d| d.message.to_string())
        .collect()
}

/// The nodes with a name, in document order.
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

fn pt(value: f64) -> Value {
    Value::Length(Abs::pt(value))
}

fn kw(word: &str) -> Value {
    Value::Keyword(name(word))
}

fn get(styled: &Styled, node: &StyledNode, property: &str) -> Value {
    styled.value(node, property).cloned().unwrap_or(Value::None)
}

#[test]
fn specificity_then_source_order() {
    let world = world(
        "= Forces\n\n#heading(level: 2, class: \"hard\")[Motion] <sec:motion>\n",
        &[(
            "styles/main.fsty",
            "heading.hard { space-before: 3pt; }\n\
             heading { space-before: 1pt; }\n\
             heading[level=2] { space-after: 5pt; }\n\
             heading { space-after: 2pt; }\n\
             #{sec:motion} { indent: 7pt; }\n\
             heading.hard { indent: 6pt; }\n\
             heading { align: center; }\n\
             heading { align: end; }\n",
        )],
    );
    let styled = styled(&world, "exam", Target::Pdf);
    let headings = all(&styled, "heading");
    let (h1, h2) = (headings[0], headings[1]);
    // A class beats an element name written later.
    assert_eq!(get(&styled, h2, "space-before"), pt(3.0));
    assert_eq!(get(&styled, h1, "space-before"), pt(1.0));
    // An attribute test beats it too.
    assert_eq!(get(&styled, h2, "space-after"), pt(5.0));
    assert_eq!(get(&styled, h1, "space-after"), pt(2.0));
    // An id beats a class.
    assert_eq!(get(&styled, h2, "indent"), pt(7.0));
    // Equal specificity: the later rule wins.
    assert_eq!(get(&styled, h1, "align"), kw("end"));
}

#[test]
fn origins_outrank_specificity() {
    let sheets = [
        (
            "styles/main.fsty",
            "@import \"theme.fsty\";\n\
             paragraph { space-before: 2pt; space-after: 2pt; indent: 2pt; }\n\
             @target pptx { paragraph { space-after: 4pt; } }\n",
        ),
        (
            "styles/theme.fsty",
            "#{p} { space-before: 1pt; } paragraph.x { leading: 1pt; }\n",
        ),
        (
            "styles/markscheme.fsty",
            "paragraph { space-before: 3pt; }\n",
        ),
        ("styles/overrides.fsty", "paragraph { indent: 5pt; }\n"),
    ];
    let world = world(
        "#rule paragraph { leading: 9pt; }\nSome text.\n\n#figure[] <p>\n",
        &sheets,
    );

    // The theme's id rule loses to the project's element rule.
    let exam = styled(&world, "exam", Target::Pdf);
    let p = first(&exam, "paragraph");
    assert_eq!(get(&exam, p, "space-before"), pt(2.0));
    assert_eq!(get(&exam, p, "space-after"), pt(2.0));
    // The overrides sheet beats the project; local overrides beat both.
    assert_eq!(get(&exam, p, "indent"), pt(5.0));
    assert_eq!(get(&exam, p, "leading"), pt(9.0));

    // The variant's overlay beats the project, and a target overlay
    // applies to its target only.
    let markscheme = styled(&world, "markscheme", Target::Pdf);
    assert_eq!(
        get(&markscheme, first(&markscheme, "paragraph"), "space-before"),
        pt(3.0)
    );
    let slides = styled(&world, "exam", Target::Pptx);
    assert_eq!(
        get(&slides, first(&slides, "paragraph"), "space-after"),
        pt(4.0)
    );
}

#[test]
fn variant_overlays_in_a_stylesheet() {
    let world = world(
        "Text.\n",
        &[(
            "styles/main.fsty",
            "@variant markscheme { paragraph { color: #ff0000; } }\n\
             @variant exam, markscheme { @target pdf { paragraph { indent: 1pt; } } }\n",
        )],
    );
    let exam = styled(&world, "exam", Target::Pdf);
    let p = first(&exam, "paragraph");
    assert_eq!(get(&exam, p, "color"), Value::Color(Color::BLACK));
    assert_eq!(get(&exam, p, "indent"), pt(1.0));
    let markscheme = styled(&world, "markscheme", Target::Pdf);
    let p = first(&markscheme, "paragraph");
    assert_eq!(
        get(&markscheme, p, "color"),
        Value::Color(Color::rgb(255, 0, 0))
    );
    let slides = styled(&world, "exam", Target::Pptx);
    assert_eq!(get(&slides, first(&slides, "paragraph"), "indent"), pt(0.0));
}

#[test]
fn text_properties_inherit_and_box_properties_do_not() {
    let world = world(
        "= Forces\n\nA _ball_ is thrown.\n",
        &[(
            "styles/main.fsty",
            "section { font-size: 20pt; color: #00ff00; space-before: 8pt; }\n\
             emph { leading: 1.5em; tracking: 0.1em; }\n\
             heading { font-size: 1.5em; }\n",
        )],
    );
    let styled = styled(&world, "exam", Target::Pdf);
    let emph = first(&styled, "emph");
    assert_eq!(get(&styled, emph, "font-size"), pt(20.0));
    assert_eq!(
        get(&styled, emph, "color"),
        Value::Color(Color::rgb(0, 255, 0))
    );
    assert_eq!(get(&styled, emph, "space-before"), pt(0.0));
    assert_eq!(
        get(&styled, first(&styled, "section"), "space-before"),
        pt(8.0)
    );
    // `em` is the element's own font size, and for the font size its
    // parent's.
    assert_eq!(get(&styled, emph, "tracking"), pt(2.0));
    assert_eq!(
        get(&styled, first(&styled, "heading"), "font-size"),
        pt(30.0)
    );
    // Initial values.
    let doc = first(&styled, "doc");
    assert_eq!(get(&styled, doc, "font-size"), pt(11.0));
    assert_eq!(get(&styled, doc, "numbering"), Value::None);
}

#[test]
fn selectors() {
    let world = world(
        "= A\n\nOne.\n\nTwo.\n\nThree.\n\n- x\n- *y*\n\n\
         #exam.question(marks: 3)[Q] #exam.example[#statement[S] #solution[T]]\n",
        &[(
            "styles/main.fsty",
            // Undo the base stylesheet's spaces, so that only these
            // rules set them.
            "paragraph { space-before: 0pt; space-after: 0pt; }\n\
             heading + paragraph { indent: 1pt; }\n\
             paragraph:first { space-before: 1pt; }\n\
             paragraph:nth(3) { space-before: 3pt; }\n\
             list-item:last { space-after: 9pt; }\n\
             section > paragraph { tracking: 1pt; }\n\
             doc paragraph { leading: 1pt; }\n\
             doc > paragraph { leading: 2pt; }\n\
             list-item:has(strong) { indent: 4pt; }\n\
             paragraph:has(strong) { indent: 8pt; }\n\
             question[marks>2] { indent: 5pt; }\n\
             exam.example > solution { indent: 6pt; }\n\
             example statement { indent: 7pt; }\n",
        )],
    );
    let styled = styled(&world, "exam", Target::Pdf);
    let ps = all(&styled, "paragraph");
    assert_eq!(get(&styled, ps[0], "indent"), pt(1.0));
    assert_eq!(get(&styled, ps[1], "indent"), pt(0.0));
    // The heading is the section's first child.
    assert_eq!(get(&styled, ps[0], "space-before"), pt(0.0));
    assert_eq!(get(&styled, ps[1], "space-before"), pt(3.0));
    assert_eq!(get(&styled, ps[2], "space-after"), pt(0.0));
    assert_eq!(get(&styled, ps[0], "tracking"), pt(1.0));
    assert_eq!(get(&styled, ps[0], "leading"), pt(1.0));
    let items = all(&styled, "list-item");
    // `:has` tests direct children only: an item's text is in a paragraph.
    assert_eq!(get(&styled, items[1], "indent"), pt(0.0));
    assert_eq!(get(&styled, ps[3], "indent"), pt(0.0));
    assert_eq!(get(&styled, ps[4], "indent"), pt(8.0));
    assert_eq!(get(&styled, items[0], "space-after"), pt(0.0));
    assert_eq!(get(&styled, items[1], "space-after"), pt(9.0));
    assert_eq!(
        get(&styled, first(&styled, "exam.question"), "indent"),
        pt(5.0)
    );
    // Slots are parts that selectors name.
    assert_eq!(get(&styled, first(&styled, "solution"), "indent"), pt(6.0));
    assert_eq!(get(&styled, first(&styled, "statement"), "indent"), pt(7.0));
}

#[test]
fn tokens_values_and_component_properties() {
    let world = world(
        "Text.\n",
        &[
            (
                "styles/main.fsty",
                "@import \"tokens.fsty\";\n\
                 paragraph {\n\
                   font-size: $body * 1.5; color: $accent; font: \"Source Serif 4\", \"Noto Serif\";\n\
                   numbering: \"(a)\"; padding: 1pt 2pt; transition: fade 300ms;\n\
                   background: rgb(\"1f5fa8\"); mark-gap: 2pt; exam.mark-gap: calc.max(3pt, 1pt);\n\
                   trailer: align-end \"[\" field(marks) \" marks]\";\n\
                 }\n",
            ),
            (
                "styles/tokens.fsty",
                "$body: 10pt;\n$accent: $blue;\n$blue: #1f5fa8;\n",
            ),
        ],
    );
    let styled = styled(&world, "exam", Target::Pdf);
    let p = first(&styled, "paragraph");
    assert_eq!(get(&styled, p, "font-size"), pt(15.0));
    assert_eq!(
        get(&styled, p, "color"),
        Value::Color(Color::rgb(0x1f, 0x5f, 0xa8))
    );
    assert_eq!(
        get(&styled, p, "background"),
        Value::Color(Color::rgb(0x1f, 0x5f, 0xa8))
    );
    assert_eq!(
        get(&styled, p, "font"),
        Value::Array(EcoVec::from([
            Value::Str("Source Serif 4".into()),
            Value::Str("Noto Serif".into())
        ]))
    );
    assert_eq!(get(&styled, p, "numbering"), Value::Str("(a)".into()));
    assert_eq!(
        get(&styled, p, "padding"),
        Value::Array(EcoVec::from([pt(1.0), pt(2.0)]))
    );
    let Value::Array(transition) = get(&styled, p, "transition") else {
        panic!()
    };
    assert_eq!(transition[0], kw("fade"));
    assert!(matches!(transition[1], Value::Duration(d) if (d.get() - 0.3).abs() < 1e-9));
    // The qualified declaration comes later and wins.
    assert_eq!(get(&styled, p, "exam.mark-gap"), pt(3.0));
    // Templates are kept for the template stage.
    let trailer = styled.properties.find(&"trailer".parse_name()).unwrap();
    assert!(p.style.value(&styled.properties, trailer).is_none());
}

trait ParseName {
    fn parse_name(&self) -> folio_component::QualifiedName;
}

impl ParseName for str {
    fn parse_name(&self) -> folio_component::QualifiedName {
        folio_component::QualifiedName::new(self).unwrap()
    }
}

#[test]
fn local_overrides_and_hints() {
    let world = world(
        "#figure(placement: wrap, width: 1/3, override: (space-before: 24pt))[] \
         #figure(override@markscheme: (space-before: 2pt))[]\n",
        &[(
            "styles/main.fsty",
            "figure { space-before: 1pt; placement: top; }\n",
        )],
    );
    let exam = styled(&world, "exam", Target::Pdf);
    let figures = all(&exam, "figure");
    assert_eq!(get(&exam, figures[0], "space-before"), pt(24.0));
    assert_eq!(get(&exam, figures[0], "placement"), kw("wrap"));
    assert_eq!(get(&exam, figures[1], "placement"), kw("top"));
    assert!(
        matches!(get(&exam, figures[0], "width"), Value::Number(n) if (n.get() - 1.0 / 3.0).abs() < 1e-9)
    );
    assert_eq!(get(&exam, figures[1], "space-before"), pt(1.0));
    let markscheme = styled(&world, "markscheme", Target::Pdf);
    assert_eq!(
        get(&markscheme, all(&markscheme, "figure")[1], "space-before"),
        pt(2.0)
    );
}

/// Content names a token `tok.name` (SPEC §5.4): evaluation keeps the name
/// and each build resolves it against its own tokens, so a variant's
/// overlay that redefines the token changes the value in that variant.
#[test]
fn token_references_in_content() {
    let tokens = world(
        "#figure(override: (space-before: tok.gap, color: tok.warn), width: tok.half)[] \
         #figure(override: (padding: (tok.gap, 2pt)))[]\n",
        &[
            (
                "styles/main.fsty",
                "$gap: 6pt;\n$warn: #d62728;\n$half: 50%;\n",
            ),
            ("styles/markscheme.fsty", "$gap: 9pt;\n"),
        ],
    );
    let exam = styled(&tokens, "exam", Target::Pdf);
    let figures = all(&exam, "figure");
    assert_eq!(get(&exam, figures[0], "space-before"), pt(6.0));
    assert_eq!(
        get(&exam, figures[0], "color"),
        Value::Color(Color::rgb(0xd6, 0x27, 0x28))
    );
    let literal = world("#figure(width: 50%)[]\n", &[]);
    let literal = styled(&literal, "exam", Target::Pdf);
    assert_eq!(
        get(&exam, figures[0], "width"),
        get(&literal, first(&literal, "figure"), "width")
    );
    let literal = world("#figure(override: (padding: (6pt, 2pt)))[]\n", &[]);
    let literal = styled(&literal, "exam", Target::Pdf);
    assert_eq!(
        get(&exam, figures[1], "padding"),
        get(&literal, first(&literal, "figure"), "padding")
    );
    let markscheme = styled(&tokens, "markscheme", Target::Pdf);
    assert_eq!(
        get(&markscheme, all(&markscheme, "figure")[0], "space-before"),
        pt(9.0)
    );
    // The hint resolves like any other value; nothing lays figure widths
    // out yet.
    assert_eq!(
        messages(&tokens),
        ["`width` is not implemented yet for `figure`, so it has no effect"]
    );

    // An unknown token is an error at the element, in the build.
    let unknown = world("#figure(override: (space-before: tok.gpa))[]\n", &[]);
    assert_eq!(messages(&unknown), ["unknown token `$gpa`"]);

    // A script cannot compute with a token reference.
    let computed = world("#figure(override: (space-before: tok.gap * 2))[]\n", &[]);
    let compiled = folio::compile(&computed, Target::Pdf, Some(&name("exam"))).unwrap();
    assert!(
        compiled.diagnostics[0]
            .message
            .starts_with("cannot multiply a token and an int: a token's value is known only"),
        "{:?}",
        compiled.diagnostics
    );
}

/// Inline declarations rank by form, whatever order their fields are
/// written in (SPEC §5.4): hints, then scoped hints, then `override`, then
/// scoped `override@…`.
#[test]
fn inline_declarations_rank_by_form() {
    // Each pair, in both orders; the winner is the same either way.
    let cases = [
        // A hint against the override attribute.
        ("width: 1pt, override: (width: 2pt)", 2.0, 2.0),
        ("override: (width: 2pt), width: 1pt", 2.0, 2.0),
        // The override attribute against a scoped one.
        (
            "override: (width: 1pt), override@markscheme: (width: 2pt)",
            1.0,
            2.0,
        ),
        (
            "override@markscheme: (width: 2pt), override: (width: 1pt)",
            1.0,
            2.0,
        ),
        // A hint against a scoped override.
        ("width: 1pt, override@markscheme: (width: 2pt)", 1.0, 2.0),
        ("override@markscheme: (width: 2pt), width: 1pt", 1.0, 2.0),
        // A hint against a scoped hint.
        ("width: 1pt, width@markscheme: 2pt", 1.0, 2.0),
        ("width@markscheme: 2pt, width: 1pt", 1.0, 2.0),
        // A scoped hint against the override attribute.
        ("width@markscheme: 2pt, override: (width: 1pt)", 1.0, 1.0),
        ("override: (width: 1pt), width@markscheme: 2pt", 1.0, 1.0),
    ];
    for (fields, exam_wins, markscheme_wins) in cases {
        let world = world(&format!("#figure({fields})[]\n"), &[]);
        let exam = styled(&world, "exam", Target::Pdf);
        let markscheme = styled(&world, "markscheme", Target::Pdf);
        assert_eq!(
            get(&exam, all(&exam, "figure")[0], "width"),
            pt(exam_wins),
            "exam: {fields}"
        );
        assert_eq!(
            get(&markscheme, all(&markscheme, "figure")[0], "width"),
            pt(markscheme_wins),
            "markscheme: {fields}"
        );
    }
}

/// Positional pseudo-classes and `+` count the elements of the body only;
/// parts (content fields other than the body) are not siblings and have
/// no position (SPEC §5.1).
#[test]
fn parts_are_not_siblings() {
    // The indent of each paragraph in a figure with a caption, and of
    // each paragraph of a document with a title, with one rule.
    let indents = |rule: &str| {
        let world = world(
            "#doc(title: [T])\n\nA.\n\nB.\n\nC.\n\n#figure(caption: [Caption])[\n  D.\n\n  E.\n\n  F.\n]\n",
            &[("styles/main.fsty", &format!("{rule} {{ indent: 1pt; }}\n"))],
        );
        let exam = styled(&world, "exam", Target::Pdf);
        all(&exam, "paragraph")
            .into_iter()
            .map(|p| get(&exam, p, "indent") == pt(1.0))
            .collect::<Vec<_>>()
    };
    let (t, f) = (true, false);
    assert_eq!(indents("figure > paragraph:first"), [f, f, f, t, f, f]);
    assert_eq!(indents("figure > paragraph:last"), [f, f, f, f, f, t]);
    assert_eq!(indents("figure > paragraph:nth(1)"), [f, f, f, t, f, f]);
    assert_eq!(indents("figure > paragraph:nth(3)"), [f, f, f, f, f, t]);
    assert_eq!(indents("doc > paragraph:first"), [t, f, f, f, f, f]);
    assert_eq!(indents("doc > paragraph:nth(2)"), [f, t, f, f, f, f]);
    assert_eq!(indents("paragraph + paragraph"), [f, t, t, f, t, t]);

    // A part has no position and no previous sibling, and the body's
    // elements do not see it before them.
    let caption = |rule: &str| {
        let world = world(
            "#figure(caption: [Caption])[\n  D.\n]\n",
            &[("styles/main.fsty", &format!("{rule} {{ indent: 1pt; }}\n"))],
        );
        let exam = styled(&world, "exam", Target::Pdf);
        let caption = all(&exam, "caption")[0];
        let paragraph = all(&exam, "paragraph")[0];
        (
            get(&exam, caption, "indent") == pt(1.0),
            get(&exam, paragraph, "indent") == pt(1.0),
        )
    };
    assert_eq!(caption("caption:first"), (false, false));
    assert_eq!(caption("caption:last"), (false, false));
    assert_eq!(caption("caption:nth(1)"), (false, false));
    assert_eq!(caption("caption + paragraph"), (false, false));
    assert_eq!(caption("paragraph + caption"), (false, false));
    // Parts stay addressable by name.
    assert_eq!(caption("figure > caption"), (true, false));
}

#[test]
fn scoped_rules_apply_inside_their_element() {
    let world = world(
        "Before.\n\n#exam.question(marks: 1)[\n  #rule paragraph { indent: 3pt; }\n  Inside.\n]\n\nAfter.\n\n\
         #exam.question(marks: 1)[\n  #rule @target pptx exam.question { indent: 1pt; }\n  Text.\n]\n",
        &[],
    );
    let exam = styled(&world, "exam", Target::Pdf);
    let ps = all(&exam, "paragraph");
    assert_eq!(get(&exam, ps[0], "indent"), pt(0.0));
    assert_eq!(get(&exam, ps[1], "indent"), pt(3.0));
    assert_eq!(get(&exam, ps[2], "indent"), pt(0.0));
    // A scoped rule styles its enclosing element too, in its target only.
    let questions = all(&exam, "exam.question");
    assert_eq!(get(&exam, questions[1], "indent"), pt(0.0));
    let slides = styled(&world, "exam", Target::Pptx);
    assert_eq!(
        get(&slides, all(&slides, "exam.question")[1], "indent"),
        pt(1.0)
    );
}

#[test]
fn scoped_rules_take_stylesheet_values() {
    // The declarations of `#rule` are written as in a stylesheet (SPEC
    // §5.4): hex colours, and tokens resolved against the build's sheets.
    let known = world(
        "#rule paragraph { color: #d62728; indent: $gap * 2; }\nText.\n",
        &[("styles/main.fsty", "$gap: 1.5pt;\n")],
    );
    let exam = styled(&known, "exam", Target::Pdf);
    let p = first(&exam, "paragraph");
    assert_eq!(
        get(&exam, p, "color"),
        Value::Color(Color::rgb(0xd6, 0x27, 0x28))
    );
    assert_eq!(get(&exam, p, "indent"), pt(3.0));
    assert!(messages(&known).is_empty(), "{:?}", messages(&known));

    // A token that no sheet of the build defines is an error at the
    // declaration.
    let unknown = world("#rule paragraph { indent: $gpa; }\nText.\n", &[]);
    assert_eq!(messages(&unknown), ["unknown token `$gpa`"]);

    // `tok.warn` is how content outside a scoped rule names a token; in a
    // rule, as in a stylesheet, the error says to write `$warn`.
    let content_form = world("#rule paragraph { color: tok.warn; }\nText.\n", &[]);
    let compiled = folio::compile(&content_form, Target::Pdf, Some(&name("exam"))).unwrap();
    let diagnostic = &compiled.diagnostics[0];
    assert_eq!(
        diagnostic.message,
        "`tok.warn` names a token in content only"
    );
    assert_eq!(diagnostic.hints[0], "write `$warn` here");
}

#[test]
fn step_rules_are_collected_not_applied() {
    let world = world(
        "#rule paragraph:step(3) { color: rgb(\"00ff00\"); }\nText.\n",
        &[(
            "styles/main.fsty",
            "paragraph:step(2-) { color: #ff0000; }\n",
        )],
    );
    let styled = styled(&world, "exam", Target::Pdf);
    assert_eq!(styled.step_rules.len(), 1);
    assert_eq!(styled.step_rules[0].steps.as_slice(), [(Some(3), Some(3))]);
    assert_eq!(
        get(&styled, first(&styled, "paragraph"), "color"),
        Value::Color(Color::BLACK)
    );
}

#[test]
fn errors() {
    let world = world(
        "Text.\n",
        &[(
            "styles/main.fsty",
            "paragraph { numbering: decimal; columns: 2; colour: #000; color: $acent; }\n\
             paragraph:frist { indent: 1pt; }\n\
             $accent: #000;\n\
             @variant solutions { paragraph { indent: 1pt; } }\n\
             paragraph { space-before: 3; font-size: 1pt 2pt; }\n\
             paragraph { display: blok; font-style: \"italic\"; display: placeholder(\"x\"); }\n\
             paragraph:nth-col(2) { indent: 1pt; }\n",
        )],
    );
    assert_eq!(
        messages(&world),
        [
            // Overlays are checked while the stylesheets are gathered.
            "no variant `solutions` is declared",
            "wrong value for `numbering`: expected none | str, found keyword `decimal`",
            "unknown property `columns`",
            "unknown property `colour`",
            "unknown token `$acent`",
            "unknown pseudo-class `:frist`; the rule is ignored",
            "wrong value for `space-before`: expected length, found int",
            "wrong value for `font-size`: expected length, found a list (length, length)",
            "wrong value for `display`: expected auto | block | inline | none | hidden | answer-space | placeholder(..), found keyword `blok`",
            // Stylesheets write keywords bare.
            "wrong value for `font-style`: expected normal | italic | oblique, found string",
            // Valid, but nothing reads it yet.
            "`display` is not implemented yet, so this declaration has no effect",
            "`:nth-col(n)` styles table cells, and there are no tables yet; the rule matches nothing",
        ]
    );
}

#[test]
fn stylesheet_errors_are_reported_when_content_fails() {
    let world = world(
        "#nope()\n",
        &[("styles/main.fsty", "paragraph { colour: #000; }\n")],
    );
    let messages = messages(&world);
    assert!(
        messages.contains(&"unknown property `colour`".to_string()),
        "{messages:?}"
    );
}

#[test]
fn override_errors_name_their_field() {
    let world = world("#figure(override: (space-before: \"wide\"))[]\n", &[]);
    assert_eq!(
        messages(&world),
        ["wrong value for `space-before` in `override`: expected length, found string"]
    );
}

#[test]
fn em_initial_values_follow_the_font_size() {
    let world = world(
        "Text.\n",
        &[(
            "styles/main.fsty",
            "doc { font-size: 20pt; font: \"Source Serif 4\"; }\n",
        )],
    );
    let styled = styled(&world, "exam", Target::Pdf);
    // `leading` is 0.65em initially: of the root's own font size.
    assert_eq!(get(&styled, first(&styled, "doc"), "leading"), pt(13.0));
    assert_eq!(
        get(&styled, first(&styled, "paragraph"), "leading"),
        pt(13.0)
    );
    // A property that takes a list takes a single item as a list of one.
    assert_eq!(
        get(&styled, first(&styled, "paragraph"), "font"),
        Value::Array(EcoVec::from([Value::Str("Source Serif 4".into())]))
    );
}

#[test]
fn equal_styles_share_memory() {
    let world = world(
        "= A\n\nOne.\n\nTwo.\n\n= B\n\nThree.\n\n#figure[] #figure(class: \"wide\")[]\n",
        &[(
            "styles/main.fsty",
            // The figures' spaces, which the base stylesheet sets, are
            // reset to their initial values: an equal group is shared.
            "paragraph { indent: 1pt; color: #f00; } .wide { color: #f00; }\n\
             figure { space-before: 0pt; space-after: 0pt; }\n",
        )],
    );
    let styled = styled(&world, "exam", Target::Pdf);
    let ps = all(&styled, "paragraph");
    // Paragraphs in different sections with equal styles share one style.
    assert!(ps[0].style.ptr_eq(&ps[1].style));
    assert!(ps[0].style.ptr_eq(&ps[2].style));
    // Untouched groups are shared with the parent or the initial style.
    let section = first(&styled, "section");
    let block = folio::style::GroupId::BLOCK;
    let figures = all(&styled, "figure");
    assert!(Arc::ptr_eq(
        section.style.group(block),
        figures[0].style.group(block)
    ));
    // Equal groups of different styles are shared.
    let text = folio::style::GroupId::TEXT;
    assert!(Arc::ptr_eq(
        ps[0].style.group(text),
        figures[1].style.group(text)
    ));
    assert!(!ps[0].style.ptr_eq(&figures[1].style));
    // Paint changes leave the layout hash alone.
    assert_ne!(figures[0].style.hash_value(), figures[1].style.hash_value());
    assert_eq!(
        figures[0].style.layout_hash(),
        figures[1].style.layout_hash()
    );
}

/// A world whose matching queries are counted.
fn counted(main: &str, sheet: &str) -> (World, Arc<Mutex<usize>>) {
    let count = Arc::new(Mutex::new(0));
    let sink = count.clone();
    let mut world = World::builder()
        .on_event(move |event| {
            if let Event::Execute { query } = event
                && query == "match_node"
            {
                *sink.lock().unwrap() += 1;
            }
        })
        .build(MANIFEST, None)
        .unwrap();
    world.set_source("main.fol", main).unwrap();
    world.set_source("styles/main.fsty", sheet).unwrap();
    world.set_source("styles/markscheme.fsty", "").unwrap();
    world.set_source("styles/overrides.fsty", "").unwrap();
    (world, count)
}

fn restyled(world: &mut World, count: &Mutex<usize>, main: &str) -> (usize, Styled) {
    *count.lock().unwrap() = 0;
    world.set_source("main.fol", main).unwrap();
    let styled = styled(world, "exam", Target::Pdf);
    (*count.lock().unwrap(), styled)
}

#[test]
fn an_insertion_restyles_only_affected_siblings() {
    // Two sections of five labelled figures, each holding a paragraph with
    // a labelled emphasis, so that every node is distinct. Each edit inserts
    // a figure at the start of the second section, after its heading. The
    // new figure, its paragraph and its emphasis are matched in every case.
    let figure = |id: &str| format!("#figure[_x_ <e-{id}>] <f-{id}>\n\n");
    let section = |name: &str, extra: &str| {
        let mut text = format!("= {name}\n\n{extra}");
        for i in 1..=5 {
            text.push_str(&figure(&format!("{name}{i}")));
        }
        text
    };
    let before = format!("{}{}", section("A", ""), section("B", ""));
    let after = format!("{}{}", section("A", ""), section("B", &figure("new")));
    let new = 3;

    // No rule depends on siblings: only the new nodes are matched.
    let (mut world, count) = counted(&before, "figure { indent: 1pt; } emph { color: #f00; }\n");
    styled(&world, "exam", Target::Pdf);
    let (matched, _) = restyled(&mut world, &count, &after);
    assert_eq!(matched, new);

    // `heading + figure`: also the figure the new one displaces, whose
    // previous sibling changed, but not its descendants.
    let sheet = "heading + figure { indent: 1pt; } emph { color: #f00; }\n";
    let (mut world, count) = counted(&before, sheet);
    styled(&world, "exam", Target::Pdf);
    let (matched, out) = restyled(&mut world, &count, &after);
    assert_eq!(matched, new + 1);
    let figures = all(&out, "figure");
    assert_eq!(get(&out, figures[5], "indent"), pt(1.0));
    assert_eq!(get(&out, figures[6], "indent"), pt(0.0));

    // `:nth`: every later sibling in the second section moved; the first
    // section and the descendants of the moved figures are untouched,
    // because no rule looks at their ancestors' positions.
    let sheet = "figure:nth(2) { indent: 1pt; } emph { color: #f00; }\n";
    let (mut world, count) = counted(&before, sheet);
    styled(&world, "exam", Target::Pdf);
    let (matched, out) = restyled(&mut world, &count, &after);
    assert_eq!(matched, new + 5);
    let figures = all(&out, "figure");
    assert_eq!(get(&out, figures[0], "indent"), pt(1.0));
    assert_eq!(get(&out, figures[5], "indent"), pt(1.0));
    assert_eq!(get(&out, figures[6], "indent"), pt(0.0));

    // A rule that looks at an ancestor's position re-matches the
    // descendants of the moved figures instead: their paragraphs and
    // emphases. The figures themselves are styled by no such rule.
    let sheet = "figure:nth(2) emph { color: #f00; }\n";
    let (mut world, count) = counted(&before, sheet);
    styled(&world, "exam", Target::Pdf);
    let (matched, out) = restyled(&mut world, &count, &after);
    assert_eq!(matched, new + 5 * 2);
    let emphs = all(&out, "emph");
    // The heading is the first child of a section.
    assert_eq!(
        get(&out, emphs[0], "color"),
        Value::Color(Color::rgb(255, 0, 0))
    );
    assert_eq!(get(&out, emphs[1], "color"), Value::Color(Color::BLACK));
    assert_eq!(
        get(&out, emphs[5], "color"),
        Value::Color(Color::rgb(255, 0, 0))
    );
    assert_eq!(get(&out, emphs[6], "color"), Value::Color(Color::BLACK));
}
