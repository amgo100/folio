//! Tests of the writer: the structure tree it builds from fragments, the
//! text it gives glyphs, its fallback from PDF/UA-1, and determinism.

use std::fmt::Write as _;

use folio_component::{
    Abs, Color, Em, FontId, Fragment, FragmentItem, Glyph, GlyphRun, Location, Paint, Path, Point,
    Shape, Size, SourceRef,
};
use folio_fonts::{Font, bundled};
use krilla::tagging::{Node, TagKind, TagTree};

use super::*;

/// The fonts: every id is the first bundled face.
struct Fonts;

impl FontTable for Fonts {
    fn font(&self, _: FontId) -> Option<Font> {
        bundled()[0].faces().ok()?.first().cloned()
    }
}

struct NoFonts;

impl FontTable for NoFonts {
    fn font(&self, _: FontId) -> Option<Font> {
        None
    }
}

/// A glyph run of `text` from node `source`, a glyph per character, each
/// showing its character of the node's text.
fn run(text: &str, source: u64) -> FragmentItem {
    run_with(text, source, |range| range)
}

/// A glyph run whose glyphs show the bytes `shows` gives for each glyph's
/// own bytes.
fn run_with(text: &str, source: u64, shows: impl Fn(Range<u32>) -> Range<u32>) -> FragmentItem {
    let glyphs = text
        .char_indices()
        .map(|(i, c)| {
            let range = i as u32..(i + c.len_utf8()) as u32;
            Glyph {
                id: c as u16,
                x_advance: Em::new(0.5),
                x_offset: Em::new(0.0),
                y_offset: Em::new(0.0),
                range: range.clone(),
                source: (SourceRef(source), shows(range)),
            }
        })
        .collect();
    FragmentItem::Text(GlyphRun {
        font: FontId(0),
        size: Abs::pt(10.0),
        fill: Paint::Solid(Color::BLACK),
        text: text.into(),
        glyphs,
    })
}

/// A box of node `source` holding `items`, stacked downwards.
fn boxed(source: u64, items: Vec<FragmentItem>) -> FragmentItem {
    let mut fragment =
        Fragment::new(Size::new(Abs::pt(300.0), Abs::pt(100.0))).with_source(SourceRef(source));
    for (i, item) in items.into_iter().enumerate() {
        fragment.push(
            Point::new(Abs::zero(), Abs::pt(12.0 * i as f64 + 10.0)),
            item,
        );
    }
    FragmentItem::Group(fragment)
}

/// A box of instance `instance` of node `source` holding `items`, as
/// the page builder makes it for a block whose location it knows.
fn located(source: u64, instance: u32, items: Vec<FragmentItem>) -> FragmentItem {
    let FragmentItem::Group(fragment) = boxed(source, items) else {
        unreachable!()
    };
    FragmentItem::Group(fragment.with_location(Location {
        source: SourceRef(source),
        instance,
    }))
}

/// A page whose body region, a box of the root as the page builder dresses
/// it, holds `items`.
fn page(items: Vec<FragmentItem>) -> Fragment {
    let mut page =
        Fragment::new(Size::new(Abs::pt(400.0), Abs::pt(600.0))).with_source(SourceRef(1));
    let FragmentItem::Group(body) = boxed(1, items) else {
        unreachable!()
    };
    page.push(
        Point::new(Abs::pt(50.0), Abs::pt(50.0)),
        FragmentItem::Group(body),
    );
    page
}

fn tag(role: Role, source: u64, children: Vec<Tag>) -> Tag {
    Tag {
        children,
        ..Tag::new(role, SourceRef(source))
    }
}

fn heading(source: u64, text: &str) -> Tag {
    Tag {
        text: Some(text.into()),
        ..Tag::new(Role::Heading, SourceRef(source))
    }
}

fn info(title: Option<&str>) -> Info {
    Info {
        title: title.map(Into::into),
        subject: Some("Mechanics".into()),
        lang: Some("en".into()),
    }
}

/// No destinations, links or page labels.
static NO_NAVIGATION: Navigation = Navigation {
    destinations: Vec::new(),
    links: Vec::new(),
    page_labels: Vec::new(),
};

fn document<'a>(pages: &'a [Fragment], structure: &'a Tag) -> Document<'a> {
    Document {
        pages: pages.iter().collect(),
        fonts: &Fonts,
        structure,
        info: info(Some("Forces and motion")),
        navigation: &NO_NAVIGATION,
    }
}

/// The tag tree and headings the writer builds, as indented text: an
/// element per line, `·` for each piece of marked content.
fn tagged(document: &Document<'_>) -> (String, Vec<(u16, String, usize)>) {
    let mut out = String::new();
    let mut headings = Vec::new();
    let rendered = render_with(document, true, &mut |tree: &TagTree, found| {
        describe(&tree.children, 0, &mut out);
        headings = found
            .iter()
            .map(|(level, text, page, _)| (*level, text.to_string(), *page))
            .collect();
    });
    assert!(rendered.result.is_ok(), "{:?}", rendered.result.err());
    (out, headings)
}

fn describe(nodes: &[Node], depth: usize, out: &mut String) {
    let leaves = nodes.iter().filter(|n| matches!(n, Node::Leaf(_))).count();
    if leaves > 0 {
        writeln!(out, "{}{}", "  ".repeat(depth), "·".repeat(leaves)).unwrap();
    }
    for node in nodes {
        if let Node::Group(group) = node {
            writeln!(out, "{}{}", "  ".repeat(depth), name(&group.tag)).unwrap();
            describe(&group.children, depth + 1, out);
        }
    }
}

fn name(tag: &TagKind) -> String {
    match tag {
        TagKind::Hn(h) => format!("H{} {}", h.level().get(), h.title().unwrap_or_default()),
        TagKind::L(l) => format!("L {:?}", l.numbering()),
        other => {
            let debug = format!("{other:?}");
            debug[..debug.find('(').unwrap_or(debug.len())].to_string()
        }
    }
}

fn text(pdf: &[u8]) -> String {
    String::from_utf8_lossy(pdf).into_owned()
}

/// The document of the tests: a title, then a section with a paragraph,
/// a list and a subsection.
fn structure() -> Tag {
    tag(
        Role::Document,
        1,
        vec![
            tag(Role::Title, 1, vec![]),
            tag(
                Role::Section,
                2,
                vec![
                    heading(2, "Forces"),
                    tag(Role::Paragraph, 3, vec![]),
                    tag(
                        Role::List(ListKind::Bulleted),
                        4,
                        vec![tag(
                            Role::ListItem,
                            4,
                            vec![tag(Role::Paragraph, 5, vec![])],
                        )],
                    ),
                    tag(
                        Role::Section,
                        6,
                        vec![heading(6, "Friction"), tag(Role::Group, 7, vec![])],
                    ),
                ],
            ),
        ],
    )
}

/// The structure's boxes as the page builder makes them: the root's box
/// in the body, holding the title part and the section.
fn one_page() -> Fragment {
    page(vec![boxed(
        1,
        vec![
            boxed(1, vec![run("Forces and motion", 1)]),
            boxed(
                2,
                vec![
                    boxed(2, vec![run("Forces", 12)]),
                    boxed(3, vec![run("A ball falls.", 13)]),
                    boxed(4, vec![boxed(4, vec![boxed(5, vec![run("One", 15)])])]),
                    boxed(
                        6,
                        vec![
                            boxed(6, vec![run("Friction", 16)]),
                            boxed(7, vec![run("Rough.", 17)]),
                        ],
                    ),
                ],
            ),
        ],
    )])
}

/// A fragment within the box of a node with the same source is a box of
/// its child (a heading in its section, a part in its element), and text
/// that a section or group holds goes in a `P`; list items have bodies.
#[test]
fn structure_follows_the_semantic_tree() {
    let pages = [one_page()];
    let structure = structure();
    let (tree, headings) = tagged(&document(&pages, &structure));
    assert_eq!(
        tree,
        "\
Title
  ·
Section
  H1 Forces
    ·
  P
    ·
  L Disc
    LI
      LBody
        P
          ·
  Section
    H2 Friction
      ·
    Div
      P
        ·
"
    );
    assert_eq!(
        headings,
        [(1, "Forces".into(), 0), (2, "Friction".into(), 0)]
    );
}

/// A heading's level is the number of sections around it, so levels
/// never skip, and a heading outside any section is `H1`.
#[test]
fn heading_levels_follow_sections() {
    let structure = tag(
        Role::Document,
        1,
        vec![
            heading(9, "Preface"),
            tag(
                Role::Section,
                2,
                vec![tag(Role::Section, 3, vec![heading(3, "Deep")])],
            ),
        ],
    );
    let pages = [page(vec![boxed(
        1,
        vec![
            boxed(9, vec![run("Preface", 19)]),
            boxed(2, vec![boxed(3, vec![boxed(3, vec![run("Deep", 13)])])]),
        ],
    )])];
    let (tree, headings) = tagged(&document(&pages, &structure));
    assert!(tree.starts_with("H1 Preface\n"), "{tree}");
    assert!(tree.contains("    H2 Deep\n"), "{tree}");
    assert_eq!(headings[1].0, 2);
}

/// Boxes at the top of a page continue the boxes of the page before; the
/// boxes after them are new.
#[test]
fn boxes_continue_across_pages() {
    let structure = tag(
        Role::Document,
        1,
        vec![tag(
            Role::Section,
            2,
            vec![
                heading(2, "Forces"),
                tag(Role::Paragraph, 3, vec![]),
                tag(Role::Paragraph, 4, vec![]),
            ],
        )],
    );
    let first = page(vec![boxed(
        1,
        vec![boxed(
            2,
            vec![
                boxed(2, vec![run("Forces", 12)]),
                boxed(3, vec![run("A ball", 13)]),
            ],
        )],
    )]);
    let second = page(vec![boxed(
        1,
        vec![boxed(
            2,
            vec![
                boxed(3, vec![run("falls.", 13)]),
                boxed(4, vec![run("It lands.", 14)]),
            ],
        )],
    )]);
    let pages = [first, second];
    let (tree, headings) = tagged(&document(&pages, &structure));
    assert_eq!(
        tree,
        "\
Section
  H1 Forces
    ·
  P
    ··
  P
    ·
"
    );
    assert_eq!(headings, [(1, "Forces".into(), 0)]);
}

/// The blocks that the iterations of a loop make share a source and
/// differ in instance: a block at the top of a page that is another
/// instance than the block before it is a block of its own, with its own
/// structure element and bookmark, not a continuation (#251).
#[test]
fn loop_made_blocks_at_the_top_of_a_page_are_new() {
    let at = |tag: Tag, instance: u32| Tag { instance, ..tag };
    let section = |instance: u32, title: &str| {
        at(
            tag(
                Role::Section,
                2,
                vec![
                    at(heading(3, title), instance),
                    at(tag(Role::Paragraph, 4, vec![]), instance),
                ],
            ),
            instance,
        )
    };
    let structure = tag(
        Role::Document,
        1,
        vec![section(1, "Forces"), section(2, "Energy")],
    );
    let block = |instance: u32, title: &str, body: &str| {
        located(
            2,
            instance,
            vec![
                located(3, instance, vec![run(title, 13)]),
                located(4, instance, vec![run(body, 14)]),
            ],
        )
    };
    let first = page(vec![boxed(1, vec![block(1, "Forces", "A ball falls.")])]);
    let second = page(vec![boxed(1, vec![block(2, "Energy", "It is kept.")])]);
    let pages = [first, second];
    let (tree, headings) = tagged(&document(&pages, &structure));
    assert_eq!(
        tree,
        "\
Section
  H1 Forces
    ·
  P
    ·
Section
  H1 Energy
    ·
  P
    ·
"
    );
    assert_eq!(headings, [(1, "Forces".into(), 0), (1, "Energy".into(), 1)]);

    // The same instance at the top of the next page still continues.
    let first = page(vec![boxed(
        1,
        vec![located(2, 1, vec![located(3, 1, vec![run("Forces", 13)])])],
    )]);
    let second = page(vec![boxed(
        1,
        vec![
            located(2, 1, vec![located(4, 1, vec![run("A ball falls.", 14)])]),
            block(2, "Energy", "It is kept."),
        ],
    )]);
    let pages = [first, second];
    let (tree, headings) = tagged(&document(&pages, &structure));
    assert_eq!(
        tree,
        "\
Section
  H1 Forces
    ·
  P
    ·
Section
  H1 Energy
    ·
  P
    ·
"
    );
    assert_eq!(headings, [(1, "Forces".into(), 0), (1, "Energy".into(), 1)]);
}

/// Parts with their element's source are matched in order, and a box that
/// matches no node belongs to the node around it.
#[test]
fn parts_and_unknown_boxes() {
    let structure = tag(
        Role::Document,
        1,
        vec![tag(
            Role::Group,
            5,
            vec![
                tag(Role::Paragraph, 6, vec![]),
                tag(Role::Caption, 5, vec![]),
                tag(Role::Label, 5, vec![]),
            ],
        )],
    );
    let pages = [page(vec![boxed(
        1,
        vec![boxed(
            5,
            vec![
                boxed(6, vec![run("Body", 16)]),
                boxed(5, vec![run("Caption", 15)]),
                boxed(5, vec![run("Label", 15)]),
                boxed(99, vec![run("Stray", 99)]),
            ],
        )],
    )])];
    let (tree, _) = tagged(&document(&pages, &structure));
    assert_eq!(
        tree,
        "\
Div
  P
    ·
  Caption
    ·
  Lbl
    ·
  P
    ·
"
    );
}

/// The file claims PDF/UA-1, with the metadata, the outline, artifacts
/// for shapes and the text of every glyph.
#[test]
fn writes_pdf_ua_1() {
    let mut pages = [one_page()];
    pages[0].push(
        Point::zero(),
        FragmentItem::Shape {
            shape: Shape::filled(Path::rect(Abs::pt(10.0), Abs::pt(10.0)), Color::BLACK),
            source: SourceRef(1),
        },
    );
    let structure = structure();
    let written = write(&document(&pages, &structure)).unwrap();
    assert!(written.warnings.is_empty(), "{:?}", written.warnings);
    assert!(written.pdf_ua);
    let pdf = text(&written.bytes);
    assert!(pdf.starts_with("%PDF-1.7"));
    assert!(pdf.contains("pdfuaid:part"));
    assert!(pdf.contains("/Title(Forces and motion)"));
    assert!(pdf.contains("/Subject(Mechanics)"));
    assert!(pdf.contains("/Lang(en)"));
    assert!(pdf.contains("/DisplayDocTitle true"));
    // Bookmarks: Friction within Forces.
    assert!(pdf.contains("/Count -1/Title(Forces)"));
    assert!(pdf.contains("/Title(Friction)"));
    assert!(pdf.contains("/Artifact<</Type/Layout>>BDC"));
    assert!(pdf.contains("/StructTreeRoot"));
    assert!(!pdf.contains("CreationDate"));
    assert!(!pdf.contains("ModDate"));
}

/// A hyphen a break inserts has the actual text U+00AD; one a line
/// repeats has none. Their glyphs map to `-`, as the explicit hyphen's.
#[test]
fn hyphens_at_breaks() {
    let structure = tag(Role::Document, 1, vec![tag(Role::Paragraph, 3, vec![])]);
    // "co-" ends a line, the inserted hyphen shows nothing; the next line
    // repeats the explicit hyphen of "co-op", whose source it names.
    let pages = [page(vec![boxed(
        1,
        vec![boxed(
            3,
            vec![
                run("in", 13),
                run_with("-", 13, |_| 2..2),
                run_with("co-", 23, |r| r),
                run_with("-", 23, |_| 2..3),
                run("op", 33),
            ],
        )],
    )])];
    let written = write(&document(&pages, &structure)).unwrap();
    assert!(written.warnings.is_empty(), "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert!(pdf.contains("<002D>"), "the hyphen glyph maps to `-`");
    assert!(!pdf.contains("<00AD>"));
    assert!(
        pdf.contains("/ActualText<FEFF00AD>>>BDC"),
        "the inserted hyphen is U+00AD"
    );
    assert!(
        pdf.contains("/ActualText()>>BDC"),
        "the repeated hyphen has no text"
    );
    assert_eq!(pdf.matches("/ActualText").count(), 2);
}

/// Without a title the PDF cannot claim PDF/UA-1, which is no warning:
/// it is written tagged all the same.
#[test]
fn untitled_documents_are_tagged() {
    let pages = [one_page()];
    let structure = structure();
    let document = Document {
        info: info(None),
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(!written.pdf_ua);
    assert!(written.warnings.is_empty(), "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert!(!pdf.contains("pdfuaid:part"));
    assert!(pdf.contains("/StructTreeRoot"));
    assert!(pdf.contains("/S/H1"));
}

/// A document with a title that cannot meet PDF/UA-1, here for a heading
/// without text, is written tagged without the claim, with a warning that
/// says why.
#[test]
fn falls_back_from_pdf_ua_1_with_a_reason() {
    let structure = tag(
        Role::Document,
        1,
        vec![tag(Role::Section, 2, vec![tag(Role::Heading, 2, vec![])])],
    );
    let pages = [page(vec![boxed(
        1,
        vec![boxed(2, vec![boxed(2, vec![run("\u{a0}", 12)])])],
    )])];
    let written = write(&document(&pages, &structure)).unwrap();
    assert!(!written.pdf_ua);
    assert_eq!(written.warnings.len(), 1, "{:?}", written.warnings);
    assert!(
        written.warnings[0]
            .message
            .contains("this heading has no text")
    );
    assert_eq!(written.warnings[0].source, SourceRef(2));
    let pdf = text(&written.bytes);
    assert!(!pdf.contains("pdfuaid:part"));
    assert!(pdf.contains("/S/H1"));
}

/// A heading that shows nothing, here for want of its font, has no
/// bookmark, as it has no structure element.
#[test]
fn undrawn_headings_have_no_bookmark() {
    let pages = [one_page()];
    let structure = structure();
    let document = Document {
        fonts: &NoFonts,
        ..document(&pages, &structure)
    };
    let (tree, headings) = tagged(&document);
    assert!(tree.is_empty(), "{tree}");
    assert!(headings.is_empty(), "{headings:?}");
}

/// Text in a font that cannot be loaded is left out, with a warning at
/// its source.
#[test]
fn warns_of_missing_fonts() {
    let pages = [one_page()];
    let structure = structure();
    let document = Document {
        fonts: &NoFonts,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert_eq!(written.warnings.len(), 1, "one warning per font");
    assert!(written.warnings[0].message.contains("cannot be loaded"));
    assert_eq!(written.warnings[0].source, SourceRef(1));
}

/// The same input gives the same bytes, and the file's IDs follow its
/// content.
#[test]
fn deterministic() {
    let structure = structure();
    let pages = [one_page()];
    let first = write(&document(&pages, &structure)).unwrap().bytes;
    assert_eq!(first, write(&document(&pages, &structure)).unwrap().bytes);
    let other = [page(vec![boxed(1, vec![boxed(1, vec![run("Other", 1)])])])];
    let second = write(&document(&other, &structure)).unwrap().bytes;
    let id = |pdf: &[u8]| {
        let pdf = text(pdf);
        let start = pdf.find("/ID[").unwrap();
        pdf[start..start + 60].to_string()
    };
    assert_ne!(id(&first), id(&second));
}

/// A document without pages is a valid PDF: krilla gives it one blank
/// page, as a PDF needs one.
#[test]
fn no_pages() {
    let structure = tag(Role::Document, 1, vec![]);
    let written = write(&document(&[], &structure)).unwrap();
    assert!(written.warnings.is_empty(), "{:?}", written.warnings);
    assert!(text(&written.bytes).contains("/Type/Pages/Count 1"));
}

/// A hyphen in a run of its own after text from a node that maps as a
/// whole names the same source as the glyph before it, but is text: it is
/// not taken for a repeated hyphen.
#[test]
fn hyphens_from_whole_nodes_stay_text() {
    let structure = tag(Role::Document, 1, vec![tag(Role::Paragraph, 3, vec![])]);
    let whole = |_| 0..u32::MAX;
    let pages = [page(vec![boxed(
        1,
        vec![boxed(
            3,
            vec![
                run_with("co", 23, whole),
                run_with("-", 23, whole),
                run_with("op", 23, whole),
            ],
        )],
    )])];
    let written = write(&document(&pages, &structure)).unwrap();
    assert!(!text(&written.bytes).contains("/ActualText"));
}

/// An object whose content is not laid out yet is drawn as an empty frame,
/// which is not content: the page stays PDF/UA-1, and the paragraph's text
/// around it is tagged as before.
#[test]
fn objects_are_empty_frames() {
    let object = FragmentItem::Object(folio_component::ObjectBox {
        size: Size::new(Abs::pt(30.0), Abs::pt(32.0)),
        baseline: Abs::pt(24.0),
        location: Location {
            source: SourceRef(13),
            instance: 0,
        },
    });
    let plain = [one_page()];
    let mut pages = [one_page()];
    let FragmentItem::Group(body) = &mut pages[0].items.make_mut()[0].1 else {
        unreachable!()
    };
    body.push(Point::new(Abs::pt(10.0), Abs::pt(40.0)), object);
    let structure = structure();
    let written = write(&document(&pages, &structure)).unwrap();
    assert!(written.warnings.is_empty(), "{:?}", written.warnings);
    assert!(written.pdf_ua);
    assert_eq!(
        tagged(&document(&pages, &structure)),
        tagged(&document(&plain, &structure))
    );
    let pdf = text(&written.bytes);
    let artifacts = |pdf: &str| pdf.matches("/Artifact<</Type/Layout>>BDC").count();
    let before = text(&write(&document(&plain, &structure)).unwrap().bytes);
    assert_eq!(artifacts(&pdf), artifacts(&before) + 1);
}

/// A glyph run of pieces of text, each from its own node, a glyph per
/// character.
fn mixed(pieces: &[(&str, u64)]) -> FragmentItem {
    let mut text = String::new();
    let mut glyphs = Vec::new();
    for (piece, source) in pieces {
        for (i, c) in piece.char_indices() {
            let start = (text.len() + i) as u32;
            glyphs.push(Glyph {
                id: c as u16,
                x_advance: Em::new(0.5),
                x_offset: Em::new(0.0),
                y_offset: Em::new(0.0),
                range: start..start + c.len_utf8() as u32,
                source: (SourceRef(*source), 0..u32::MAX),
            });
        }
        text.push_str(piece);
    }
    FragmentItem::Text(GlyphRun {
        font: FontId(0),
        size: Abs::pt(10.0),
        fill: Paint::Solid(Color::BLACK),
        text: text.into(),
        glyphs: glyphs.into(),
    })
}

/// A section (node 2) with the heading `Forces` and a paragraph (node 3)
/// whose lines are `lines`.
fn linked(lines: Vec<FragmentItem>) -> (Vec<Fragment>, Tag) {
    let structure = tag(
        Role::Document,
        1,
        vec![tag(
            Role::Section,
            2,
            vec![heading(2, "Forces"), tag(Role::Paragraph, 3, vec![])],
        )],
    );
    let pages = vec![page(vec![boxed(
        1,
        vec![boxed(
            2,
            vec![boxed(2, vec![run("Forces", 2)]), boxed(3, lines)],
        )],
    )])];
    (pages, structure)
}

/// A destination `sec:forces` at the section (node 2), and the links of
/// reference node 30.
fn navigation(links: Vec<Option<&str>>) -> Navigation {
    Navigation {
        destinations: vec![Destination {
            name: "sec:forces".into(),
            location: Location {
                source: SourceRef(2),
                instance: Location::UNKNOWN_INSTANCE,
            },
        }],
        links: links
            .into_iter()
            .map(|destination| Link {
                source: SourceRef(30),
                destination: destination.map(Into::into),
                text: "Section\u{a0}1".into(),
            })
            .collect(),
        page_labels: Vec::new(),
    }
}

/// A reference is a link annotation over its text, to the named
/// destination of its label, tagged as a `Link` element that holds the
/// annotation and the text, so the PDF still claims PDF/UA-1.
#[test]
fn references_link_to_named_destinations() {
    let (pages, structure) = linked(vec![mixed(&[
        ("See ", 3),
        ("Section\u{a0}1", 30),
        (" now.", 3),
    ])]);
    let navigation = navigation(vec![Some("sec:forces")]);
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    assert!(written.warnings.is_empty(), "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert_eq!(pdf.matches("/Subtype/Link").count(), 1);
    assert!(pdf.contains("/Dest(sec:forces)"), "{pdf}");
    assert!(pdf.contains("/Dests<</Names[(sec:forces)"), "{pdf}");
    assert!(pdf.contains("/Contents(Section 1)"));
    assert!(pdf.contains("/S/Link"));
    let (tree, _) = tagged(&document);
    assert_eq!(
        tree,
        "Section\n  H1 Forces\n    ·\n  P\n    ··\n    Link\n      ··\n"
    );
}

/// A reference broken across lines is one link with an annotation on
/// each line; a reference that does not resolve, and one to a label whose
/// element is not on the pages, is text that goes nowhere.
#[test]
fn links_across_lines_and_links_that_go_nowhere() {
    let (pages, structure) = linked(vec![
        mixed(&[("See Sec", 3), ("tion", 30)]),
        mixed(&[("\u{a0}1", 30), (" and ", 3), ("??", 30)]),
    ]);
    let navigation = navigation(vec![Some("sec:forces"), None]);
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert_eq!(pdf.matches("/Subtype/Link").count(), 2);
    assert!(!pdf.contains("/QuadPoints["));
    let (tree, _) = tagged(&document);
    assert_eq!(
        tree,
        // `See Sec`, then ` and ??`, which goes nowhere, in one piece.
        "Section\n  H1 Forces\n    ·\n  P\n    ··\n    Link\n      ····\n"
    );

    let mut lost = navigation.clone();
    lost.destinations[0].location.source = SourceRef(99);
    let document = Document {
        navigation: &lost,
        ..document
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert!(!pdf.contains("/Subtype/Link"));
    assert!(!pdf.contains("/Dests"));
}

/// Every label has a named destination, whether or not a link goes to it.
#[test]
fn labels_without_references_have_destinations() {
    let (pages, structure) = linked(vec![run("Text.", 3)]);
    let navigation = navigation(vec![]);
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert!(pdf.contains("/Dests<</Names[(sec:forces)"), "{pdf}");
    assert!(!pdf.contains("/Subtype/Link"));
}

/// Page labels as viewers show them, for front matter, the body, the
/// steps of a slide past `z`, and sample pages.
#[test]
fn page_labels_read_as_folio_writes_them() {
    let label = |prefix: &str, numbering, number| PageLabel {
        prefix: prefix.into(),
        numbering,
        number,
    };
    use PageNumbering::*;
    assert_eq!(label("", Some(LowerRoman), 4).text(), "iv");
    assert_eq!(label("", Some(UpperRoman), 1994).text(), "MCMXCIV");
    assert_eq!(label("", Some(Arabic), 12).text(), "12");
    assert_eq!(label("4", Some(LowerAlpha), 1).text(), "4a");
    assert_eq!(label("4", Some(LowerAlpha), 26).text(), "4z");
    assert_eq!(label("4", Some(LowerAlpha), 27).text(), "4aa");
    assert_eq!(label("4", Some(LowerAlpha), 28).text(), "4ab");
    assert_eq!(label("4", Some(LowerAlpha), 702).text(), "4zz");
    assert_eq!(label("4", Some(LowerAlpha), 703).text(), "4aaa");
    assert_eq!(label("", Some(UpperAlpha), 27).text(), "AA");
    assert_eq!(label("4-s", Some(Arabic), 2).text(), "4-s2");
    assert_eq!(label("4", None, 7).text(), "4");
    assert_eq!(label("A-", Some(Arabic), 0).text(), "A-");
}

/// Page labels are written where they differ from the plain numbers, as
/// ranges of the number tree; plain numbers write none.
#[test]
fn writes_page_labels_that_differ_from_plain_numbers() {
    let one = one_page();
    let pages = vec![one.clone(), one.clone(), one.clone(), one.clone(), one];
    let structure = structure();
    let plain = Navigation {
        page_labels: (1..=5).map(PageLabel::arabic).collect(),
        ..Navigation::default()
    };
    let document = Document {
        navigation: &plain,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(!text(&written.bytes).contains("/PageLabels"));

    let roman = |number| PageLabel {
        prefix: "".into(),
        numbering: Some(PageNumbering::LowerRoman),
        number,
    };
    let step = |number| PageLabel {
        prefix: "1".into(),
        numbering: Some(PageNumbering::LowerAlpha),
        number,
    };
    let labelled = Navigation {
        page_labels: vec![roman(1), roman(2), PageLabel::arabic(1), step(1), step(27)],
        ..Navigation::default()
    };
    let document = Document {
        navigation: &labelled,
        ..document
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert!(pdf.contains("/PageLabels"), "{pdf}");
    // i, ii; 1; 1a; then 1aa as a prefix alone, which PDF's letters
    // cannot write.
    assert!(pdf.contains("/Nums[0 "), "{pdf}");
    assert!(pdf.contains("/S/r/St 1"), "{pdf}");
    assert!(pdf.contains("/S/D/St 1"), "{pdf}");
    assert!(pdf.contains("/S/a/P(1)/St 1"), "{pdf}");
    assert!(pdf.contains("/P(1aa)"), "{pdf}");
}

/// Two references of one source that meet, as those of a loop can where
/// the space between them is dropped at a line break, stay two links: a
/// run of the source's glyphs ends where it has shown its link's text.
#[test]
fn references_that_meet_stay_apart() {
    let (pages, structure) = linked(vec![
        mixed(&[("See ", 3), ("Section\u{a0}1", 30)]),
        mixed(&[("Section\u{a0}1", 30), ("Section\u{a0}1", 30)]),
    ]);
    let navigation = navigation(vec![Some("sec:forces"), None, Some("sec:forces")]);
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    // The second goes nowhere.
    assert_eq!(pdf.matches("/Subtype/Link").count(), 2, "{pdf}");
    assert_eq!(pdf.matches("/Contents(Section 1)").count(), 2);
    let (tree, _) = tagged(&document);
    assert_eq!(
        tree,
        "Section\n  H1 Forces\n    ·\n  P\n    ··\n    Link\n      ··\n    Link\n      ··\n"
    );
}

/// A link drawn by several glyph runs on a line, as where a digit falls
/// back to another font, has one annotation there. The glyphs of a
/// cluster count its bytes once, and a hyphen that a break inserts counts
/// none, so neither ends the link early.
#[test]
fn a_link_has_an_annotation_per_line() {
    // `Sec` and `tion` on one line in two runs, with a hyphen inserted
    // after them; `1` on the next line, its glyph drawn as two of one
    // cluster.
    let FragmentItem::Text(first) = mixed(&[("Sec", 30)]) else {
        unreachable!()
    };
    let FragmentItem::Text(mut hyphen) = mixed(&[("tion-", 30)]) else {
        unreachable!()
    };
    hyphen.glyphs.make_mut()[4].source.1 = 0..0;
    let FragmentItem::Text(mut last) = mixed(&[("\u{a0}1", 30)]) else {
        unreachable!()
    };
    let cluster = last.glyphs[1].clone();
    last.glyphs.push(cluster);
    let mut line = Fragment::new(Size::new(Abs::pt(300.0), Abs::pt(12.0)));
    line.push(Point::zero(), FragmentItem::Text(first));
    line.push(
        Point::new(Abs::pt(15.0), Abs::zero()),
        FragmentItem::Text(hyphen),
    );
    let (pages, structure) = linked(vec![FragmentItem::Group(line), FragmentItem::Text(last)]);
    let navigation = navigation(vec![Some("sec:forces"), Some("sec:forces")]);
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert_eq!(pdf.matches("/Subtype/Link").count(), 2, "{pdf}");
    let (tree, _) = tagged(&document);
    assert_eq!(
        tree,
        "Section\n  H1 Forces\n    ·\n  P\n    Link\n      ·····\n"
    );
}

/// A link whose text has no words is described by its destination's
/// name, as PDF/UA-1 requires a description.
#[test]
fn links_without_words_are_described_by_their_label() {
    // The glyphs show spaces; the text also has an object character,
    // which is no word either.
    let (pages, structure) = linked(vec![mixed(&[("See ", 3), (" \u{a0}", 30)])]);
    let mut navigation = navigation(vec![Some("sec:forces")]);
    navigation.links[0].text = " \u{fffc}".into();
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    assert!(text(&written.bytes).contains("/Contents(sec:forces)"));
}

/// A link without text runs until a glyph of another source.
#[test]
fn links_without_text_run_to_another_source() {
    let (pages, structure) = linked(vec![mixed(&[
        ("See ", 3),
        ("Section\u{a0}1", 30),
        (" and ", 3),
        ("Section\u{a0}1", 30),
    ])]);
    let mut navigation = navigation(vec![Some("sec:forces"), Some("sec:forces")]);
    navigation.links[0].text = "".into();
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    assert!(written.pdf_ua, "{:?}", written.warnings);
    let pdf = text(&written.bytes);
    assert_eq!(pdf.matches("/Subtype/Link").count(), 2, "{pdf}");
    assert_eq!(pdf.matches("/Contents(sec:forces)").count(), 1);
    let (tree, _) = tagged(&document);
    assert_eq!(
        tree,
        "Section\n  H1 Forces\n    ·\n  P\n    ··\n    Link\n      ··\n    Link\n      ··\n"
    );
}

/// Text from no node is no link's, and no destination is at it.
#[test]
fn detached_sources_neither_link_nor_land() {
    let (pages, structure) = linked(vec![mixed(&[("See ", 0), ("Section\u{a0}1", 30)])]);
    let mut navigation = navigation(vec![Some("sec:forces")]);
    navigation.links[0].source = SourceRef::DETACHED;
    navigation.destinations.push(Destination {
        name: "nowhere".into(),
        location: Location::DETACHED,
    });
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    let pdf = text(&written.bytes);
    assert!(!pdf.contains("/Subtype/Link"));
    assert!(!pdf.contains("(nowhere)"));
}

/// The marked-content ids of the leaves of the elements named `role` in a
/// tag tree, in tree order, by their debug form.
fn mcids(nodes: &[Node], role: &str, inside: bool, out: &mut Vec<i32>) {
    for node in nodes {
        match node {
            Node::Leaf(id) if inside => {
                let debug = format!("{id:?}");
                // krilla shows no accessor: its derived `Debug` form names
                // the id, `PageTagIdentifier { page_index: 0, mcid: 3 }`.
                let at = debug
                    .find("mcid: ")
                    .unwrap_or_else(|| panic!("no `mcid: ` in krilla's {debug}"))
                    + 6;
                let digits: String = debug[at..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                out.push(digits.parse().unwrap());
            }
            Node::Group(group) => {
                let here = inside || name(&group.tag) == role;
                mcids(&group.children, role, here, out);
            }
            _ => {}
        }
    }
}

/// A footnote's boxes, drawn inside the paragraph of its anchor, are
/// written after the rest of the page (SPEC §7.8): content order, which
/// text extraction follows, reads the paragraph whole and then the note,
/// while the structure keeps the note where its anchor is.
#[test]
fn footnotes_are_written_after_their_page() {
    let note = Tag {
        foot: true,
        ..tag(Role::Note, 4, vec![tag(Role::Paragraph, 5, vec![])])
    };
    let structure = tag(
        Role::Document,
        1,
        vec![
            tag(Role::Paragraph, 2, vec![note]),
            tag(Role::Paragraph, 3, vec![]),
        ],
    );
    let pages = [page(vec![boxed(
        1,
        vec![
            boxed(
                2,
                vec![
                    run("A ball is kicked.1", 12),
                    boxed(4, vec![boxed(5, vec![run("1 Ignore air.", 15)])]),
                    run("It flies.", 12),
                ],
            ),
            boxed(3, vec![run("Another.", 13)]),
        ],
    )])];
    let document = document(&pages, &structure);
    let mut body = Vec::new();
    let mut notes = Vec::new();
    let rendered = render_with(&document, true, &mut |tree: &TagTree, _| {
        mcids(&tree.children, "P", false, &mut body);
        mcids(&tree.children, "Note", false, &mut notes);
    });
    assert!(rendered.result.is_ok(), "{:?}", rendered.result.err());
    let (tree, _) = tagged(&document);
    assert!(tree.contains("Note"), "{tree}");
    assert!(!notes.is_empty());
    // Every piece of the body's text comes before the note's.
    let body: Vec<i32> = body.into_iter().filter(|id| !notes.contains(id)).collect();
    assert!(body.len() >= 2, "{body:?}");
    let last_body = body.iter().max().unwrap();
    assert!(
        notes.iter().all(|n| n > last_body),
        "body {body:?}, notes {notes:?}"
    );
}

/// The destination and the top of each link annotation, as the PDF
/// writes them.
fn annotations(pdf: &str) -> Vec<(String, f64)> {
    pdf.split("endobj")
        .filter(|object| object.contains("/Subtype/Link"))
        .map(|object| {
            let dest = object.split("/Dest(").nth(1).unwrap();
            let dest = dest[..dest.find(')').unwrap()].to_string();
            let rect = object.split("/Rect[").nth(1).unwrap();
            let rect = &rect[..rect.find(']').unwrap()];
            let top: f64 = rect.split_whitespace().nth(3).unwrap().parse().unwrap();
            (dest, top)
        })
        .collect()
}

/// Drawing a footnote after its page leaves each link where its text is:
/// three references from one source, the middle one in a note, go to
/// their own destinations in document order (#308).
#[test]
fn links_in_footnotes_keep_their_destinations() {
    let note = Tag {
        foot: true,
        ..tag(Role::Note, 4, vec![tag(Role::Paragraph, 5, vec![])])
    };
    let structure = tag(Role::Document, 1, vec![tag(Role::Paragraph, 2, vec![note])]);
    let pages = [page(vec![boxed(
        1,
        vec![boxed(
            2,
            vec![
                run("see", 30),
                boxed(4, vec![boxed(5, vec![run("see", 30)])]),
                run("see", 30),
            ],
        )],
    )])];
    let at = Location {
        source: SourceRef(2),
        instance: Location::UNKNOWN_INSTANCE,
    };
    let navigation = Navigation {
        destinations: ["a", "b", "c"]
            .into_iter()
            .map(|name| Destination {
                name: name.into(),
                location: at,
            })
            .collect(),
        links: ["a", "b", "c"]
            .into_iter()
            .map(|name| Link {
                source: SourceRef(30),
                destination: Some(name.into()),
                text: "see".into(),
            })
            .collect(),
        page_labels: Vec::new(),
    };
    let document = Document {
        navigation: &navigation,
        ..document(&pages, &structure)
    };
    let written = write(&document).unwrap();
    let mut found = annotations(&text(&written.bytes));
    assert_eq!(found.len(), 3, "{found:?}");
    // From the top of the page down: the body's first reference, its
    // last, then the note's, whose run lies lowest (its box is the second
    // item, with a box inside it). Drawn last, the note would take the
    // last reference's link, and the last reference the note's.
    found.sort_by(|a, b| b.1.total_cmp(&a.1));
    let order: Vec<&str> = found.iter().map(|(d, _)| d.as_str()).collect();
    assert_eq!(order, ["a", "c", "b"]);
}
