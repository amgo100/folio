//! Locations (SPEC §1.5, §2.3): every element instance of the document tree
//! has its own, which lasts across edits elsewhere, and later trees are
//! paired with the document tree by location rather than by position.

use folio::eval::{Element, Node, Value};
use folio::locate::{Location, StyledChildren};
use folio::model::Document;
use folio::style::{NodeName, Styled, StyledNode};
use folio::{Fragment, FragmentItem, Target, TextFlow, World};

const STYLE: &str = "doc { font: \"Source Serif 4\"; }\n\
                     heading { font-size: 20pt; }\n\
                     paragraph { color: #c00; }\n\
                     strong { font-weight: bold; }\n";

fn world(text: &str) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en\"\nstyles = [\"styles/main.fsty\"]\n";
    let mut world = World::new(manifest).unwrap();
    world.set_source("styles/main.fsty", STYLE).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

fn document(world: &World) -> Document {
    folio::document(world, None).unwrap().unwrap()
}

fn styles(world: &World) -> Styled {
    folio::styles(world, Target::Pdf, None).unwrap().unwrap()
}

/// Every element of the tree, depth first, with its name and plain text.
fn elements(doc: &Document) -> Vec<(String, String, Location)> {
    fn walk(element: &Element, out: &mut Vec<(String, String, Location)>) {
        let text = match element.field("body") {
            Some(Value::Content(body)) => body.plain_text().to_string(),
            _ => String::new(),
        };
        out.push((element.name.to_string(), text, element.location));
        for (_, value) in element.fields.iter() {
            if let Value::Content(content) = value {
                for node in content.nodes() {
                    if let Node::Element(child) = node {
                        walk(child, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.root, &mut out);
    out
}

/// The boxes of the pages, depth first.
fn boxes(world: &World) -> Vec<Fragment> {
    fn walk(fragment: &Fragment, out: &mut Vec<Fragment>) {
        out.push(fragment.clone());
        for (_, item) in &fragment.items {
            if let FragmentItem::Group(group) = item {
                walk(group, out);
            }
        }
    }
    let pages = folio::pages(world, Target::Pdf, None).unwrap().unwrap();
    let mut out = Vec::new();
    for page in &pages.pages {
        walk(&page.fragment, &mut out);
    }
    out
}

#[test]
fn loop_iterations_get_distinct_locations() {
    let world = world("#for i in (1, 2) [Again a *bold* word.\n\n]\n");
    let doc = document(&world);
    let all = elements(&doc);
    let paragraphs: Vec<_> = all.iter().filter(|(n, ..)| n == "paragraph").collect();
    let strongs: Vec<_> = all.iter().filter(|(n, ..)| n == "strong").collect();
    assert_eq!((paragraphs.len(), strongs.len()), (2, 2), "{all:?}");
    for pair in [&paragraphs, &strongs] {
        let (a, b) = (pair[0].2, pair[1].2);
        // Made from one node, as two instances of it.
        assert_eq!(a.source, b.source);
        assert!(!a.source.is_detached());
        assert_ne!(a, b);
    }
    // Every location in the tree is unique.
    let mut locations: Vec<_> = all.iter().map(|(.., l)| (l.source, l.instance)).collect();
    locations.sort();
    locations.dedup();
    assert_eq!(locations.len(), all.len());

    // The styled tree, the text stage and the boxes on the pages carry
    // them.
    let styled = styles(&world);
    for (_, _, location) in &all {
        let nodes = styled
            .nodes()
            .into_iter()
            .filter(|node| node.location == *location)
            .count();
        assert_eq!(nodes, 1, "{location:?}");
    }
    let text = folio::text(&world, Target::Pdf, None).unwrap().unwrap();
    let located: Vec<Location> = text.paragraphs.iter().map(|p| p.location).collect();
    assert_eq!(located, [paragraphs[0].2, paragraphs[1].2]);
    let blocks: Vec<Location> = text
        .root
        .children
        .iter()
        .filter_map(|child| match child {
            TextFlow::Block(block) => Some(block.location),
            TextFlow::Paragraph(_) => None,
        })
        .collect();
    assert_eq!(blocks, located);
    let boxes = boxes(&world);
    for location in located {
        let found = boxes.iter().filter(|b| b.location() == location).count();
        assert_eq!(found, 1, "{location:?}");
    }
}

#[test]
fn locations_last_across_an_insertion_elsewhere() {
    let text = "First paragraph.\n\n= Heading\n\nSome *bold* text.\n\n\
                #for i in (1, 2) [Again a *bold* word.\n\n]\n";
    let mut world = world(text);
    let before = elements(&document(&world));
    let before_boxes: Vec<Location> = boxes(&world).iter().map(Fragment::location).collect();

    let inserted = "An unrelated paragraph.\n\n";
    world.edit_source("main.fol", 0..0, inserted).unwrap();
    let after = elements(&document(&world));
    // The new paragraph is the root's first element; every other element
    // keeps its location.
    let new = after
        .iter()
        .position(|(name, text, _)| name == "paragraph" && text.starts_with("An unrelated"))
        .unwrap();
    let mut kept = after.clone();
    kept.remove(new);
    // The root's text has the new paragraph; the rest is as it was.
    kept[0].1 = before[0].1.clone();
    assert_eq!(kept, before);
    assert!(before.iter().all(|(.., l)| *l != after[new].2));

    // So does every box on the pages, after the new paragraph's.
    let after_boxes: Vec<Location> = boxes(&world).iter().map(Fragment::location).collect();
    for location in &before_boxes {
        assert!(after_boxes.contains(location), "{location:?} lost");
    }
}

/// The styled node of every element of the document, found as the text
/// stage finds them: by location, from the root down.
fn paired<'a>(doc: &'a Document, styled: &'a Styled) -> Vec<(&'a Element, &'a StyledNode)> {
    fn walk<'a>(
        element: &'a Element,
        node: &'a StyledNode,
        out: &mut Vec<(&'a Element, &'a StyledNode)>,
    ) {
        out.push((element, node));
        let mut kids = StyledChildren::new(node);
        for (key, value) in element.fields.iter() {
            let Value::Content(content) = value else {
                continue;
            };
            let mut part = None;
            if key != "body" {
                part = kids.part(&folio::Name::new(key).unwrap());
            }
            let mut part_kids = part.map(StyledChildren::new);
            for node in content.nodes() {
                if let Node::Element(child) = node {
                    let kids = part_kids.as_mut().unwrap_or(&mut kids);
                    if let Some(styled) = kids.element(child) {
                        walk(child, styled, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.root, &styled.root, &mut out);
    out
}

#[test]
fn a_stub_resolve_that_inserts_generated_text_keeps_styles_aligned() {
    let world = world("= Heading\n\nText one.\n\nText *two*.\n");
    let doc = document(&world);
    let styled = styles(&world);
    let expected: Vec<_> = paired(&doc, &styled)
        .into_iter()
        .map(|(element, node)| (element.location, node.style.clone()))
        .collect();
    assert_eq!(expected.len(), elements(&doc).len());

    // A stub of Resolve: generated text, such as a reference's text or a
    // footnote marker, inserted as a node of its own before the heading,
    // with a location that no element of the document has and the style of
    // a paragraph.
    let mut resolved = styled.clone();
    let section = resolved
        .root
        .children
        .make_mut()
        .iter_mut()
        .find(|node| matches!(&node.name, NodeName::Element(n) if n.to_string() == "section"))
        .unwrap();
    let paragraph = section.children[1].clone();
    let generated = StyledNode {
        location: Location {
            source: paragraph.location.source,
            instance: 99,
        },
        children: Default::default(),
        ..paragraph
    };
    let heading = section.children[0].style.clone();
    section.children.insert(0, generated);

    // Pairing by position would now give the heading a paragraph's style.
    assert_ne!(section.children[0].style, heading);

    // By location, every element keeps its style, and the generated node
    // styles none.
    let found: Vec<_> = paired(&doc, &resolved)
        .into_iter()
        .map(|(element, node)| {
            assert_eq!(element.location, node.location);
            (element.location, node.style.clone())
        })
        .collect();
    assert_eq!(found, expected);
}

#[test]
fn another_iteration_keeps_the_locations_of_the_earlier_ones() {
    // Headings open sections with their own span, and a paragraph takes
    // the span of its first node, a `strong` here: each is counted apart
    // from the elements that share its span.
    // The data is edited, not the loop, whose spans a reparse would
    // renumber.
    let mut world = world("#let parts = (1, 2)\n\n#for i in parts [= Part\n\n*Bold* start.\n\n]\n");
    let before = elements(&document(&world));
    let text = world.source("main.fol").unwrap().text().to_owned();
    let at = text.find("2)").unwrap() + 1;
    world.edit_source("main.fol", at..at, ", 3").unwrap();
    let after = elements(&document(&world));
    // The root and the first two iterations keep their locations; the
    // third adds a section, heading, paragraph and strong.
    assert_eq!(after.len(), before.len() + 4);
    let kept: Vec<_> = after.iter().map(|(n, _, l)| (n, l)).collect();
    for (name, _, location) in &before {
        assert!(kept.contains(&(name, location)), "{name} {location:?} lost");
    }
}

/// Every glyph run on the pages, with the location of the box around it.
fn runs(world: &World) -> Vec<(folio::GlyphRun, Location)> {
    fn walk(fragment: &Fragment, out: &mut Vec<(folio::GlyphRun, Location)>) {
        for (_, item) in &fragment.items {
            match item {
                FragmentItem::Group(group) => walk(group, out),
                FragmentItem::Text(run) => out.push((run.clone(), fragment.location())),
                _ => {}
            }
        }
    }
    let pages = folio::pages(world, Target::Pdf, None).unwrap().unwrap();
    let mut out = Vec::new();
    for page in &pages.pages {
        walk(&page.fragment, &mut out);
    }
    out
}

#[test]
fn glyphs_name_the_instance_they_come_from() {
    // SPEC §6.4: of two instances made from one node, each glyph names its
    // own, so the leak check can tell a hidden one from a shown one.
    let world = world("#for i in (1, 2) [Again a *bold* word.\n\n]\n");
    let all = elements(&document(&world));
    let strongs: Vec<Location> = all
        .iter()
        .filter(|(n, ..)| n == "strong")
        .map(|(.., l)| *l)
        .collect();
    let paragraphs: Vec<Location> = all
        .iter()
        .filter(|(n, ..)| n == "paragraph")
        .map(|(.., l)| *l)
        .collect();
    let mut bold = vec![String::new(); 2];
    for (run, around) in runs(&world) {
        for (i, glyph) in run.glyphs.iter().enumerate() {
            let text = &run.text[glyph.range.start as usize..glyph.range.end as usize];
            let instance = match run.instance(i) {
                Location::DETACHED => around,
                instance => instance,
            };
            match strongs.iter().position(|l| *l == instance) {
                Some(n) => bold[n].push_str(text),
                None => assert!(paragraphs.contains(&instance), "{text:?}: {instance:?}"),
            }
        }
    }
    // The glyphs of each `bold` name their own `strong`.
    assert_eq!(bold, ["bold", "bold"]);
}
