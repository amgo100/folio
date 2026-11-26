//! Images and figures through the whole pipeline (SPEC §7.2, §11.1): each
//! format read and sized as an object of its line, `width` as a length or a
//! ratio of the line, images in the PDF as their formats ask (JPEG passed
//! through, SVG as vector paths, PDF figures embedded or converted), and
//! the diagnostics of images: no alt text, a file that is no image, and
//! `width` on an element that is not an image.

use folio::{Abs, Diagnostic, FragmentItem, Severity, Target, World};

const PNG: &[u8] = include_bytes!("../../../tests/corpus/figures/assets/wave.png");
const JPEG: &[u8] = include_bytes!("../../../tests/corpus/figures/assets/photo.jpg");
const WEBP: &[u8] = include_bytes!("../../../tests/corpus/figures/assets/marker.webp");
const GIF: &[u8] = include_bytes!("../../../tests/corpus/figures/assets/spring.gif");
const SVG: &[u8] = include_bytes!("../../../tests/corpus/figures/assets/circuit.svg");
const PDF: &[u8] = include_bytes!("../../../tests/corpus/figures/assets/field.pdf");

/// The page is 100mm wide with 10mm margins: lines are 80mm wide.
const PAGE: &str = "page { page-size: (100mm, 150mm); margin-inner: 10mm; margin-outer: 10mm; }\n";

fn world(title: bool, text: &str) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en-GB\"\nstyles = [\"styles/main.fsty\"]\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source(
            "styles/main.fsty",
            &format!("{PAGE}doc {{ font: \"Source Serif 4\"; font-size: 10pt; }}\n"),
        )
        .unwrap();
    let doc = if title {
        "#doc(title: [Images])\n\n"
    } else {
        ""
    };
    world
        .set_source("main.fol", &format!("{doc}{text}"))
        .unwrap();
    for (path, bytes) in [
        ("a.png", PNG),
        ("a.jpg", JPEG),
        ("a.webp", WEBP),
        ("a.gif", GIF),
        ("a.svg", SVG),
        ("a.pdf", PDF),
        ("not-an-image.png", b"hello".as_slice()),
    ] {
        world.set_file(path, bytes.to_vec()).unwrap();
    }
    world
}

/// The images of a build's pages, in order: format name and size in
/// points, rounded to hundredths.
fn images(world: &World) -> Vec<(&'static str, f64, f64)> {
    fn walk(fragment: &folio::Fragment, out: &mut Vec<(&'static str, f64, f64)>) {
        for (_, item) in &fragment.items {
            match item {
                FragmentItem::Group(group) => walk(group, out),
                FragmentItem::Image(image) => out.push((
                    image.image.format().name(),
                    round(image.size.width),
                    round(image.size.height),
                )),
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

fn round(length: Abs) -> f64 {
    (length.to_pt() * 100.0).round() / 100.0
}

fn compile(world: &World) -> (Vec<u8>, Vec<Diagnostic>) {
    let compiled = folio::compile(world, Target::Pdf, None).unwrap();
    let pdf = compiled.output.clone().expect("a PDF");
    (pdf.to_vec(), compiled.diagnostics.to_vec())
}

/// Every format is read at its natural size: pixels and SVG user units as
/// CSS pixels (¾ pt), a PDF page as its crop box.
#[test]
fn natural_sizes() {
    let world = world(
        true,
        "#image(\"a.png\", alt: \"p\") #image(\"a.jpg\", alt: \"j\") #image(\"a.webp\", alt: \"w\") \
         #image(\"a.gif\", alt: \"g\") #image(\"a.svg\", alt: \"s\") #image(\"a.pdf\", alt: \"d\")\n",
    );
    assert_eq!(
        images(&world),
        [
            ("PNG", 180.0, 90.0),
            ("JPEG", 120.0, 90.0),
            ("WebP", 48.0, 48.0),
            ("GIF", 72.0, 36.0),
            ("SVG", 180.0, 90.0),
            ("PDF", 200.0, 100.0),
        ]
    );
}

/// `width: auto` narrows an image wider than its lines to their width; a
/// length or a ratio of the lines sets it; the height keeps the image's
/// shape.
#[test]
fn widths() {
    let line = 80.0 / 25.4 * 72.0;
    let world = world(
        true,
        "#image(\"a.pdf\", alt: \"d\")\n\n#image(\"a.png\", alt: \"p\", width: 2cm)\n\n\
         #image(\"a.svg\", alt: \"s\", width: 50%)\n\n#image(\"a.svg\", alt: \"s\", width: 1/4)\n",
    );
    let sizes = images(&world);
    let close = |a: f64, b: f64| (a - b).abs() < 0.02;
    // 200pt wide, in lines of 226.77pt: natural.
    assert_eq!(sizes[0], ("PDF", 200.0, 100.0));
    assert!(
        close(sizes[1].1, 56.69) && close(sizes[1].2, 28.35),
        "{sizes:?}"
    );
    assert!(
        close(sizes[2].1, line / 2.0) && close(sizes[2].2, line / 4.0),
        "{sizes:?}"
    );
    assert!(close(sizes[3].1, line / 4.0), "{sizes:?}");

    let wide = world_with_wide_image();
    let sizes = images(&wide);
    assert!(close(sizes[0].1, line), "narrowed to the line: {sizes:?}");
    assert!(close(sizes[0].2, line / 2.0), "{sizes:?}");
}

fn world_with_wide_image() -> World {
    let mut world = world(true, "#image(\"wide.svg\", alt: \"w\")\n");
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="500"/>"#;
    world.set_file("wide.svg", svg.to_vec()).unwrap();
    world
}

/// An image wider than its lines, by a `width` that asks for it, makes an
/// overfull line, as text does (SPEC §7.9).
#[test]
fn an_image_wider_than_its_line_is_overfull() {
    let world = world(true, "#image(\"a.svg\", alt: \"s\", width: 120mm)\n");
    let (_, diagnostics) = compile(&world);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("this line is overfull")),
        "{diagnostics:#?}"
    );
}

/// Each image is a `Figure` with its alt text, so a titled document with
/// images of every format claims PDF/UA-1. JPEG is passed through, PNG
/// compressed, and SVG and a PDF figure, converted under PDF/UA, stay
/// vector: no image XObject.
#[test]
fn pdf_figures() {
    let raster = world(
        true,
        "#figure[#image(\"a.jpg\", alt: \"A disc\")][Caption]\n\n\
         #image(\"a.png\", alt: \"A wave\")\n",
    );
    let (pdf, diagnostics) = compile(&raster);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("<pdfuaid:part>1</pdfuaid:part>"));
    assert!(text.contains("/S/Figure"));
    assert!(text.contains("/Alt(A disc)"), "the alt text");
    assert!(text.contains("/S/Caption"));
    assert!(text.contains("/DCTDecode"), "JPEG passed through");
    assert!(text.contains("/Subtype/Image"));

    let vector = world(
        true,
        "#image(\"a.svg\", alt: \"A circuit\")\n\n#image(\"a.pdf\", alt: \"A field\")\n",
    );
    let (pdf, diagnostics) = compile(&vector);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("<pdfuaid:part>1</pdfuaid:part>"));
    assert!(!text.contains("/Subtype/Image"), "SVG and PDF stay vector");
}

/// Without a title, a PDF figure is embedded as it is, as a form XObject
/// of its page.
#[test]
fn pdf_figure_embedded_without_pdf_ua() {
    let world = world(false, "#image(\"a.pdf\", alt: \"A field\")\n");
    let (pdf, diagnostics) = compile(&world);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(!text.contains("pdfuaid"));
    assert!(text.contains("/Subtype/Form"), "the page as a form XObject");
    assert!(!text.contains("/Subtype/Image"));
}

/// An image without alt text keeps a titled document from claiming
/// PDF/UA-1, with a warning at the image; the check foresees it, so the
/// file is written once.
#[test]
fn an_image_without_alt_text() {
    let world = world(true, "A wave: #image(\"a.png\").\n");
    let (pdf, diagnostics) = compile(&world);
    let warning = diagnostics
        .iter()
        .find(|d| d.message.contains("PDF/UA-1"))
        .unwrap_or_else(|| panic!("a warning: {diagnostics:#?}"));
    assert_eq!(warning.severity, Severity::Warning);
    assert!(warning.message.contains("this image has no alt text"));
    let rendered = folio::diag::render(&world, std::slice::from_ref(warning), false);
    assert!(rendered.contains("main.fol:3:10\n"), "{rendered}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(!text.contains("pdfuaid"));
    assert!(text.contains("/S/Figure"));
}

/// A file that is not an image Folio reads is an error at the image, which
/// is left out.
#[test]
fn a_file_that_is_no_image() {
    let world = world(true, "#image(\"not-an-image.png\", alt: \"x\")\n");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let error = compiled
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
        .unwrap_or_else(|| panic!("an error: {:#?}", compiled.diagnostics));
    assert!(
        error.message.contains("not an image Folio can read"),
        "{error:#?}"
    );
    assert!(images(&world).is_empty());
}

/// `width` on an element that is not an image is not implemented yet: a
/// warning once per element, at it. On an image it is no warning.
#[test]
fn width_on_other_elements() {
    let world = world(
        true,
        "#figure(width: 50%)[#image(\"a.png\", alt: \"p\", width: 2cm)][C]\n\n\
         #figure(width: 50%)[Text][D]\n",
    );
    let (_, diagnostics) = compile(&world);
    let width: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.message.contains("`width`"))
        .collect();
    assert_eq!(width.len(), 1, "{diagnostics:#?}");
    assert!(width[0].message.contains("for `figure`"), "{width:#?}");
}

/// Text in an SVG image is not drawn, with a warning.
#[test]
fn text_in_svg() {
    let mut world = world(true, "#image(\"t.svg\", alt: \"t\")\n");
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><text x="0" y="20">Hi</text></svg>"#;
    world.set_file("t.svg", svg.to_vec()).unwrap();
    let (_, diagnostics) = compile(&world);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("text of this SVG image is not drawn")),
        "{diagnostics:#?}"
    );
}

/// Changing an image's bytes at the same size changes its page's hash, so
/// the preview draws the page again.
#[test]
fn a_changed_image_changes_its_page() {
    let mut world = world(true, "#image(\"a.png\", alt: \"p\")\n");
    let before = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    let mut other = PNG.to_vec();
    // The same pixel size, another file: a byte of a chunk the size does
    // not read, here the last byte of the end chunk's CRC.
    let last = other.len() - 1;
    other[last] ^= 1;
    world.set_file("a.png", other).unwrap();
    let after = folio::pages(&world, Target::Pdf, None).unwrap().unwrap();
    assert_eq!(images(&world)[0], ("PNG", 180.0, 90.0));
    assert_eq!(after.changed(&before), [0]);
}

/// In a region narrower than the body, as floats and other masters will
/// make, an image sized by its lines is sized by the region's: `width: auto`
/// narrows to it, and a ratio is of it.
#[test]
fn images_in_a_narrower_region() {
    use folio::layout::{PageCache, Regions};
    let world = world(
        true,
        "#image(\"a.pdf\", alt: \"d\")\n\n#image(\"a.svg\", alt: \"s\", width: 50%)\n",
    );
    let line = 80.0 / 25.4 * 72.0;
    let narrow = Abs::pt(line - 100.0);
    let width = |_: usize| narrow;
    let regions = Regions {
        width: Some(&width),
        ..Regions::default()
    };
    let styled = folio::styles(&world, Target::Pdf, None).unwrap().unwrap();
    let lines = folio::lines(&world, Target::Pdf, None).unwrap().unwrap();
    let text = folio::text(&world, Target::Pdf, None).unwrap().unwrap();
    let fonts = folio::fonts(&world).unwrap();
    let pages = folio::layout::paginate(
        &text,
        &lines,
        &styled,
        &fonts,
        &folio::layout::Cache::new(),
        &PageCache::new(),
        regions,
    );
    let mut sizes = Vec::new();
    fn walk(fragment: &folio::Fragment, out: &mut Vec<(f64, f64)>) {
        for (_, item) in &fragment.items {
            match item {
                FragmentItem::Group(group) => walk(group, out),
                FragmentItem::Image(image) => {
                    out.push((round(image.size.width), round(image.size.height)))
                }
                _ => {}
            }
        }
    }
    for page in &pages.pages {
        walk(&page.fragment, &mut sizes);
    }
    let narrow = narrow.to_pt();
    let close = |a: f64, b: f64| (a - b).abs() < 0.02;
    assert!(
        close(sizes[0].0, narrow) && close(sizes[0].1, narrow / 2.0),
        "{sizes:?}"
    );
    assert!(close(sizes[1].0, narrow / 2.0), "{sizes:?}");
}

/// A PDF figure of a newer version than the output's (PDF 2.0 in a PDF 1.7
/// file) is converted to vector paths with a warning, rather than failing
/// the build, and the check foresees the warning.
#[test]
fn a_newer_pdf_figure_is_converted() {
    let mut world = world(false, "#image(\"new.pdf\", alt: \"A field\")\n");
    let newer = [b"%PDF-2.0".as_slice(), &PDF[8..]].concat();
    world.set_file("new.pdf", newer).unwrap();
    let (pdf, diagnostics) = compile(&world);
    let warning = diagnostics
        .iter()
        .find(|d| d.message.contains("newer PDF version"))
        .unwrap_or_else(|| panic!("a warning: {diagnostics:#?}"));
    assert_eq!(warning.severity, Severity::Warning);
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.starts_with("%PDF-1.7"));
    let checked = folio::check(&world, Target::Pdf, None).unwrap();
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.message.contains("newer PDF version")),
        "the check foresees it"
    );
}

/// An image whose natural size is past the longest length Folio lays out
/// is an error at the image.
#[test]
fn an_image_too_large_to_lay_out() {
    let mut world = world(true, "#image(\"huge.svg\", alt: \"h\")\n");
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="1e30"/>"#;
    world.set_file("huge.svg", svg.to_vec()).unwrap();
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("too large")),
        "{:#?}",
        compiled.diagnostics
    );
}

/// With `width: auto`, an image in a paragraph whose first line is
/// indented is narrowed to the width that line leaves, so it never makes
/// that line overfull.
#[test]
fn auto_width_leaves_room_for_the_indent() {
    let mut world = world(true, "#image(\"wide.svg\", alt: \"w\")\n");
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="500"/>"#;
    world.set_file("wide.svg", svg.to_vec()).unwrap();
    world
        .set_source(
            "styles/main.fsty",
            &format!(
                "{PAGE}doc {{ font: \"Source Serif 4\"; font-size: 10pt; }}\nparagraph {{ indent: 20pt; }}\n"
            ),
        )
        .unwrap();
    let line = 80.0 / 25.4 * 72.0;
    let sizes = images(&world);
    assert!((sizes[0].1 - (line - 20.0)).abs() < 0.02, "{sizes:?}");
    let (_, diagnostics) = compile(&world);
    assert!(
        !diagnostics.iter().any(|d| d.message.contains("overfull")),
        "{diagnostics:#?}"
    );
}
