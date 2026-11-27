//! Untrusted figures (SPEC §14.3, #333): an image file that would hang the
//! build, take more memory than the machine has or overflow the stack is
//! an error that says the image cannot be read, given in bounded time by
//! `folio check` and `folio build` alike. The figures are in a titled
//! document, so that PDF figures are converted to vector paths (PDF/UA),
//! the path that reads them whole.

use std::time::{Duration, Instant};

use folio::{Diagnostic, Severity, Target, World};

const FORM_CYCLE: &[u8] = include_bytes!("../../folio-core/tests/untrusted/form-cycle.pdf");
const FORM_CYCLE_ONCE: &[u8] =
    include_bytes!("../../folio-core/tests/untrusted/form-cycle-once.pdf");

/// How long one check or build of a hostile figure may take. The cases
/// take well under a second in a debug build; before #333 they ran for
/// minutes or aborted.
const BOUND: Duration = Duration::from_secs(20);

fn world(bytes: Vec<u8>, name: &str) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en-GB\"\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source(
            "main.fol",
            &format!("#doc(title: [Figures])\n\n#image(\"{name}\", alt: \"x\")\n"),
        )
        .unwrap();
    world.set_file(name, bytes).unwrap();
    world
}

/// The error a hostile figure gives, the same from `check` and `build`,
/// each within [`BOUND`].
fn error(bytes: Vec<u8>, name: &str) -> String {
    let world = world(bytes, name);
    let first = |diagnostics: &[Diagnostic]| {
        diagnostics
            .iter()
            .find(|d| d.severity == Severity::Error)
            .unwrap_or_else(|| panic!("an error: {diagnostics:#?}"))
            .message
            .to_string()
    };
    let start = Instant::now();
    let checked = folio::check(&world, Target::Pdf, None).unwrap();
    assert!(start.elapsed() < BOUND, "check took {:?}", start.elapsed());
    let start = Instant::now();
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(start.elapsed() < BOUND, "build took {:?}", start.elapsed());
    let (check, build) = (first(&checked.diagnostics), first(&compiled.diagnostics));
    assert_eq!(check, build, "check and build disagree");
    check
}

/// A PDF of objects 1 to n, numbered in order: a catalog (1), a page tree
/// (2), one page (3) whose content stream is object 4, and `rest`, from
/// object 5 on.
fn pdf(page: &str, content: &[u8], rest: &[Vec<u8>]) -> Vec<u8> {
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R {page} >>")
            .into_bytes(),
        stream("", content),
    ];
    objects.extend_from_slice(rest);
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A PNG of a size, with a header and no pixels: enough for every reader
/// that refuses it by its size.
fn png_header(width: u32, height: u32, bit_depth: u8, colour: u8) -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[bit_depth, colour, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    // A zlib stream of one empty stored block: the pixels are missing.
    chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x01, 0x01, 0x00, 0x00, 0xff, 0xff],
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn svg_with_png(png: &[u8]) -> Vec<u8> {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image width="10" height="10" href="data:image/png;base64,{}"/></svg>"#,
        base64(png)
    )
    .into_bytes()
}

fn cannot_be_read(message: &str, format: &str) {
    assert!(
        message.starts_with(&format!("this {format} file cannot be read")),
        "{message}"
    );
}

/// A form XObject whose resources name itself, drawn twice by itself:
/// it ran without end before #333.
#[test]
fn a_form_that_draws_itself() {
    for bytes in [FORM_CYCLE, FORM_CYCLE_ONCE] {
        let message = error(bytes.to_vec(), "cycle.pdf");
        cannot_be_read(&message, "PDF");
        assert!(message.contains("draws itself"), "{message}");
    }
}

/// Forms that each draw the next twice, 40 deep, with no cycle: 2⁴⁰
/// drawings, which the reader's work limit stops.
#[test]
fn forms_that_draw_each_other_exponentially() {
    let depth = 40;
    let mut forms = Vec::new();
    for i in 0..depth {
        let object = 5 + i;
        let (resources, body) = if i + 1 < depth {
            (
                format!("/Resources << /XObject << /X {} 0 R >> >>", object + 1),
                b"/X Do /X Do".to_vec(),
            )
        } else {
            (String::new(), b"0 0 1 1 re f".to_vec())
        };
        forms.push(stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] {resources}"),
            &body,
        ));
    }
    let bytes = pdf("/Resources << /XObject << /X 5 0 R >> >>", b"/X Do", &forms);
    let message = error(bytes, "forms.pdf");
    cannot_be_read(&message, "PDF");
    assert!(message.contains("more work"), "{message}");
}

/// 100,000 nested arrays in a page's dictionary overflowed the stack.
#[test]
fn deeply_nested_arrays() {
    let depth = 100_000;
    let page = format!("/Deep {}{}", "[".repeat(depth), "]".repeat(depth));
    let message = error(pdf(&page, b"", &[]), "deep.pdf");
    cannot_be_read(&message, "PDF");
}

/// Nesting within the parser's limit is read.
#[test]
fn nesting_within_the_limit_is_read() {
    let page = format!("/Deep {}{}", "[".repeat(100), "]".repeat(100));
    let world = world(pdf(&page, b"0 0 1 1 re f", &[]), "nested.pdf");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(
        compiled
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "{:#?}",
        compiled.diagnostics
    );
}

/// A 40,000 × 40,000 image XObject in a PDF figure: it took gigabytes
/// before #333.
#[test]
fn a_huge_image_in_a_pdf() {
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 40000 /Height 40000 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8",
        b"",
    );
    let bytes = pdf(
        "/Resources << /XObject << /I 5 0 R >> >>",
        b"100 0 0 100 0 0 cm /I Do",
        &[image],
    );
    let message = error(bytes, "huge.pdf");
    cannot_be_read(&message, "PDF");
    assert!(message.contains("256 MB"), "{message}");
}

/// An inline image counts too.
#[test]
fn a_huge_inline_image_in_a_pdf() {
    let bytes = pdf(
        "",
        b"q 100 0 0 100 0 0 cm BI /W 40000 /H 40000 /CS /RGB /BPC 8 ID \x00 EI Q",
        &[],
    );
    let message = error(bytes, "inline.pdf");
    cannot_be_read(&message, "PDF");
    assert!(message.contains("256 MB"), "{message}");
}

/// A 40,000 × 40,000 PNG as a `data:` URL in an SVG drawing: `check` was
/// silent and `build` aborted before #333.
#[test]
fn a_huge_raster_in_an_svg() {
    let message = error(svg_with_png(&png_header(40_000, 40_000, 8, 6)), "huge.svg");
    cannot_be_read(&message, "SVG");
    assert!(message.contains("256 MB"), "{message}");
}

/// Rasters in an SVG drawing count together.
#[test]
fn rasters_in_an_svg_count_together() {
    let a = base64(&png_header(6_000, 6_000, 8, 6));
    let b = base64(&png_header(6_000, 6_001, 8, 6));
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image width="5" height="5" href="data:image/png;base64,{a}"/><image x="5" width="5" height="5" href="data:image/png;base64,{b}"/></svg>"#
    );
    let message = error(svg.into_bytes(), "two.svg");
    cannot_be_read(&message, "SVG");
    assert!(message.contains("275 MB"), "{message}");
}

/// An 8,192 × 8,192 16-bit RGBA PNG takes eight bytes a pixel, 512 MB:
/// more than the limit, which counts bytes, not pixels.
#[test]
fn a_sixteen_bit_png_counts_its_bytes() {
    let world = world(png_header(8_192, 8_192, 16, 6), "deep.png");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let error = compiled
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
        .unwrap_or_else(|| panic!("an error: {:#?}", compiled.diagnostics));
    assert_eq!(
        error.message,
        "this PNG image is too large: 8192 × 8192 pixels, 512 MB decoded, more than the 256 MB Folio reads"
    );
}

/// A build of a figure that is read, within [`BOUND`], with no error.
fn read(bytes: Vec<u8>, name: &str) {
    let world = world(bytes, name);
    let start = Instant::now();
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(start.elapsed() < BOUND, "build took {:?}", start.elapsed());
    assert!(
        compiled
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "{:#?}",
        compiled.diagnostics
    );
}

/// An image drawn many times counts once: one decoding of it is in memory
/// at a time.
#[test]
fn an_image_drawn_often_counts_once() {
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 4000 /Height 4000 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8",
        b"",
    );
    let content = "q 5 0 0 5 0 0 cm /I Do Q ".repeat(20);
    read(
        pdf(
            "/Resources << /XObject << /I 5 0 R >> >>",
            content.as_bytes(),
            &[image],
        ),
        "often.pdf",
    );
}

/// JBIG2 and CCITT images take their size from their own data or decode
/// parameters, not the image dictionary; one that claims a huge page is
/// not decoded (the figure is drawn without it), rather than taking
/// gigabytes.
#[test]
fn a_huge_ccitt_image_behind_a_small_dictionary() {
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
         /BitsPerComponent 1 /Filter /CCITTFaxDecode \
         /DecodeParms << /K -1 /Columns 1000000 /Rows 100000 >>",
        b"\x00\x00\x00\x00",
    );
    read(
        pdf(
            "/Resources << /XObject << /I 5 0 R >> >>",
            b"100 0 0 100 0 0 cm /I Do",
            &[image],
        ),
        "ccitt.pdf",
    );
}
