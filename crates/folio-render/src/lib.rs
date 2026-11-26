//! Display lists, their binary wire format, and rendering (SPEC §1.5,
//! §12.2).
//!
//! A [`DisplayList`] holds the drawing commands for one fragment, such as
//! a page, flattened in painting order: glyph runs, paths, the frames of
//! objects whose content is not laid out yet, and the boxes of nested
//! fragments. Every item names the source node it is drawn for, and every
//! glyph the bytes of its node's text, so a list is also its fragment's
//! span map ([`SpanMap`]).
//!
//! [`wire`] encodes lists in one compact, versioned binary format, which the
//! preview's websocket, Folio Studio's `MessagePort` and live clients use
//! unchanged. [`render`] rasterises a list at a scale, [`rasterize_glyph`]
//! one glyph at a size for clients that draw lists themselves, and [`svg`]
//! writes a list as SVG; [`Image::png`] encodes a rendering as PNG.
//!
//! The crate uses no threads, no async runtime and no file system, so it
//! compiles to WebAssembly (SPEC §16.1). Fonts arrive as bytes, through
//! [`Fonts`].
//!
//! ```
//! use folio_component::{Abs, Fragment, Path, Point, Shape, Size, Color, FragmentItem, SourceRef};
//! use folio_render::{DisplayList, Options, Outlines, wire};
//!
//! let mut page = Fragment::new(Size::new(Abs::pt(20.0), Abs::pt(10.0)));
//! let square = Shape::filled(Path::rect(Abs::pt(5.0), Abs::pt(5.0)), Color::BLACK);
//! page.push(
//!     Point::new(Abs::pt(2.0), Abs::pt(2.0)),
//!     FragmentItem::Shape { shape: square, source: SourceRef::DETACHED },
//! );
//! let list = DisplayList::from_fragment(&page);
//!
//! let bytes = wire::encode(&list);
//! assert_eq!(wire::decode(&bytes), Ok(wire::Message::List(list.clone())));
//!
//! let fonts: Vec<folio_render::FontBytes<'_>> = Vec::new();
//! let image = folio_render::render(&list, &fonts, &mut Outlines::new(), &Options::default())
//!     .unwrap();
//! assert_eq!((image.width, image.height), (20, 10));
//! ```

#![forbid(unsafe_code)]

mod list;
mod raster;
mod svg;
pub mod wire;

pub use self::list::{
    Command, DisplayList, FRAME_THICKNESS, GlyphItem, GlyphSpan, Glyphs, Hit, Item, PathShape,
    Point, Rect, Segment, Span, SpanMap, SpanMismatch, Stroke,
};
pub use self::raster::{
    FontBytes, Fonts, GlyphBitmap, Image, MAX_SIDE, Options, Outlines, RenderError,
    rasterize_glyph, render,
};
pub use self::svg::svg;
