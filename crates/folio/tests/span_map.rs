//! Span maps and node identity (SPEC §1.5, §12.2, §12.4) through the whole
//! pipeline: every glyph and box of a document's pages resolves to the
//! source it shows, ligatures, right-to-left text and hyphens included, and
//! the nodes they come from keep their identity across edits elsewhere.

use std::collections::{BTreeMap, HashMap};

use folio::span_map::{self, Placed};
use folio::{Fragment, FragmentItem, NodeId, Pages, Source, Target, World};
use folio_syntax::SyntaxKind;

const PAGE: &str = "page { page-size: A5; margin-top: 15mm; margin-bottom: 15mm; \
                    margin-inner: 40mm; margin-outer: 40mm; }\n\
                    doc { font: \"Source Serif 4\", \"New Computer Modern\"; }\n\
                    figure { padding: 4pt; border: 1pt solid #000000; background: #eeeeee; }\n";

/// Text, markup, escapes, shorthands, quotation marks, strings, links,
/// `raw`, a list, a figure, Hebrew between English, and words long enough
/// to be hyphenated in narrow lines, full of ligatures.
const DOCUMENT: &str = "= Office hours\n\n\
    The *official* \\#1 rule -- \"affluent\" and 'efficient' offices are baffling.\n\n\
    Peace is שלום in Hebrew, and difficult scaffolding sufficiently afflicts traffic \
    officials effortlessly.\n\n\
    #\"A string\" and #\"esc\\\"aped\" text, `inline raw` and https://example.com here.\n\n\
    ```rust\nlet office = 1;\n  let affluent;\n```\n\n\
    - A list item with difficult scaffolding.\n\n\
    #figure[In a figure: sufficiently afflicted traffic officials.]\n\n\
    Joined #([text] + \"plus\") and #figure([Figured] + \"inside\").\n\n\
    The end, with incomprehensibilities and characteristically flamboyant affirmations.\n";

fn world(style: &str, text: &str) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en\"\nstyles = [\"styles/main.fsty\"]\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source("styles/main.fsty", &format!("{PAGE}{style}"))
        .unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn pages(world: &World) -> Pages {
    folio::pages(world, Target::Pdf, None).unwrap().unwrap()
}

/// What the glyphs of a document showed, by the kind of node they map to.
#[derive(Debug, Default)]
struct Seen {
    kinds: HashMap<SyntaxKind, usize>,
    ligatures: usize,
    hebrew: usize,
    inserted_hyphens: usize,
    repeated_hyphens: usize,
    joined: usize,
}

/// Checks that every glyph of the pages resolves to the source it shows,
/// and reports what they showed.
fn check_glyphs(world: &World, pages: &Pages) -> Seen {
    let source = world.source("main.fol").unwrap();
    let text = source.text();
    let mut seen = Seen::default();
    for page in &pages.pages {
        for placed in span_map::glyphs(&page.fragment) {
            let location = span_map::locate_glyph(world, placed.glyph)
                .unwrap_or_else(|| panic!("{:?} does not resolve", placed.text()));
            assert_eq!(location.path, "main.fol");
            let written = &text[location.bytes.clone()];
            let node = source.find(span_map::span(placed.glyph.source.0)).unwrap();
            let kind = node.kind();
            *seen.kinds.entry(kind).or_default() += 1;
            check_glyph(&placed, kind, written, &source, location.bytes, &mut seen);
        }
    }
    seen
}

fn check_glyph(
    placed: &Placed<'_>,
    kind: SyntaxKind,
    written: &str,
    source: &Source,
    bytes: std::ops::Range<usize>,
    seen: &mut Seen,
) {
    let shown = placed.text();
    let what = format!("{shown:?} as {kind:?} at {bytes:?}, written {written:?}");
    if shown.chars().count() > 1 && shown.chars().all(char::is_alphabetic) {
        seen.ligatures += 1;
    }
    if shown
        .chars()
        .any(|c| ('\u{05D0}'..='\u{05EA}').contains(&c))
    {
        seen.hebrew += 1;
    }
    match kind {
        SyntaxKind::Text | SyntaxKind::Link | SyntaxKind::Str | SyntaxKind::Raw => {
            if placed.glyph.source.1.is_empty() {
                // A hyphen that a break inserts, inside a word.
                assert_eq!(shown, "-", "{what}");
                assert!(bytes.is_empty(), "{what}");
                let text = source.text();
                let before = text[..bytes.start].chars().next_back().unwrap();
                let after = text[bytes.start..].chars().next().unwrap();
                assert!(before.is_alphabetic() && after.is_alphabetic(), "{what}");
                seen.inserted_hyphens += 1;
            } else if shown == "\u{2028}" {
                // A line break of `raw` stands for what separates two lines.
                assert!(written.contains('\n'), "{what}");
            } else if kind == SyntaxKind::Str && written.contains('\\') {
                // A string with escapes maps as a whole.
                assert!(written.starts_with('"') && written.ends_with('"'), "{what}");
            } else {
                if shown == "-" && placed.glyph.source.1.len() == 1 && written == "-" {
                    seen.repeated_hyphens += 1;
                }
                assert_eq!(written, shown, "{what}");
            }
        }
        // A string joined to content shows as the expression or element
        // that holds it.
        SyntaxKind::Parenthesized | SyntaxKind::FuncCall => {
            assert!(written.contains(shown), "{what}");
            seen.joined += 1;
        }
        // An item's marker, which the base stylesheet sets, stands for
        // the whole item (SPEC §4.5).
        SyntaxKind::ListItem | SyntaxKind::EnumItem => {
            assert!(written.starts_with(['-', '+']), "{what}");
        }
        SyntaxKind::Space => assert!(written.trim().is_empty() && !written.is_empty(), "{what}"),
        SyntaxKind::Escape => assert_eq!(written, format!("\\{shown}"), "{what}"),
        SyntaxKind::Shorthand => assert_eq!(written, "--", "{what}"),
        SyntaxKind::SmartQuote => assert!(written == "\"" || written == "'", "{what}"),
        _ => panic!("a glyph maps to an unexpected node: {what}"),
    }
}

#[test]
fn every_glyph_resolves_to_the_source_it_shows() {
    let world = world("", DOCUMENT);
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let seen = check_glyphs(&world, &pages(&world));
    for kind in [
        SyntaxKind::Text,
        SyntaxKind::Space,
        SyntaxKind::Escape,
        SyntaxKind::Shorthand,
        SyntaxKind::SmartQuote,
        SyntaxKind::Str,
        SyntaxKind::Raw,
        SyntaxKind::Link,
    ] {
        assert!(
            seen.kinds.contains_key(&kind),
            "no glyph of {kind:?}: {seen:?}"
        );
    }
    assert!(seen.ligatures > 0, "no ligature: {seen:?}");
    assert!(seen.hebrew > 0, "no Hebrew: {seen:?}");
    assert!(seen.inserted_hyphens > 0, "no hyphenated line: {seen:?}");
    assert!(seen.joined >= 2, "no joined string: {seen:?}");
}

#[test]
fn a_repeated_polish_hyphen_resolves_to_the_hyphen_it_repeats() {
    let words = "biało-czerwona flaga i polsko-niemiecka granica oraz naukowo-techniczny \
                 postęp w badawczo-rozwojowym ośrodku ";
    let world = world(
        "page { margin-inner: 30mm; margin-outer: 30mm; }\ndoc { lang: \"pl\"; }\n",
        &format!("{}\n", words.repeat(4)),
    );
    let seen = check_glyphs(&world, &pages(&world));
    assert!(seen.repeated_hyphens > 0, "no repeated hyphen: {seen:?}");
}

/// Every fragment, with the fragments it holds, depth first.
fn fragments<'a>(fragment: &'a Fragment, out: &mut Vec<&'a Fragment>) {
    out.push(fragment);
    for (_, item) in &fragment.items {
        if let FragmentItem::Group(group) = item {
            fragments(group, out);
        }
    }
}

#[test]
fn every_box_and_shape_resolves_to_its_element() {
    let world = world("", DOCUMENT);
    let pages = pages(&world);
    let source = world.source("main.fol").unwrap();
    let mut shapes = 0;
    let mut figures = 0;
    for page in &pages.pages {
        let mut all = Vec::new();
        fragments(&page.fragment, &mut all);
        for fragment in all {
            let location = span_map::locate_fragment(&world, fragment)
                .unwrap_or_else(|| panic!("a box does not resolve: {:?}", fragment.size));
            let written = &source.text()[location.bytes.clone()];
            if written.starts_with("#figure") || written.starts_with("figure[") {
                figures += 1;
            }
            for (_, item) in &fragment.items {
                if let FragmentItem::Shape { source: shape, .. } = item {
                    // A figure's background and borders are drawn for it.
                    assert_eq!(*shape, fragment.source);
                    shapes += 1;
                }
            }
        }
        // The page and its body are boxes of the whole document.
        let location = span_map::locate_fragment(&world, &page.fragment).unwrap();
        assert_eq!(location.bytes, 0..source.text().len());
    }
    assert!(
        figures > 0 && shapes > 0,
        "{figures} figures, {shapes} shapes"
    );
}

/// The id and text of the node each glyph comes from.
fn glyph_nodes(world: &World) -> BTreeMap<NodeId, String> {
    let source = world.source("main.fol").unwrap();
    let mut out = BTreeMap::new();
    for page in &pages(world).pages {
        for placed in span_map::glyphs(&page.fragment) {
            let span = span_map::span(placed.glyph.source.0);
            let id = world
                .node_id(span)
                .unwrap_or_else(|| panic!("{:?} has no node id", placed.text()));
            let node = source.find(span).unwrap();
            out.insert(id, source.text()[node.range()].to_owned());
        }
    }
    out
}

#[test]
fn node_ids_are_stable_across_edits_elsewhere() {
    let mut world = world("", DOCUMENT);
    let before = glyph_nodes(&world);

    // Typing in the first paragraph, typing in the figure (which reparses
    // its content block and numbers its nodes anew), and a new paragraph
    // before the list.
    let edits = [
        ("official", "very official"),
        ("In a figure", "Inside a figure"),
        ("- A list item", "A new paragraph.\n\n- A list item"),
    ];
    for (from, to) in edits {
        let text = world.source("main.fol").unwrap().text().to_owned();
        let at = text.find(from).unwrap();
        world
            .edit_source("main.fol", at..at + from.len(), to)
            .unwrap();
    }
    let source = world.source("main.fol").unwrap();
    let written = |id: NodeId| {
        let span = world.node_span(id)?;
        Some(source.text()[source.find(span)?.range()].to_owned())
    };

    // Every node whose text the edits left alone keeps its id, and the id
    // leads to the node in the new revision. Where a glyph still shows it
    // (a space may now end a line instead), it is the same node.
    let changed = ["official", "In a figure"];
    let after = glyph_nodes(&world);
    let mut kept = 0;
    for (id, text) in &before {
        if changed.contains(&text.as_str()) {
            continue;
        }
        assert_eq!(written(*id).as_ref(), Some(text), "{text:?} lost its id");
        if let Some(now) = after.get(id) {
            assert_eq!(now, text);
        }
        kept += 1;
    }
    assert!(kept > 20, "{kept} nodes kept");

    // The text nodes the edits changed keep theirs too.
    for (old, new) in changed
        .into_iter()
        .zip(["very official", "Inside a figure"])
    {
        let id = before
            .iter()
            .find(|(_, text)| text.as_str() == old)
            .map(|(id, _)| *id)
            .unwrap();
        assert_eq!(written(id).as_deref(), Some(new), "{old:?} lost its id");
    }

    // The pages after the edits resolve as before.
    check_glyphs(&world, &pages(&world));
}
