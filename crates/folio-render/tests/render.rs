//! Rendering display lists: glyphs from font bytes, paths, frames, SVG and
//! PNG, and the display list of a fragment with its span map.

use folio_component::{
    Abs, Color, Em, FontId, Fragment, FragmentItem, Glyph, GlyphRun, Location, ObjectBox, Paint,
    Path, Point, Shape, Size, SourceRef,
};
use folio_render::{
    Command, DisplayList, FontBytes, Image, Options, Outlines, Span, rasterize_glyph, render, svg,
};
use skrifa::MetadataProvider;

fn font() -> FontBytes<'static> {
    let file = folio_fonts::bundled()
        .into_iter()
        .find(|f| matches!(f.origin(), folio_fonts::FontOrigin::Bundled(n) if n == "SourceSans3-Regular.otf"))
        .expect("a bundled sans");
    // The bundled data is compiled in, so it lives as long as the process.
    let data: &'static [u8] = Box::leak(file.data().to_vec().into_boxed_slice());
    FontBytes { data, index: 0 }
}

fn glyph_id(font: FontBytes<'_>, c: char) -> u16 {
    let font = skrifa::FontRef::from_index(font.data, font.index).unwrap();
    font.charmap().map(c).unwrap().to_u32() as u16
}

/// A page with "HI" in black at 20pt, a red square and an object's frame,
/// nested in a box.
fn page(font: FontBytes<'_>) -> Fragment {
    let source = SourceRef(42);
    let run = GlyphRun {
        font: FontId(0),
        size: Abs::pt(20.0),
        fill: Paint::Solid(Color::BLACK),
        text: "HI".into(),
        glyphs: ['H', 'I']
            .iter()
            .enumerate()
            .map(|(i, &c)| Glyph {
                id: glyph_id(font, c),
                x_advance: Em::new(0.6),
                x_offset: Em::new(0.0),
                y_offset: Em::new(0.0),
                range: i as u32..i as u32 + 1,
                source: (source, i as u32..i as u32 + 1),
            })
            .collect(),
        instances: Default::default(),
    };
    let mut inner =
        Fragment::new(Size::new(Abs::pt(60.0), Abs::pt(30.0))).with_location(Location {
            source,
            instance: 3,
        });
    inner.push(
        Point::new(Abs::pt(5.0), Abs::pt(22.0)),
        FragmentItem::Text(run),
    );
    let mut page = Fragment::new(Size::new(Abs::pt(100.0), Abs::pt(50.0)));
    page.push_fragment(Point::new(Abs::pt(10.0), Abs::pt(10.0)), inner);
    page.push(
        Point::new(Abs::pt(80.0), Abs::pt(5.0)),
        FragmentItem::Shape {
            shape: Shape::filled(
                Path::rect(Abs::pt(10.0), Abs::pt(10.0)),
                Color::rgb(255, 0, 0),
            ),
            source: SourceRef::DETACHED,
        },
    );
    page.push(
        Point::new(Abs::pt(75.0), Abs::pt(25.0)),
        FragmentItem::Object(ObjectBox {
            size: Size::new(Abs::pt(20.0), Abs::pt(20.0)),
            baseline: Abs::pt(20.0),
            location: Location {
                source: SourceRef(43),
                instance: 0,
            },
        }),
    );
    page
}

fn pixel(image: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    image.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn display_list_of_a_fragment() {
    let font = font();
    let list = DisplayList::from_fragment(&page(font));
    assert_eq!((list.width, list.height), (100.0, 50.0));
    assert_eq!(list.items.len(), 4);
    let Command::Box(rect) = &list.items[0].command else {
        panic!("a box first");
    };
    assert_eq!((rect.x, rect.y, rect.width), (10.0, 10.0, 60.0));
    assert_eq!(
        list.items[0].span,
        Span {
            source: SourceRef(42),
            instance: 3
        }
    );
    let Command::Glyphs(run) = &list.items[1].command else {
        panic!("glyphs second");
    };
    assert_eq!((run.origin.x, run.origin.y), (15.0, 32.0));
    assert_eq!(run.glyphs[1].advance, 12.0);
    assert_eq!(list.items[1].span, list.items[0].span);
    let placed: Vec<_> = run.positioned().map(|(at, _)| at.x).collect();
    assert_eq!(placed, [15.0, 27.0]);
    assert!(matches!(list.items[2].command, Command::Path(_)));
    assert!(matches!(list.items[3].command, Command::Frame(_)));

    // Hit-testing: the second glyph, inside the box.
    let hits = list.hit(30.0, 25.0);
    assert_eq!(hits[0].item, 1);
    assert_eq!(hits[0].glyph, Some(1));
    assert_eq!(hits[1].item, 0);
    assert!(list.hit(99.0, 1.0).is_empty());
}

#[test]
fn renders_glyphs_paths_and_frames() {
    let font = font();
    let list = DisplayList::from_fragment(&page(font));
    let fonts = vec![font];
    let mut outlines = Outlines::new();
    let image = render(&list, &fonts, &mut outlines, &Options::default()).unwrap();
    assert_eq!((image.width, image.height), (100, 50));
    // White background, the red square, the frame's edge.
    assert_eq!(pixel(&image, 1, 1), [255, 255, 255, 255]);
    assert_eq!(pixel(&image, 85, 10), [255, 0, 0, 255]);
    assert_ne!(pixel(&image, 75, 35), [255, 255, 255, 255]);
    assert_eq!(pixel(&image, 85, 35), [255, 255, 255, 255]);
    // The stem of the H, from x = 15 + its left side bearing.
    let dark = (15..25).filter(|&x| pixel(&image, x, 25)[0] < 64).count();
    assert!(dark >= 2, "the H is drawn");

    // Twice the scale: twice the size, and the same picture.
    let big = render(
        &list,
        &fonts,
        &mut outlines,
        &Options {
            scale: 2.0,
            background: None,
        },
    )
    .unwrap();
    assert_eq!((big.width, big.height), (200, 100));
    assert_eq!(pixel(&big, 2, 2), [0, 0, 0, 0]);
    assert_eq!(pixel(&big, 170, 20), [255, 0, 0, 255]);

    // Without the font, the glyphs are left out and the rest is drawn.
    let none: Vec<FontBytes<'_>> = Vec::new();
    let bare = render(&list, &none, &mut Outlines::new(), &Options::default()).unwrap();
    assert!((15..25).all(|x| pixel(&bare, x, 25) == [255, 255, 255, 255]));
    assert_eq!(pixel(&bare, 85, 10), [255, 0, 0, 255]);

    // Rendering is deterministic.
    let again = render(&list, &fonts, &mut Outlines::new(), &Options::default()).unwrap();
    assert_eq!(again, image);
}

#[test]
fn png_round_trips() {
    let font = font();
    let list = DisplayList::from_fragment(&page(font));
    let image = render(
        &list,
        &vec![font],
        &mut Outlines::new(),
        &Options::default(),
    )
    .unwrap();
    let png = image.png().unwrap();
    assert_eq!(&png[1..4], b"PNG");
    assert_eq!(Image::from_png(&png).unwrap(), image);

    let clear = render(
        &list,
        &vec![font],
        &mut Outlines::new(),
        &Options {
            scale: 1.0,
            background: None,
        },
    )
    .unwrap();
    assert_eq!(Image::from_png(&clear.png().unwrap()).unwrap(), clear);
    assert!(Image::from_png(b"not a png").is_err());
}

#[test]
fn sizes_are_limited() {
    let list = DisplayList {
        width: 0.0,
        height: 10.0,
        span: Span::DETACHED,
        items: vec![],
    };
    let fonts: Vec<FontBytes<'_>> = Vec::new();
    assert!(render(&list, &fonts, &mut Outlines::new(), &Options::default()).is_err());
    let list = DisplayList { width: 1e9, ..list };
    assert!(render(&list, &fonts, &mut Outlines::new(), &Options::default()).is_err());
}

#[test]
fn glyph_bitmaps() {
    let font = font();
    let fonts = vec![font];
    let mut outlines = Outlines::new();
    let h = glyph_id(font, 'H');
    let small = rasterize_glyph(&fonts, &mut outlines, FontId(0), h, 20.0, 0.0).unwrap();
    let big = rasterize_glyph(&fonts, &mut outlines, FontId(0), h, 40.0, 0.0).unwrap();
    assert!(small.top < 0, "an H stands above its baseline");
    assert!(small.top + small.height as i32 <= 1);
    assert_eq!(small.coverage.len(), (small.width * small.height) as usize);
    assert!(small.coverage.contains(&255));
    assert!(big.height >= 2 * small.height - 2);
    let shifted = rasterize_glyph(&fonts, &mut outlines, FontId(0), h, 20.0, 0.5).unwrap();
    assert_ne!(shifted.coverage, small.coverage);
    let space = glyph_id(font, ' ');
    assert_eq!(
        rasterize_glyph(&fonts, &mut outlines, FontId(0), space, 20.0, 0.0),
        None
    );
    assert_eq!(
        rasterize_glyph(&fonts, &mut outlines, FontId(1), h, 20.0, 0.0),
        None
    );
}

#[test]
fn svg_output() {
    let font = font();
    let list = DisplayList::from_fragment(&page(font));
    let text = svg(&list, &vec![font], &mut Outlines::new());
    assert!(text.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100pt\""));
    assert!(text.contains("viewBox=\"0 0 100 50\""));
    assert_eq!(text.matches("<use ").count(), 2);
    assert_eq!(text.matches("<path id=").count(), 2);
    assert!(text.contains("transform=\"matrix(0.02 0 0 -0.02 15 32)\""));
    assert!(text.contains("<path d=\"M80 5L90 5L90 15L80 15Z\" fill=\"#ff0000\""));
    assert!(text.contains("<rect x=\"75.25\" y=\"25.25\" width=\"19.5\""));
    assert!(text.ends_with("</svg>\n"));
    assert_eq!(svg(&list, &vec![font], &mut Outlines::new()), text);
}

/// A horizontal line 2pt thick from (10, 10) to (90, 10), dashed with one
/// length, which repeats: 10 on, 10 off.
fn dashed_line() -> DisplayList {
    use folio_component::{Dash, Stroke};
    let mut path = Path::new();
    path.move_to(Point::new(Abs::pt(10.0), Abs::pt(10.0)));
    path.line_to(Point::new(Abs::pt(90.0), Abs::pt(10.0)));
    let mut stroke = Stroke::solid(Color::BLACK, Abs::pt(2.0));
    stroke.dash = Some(Dash {
        array: [Abs::pt(10.0)].into_iter().collect(),
        phase: Abs::zero(),
    });
    let mut page = Fragment::new(Size::new(Abs::pt(100.0), Abs::pt(20.0)));
    page.push(
        Point::zero(),
        FragmentItem::Shape {
            shape: Shape::stroked(path, stroke),
            source: SourceRef(9),
        },
    );
    DisplayList::from_fragment(&page)
}

#[test]
fn odd_dash_arrays_repeat() {
    let list = dashed_line();
    let none: Vec<FontBytes<'_>> = Vec::new();
    let image = render(&list, &none, &mut Outlines::new(), &Options::default()).unwrap();
    assert_eq!(pixel(&image, 15, 10)[0], 0, "a dash");
    assert_eq!(pixel(&image, 25, 10)[0], 255, "a gap");
    assert_eq!(pixel(&image, 35, 10)[0], 0, "the next dash");
}

#[test]
fn strokes_are_hit_beside_their_path() {
    let list = dashed_line();
    assert_eq!(list.hit(50.0, 10.8).len(), 1);
    assert_eq!(list.hit(50.0, 9.2).len(), 1);
    assert!(list.hit(50.0, 15.0).is_empty());
}
