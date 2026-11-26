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

fn document<'a>(pages: &'a [Fragment], structure: &'a Tag) -> Document<'a> {
    Document {
        pages: pages.iter().collect(),
        fonts: &Fonts,
        structure,
        info: info(Some("Forces and motion")),
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
