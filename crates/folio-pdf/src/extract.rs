//! Text as a reader of a PDF sees it (SPEC §6.4): the text of the glyphs
//! drawn on each page, mapped back to Unicode through the fonts'
//! `ToUnicode` maps as a viewer's search and copy do, and every text
//! string of the file, which holds its metadata, bookmarks, alternate
//! descriptions and structure titles, and its XMP metadata.
//!
//! The leak check searches this text for what a variant hides, as a second
//! line of defence behind provenance: it reads the file that was written,
//! not what the writer was given.

use std::borrow::Cow;

use hayro_svg::hayro_interpret::font::Glyph;
use hayro_svg::hayro_interpret::hayro_cmap::BfString;
use hayro_svg::hayro_interpret::util::TransformExt as _;
use hayro_svg::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache,
    InterpreterSettings, Paint, PathDrawMode, SoftMask, interpret_page,
};
use hayro_svg::hayro_syntax::Pdf;
use hayro_svg::hayro_syntax::object::{Dict, MaybeRef, Object};
use kurbo::{Affine, BezPath, Rect};

/// The text of a PDF file as a reader sees it.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Extracted {
    /// The text of each page's glyphs, in the order they are drawn.
    pub pages: Vec<String>,
    /// The file's text strings, in the order of its objects: the document
    /// information, bookmark titles, alternate descriptions and the other
    /// strings of its dictionaries and arrays; and its XMP metadata, as
    /// the text between its tags.
    pub strings: Vec<String>,
}

/// Extracts the text of a PDF file as a reader sees it, or `None` when the
/// file cannot be read.
pub fn extract_text(pdf: &[u8]) -> Option<Extracted> {
    let pdf = Pdf::new(pdf.to_vec()).ok()?;
    let mut extracted = Extracted::default();
    let cache = InterpreterCache::new();
    for page in pdf.pages().iter() {
        let (width, height) = page.render_dimensions();
        let mut context = Context::new(
            page.initial_transform(true).to_kurbo(),
            Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            &cache,
            page.xref(),
            InterpreterSettings::default(),
        );
        let mut device = TextDevice::default();
        interpret_page(page, &mut context, &mut device);
        extracted.pages.push(device.text);
    }
    for object in pdf.objects() {
        strings(&object, &mut extracted.strings);
    }
    Some(extracted)
}

/// Collects the text strings of an object and of the objects it holds
/// directly; objects it refers to are collected as objects of the file.
fn strings(object: &Object<'_>, out: &mut Vec<String>) {
    match object {
        Object::String(string) => out.push(text_string(string.as_bytes())),
        Object::Array(array) => {
            for item in array.raw_iter() {
                if let MaybeRef::NotRef(item) = item {
                    strings(&item, out);
                }
            }
        }
        Object::Dict(dict) => dict_strings(dict, out),
        Object::Stream(stream) => {
            dict_strings(stream.dict(), out);
            let metadata = stream
                .dict()
                .get::<hayro_svg::hayro_syntax::object::Name<'_>>(b"Type")
                .is_some_and(|name| name.as_ref() == b"Metadata");
            if metadata && let Ok(data) = stream.decoded() {
                out.push(xml_text(&data));
            }
        }
        _ => {}
    }
}

/// Collects the text strings of a dictionary's values.
fn dict_strings(dict: &Dict<'_>, out: &mut Vec<String>) {
    for (_, value) in dict.entries() {
        if let MaybeRef::NotRef(value) = value {
            strings(&value, out);
        }
    }
}

/// A PDF text string: UTF-16BE after its byte order mark, UTF-8 after
/// its, or else PDFDocEncoding, read as Latin-1, which it matches for the
/// letters and digits the leak check compares.
fn text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| char::from(b)).collect()
    }
}

/// The text of XML between its tags, with the five predefined entities
/// and character references decoded.
fn xml_text(data: &[u8]) -> String {
    let xml: Cow<'_, str> = String::from_utf8_lossy(data);
    let mut out = String::new();
    let mut in_tag = false;
    let mut chars = xml.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            '&' if !in_tag => {
                // An entity ends at `;` within a few characters; an `&`
                // that starts none is text.
                let rest: String = chars.clone().take(ENTITY).collect();
                match rest.find(';') {
                    Some(end) => {
                        out.extend(entity_char(&rest[..end]));
                        for _ in rest[..=end].chars() {
                            chars.next();
                        }
                    }
                    None => out.push('&'),
                }
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// The longest XML entity or character reference read, with its `;`.
const ENTITY: usize = 12;

/// The character an XML entity or character reference stands for.
fn entity_char(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = entity.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// A device that keeps the Unicode text of the glyphs drawn and nothing
/// else, with a space where a glyph starts a new line (it is lower or
/// higher than the last, or left of it), so that words of two lines do not
/// run together.
#[derive(Default)]
struct TextDevice {
    text: String,
    /// Where the last glyph was drawn.
    last: Option<kurbo::Point>,
}

impl<'a> Device<'a> for TextDevice {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &Paint<'a>, _: &PathDrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        _: &Paint<'a>,
        _: &GlyphDrawMode,
    ) {
        let at = (transform * glyph_transform) * kurbo::Point::ZERO;
        if let Some(last) = self.last
            && ((at.y - last.y).abs() > 0.5 || at.x < last.x)
        {
            self.text.push(' ');
        }
        self.last = Some(at);
        match glyph.as_unicode() {
            Some(BfString::Char(c)) => self.text.push(c),
            Some(BfString::String(s)) => self.text.push_str(&s),
            None => {}
        }
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_text_decodes_entities_and_keeps_stray_ampersands() {
        assert_eq!(
            xml_text(b"<a x='1'>AT&amp;T &#x41;&#66;</a><b>R&D notes and more</b>").trim(),
            "AT&T AB  R&D notes and more"
        );
    }

    #[test]
    fn text_strings_are_read_by_their_encoding() {
        assert_eq!(
            text_string(&[0xFE, 0xFF, 0x00, 0x41, 0x01, 0x41]),
            "A\u{141}"
        );
        assert_eq!(text_string(b"Forces"), "Forces");
        assert_eq!(text_string(&[0xEF, 0xBB, 0xBF, 0xC5, 0x81]), "\u{141}");
    }
}
