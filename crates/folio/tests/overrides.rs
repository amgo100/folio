//! Local overrides (SPEC §5.4): the styled span or block, the lint
//! `override-literal`, and the list of overrides that
//! `folio check --report overrides` and the language server show.

use folio::diag::locate;
use folio::style::{NodeName, Styled, StyledNode};
use folio::{BuildKey, Name, OverrideForm, Target, World};
use folio_component::{Abs, Color, Value};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
styles = ["styles/main.fsty"]

[variants.exam]
targets = ["pdf"]

[variants.markscheme]
targets = ["pdf"]

[formatting]
overrides-sheet = "styles/overrides.fsty"
"#;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

fn world(main: &str, sheets: &[(&str, &str)]) -> World {
    let mut world = World::builder().build(MANIFEST, None).unwrap();
    world.set_source("main.fol", main).unwrap();
    world.set_source("styles/main.fsty", "").unwrap();
    world.set_source("styles/overrides.fsty", "").unwrap();
    for (path, text) in sheets {
        world.set_source(path, text).unwrap();
    }
    world
}

fn builds() -> Vec<BuildKey> {
    ["exam", "markscheme"]
        .into_iter()
        .map(|variant| BuildKey {
            variant: Some(name(variant)),
            version: None,
            target: Target::Pdf,
        })
        .collect()
}

/// The diagnostics of the exam build: severity, code and message.
fn diagnostics(world: &World) -> Vec<(String, Option<String>, String)> {
    folio::compile(world, Target::Pdf, Some(&name("exam")))
        .unwrap()
        .diagnostics
        .iter()
        .map(|d| {
            (
                d.severity.name().to_string(),
                d.code.as_ref().map(ToString::to_string),
                d.message.to_string(),
            )
        })
        .collect()
}

#[track_caller]
fn styled(world: &World, variant: &str) -> Styled {
    let variant = name(variant);
    let compiled = folio::compile(world, Target::Pdf, Some(&variant)).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    folio::styles(world, Target::Pdf, Some(&variant))
        .unwrap()
        .unwrap()
}

fn all<'a>(styled: &'a Styled, element: &str) -> Vec<&'a StyledNode> {
    styled
        .nodes()
        .into_iter()
        .filter(|n| matches!(&n.name, NodeName::Element(q) if q.to_string() == element))
        .collect()
}

fn get(styled: &Styled, node: &StyledNode, property: &str) -> Value {
    styled.value(node, property).cloned().unwrap_or(Value::None)
}

const RED: Color = Color::rgb(0xd6, 0x27, 0x28);

#[test]
fn styled_span_styles_its_content() {
    let world = world(
        "Please #styled(color: tok.warn, font-weight: 700)[read carefully] now.\n",
        &[("styles/main.fsty", "$warn: #d62728;\n")],
    );
    let exam = styled(&world, "exam");
    let span = all(&exam, "styled");
    assert_eq!(span.len(), 1);
    assert_eq!(get(&exam, span[0], "color"), Value::Color(RED));
    assert_eq!(get(&exam, span[0], "font-weight"), Value::Int(700));
    // The paragraph around it keeps its own style.
    let paragraph = all(&exam, "paragraph")[0];
    assert_eq!(get(&exam, paragraph, "color"), Value::Color(Color::BLACK));
    // The span is in the paragraph's line, not a block of its own.
    let document = folio::document(&world, Some(&name("exam")))
        .unwrap()
        .unwrap();
    let repr = document.repr();
    assert!(
        repr.contains("paragraph") && !repr.contains("\n  styled"),
        "{repr}"
    );
}

#[test]
fn styled_block_holds_paragraphs() {
    let world = world(
        "#styled(font-size: 14pt)[First paragraph.\n\nSecond paragraph.]\n",
        &[],
    );
    let exam = styled(&world, "exam");
    let block = all(&exam, "styled")[0];
    assert_eq!(get(&exam, block, "font-size"), Value::Length(Abs::pt(14.0)));
    let paragraphs = all(&exam, "paragraph");
    assert_eq!(paragraphs.len(), 2);
    for paragraph in paragraphs {
        assert_eq!(
            get(&exam, paragraph, "font-size"),
            Value::Length(Abs::pt(14.0))
        );
    }
    // It lays out: the text reaches the pages.
    let compiled = folio::compile(&world, Target::Pdf, Some(&name("exam"))).unwrap();
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
}

#[test]
fn styled_wins_over_scoped_rules() {
    // A styled span is the strongest form (SPEC §5.4): it wins over a
    // scoped rule around it and over the overrides sheet.
    let world = world(
        "#rule styled { color: $green; }\n#styled(color: tok.red)[Text] <s:one>\n",
        &[
            ("styles/main.fsty", "$red: #d62728;\n$green: #00ff00;\n"),
            ("styles/overrides.fsty", "#{s:one} { color: #0000ff; }\n"),
        ],
    );
    let exam = styled(&world, "exam");
    assert_eq!(
        get(&exam, all(&exam, "styled")[0], "color"),
        Value::Color(RED)
    );
}

#[test]
fn styled_fields_are_checked_as_properties() {
    let unknown = world("#styled(colr: \"red\")[Text]\n", &[]);
    let found = diagnostics(&unknown);
    assert!(
        found
            .iter()
            .any(|(s, _, m)| s == "error" && m.starts_with("unknown property `colr`")),
        "{found:?}"
    );
    let wrong = world("#styled(font-size: \"big\")[Text]\n", &[]);
    let found = diagnostics(&wrong);
    assert!(
        found
            .iter()
            .any(|(s, _, m)| s == "error" && m.starts_with("wrong value for `font-size`")),
        "{found:?}"
    );
    // Its properties cannot be limited to a variant: a scoped rule can.
    let scoped = world("#styled(color@markscheme: tok.red)[Text]\n", &[]);
    let found = diagnostics(&scoped);
    assert!(
        found.iter().any(|(s, _, m)| s == "error"
            && m == "a property of `styled` cannot be limited to a variant or target"),
        "{found:?}"
    );
}

#[test]
fn raw_colours_are_linted() {
    // A literal colour in content or in a scoped rule: a token keeps it in
    // step with the stylesheets.
    for main in [
        "#figure(override: (color: rgb(\"d62728\")))[]\n",
        "#styled(color: rgb(\"d62728\"))[Text]\n",
        "#rule paragraph { color: #d62728; }\nText.\n",
        "#rule paragraph { color: rgb(\"d62728\"); }\nText.\n",
    ] {
        let world = world(main, &[]);
        let found = diagnostics(&world);
        assert!(
            found
                .iter()
                .any(|(s, c, _)| s == "warning" && c.as_deref() == Some("override-literal")),
            "{main}: {found:?}"
        );
    }
    // A token is not a literal; neither is a colour in a stylesheet.
    for main in [
        "#figure(override: (color: tok.red))[]\n",
        "#styled(color: tok.red)[Text]\n",
        "#rule paragraph { color: $red; }\nText.\n",
    ] {
        let world = world(
            main,
            &[(
                "styles/main.fsty",
                "$red: #d62728;\nparagraph { color: #00ff00; }\n",
            )],
        );
        let found = diagnostics(&world);
        assert!(
            found
                .iter()
                .all(|(_, c, _)| c.as_deref() != Some("override-literal")),
            "{main}: {found:?}"
        );
    }
}

/// Each form is listed, with its span, its subject, its properties and
/// its scope (SPEC §5.4).
#[test]
fn every_form_is_listed_with_its_span() {
    let main = "#rule @variant markscheme question { numbering: \"(a)\"; }\n\
                #figure(placement: wrap, width: 1/3, override: (space-before: 24pt))[] <fig:ball>\n\
                #figure(override@markscheme: (space-before: 2pt), width@markscheme: 50%)[]\n\
                Read #styled(font-weight: 700)[carefully].\n";
    let world = world(
        main,
        &[(
            "styles/overrides.fsty",
            "#{fig:ball} { space-after: 4pt; }\nfigure { space-after: 1pt; }\n",
        )],
    );
    let list = folio::overrides(&world, builds()).unwrap();
    let text = |span| {
        let location = locate(&world, folio_syntax::DiagSpan::from(span)).unwrap();
        let source = if location.path == "main.fol" {
            main.to_string()
        } else {
            "#{fig:ball} { space-after: 4pt; }\nfigure { space-after: 1pt; }\n".to_string()
        };
        (
            location.path.to_string(),
            source[location.bytes].to_string(),
        )
    };
    let rows: Vec<_> = list
        .iter()
        .map(|o| {
            (
                o.form,
                o.subject.to_string(),
                o.properties
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                o.scope.as_ref().map(ToString::to_string),
            )
        })
        .collect();
    let strs = |items: &[&str]| items.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            (
                OverrideForm::IdRule,
                "#{fig:ball}".into(),
                strs(&["space-after"]),
                None
            ),
            (
                OverrideForm::ScopedRule,
                "question".into(),
                strs(&["numbering"]),
                Some("variant markscheme".into())
            ),
            (
                OverrideForm::Attribute,
                "figure".into(),
                strs(&["space-before"]),
                None
            ),
            (
                OverrideForm::Hint,
                "figure".into(),
                strs(&["placement", "width"]),
                None
            ),
            (
                OverrideForm::Attribute,
                "figure".into(),
                strs(&["space-before"]),
                Some("markscheme".into())
            ),
            (
                OverrideForm::Hint,
                "figure".into(),
                strs(&["width"]),
                Some("markscheme".into())
            ),
            (
                OverrideForm::Styled,
                "styled".into(),
                strs(&["font-weight"]),
                None
            ),
        ]
    );
    assert_eq!(list[0].label.as_deref(), Some("fig:ball"));
    assert_eq!(list[2].label.as_deref(), Some("fig:ball"));
    let spans: Vec<_> = list.iter().map(|o| text(o.span)).collect();
    assert_eq!(
        spans[0],
        (
            "styles/overrides.fsty".into(),
            "#{fig:ball} { space-after: 4pt; }".into()
        )
    );
    assert!(
        spans[1].1.starts_with("rule @variant markscheme question"),
        "{spans:?}"
    );
    assert!(
        spans[2].1.starts_with("figure(placement: wrap"),
        "{spans:?}"
    );
    assert!(
        spans[4].1.starts_with("figure(override@markscheme"),
        "{spans:?}"
    );
    assert!(
        spans[6].1.starts_with("styled(font-weight: 700)"),
        "{spans:?}"
    );
}

#[test]
fn overrides_made_in_a_loop_are_listed_once() {
    let world = world(
        "#for i in range(3) [#figure(override: (space-before: 2pt))[]]\n",
        &[],
    );
    let list = folio::overrides(&world, builds()).unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0].form, OverrideForm::Attribute);
}
