//! Images as layout places them and writers draw them (SPEC §7.2, §11.1):
//! the bytes of an image file and its format, shared, with a hash of the
//! bytes taken once, so that fragments that hold an image compare and hash
//! cheaply.

use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// The format of an image file, as its first bytes tell it.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ImageFormat {
    /// A JPEG image, which PDF shows as it is.
    Jpeg,
    /// A PNG image.
    Png,
    /// A WebP image.
    Webp,
    /// A GIF image (its first frame).
    Gif,
    /// An SVG drawing, which stays vector.
    Svg,
    /// A page of a PDF document: the first.
    Pdf,
}

impl ImageFormat {
    /// The format of a file from its first bytes, or `None` for a file of a
    /// format Folio does not read. An SVG file is told by an `<svg` element
    /// near its start, after an XML declaration, comments or a doctype.
    pub fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.starts_with(b"%PDF-") {
            Some(Self::Pdf)
        } else if is_svg(bytes) {
            Some(Self::Svg)
        } else {
            None
        }
    }

    /// Whether images of the format are drawings rather than pixels.
    pub fn is_vector(self) -> bool {
        matches!(self, Self::Svg | Self::Pdf)
    }

    /// The format's name, for messages: `PNG`, `SVG`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Webp => "WebP",
            Self::Gif => "GIF",
            Self::Svg => "SVG",
            Self::Pdf => "PDF",
        }
    }
}

/// Whether a file looks like SVG: after a byte order mark, white space,
/// and the XML prologue (declaration, processing instructions, comments
/// and a doctype, with its internal subset), its first element is `svg`,
/// with or without a prefix. Gzip-compressed SVG is not read.
fn is_svg(bytes: &[u8]) -> bool {
    let mut rest = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    loop {
        rest = rest.trim_ascii_start();
        if rest.starts_with(b"<?") {
            rest = after(rest, b"?>");
        } else if rest.starts_with(b"<!--") {
            rest = after(rest, b"-->");
        } else if rest.starts_with(b"<!") {
            // A doctype, whose internal subset in brackets may hold `>`.
            let mut depth = 0usize;
            let end = rest.iter().position(|&b| {
                match b {
                    b'[' => depth += 1,
                    b']' => depth = depth.saturating_sub(1),
                    b'>' if depth == 0 => return true,
                    _ => {}
                }
                false
            });
            rest = end.map_or(&[], |i| &rest[i + 1..]);
        } else {
            break;
        }
    }
    let Some(tag) = rest.strip_prefix(b"<") else {
        return false;
    };
    let name_end = tag
        .iter()
        .position(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'))
        .unwrap_or(tag.len());
    let name = &tag[..name_end];
    let local = name.rsplit(|&b| b == b':').next().unwrap_or(name);
    local == b"svg"
}

/// The bytes after the first `end` in `bytes`, or none.
fn after<'a>(bytes: &'a [u8], end: &[u8]) -> &'a [u8] {
    bytes
        .windows(end.len())
        .position(|w| w == end)
        .map_or(&[], |i| &bytes[i + end.len()..])
}

/// An image file, cheap to clone: its bytes and format. Two images are
/// equal when their bytes are; they hash by a hash of their bytes, taken
/// when the image is made.
#[derive(Clone)]
pub struct Image(Arc<Repr>);

struct Repr {
    bytes: Arc<[u8]>,
    format: ImageFormat,
    hash: u64,
}

impl Image {
    /// An image of a format from its bytes. The format is not checked
    /// against the bytes ([`ImageFormat::detect`] tells it).
    pub fn new(bytes: Arc<[u8]>, format: ImageFormat) -> Self {
        let hash = hash_bytes(&bytes);
        Self(Arc::new(Repr {
            bytes,
            format,
            hash,
        }))
    }

    /// The file's bytes.
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.0.bytes
    }

    /// Its format.
    pub fn format(&self) -> ImageFormat {
        self.0.format
    }

    /// A hash of its bytes, the same on every platform and in every run.
    pub fn content_hash(&self) -> u64 {
        self.0.hash
    }
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.hash == other.0.hash
                && self.0.format == other.0.format
                && (Arc::ptr_eq(&self.0.bytes, &other.0.bytes) || self.0.bytes == other.0.bytes))
    }
}

impl Eq for Image {}

impl Hash for Image {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.format.hash(state);
        state.write_u64(self.0.hash);
    }
}

impl Debug for Image {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Image({}, {} bytes, {:016x})",
            self.0.format.name(),
            self.0.bytes.len(),
            self.0.hash
        )
    }
}

/// A hash of bytes that does not depend on the platform or the run: each
/// word of eight bytes, little-endian, is mixed in by a rotation, an
/// exclusive or and a multiplication by an odd constant, after the length.
fn hash_bytes(bytes: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut state: u64 = 0x243f_6a88_85a3_08d3;
    let mut mix = |word: u64| state = (state.rotate_left(5) ^ word).wrapping_mul(K);
    mix(bytes.len() as u64);
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        mix(u64::from_le_bytes(chunk.try_into().expect("eight bytes")));
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let mut word = [0; 8];
        word[..rest.len()].copy_from_slice(rest);
        mix(u64::from_le_bytes(word));
    }
    // A final avalanche, so that close inputs give distant hashes.
    let mut h = state;
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats() {
        assert_eq!(
            ImageFormat::detect(b"\xFF\xD8\xFF\xE0..."),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            ImageFormat::detect(b"\x89PNG\r\n\x1a\n...."),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            ImageFormat::detect(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(ImageFormat::Webp)
        );
        assert_eq!(ImageFormat::detect(b"GIF89a.."), Some(ImageFormat::Gif));
        assert_eq!(ImageFormat::detect(b"%PDF-1.7\n"), Some(ImageFormat::Pdf));
        assert_eq!(
            ImageFormat::detect(b"<?xml version=\"1.0\"?>\n<svg xmlns=\"\"/>"),
            Some(ImageFormat::Svg)
        );
        let long = format!(
            "<?xml version=\"1.0\"?>\n<!-- {} -->\n<!DOCTYPE svg [<!ENTITY a \"<b>\">]>\n<svg:svg xmlns:svg=\"\"/>",
            "licence ".repeat(400)
        );
        assert_eq!(ImageFormat::detect(long.as_bytes()), Some(ImageFormat::Svg));
        assert_eq!(ImageFormat::detect(b"<html><body><svg/>"), None);
        assert_eq!(ImageFormat::detect(b"<svgx/>"), None);
        assert_eq!(ImageFormat::detect(b"<html><body>"), None);
        assert_eq!(ImageFormat::detect(b"hello"), None);
        assert_eq!(ImageFormat::detect(b""), None);
    }

    #[test]
    fn equal_by_bytes() {
        let a = Image::new(Arc::from(&b"abc"[..]), ImageFormat::Png);
        let b = Image::new(Arc::from(&b"abc"[..]), ImageFormat::Png);
        let c = Image::new(Arc::from(&b"abd"[..]), ImageFormat::Png);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a.content_hash(), c.content_hash());
        assert_eq!(a.content_hash(), b.content_hash());
    }
}
