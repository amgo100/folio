//! PDF output (SPEC §11.1) through the whole pipeline: the documents of
//! the test corpus in `tests/corpus` are tagged PDF/UA-1, with the
//! structure of their content, their metadata and their bookmarks, and
//! compile to the same bytes every time.
//!
//! With `FOLIO_PDF_OUT` set to a directory, the test also writes each
//! document's PDF there, with the text a reader should extract from it, for
//! `tests/pdf/corpus.py` to check with veraPDF and `pdftotext`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use folio::{
    Diagnostic, Fragment, FragmentItem, Name, Point, Severity, SourceRef, Target, Text, TextBlock,
    TextFlow, World,
};

/// An element instance: its source and instance number.
type Location = (SourceRef, u32);

/// The corpus directory.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus")
}

/// A world with a corpus project: its manifest, every source file, and its
/// other files, such as images, as data files.
fn project(dir: &Path) -> World {
    let manifest = fs::read_to_string(dir.join("folio.toml")).unwrap();
    let mut world = World::new(&manifest).unwrap();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path.strip_prefix(dir).unwrap().to_str().unwrap();
                let relative = relative.replace('\\', "/");
                match path.extension().and_then(|e| e.to_str()) {
                    Some("fol" | "fsty") => {
                        let text = fs::read_to_string(&path).unwrap();
                        world.set_source(&relative, &text).unwrap();
                    }
                    _ if relative == "folio.toml" || relative.starts_with("out/") => {}
                    _ => world.set_file(&relative, fs::read(&path).unwrap()).unwrap(),
                }
            }
        }
    }
    world
}

/// The corpus projects, by name, in order.
fn projects() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(corpus())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("folio.toml").exists())
        .map(|p| (p.file_name().unwrap().to_str().unwrap().to_string(), p))
        .collect();
    out.sort();
    out
}

fn build(world: &World) -> (Vec<u8>, Vec<Diagnostic>) {
    build_variant(world, None)
}

fn build_variant(world: &World, variant: Option<&Name>) -> (Vec<u8>, Vec<Diagnostic>) {
    let compiled = folio::compile(world, Target::Pdf, variant).unwrap();
    let pdf = compiled.output.clone().expect("a PDF");
    (pdf.to_vec(), compiled.diagnostics.to_vec())
}

/// The variants a project builds as PDF: `None` for a project without
/// variants.
fn pdf_variants(world: &World) -> Vec<Option<Name>> {
    world
        .builds()
        .into_iter()
        .filter(|(_, target)| *target == Target::Pdf)
        .map(|(variant, _)| variant)
        .collect()
}

/// The text of a build as a reader should extract it: in order, one
/// paragraph or display formula per line, without the objects that layout
/// does not draw yet. A formula's text is the text of the glyphs drawn for
/// it, in the order they are drawn: each inline chunk at its object
/// character, a display's lines and their numbers as one line. Each
/// page's footnotes follow its body, where the page ends (SPEC §7.8): the
/// text drawn in their boxes, a line at a time, as the page shows them.
fn expected_text(world: &World, variant: Option<&Name>) -> String {
    let text = folio::text(world, Target::Pdf, variant).unwrap().unwrap();
    let pages = folio::pages(world, Target::Pdf, variant).unwrap().unwrap();
    let lines = folio::lines(world, Target::Pdf, variant).unwrap().unwrap();
    let maths: HashSet<Location> = text
        .maths
        .iter()
        .map(|m| (m.location.source, m.location.instance))
        .collect();
    let notes: HashSet<Location> = text
        .notes
        .iter()
        .map(|n| (n.location.source, n.location.instance))
        .collect();
    // The text of each box of a formula, by the formula, in drawing order;
    // the page each formula starts on; and the text of each page's notes.
    let mut drawn: HashMap<Location, Vec<String>> = HashMap::new();
    let mut first_page: HashMap<Location, usize> = HashMap::new();
    let mut on_page = Vec::new();
    for (i, page) in pages.pages.iter().enumerate() {
        collect_maths(&page.fragment, &maths, &mut drawn, &mut |at| {
            first_page.entry(at).or_insert(i);
        });
        let mut page_notes = String::new();
        collect_notes(&page.fragment, &notes, Point::zero(), &mut page_notes);
        on_page.push(page_notes);
    }
    let mut line_page = HashMap::new();
    for (i, page) in pages.pages.iter().enumerate() {
        for line in &page.lines {
            line_page.insert((line.paragraph, line.line), i);
        }
    }
    let mut w = Expected {
        text: &text,
        drawn: &drawn,
        first_page: &first_page,
        line_page: &line_page,
        lines: &lines,
        notes: &on_page,
        page: 0,
        out: String::new(),
    };
    w.block(&text.root);
    w.reach(on_page.len());
    w.out
}

/// The text of the boxes of formulas in `fragment`, by formula, calling
/// `seen` with each.
fn collect_maths(
    fragment: &Fragment,
    maths: &HashSet<Location>,
    drawn: &mut HashMap<Location, Vec<String>>,
    seen: &mut dyn FnMut(Location),
) {
    for (_, item) in fragment.items.iter() {
        if let FragmentItem::Group(group) = item {
            let location = (group.location().source, group.location().instance);
            if maths.contains(&location) {
                let mut text = String::new();
                runs(group, &mut text);
                drawn.entry(location).or_default().push(text);
                seen(location);
            } else {
                collect_maths(group, maths, drawn, seen);
            }
        }
    }
}

/// The text of the boxes of notes in `fragment`, at `at`, each line of it
/// on a line of its own.
fn collect_notes(fragment: &Fragment, notes: &HashSet<Location>, at: Point, out: &mut String) {
    for (pos, item) in fragment.items.iter() {
        if let FragmentItem::Group(group) = item {
            let pos = Point::new(at.x + pos.x, at.y + pos.y);
            let location = (group.location().source, group.location().instance);
            if notes.contains(&location) {
                let mut found = Vec::new();
                positioned(group, pos, &mut found);
                let mut y = None;
                for (line, run) in found {
                    if y != Some(line) {
                        out.push('\n');
                        y = Some(line);
                    }
                    out.push_str(&run);
                }
                out.push('\n');
            } else {
                collect_notes(group, notes, pos, out);
            }
        }
    }
}

/// The glyph runs in `fragment`, at `at`, in drawing order, each with the
/// baseline of the line it is on, to a tenth of a point (a raised marker
/// is on the line of the text after it).
fn positioned(fragment: &Fragment, at: Point, out: &mut Vec<(i64, String)>) {
    for (pos, item) in fragment.items.iter() {
        let pos = Point::new(at.x + pos.x, at.y + pos.y);
        match item {
            FragmentItem::Text(run) => {
                let y = (pos.y.to_pt() / 4.0).round() as i64;
                let y = out
                    .last()
                    .map_or(y, |(last, _)| if (last - y).abs() <= 1 { *last } else { y });
                out.push((y, run.text.to_string()));
            }
            FragmentItem::Group(group) => positioned(group, pos, out),
            _ => {}
        }
    }
}

/// The text of the glyph runs in `fragment`, in drawing order.
fn runs(fragment: &Fragment, out: &mut String) {
    for (_, item) in fragment.items.iter() {
        match item {
            FragmentItem::Text(run) => out.push_str(&run.text),
            FragmentItem::Group(group) => runs(group, out),
            _ => {}
        }
    }
}

/// The expected text being written, and the page it has reached.
struct Expected<'a> {
    text: &'a Text,
    drawn: &'a HashMap<Location, Vec<String>>,
    first_page: &'a HashMap<Location, usize>,
    line_page: &'a HashMap<(usize, usize), usize>,
    lines: &'a folio::Lines,
    notes: &'a [String],
    page: usize,
    out: String,
}

impl Expected<'_> {
    /// Writes the notes of the pages before `page`, which the text has
    /// reached.
    fn reach(&mut self, page: usize) {
        while self.page < page {
            self.out.push_str(&self.notes[self.page]);
            self.page += 1;
        }
    }

    fn block(&mut self, block: &TextBlock) {
        let text = self.text;
        let location = (block.location.source, block.location.instance);
        if let Some(boxes) = self.drawn.get(&location)
            && text
                .maths
                .iter()
                .any(|m| m.display && m.location == block.location)
        {
            if let Some(&page) = self.first_page.get(&location) {
                self.reach(page);
            }
            self.out.push_str(&boxes.concat());
            self.out.push('\n');
            return;
        }
        for child in block.children.iter() {
            match child {
                TextFlow::Block(child) => self.block(child),
                TextFlow::Paragraph(index) => self.paragraph(*index),
            }
        }
    }

    fn paragraph(&mut self, index: usize) {
        let text = self.text;
        let paragraph = &text.paragraphs[index];
        let starts: Vec<usize> = self.lines.paragraphs[index]
            .lines
            .iter()
            .map(|l| l.range.start)
            .collect();
        let mut line = String::new();
        let mut objects = paragraph.objects.iter();
        for (at, c) in paragraph.shaped.text().char_indices() {
            // A page that starts with this line ends the one before it.
            if let Some(l) = starts.iter().position(|&s| s == at)
                && let Some(&page) = self.line_page.get(&(index, l))
                && page > self.page
            {
                self.out.push_str(line.trim());
                self.out.push('\n');
                line.clear();
                self.reach(page);
            }
            match c {
                '\u{FFFC}' => {
                    let chunk = objects
                        .next()
                        .and_then(|o| {
                            Some(((o.location.source, o.location.instance), o.math?.chunk))
                        })
                        .and_then(|(at, chunk)| self.drawn.get(&at)?.get(chunk));
                    if let Some(chunk) = chunk {
                        line.push_str(chunk);
                    }
                }
                '\u{2028}' => line.push(' '),
                c => line.push(c),
            }
        }
        self.out.push_str(line.trim());
        self.out.push('\n');
    }
}

/// Every corpus document, in each variant it builds as PDF, is a PDF/UA-1
/// file whose structure follows its content, and compiles to the same bytes
/// in a new world.
#[test]
fn corpus_is_pdf_ua_1() {
    let out = std::env::var_os("FOLIO_PDF_OUT").map(PathBuf::from);
    let projects = projects();
    assert!(projects.len() >= 3, "the corpus has its documents");
    for (project_name, dir) in projects {
        let world = project(&dir);
        for variant in pdf_variants(&world) {
            check_pdf(
                &project_name,
                &dir,
                &world,
                variant.as_ref(),
                out.as_deref(),
            );
        }
    }
}

/// Checks one PDF of a corpus project, and writes it to `out`.
fn check_pdf(
    project_name: &str,
    dir: &Path,
    world: &World,
    variant: Option<&Name>,
    out: Option<&Path>,
) {
    // Named as `folio build` names its outputs (SPEC §6.4).
    let name = match variant {
        Some(variant) => format!("{project_name}-{variant}"),
        None => project_name.to_string(),
    };
    let (pdf, diagnostics) = build_variant(world, variant);
    let problems: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error || d.message.contains("PDF"))
        .collect();
    assert!(problems.is_empty(), "{name}: {problems:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("<pdfuaid:part>1</pdfuaid:part>"), "{name}");
    for element in ["/S/Document", "/S/Title", "/S/Sect", "/S/H1", "/S/P"] {
        assert!(text.contains(element), "{name} has no {element}");
    }
    assert!(text.contains("/Type/Outlines"), "{name}");
    assert_eq!(
        build_variant(&project(dir), variant).0,
        pdf,
        "{name} is deterministic"
    );
    if let Some(out) = out {
        fs::create_dir_all(out).unwrap();
        fs::write(out.join(format!("{name}.pdf")), &pdf).unwrap();
        fs::write(
            out.join(format!("{name}.txt")),
            expected_text(world, variant),
        )
        .unwrap();
    }
}

/// The notes use every structure role that the built-in elements map to.
#[test]
fn notes_have_every_role() {
    let (pdf, _) = build(&project(&corpus().join("notes")));
    let text = String::from_utf8_lossy(&pdf);
    for element in [
        "/S/H2",
        "/S/H3",
        "/S/L",
        "/S/LI",
        "/S/Lbl",
        "/S/LBody",
        "/S/Note",
        "/S/Caption",
        "/S/Code",
        "/S/Div",
    ] {
        assert!(text.contains(element), "the notes have no {element}");
    }
    assert!(text.contains("/Title(Waves and Oscillations)"));
    assert!(text.contains("/Subject(A-level Physics, Year 12 notes)"));
    assert!(text.contains("/Lang(en-GB)"));
    assert!(
        text.contains("/Title(Simple harmonic motion)"),
        "a bookmark"
    );
}

/// The title in the metadata is the title as shown, with the language's
/// quotation marks.
#[test]
fn metadata_title_as_shown() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let mut world = World::new(manifest).unwrap();
    let main = "#doc(title: [The \"ideal\" gas])\n\nA ball falls.\n";
    world.set_source("main.fol", main).unwrap();
    let (pdf, diagnostics) = build(&world);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">The \u{201C}ideal\u{201D} gas</rdf:li>"), "{text}");
}

/// A document without a title is written tagged, but cannot claim
/// PDF/UA-1, which requires one; that is no warning.
#[test]
fn untitled_document() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source("main.fol", "= Forces\n\nA ball falls.\n")
        .unwrap();
    let (pdf, diagnostics) = build(&world);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(!text.contains("pdfuaid"));
    assert!(text.contains("/S/H1"));
}

/// A document with a title that cannot meet PDF/UA-1 is written tagged,
/// with a warning that says why, at the heading.
#[test]
fn heading_without_text() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let mut world = World::new(manifest).unwrap();
    // A no-break space draws a glyph but gives the heading no text.
    let main = "#doc(title: [Forces])\n\n= ~\n\nA ball falls.\n";
    world.set_source("main.fol", main).unwrap();
    let (pdf, diagnostics) = build(&world);
    let warning = diagnostics
        .iter()
        .find(|d| d.message.contains("PDF/UA-1"))
        .unwrap_or_else(|| panic!("a warning: {diagnostics:#?}"));
    assert_eq!(warning.severity, Severity::Warning);
    assert!(warning.message.contains("this heading has no text"));
    let rendered = folio::diag::render(&world, std::slice::from_ref(warning), false);
    assert!(rendered.contains("main.fol:3:1\n"), "{rendered}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(!text.contains("pdfuaid"));
}

/// The sections a loop makes share their source and differ in instance:
/// each gets its structure element and bookmark, also those that start at
/// the top of a page, which matching by source alone took for the section
/// before continuing (#251).
#[test]
fn loop_made_sections_are_tagged_apart() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let mut world = World::new(manifest).unwrap();
    // Thirty sections over several pages, so that some start a page.
    let body = "A ball falls. ".repeat(20);
    let main =
        format!("#doc(title: [Forces])\n\n#for i in range(30) [\n\n= Part #i\n\n{body}\n\n]\n");
    world.set_source("main.fol", &main).unwrap();
    let (pdf, diagnostics) = build(&world);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert_eq!(text.matches("/S/Sect").count(), 30);
    assert_eq!(text.matches("/S/H1").count(), 30);
    for i in 0..30 {
        assert!(
            text.contains(&format!("/Title(Part {i})")),
            "a bookmark for part {i}"
        );
    }
}

/// A document with references, for the navigation tests: labelled
/// sections, a reference to each, one in a loop, and one to a label no
/// element has.
const NAVIGATION: &str = "#doc(title: [Forces])

= Forces <sec:forces>

A ball falls. See @sec:energy for what it loses.

= Energy <sec:energy>

Compare @sec:forces with this section.

#for i in range(2) [As in @sec:forces, again. ]

Nothing has @sec:missing.
";

/// References are links to the named destinations of their labels, tagged
/// as `Link` elements, and the PDF still claims PDF/UA-1. With
/// `FOLIO_PDF_OUT` set, the PDF goes there too, for `tests/pdf/corpus.py`
/// to check with veraPDF.
#[test]
fn references_are_links() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let mut world = World::new(manifest).unwrap();
    world.set_source("main.fol", NAVIGATION).unwrap();
    let (pdf, diagnostics) = build(&world);
    let problems: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| !d.message.contains("sec:missing"))
        .collect();
    assert!(problems.is_empty(), "{problems:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("<pdfuaid:part>1</pdfuaid:part>"), "PDF/UA-1");
    // Four references resolve, two of them made by the loop; the one to
    // a missing label goes nowhere.
    assert_eq!(text.matches("/Subtype/Link").count(), 4, "{text}");
    assert_eq!(text.matches("/S/Link").count(), 4);
    assert_eq!(text.matches("/Dest(sec:forces)").count(), 3);
    assert_eq!(text.matches("/Dest(sec:energy)").count(), 1);
    // The alternative description is the reference's text.
    assert_eq!(text.matches("/Contents(Section 1)").count(), 3);
    assert_eq!(text.matches("/Contents(Section 2)").count(), 1);
    assert!(
        text.contains("/Dests<</Names[(sec:energy)") && text.contains("(sec:forces)"),
        "a name tree of the labels, in order"
    );
    // Plain page numbers need no labels.
    assert!(!text.contains("/PageLabels"));
    if let Some(out) = std::env::var_os("FOLIO_PDF_OUT").map(PathBuf::from) {
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("navigation.pdf"), &pdf).unwrap();
        fs::write(out.join("navigation.txt"), expected_text(&world, None)).unwrap();
    }
}

/// A document with footnotes on small pages: a multi-line note in the
/// middle of a paragraph, one long enough to continue on the next page,
/// and notes numbered from 1 on each page (SPEC §7.8).
const FOOTNOTES: &str = "#doc(title: [Footnotes])

A ball is kicked at 35 degrees to the horizontal.#footnote[Treat the ball as a particle, and \
ignore air resistance in every part of the question, as the mark scheme assumes.] Use the \
equations of motion to find how far it goes, and how long it is in the air before it lands on \
the ground again.

The time of flight depends only on the height of the cliff and not on the speed \
of the throw. A faster ball lands further away, but it lands at the same moment as a slower one \
thrown at the same time, because both fall the same height from rest in the vertical direction. \
This surprises many students when they first meet it, and a simple demonstration with two coins \
shows it well.

The range is the horizontal speed multiplied by the time of flight.#footnote[This holds only \
while the ground below is level. On a slope the ball lands earlier or later, and the range must \
be found from where the path meets the ground. The same method works for a ball thrown at an \
angle, once its velocity has been resolved into horizontal and vertical components, each of \
which is treated on its own as before. A worked example follows in the next lesson, with a table \
of results to compare with the experiment, and a short set of exam questions to try at home. \
The questions start with a ball thrown from a table, which is easy to film with a phone held at \
the side, and end with a ball thrown up a slope, which needs both components at once. Bring a \
calculator, a ruler and squared paper, and the notes from the last two lessons.] A \
ball thrown at twice the speed travels twice as far before it lands.

Another paragraph after it, which runs on for a while so that the page fills and the notes \
must share it with the body text above them.
";

/// Each page's footnotes are written after its body (#308): the text a
/// reader extracts in content order reads every sentence whole. With
/// `FOLIO_PDF_OUT` set, the PDF and its expected text go there, for
/// `tests/pdf/corpus.py` to check the order with `pdftotext`.
#[test]
fn footnotes_follow_their_page_in_content_order() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en\"\nstyles = [\"main.fsty\"]\n";
    let mut world = World::new(manifest).unwrap();
    world.set_source("main.fol", FOOTNOTES).unwrap();
    world
        .set_source(
            "main.fsty",
            "page { page-size: 105mm 148mm; margin-top: 12mm; margin-bottom: 12mm; \
             margin-inner: 12mm; margin-outer: 12mm; counter-reset: footnote; }\n",
        )
        .unwrap();
    let (pdf, diagnostics) = build(&world);
    let problems: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error || d.message.contains("PDF"))
        .collect();
    assert!(problems.is_empty(), "{problems:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("<pdfuaid:part>1</pdfuaid:part>"));
    let pages = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    assert!(pages.pages.len() >= 2, "the notes continue on another page");
    // The expected text has the first note after the whole first
    // sentence around its anchor, and the continued note's mark.
    let expected = expected_text(&world, None);
    let flat = expected.split_whitespace().collect::<Vec<_>>().join(" ");
    let sentence = flat.find("Use the equations of motion").unwrap();
    let note = flat.find("Treat the ball as a particle").unwrap();
    assert!(sentence < note, "{flat}");
    assert!(flat.contains("(cont.)"), "{flat}");
    if let Some(out) = std::env::var_os("FOLIO_PDF_OUT").map(PathBuf::from) {
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("footnotes.pdf"), &pdf).unwrap();
        fs::write(out.join("footnotes.txt"), expected).unwrap();
    }
}
