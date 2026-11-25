//! PDF output through `krilla` (SPEC §11.1): tagging, PDF/UA, PDF/A, font
//! subsetting, colour management and import of PDF figures.
//!
//! The Emit stage (SPEC §2.1) hands [`write()`] a build's pages as fragments,
//! with the fonts their glyph runs name. Until the writer goes through
//! krilla, it writes a valid, untagged PDF 1.7 with one page of the right
//! size per fragment and nothing drawn on it, so the pipeline runs end to
//! end. Like every writer, it reads no clock: the same pages give the same
//! bytes (SPEC §2.4).

#![forbid(unsafe_code)]

use std::fmt::{self, Debug, Formatter, Write as _};

use folio_component::{FontId, Fragment};
use folio_fonts::Font;

/// What the PDF writer writes: a build's pages, in order, and the fonts
/// their glyph runs name.
pub struct Document<'a> {
    /// The pages: fragments whose size is the page's, holding everything
    /// on it.
    pub pages: Vec<&'a Fragment>,
    /// The fonts that glyph runs name by [`FontId`].
    pub fonts: &'a dyn FontTable,
}

impl Debug for Document<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("pages", &self.pages)
            .finish_non_exhaustive()
    }
}

/// The fonts of a build, by the ids that glyph runs name them by.
pub trait FontTable {
    /// The font with an id, or `None` when it cannot be loaded.
    fn font(&self, id: FontId) -> Option<Font>;
}

/// Writes a document as a PDF file.
pub fn write(document: &Document<'_>) -> Vec<u8> {
    let mut pdf = Writer::default();
    pdf.out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");

    // Objects 1 and 2 are the catalog and the page tree; each page then
    // takes two: the page and its content stream.
    let page_id = |i: usize| 3 + 2 * i;
    pdf.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let mut kids = String::new();
    for i in 0..document.pages.len() {
        if i > 0 {
            kids.push(' ');
        }
        write!(kids, "{} 0 R", page_id(i)).expect("writing to a string");
    }
    pdf.object(
        2,
        &format!(
            "<< /Type /Pages /Kids [{kids}] /Count {} >>",
            document.pages.len()
        ),
    );
    for (i, page) in document.pages.iter().enumerate() {
        let id = page_id(i);
        let width = number(page.size.width.to_pt());
        let height = number(page.size.height.to_pt());
        pdf.object(
            id,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
                 /Resources << >> /Contents {} 0 R >>",
                id + 1
            ),
        );
        pdf.object(id + 1, "<< /Length 0 >>\nstream\n\nendstream");
    }
    pdf.finish(1)
}

/// A PDF file being written: its bytes, and the offset of each object.
#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    offsets: Vec<usize>,
}

impl Writer {
    /// Writes the object with an id, which must be the next one.
    fn object(&mut self, id: usize, body: &str) {
        debug_assert_eq!(id, self.offsets.len() + 1, "objects are written in order");
        self.offsets.push(self.out.len());
        self.out
            .extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    /// Writes the cross-reference table and the trailer.
    fn finish(mut self, root: usize) -> Vec<u8> {
        let xref = self.out.len();
        let mut tail = format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len() + 1);
        for offset in &self.offsets {
            writeln!(tail, "{offset:010} 00000 n ").expect("writing to a string");
        }
        write!(
            tail,
            "trailer\n<< /Size {} /Root {root} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            self.offsets.len() + 1
        )
        .expect("writing to a string");
        self.out.extend_from_slice(tail.as_bytes());
        self.out
    }
}

/// A length in points as a PDF number: at most three decimals, without
/// trailing zeros, and never negative zero.
fn number(value: f64) -> String {
    let value = if value.is_finite() { value } else { 0.0 };
    let rounded = (value * 1000.0).round() / 1000.0;
    let mut text = format!("{:.3}", if rounded == 0.0 { 0.0 } else { rounded });
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use folio_component::{Abs, Size};

    struct NoFonts;

    impl FontTable for NoFonts {
        fn font(&self, _: FontId) -> Option<Font> {
            None
        }
    }

    fn a4() -> Fragment {
        Fragment::new(Size::new(Abs::pt(595.276), Abs::pt(841.89)))
    }

    #[test]
    fn writes_one_page_per_fragment() {
        let (first, second) = (a4(), a4());
        let pdf = write(&Document {
            pages: vec![&first, &second],
            fonts: &NoFonts,
        });
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.7\n"));
        assert!(text.ends_with("%%EOF\n"));
        assert!(text.contains("/Kids [3 0 R 5 0 R] /Count 2"));
        assert_eq!(text.matches("/MediaBox [0 0 595.276 841.89]").count(), 2);
        assert_eq!(
            pdf,
            write(&Document {
                pages: vec![&first, &second],
                fonts: &NoFonts,
            })
        );
    }

    /// Every entry of the cross-reference table points at its object.
    #[test]
    fn offsets_point_at_objects() {
        let page = a4();
        let pdf = write(&Document {
            pages: vec![&page],
            fonts: &NoFonts,
        });
        // Everything after the binary comment of the header is ASCII.
        let ascii = |from: usize| std::str::from_utf8(&pdf[from..]).unwrap();
        let start: usize = ascii(16)
            .rsplit("startxref\n")
            .next()
            .and_then(|tail| tail.lines().next())
            .and_then(|line| line.parse().ok())
            .unwrap();
        assert!(ascii(start).starts_with("xref\n0 5\n"));
        let entries = ascii(start).lines().skip(3).take(4);
        for (id, entry) in (1..).zip(entries) {
            let offset: usize = entry[..10].parse().unwrap();
            assert!(ascii(offset).starts_with(&format!("{id} 0 obj\n")));
        }
    }

    #[test]
    fn numbers() {
        assert_eq!(number(595.2756), "595.276");
        assert_eq!(number(842.0), "842");
        assert_eq!(number(-0.0001), "0");
        assert_eq!(number(f64::NAN), "0");
    }
}
