//! The binary wire format of display lists (SPEC §12.2): one compact,
//! versioned encoding, used unchanged by the preview's websocket, Folio
//! Studio's `MessagePort` and live clients.
//!
//! # Layout
//!
//! A message starts with the four bytes `F`, `D`, `L` and the format
//! [`VERSION`], then a kind byte: [`Message::List`] (`0`), a whole display
//! list, or [`Message::Spans`] (`1`), only its span map, for a page that
//! looks the same after an edit but whose sources moved. Integers are
//! unsigned LEB128 varints, signed ones zigzag-encoded first; lengths are
//! little-endian IEEE 754 `f32`s and must be finite; text is a varint byte
//! length and UTF-8.
//!
//! A list is its width, height and span, the number of items, and each
//! item as a tag, the byte length of its body, and the body: the item's
//! span, then its command. A glyph stores its id, advance and offsets
//! (offsets only when not zero, as flagged), its range of the run's text
//! as the distance from the previous glyph's end and a length, and its
//! span-map entry as a difference from the previous glyph's node and the
//! same pair for the node's bytes. A span map holds the same spans in the
//! same order, without the drawing.
//!
//! # Compatibility
//!
//! Every item's body starts with its span, in every version. A decoder
//! keeps an item whose tag it does not know as [`Command::Unknown`], with
//! its span and nothing to draw, so that the items keep the indices a span
//! map names them by; and it skips the bytes at the end of an item's body
//! that it does not read. A later version of the format may therefore add
//! commands, and fields at the end of an item, without a new version.
//! Every other change raises [`VERSION`], and a decoder rejects a version
//! it does not know.
//!
//! [`Command::Unknown`]: crate::Command::Unknown

use std::fmt::{self, Display, Formatter};
use std::ops::Range;

use folio_component::{Color, FillRule, FontId, LineCap, LineJoin, SourceRef};

use crate::list::{
    Command, DisplayList, GlyphItem, Glyphs, Item, PathShape, Point, Rect, Segment, Span, SpanMap,
    Stroke, Unknown,
};

/// The magic bytes every message starts with.
pub const MAGIC: [u8; 3] = *b"FDL";

/// The version of the format this crate writes and reads.
pub const VERSION: u8 = 1;

const KIND_LIST: u8 = 0;
const KIND_SPANS: u8 = 1;

const TAG_GLYPHS: u64 = 0;
const TAG_PATH: u64 = 1;
const TAG_FRAME: u64 = 2;
const TAG_BOX: u64 = 3;

const SEG_MOVE: u8 = 0;
const SEG_LINE: u8 = 1;
const SEG_CUBIC: u8 = 2;
const SEG_CLOSE: u8 = 3;

const PAINT_SOLID: u8 = 0;

/// How many elements of a list the decoder reserves room for before it
/// reads them: a count only bounds how many elements the bytes left could
/// hold, and an element takes more memory than its fewest bytes, so a
/// longer list grows as it is read.
const PREALLOCATE: usize = 256;

/// A decoded message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A whole display list.
    List(DisplayList),
    /// The span map of a list the receiver already has
    /// ([`DisplayList::set_spans`]).
    Spans(SpanMap),
}

/// Encodes a whole display list.
///
/// Its lengths must be finite, as those of a list made from a fragment
/// are: [`decode`] refuses a message with one that is not.
pub fn encode(list: &DisplayList) -> Vec<u8> {
    let mut w = Writer::header(KIND_LIST);
    w.f32(list.width);
    w.f32(list.height);
    w.span(list.span);
    w.uint(list.items.len() as u64);
    let mut body = Writer::default();
    for item in &list.items {
        body.buf.clear();
        body.span(item.span);
        let tag = match &item.command {
            Command::Glyphs(run) => {
                body.glyphs(run, item.span.source);
                TAG_GLYPHS
            }
            Command::Path(shape) => {
                body.path(shape);
                TAG_PATH
            }
            Command::Frame(rect) => {
                body.rect(*rect);
                TAG_FRAME
            }
            Command::Box(rect) => {
                body.rect(*rect);
                TAG_BOX
            }
            Command::Unknown(unknown) => unknown.tag,
        };
        w.uint(tag);
        w.uint(body.buf.len() as u64);
        w.buf.extend_from_slice(&body.buf);
    }
    w.buf
}

/// Encodes the span map of a display list alone.
pub fn encode_spans(spans: &SpanMap) -> Vec<u8> {
    let mut w = Writer::header(KIND_SPANS);
    w.span(spans.span);
    w.uint(spans.items.len() as u64);
    for (span, glyphs) in &spans.items {
        w.span(*span);
        w.uint(glyphs.len() as u64);
        let mut prev = (span.source, 0);
        for source in glyphs {
            w.glyph_source(source, &mut prev);
        }
    }
    w.buf
}

/// Decodes a message.
///
/// # Errors
///
/// When the bytes are not a message of this version of the format, or are
/// cut short, or hold a length that is not finite or text that is not
/// UTF-8. Decoding never panics, whatever the bytes, and allocates memory
/// in proportion to the elements it has read, which the input's size
/// bounds, never to a count the input claims.
pub fn decode(bytes: &[u8]) -> Result<Message, DecodeError> {
    let mut r = Reader { bytes, pos: 0 };
    if r.take(3)? != MAGIC {
        return Err(DecodeError::Magic);
    }
    let version = r.u8()?;
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    let message = match r.u8()? {
        KIND_LIST => Message::List(r.list()?),
        KIND_SPANS => Message::Spans(r.spans()?),
        kind => return Err(DecodeError::Kind(kind)),
    };
    if r.pos != bytes.len() {
        return Err(DecodeError::Trailing);
    }
    Ok(message)
}

/// Why bytes could not be decoded.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum DecodeError {
    /// The bytes do not start with [`MAGIC`].
    Magic,
    /// The message is of a version this decoder does not read.
    Version(u8),
    /// The message is of a kind this version does not have.
    Kind(u8),
    /// The bytes end in the middle of a value.
    Truncated,
    /// A varint is longer than its type, or a count larger than the bytes
    /// left could hold.
    Overflow,
    /// A length is not finite.
    NotFinite,
    /// Text is not UTF-8, or a glyph's range of it is out of bounds,
    /// reversed or not on character boundaries.
    Text,
    /// A value of an enumeration this version does not have.
    Invalid,
    /// Bytes follow the message.
    Trailing,
}

impl Display for DecodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Magic => f.write_str("not a display list: the magic bytes are missing"),
            Self::Version(v) => write!(
                f,
                "display list format version {v} is not supported (this is version {VERSION})"
            ),
            Self::Kind(k) => write!(f, "unknown display list message kind {k}"),
            Self::Truncated => f.write_str("the display list ends early"),
            Self::Overflow => f.write_str("a number in the display list is out of range"),
            Self::NotFinite => f.write_str("a length in the display list is not finite"),
            Self::Text => f.write_str("text in the display list is not valid"),
            Self::Invalid => f.write_str("an unknown value in the display list"),
            Self::Trailing => f.write_str("bytes follow the display list"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[derive(Default)]
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn header(kind: u8) -> Self {
        let mut buf = MAGIC.to_vec();
        buf.push(VERSION);
        buf.push(kind);
        Self { buf }
    }

    fn uint(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    fn int(&mut self, value: i64) {
        self.uint(((value << 1) ^ (value >> 63)) as u64);
    }

    fn f32(&mut self, value: f32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    fn point(&mut self, p: Point) {
        self.f32(p.x);
        self.f32(p.y);
    }

    fn rect(&mut self, rect: Rect) {
        self.f32(rect.x);
        self.f32(rect.y);
        self.f32(rect.width);
        self.f32(rect.height);
    }

    fn color(&mut self, color: Color) {
        self.buf
            .extend_from_slice(&[color.r, color.g, color.b, color.a]);
    }

    fn paint(&mut self, color: Color) {
        self.buf.push(PAINT_SOLID);
        self.color(color);
    }

    fn span(&mut self, span: Span) {
        self.uint(span.source.0);
        self.uint(u64::from(span.instance));
    }

    /// A glyph's span-map entry, as differences from the previous glyph's
    /// node and the end of its bytes.
    fn glyph_source(
        &mut self,
        (source, bytes): &(SourceRef, Range<u32>),
        prev: &mut (SourceRef, u32),
    ) {
        if *source != prev.0 {
            prev.1 = 0;
        }
        self.int(source.0.wrapping_sub(prev.0.0) as i64);
        self.int(i64::from(bytes.start) - i64::from(prev.1));
        self.int(i64::from(bytes.end) - i64::from(bytes.start));
        *prev = (*source, bytes.end);
    }

    fn glyphs(&mut self, run: &Glyphs, node: SourceRef) {
        self.point(run.origin);
        self.uint(u64::from(run.font.0));
        self.f32(run.size);
        self.paint(run.fill);
        self.uint(run.text.len() as u64);
        self.buf.extend_from_slice(run.text.as_bytes());
        self.uint(run.glyphs.len() as u64);
        let mut text_end = 0u32;
        let mut prev = (node, 0);
        for glyph in &run.glyphs {
            self.uint(u64::from(glyph.id));
            self.f32(glyph.advance);
            // An offset is left out when it is `0.0`, by its bits, so that
            // `-0.0` survives the round trip.
            let (dx, dy) = (glyph.dx.to_bits() != 0, glyph.dy.to_bits() != 0);
            self.buf.push(u8::from(dx) | (u8::from(dy) << 1));
            if dx {
                self.f32(glyph.dx);
            }
            if dy {
                self.f32(glyph.dy);
            }
            self.int(i64::from(glyph.text.start) - i64::from(text_end));
            self.int(i64::from(glyph.text.end) - i64::from(glyph.text.start));
            text_end = glyph.text.end;
            self.glyph_source(&glyph.source, &mut prev);
        }
    }

    fn path(&mut self, shape: &PathShape) {
        self.uint(shape.segments.len() as u64);
        for segment in &shape.segments {
            match *segment {
                Segment::MoveTo(p) => {
                    self.buf.push(SEG_MOVE);
                    self.point(p);
                }
                Segment::LineTo(p) => {
                    self.buf.push(SEG_LINE);
                    self.point(p);
                }
                Segment::CubicTo(a, b, c) => {
                    self.buf.push(SEG_CUBIC);
                    self.point(a);
                    self.point(b);
                    self.point(c);
                }
                Segment::Close => self.buf.push(SEG_CLOSE),
            }
        }
        match shape.fill {
            Some(color) => {
                self.buf.push(1);
                self.paint(color);
            }
            None => self.buf.push(0),
        }
        self.buf.push(match shape.fill_rule {
            FillRule::NonZero => 0,
            FillRule::EvenOdd => 1,
        });
        match &shape.stroke {
            Some(stroke) => {
                self.buf.push(1);
                self.paint(stroke.color);
                self.f32(stroke.thickness);
                self.buf.push(match stroke.cap {
                    LineCap::Butt => 0,
                    LineCap::Round => 1,
                    LineCap::Square => 2,
                });
                self.buf.push(match stroke.join {
                    LineJoin::Miter => 0,
                    LineJoin::Round => 1,
                    LineJoin::Bevel => 2,
                });
                self.f32(stroke.miter_limit);
                match &stroke.dash {
                    Some((array, phase)) => {
                        self.buf.push(1);
                        self.uint(array.len() as u64);
                        for &d in array {
                            self.f32(d);
                        }
                        self.f32(*phase);
                    }
                    None => self.buf.push(0),
                }
            }
            None => self.buf.push(0),
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if n > self.remaining() {
            return Err(DecodeError::Truncated);
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn uint(&mut self) -> Result<u64, DecodeError> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.u8()?;
            let bits = u64::from(byte & 0x7f);
            if shift == 63 && bits > 1 {
                return Err(DecodeError::Overflow);
            }
            value |= bits << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(DecodeError::Overflow)
    }

    fn int(&mut self) -> Result<i64, DecodeError> {
        let value = self.uint()?;
        Ok(((value >> 1) as i64) ^ -((value & 1) as i64))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        u32::try_from(self.uint()?).map_err(|_| DecodeError::Overflow)
    }

    /// A count of things that each take at least `min` bytes, checked
    /// against the bytes left so that no allocation exceeds the input.
    fn count(&mut self, min: usize) -> Result<usize, DecodeError> {
        let count = usize::try_from(self.uint()?).map_err(|_| DecodeError::Overflow)?;
        if count.saturating_mul(min) > self.remaining() {
            return Err(DecodeError::Overflow);
        }
        Ok(count)
    }

    fn f32(&mut self) -> Result<f32, DecodeError> {
        let bytes = self.take(4)?;
        let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(DecodeError::NotFinite)
        }
    }

    fn point(&mut self) -> Result<Point, DecodeError> {
        Ok(Point {
            x: self.f32()?,
            y: self.f32()?,
        })
    }

    fn rect(&mut self) -> Result<Rect, DecodeError> {
        Ok(Rect {
            x: self.f32()?,
            y: self.f32()?,
            width: self.f32()?,
            height: self.f32()?,
        })
    }

    fn color(&mut self) -> Result<Color, DecodeError> {
        let c = self.take(4)?;
        Ok(Color::rgba(c[0], c[1], c[2], c[3]))
    }

    fn paint(&mut self) -> Result<Color, DecodeError> {
        match self.u8()? {
            PAINT_SOLID => self.color(),
            _ => Err(DecodeError::Invalid),
        }
    }

    fn flag(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::Invalid),
        }
    }

    fn span(&mut self) -> Result<Span, DecodeError> {
        Ok(Span {
            source: SourceRef(self.uint()?),
            instance: self.u32()?,
        })
    }

    /// A range given as the distance of its start from `base` and the
    /// distance of its end from its start.
    fn range(&mut self, base: u32) -> Result<Range<u32>, DecodeError> {
        let start = i64::from(base)
            .checked_add(self.int()?)
            .ok_or(DecodeError::Overflow)?;
        let end = start
            .checked_add(self.int()?)
            .ok_or(DecodeError::Overflow)?;
        let start = u32::try_from(start).map_err(|_| DecodeError::Overflow)?;
        let end = u32::try_from(end).map_err(|_| DecodeError::Overflow)?;
        Ok(start..end)
    }

    fn glyph_source(
        &mut self,
        prev: &mut (SourceRef, u32),
    ) -> Result<(SourceRef, Range<u32>), DecodeError> {
        let source = SourceRef(prev.0.0.wrapping_add(self.int()? as u64));
        if source != prev.0 {
            prev.1 = 0;
        }
        let bytes = self.range(prev.1)?;
        *prev = (source, bytes.end);
        Ok((source, bytes))
    }

    fn list(&mut self) -> Result<DisplayList, DecodeError> {
        let width = self.f32()?;
        let height = self.f32()?;
        let span = self.span()?;
        // A tag, a length and a span of two varints at least.
        let count = self.count(4)?;
        let mut items = Vec::with_capacity(count.min(PREALLOCATE));
        for _ in 0..count {
            let tag = self.uint()?;
            let len = usize::try_from(self.uint()?).map_err(|_| DecodeError::Overflow)?;
            let mut body = Reader {
                bytes: self.take(len)?,
                pos: 0,
            };
            let span = body.span()?;
            let command = match tag {
                TAG_GLYPHS => Command::Glyphs(body.glyphs(span.source)?),
                TAG_PATH => Command::Path(body.path()?),
                TAG_FRAME => Command::Frame(body.rect()?),
                TAG_BOX => Command::Box(body.rect()?),
                // A command of a later version of the format: its span is
                // kept, the rest of its body skipped.
                tag => Command::Unknown(Unknown { tag }),
            };
            items.push(Item { command, span });
        }
        Ok(DisplayList {
            width,
            height,
            span,
            items,
        })
    }

    fn spans(&mut self) -> Result<SpanMap, DecodeError> {
        let span = self.span()?;
        let count = self.count(3)?;
        let mut items = Vec::with_capacity(count.min(PREALLOCATE));
        for _ in 0..count {
            let span = self.span()?;
            let glyphs = self.count(3)?;
            let mut prev = (span.source, 0);
            let glyphs = (0..glyphs)
                .map(|_| self.glyph_source(&mut prev))
                .collect::<Result<Vec<_>, _>>()?;
            items.push((span, glyphs));
        }
        Ok(SpanMap { span, items })
    }

    fn glyphs(&mut self, node: SourceRef) -> Result<Glyphs, DecodeError> {
        let origin = self.point()?;
        let font = FontId(self.u32()?);
        let size = self.f32()?;
        let fill = self.paint()?;
        let len = self.count(1)?;
        let text = std::str::from_utf8(self.take(len)?).map_err(|_| DecodeError::Text)?;
        // An id, an advance, flags, a range and a source: 10 bytes at least.
        let count = self.count(10)?;
        let mut glyphs = Vec::with_capacity(count.min(PREALLOCATE));
        let mut text_end = 0u32;
        let mut prev = (node, 0);
        for _ in 0..count {
            let id = u16::try_from(self.uint()?).map_err(|_| DecodeError::Overflow)?;
            let advance = self.f32()?;
            let flags = self.u8()?;
            if flags & !0b11 != 0 {
                return Err(DecodeError::Invalid);
            }
            let dx = if flags & 1 != 0 { self.f32()? } else { 0.0 };
            let dy = if flags & 2 != 0 { self.f32()? } else { 0.0 };
            let range = self.range(text_end)?;
            if range.start > range.end
                || !text.is_char_boundary(range.start as usize)
                || !text.is_char_boundary(range.end as usize)
            {
                return Err(DecodeError::Text);
            }
            text_end = range.end;
            let source = self.glyph_source(&mut prev)?;
            glyphs.push(GlyphItem {
                id,
                advance,
                dx,
                dy,
                text: range,
                source,
            });
        }
        Ok(Glyphs {
            origin,
            font,
            size,
            fill,
            text: text.into(),
            glyphs,
        })
    }

    fn path(&mut self) -> Result<PathShape, DecodeError> {
        let count = self.count(1)?;
        let mut segments = Vec::with_capacity(count.min(PREALLOCATE));
        for _ in 0..count {
            segments.push(match self.u8()? {
                SEG_MOVE => Segment::MoveTo(self.point()?),
                SEG_LINE => Segment::LineTo(self.point()?),
                SEG_CUBIC => Segment::CubicTo(self.point()?, self.point()?, self.point()?),
                SEG_CLOSE => Segment::Close,
                _ => return Err(DecodeError::Invalid),
            });
        }
        let fill = if self.flag()? {
            Some(self.paint()?)
        } else {
            None
        };
        let fill_rule = match self.u8()? {
            0 => FillRule::NonZero,
            1 => FillRule::EvenOdd,
            _ => return Err(DecodeError::Invalid),
        };
        let stroke = if self.flag()? {
            let color = self.paint()?;
            let thickness = self.f32()?;
            let cap = match self.u8()? {
                0 => LineCap::Butt,
                1 => LineCap::Round,
                2 => LineCap::Square,
                _ => return Err(DecodeError::Invalid),
            };
            let join = match self.u8()? {
                0 => LineJoin::Miter,
                1 => LineJoin::Round,
                2 => LineJoin::Bevel,
                _ => return Err(DecodeError::Invalid),
            };
            let miter_limit = self.f32()?;
            let dash = if self.flag()? {
                let count = self.count(4)?;
                let array = (0..count)
                    .map(|_| self.f32())
                    .collect::<Result<Vec<_>, _>>()?;
                Some((array, self.f32()?))
            } else {
                None
            };
            Some(Stroke {
                color,
                thickness,
                cap,
                join,
                miter_limit,
                dash,
            })
        } else {
            None
        };
        Ok(PathShape {
            segments,
            fill,
            fill_rule,
            stroke,
        })
    }
}
