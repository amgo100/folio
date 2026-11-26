//! The wire format (SPEC §12.2): every display list and span map decodes
//! to what was encoded, damaged bytes fail to decode without a panic, and
//! a decoder skips what a later version may add.

use folio_component::{Color, FillRule, FontId, LineCap, LineJoin, SourceRef};
use folio_render::wire::{self, DecodeError, MAGIC, Message, VERSION};
use folio_render::{
    Command, DisplayList, GlyphItem, Glyphs, Item, PathShape, Point, Rect, Segment, Span, Stroke,
};
use proptest::prelude::*;

fn finite() -> impl Strategy<Value = f32> {
    prop_oneof![
        Just(0.0f32),
        Just(-0.0f32),
        -2000.0f32..2000.0,
        any::<f32>().prop_filter("finite", |v| v.is_finite()),
    ]
}

fn point() -> impl Strategy<Value = Point> {
    (finite(), finite()).prop_map(|(x, y)| Point { x, y })
}

fn rect() -> impl Strategy<Value = Rect> {
    (finite(), finite(), finite(), finite()).prop_map(|(x, y, width, height)| Rect {
        x,
        y,
        width,
        height,
    })
}

fn color() -> impl Strategy<Value = Color> {
    any::<[u8; 4]>().prop_map(|[r, g, b, a]| Color::rgba(r, g, b, a))
}

fn span() -> impl Strategy<Value = Span> {
    (any::<u64>(), any::<u32>()).prop_map(|(s, instance)| Span {
        source: SourceRef(s),
        instance,
    })
}

fn source() -> impl Strategy<Value = (SourceRef, std::ops::Range<u32>)> {
    (
        prop_oneof![any::<u64>(), 0u64..8],
        any::<u32>(),
        any::<u32>(),
    )
        .prop_map(|(s, a, b)| (SourceRef(s), a..b))
}

fn glyphs() -> impl Strategy<Value = Glyphs> {
    (
        point(),
        any::<u32>(),
        finite(),
        color(),
        "[a-zł-ż ]{0,12}",
        prop::collection::vec(
            (
                any::<u16>(),
                finite(),
                finite(),
                finite(),
                any::<prop::sample::Index>(),
                any::<prop::sample::Index>(),
                source(),
            ),
            0..12,
        ),
    )
        .prop_map(|(origin, font, size, fill, text, glyphs)| {
            let bounds: Vec<u32> = text
                .char_indices()
                .map(|(i, _)| i as u32)
                .chain([text.len() as u32])
                .collect();
            let glyphs = glyphs
                .into_iter()
                .map(|(id, advance, dx, dy, a, b, source)| {
                    let (a, b) = (*a.get(&bounds), *b.get(&bounds));
                    GlyphItem {
                        id,
                        advance,
                        dx,
                        dy,
                        text: a.min(b)..a.max(b),
                        source,
                    }
                })
                .collect();
            Glyphs {
                origin,
                font: FontId(font),
                size,
                fill,
                text: text.into(),
                glyphs,
            }
        })
}

fn segment() -> impl Strategy<Value = Segment> {
    prop_oneof![
        point().prop_map(Segment::MoveTo),
        point().prop_map(Segment::LineTo),
        (point(), point(), point()).prop_map(|(a, b, c)| Segment::CubicTo(a, b, c)),
        Just(Segment::Close),
    ]
}

fn stroke() -> impl Strategy<Value = Stroke> {
    (
        color(),
        finite(),
        prop_oneof![
            Just(LineCap::Butt),
            Just(LineCap::Round),
            Just(LineCap::Square)
        ],
        prop_oneof![
            Just(LineJoin::Miter),
            Just(LineJoin::Round),
            Just(LineJoin::Bevel)
        ],
        finite(),
        prop::option::of((prop::collection::vec(finite(), 0..4), finite())),
    )
        .prop_map(|(color, thickness, cap, join, miter_limit, dash)| Stroke {
            color,
            thickness,
            cap,
            join,
            miter_limit,
            dash,
        })
}

fn command() -> impl Strategy<Value = Command> {
    prop_oneof![
        glyphs().prop_map(Command::Glyphs),
        (
            prop::collection::vec(segment(), 0..8),
            prop::option::of(color()),
            prop_oneof![Just(FillRule::NonZero), Just(FillRule::EvenOdd)],
            prop::option::of(stroke()),
        )
            .prop_map(
                |(segments, fill, fill_rule, stroke)| Command::Path(PathShape {
                    segments,
                    fill,
                    fill_rule,
                    stroke,
                })
            ),
        rect().prop_map(Command::Frame),
        rect().prop_map(Command::Box),
    ]
}

fn list() -> impl Strategy<Value = DisplayList> {
    (
        finite(),
        finite(),
        span(),
        prop::collection::vec(
            (command(), span()).prop_map(|(command, span)| Item { command, span }),
            0..8,
        ),
    )
        .prop_map(|(width, height, span, items)| DisplayList {
            width,
            height,
            span,
            items,
        })
}

/// Floats compared by their bits, so that `-0.0` and `0.0` differ.
fn same(a: &DisplayList, b: &DisplayList) -> bool {
    format!("{a:?}") == format!("{b:?}") && a == b
}

proptest! {
    #[test]
    fn lists_round_trip(list in list()) {
        let bytes = wire::encode(&list);
        let Ok(Message::List(decoded)) = wire::decode(&bytes) else {
            panic!("the list does not decode");
        };
        prop_assert!(same(&decoded, &list), "{decoded:?} != {list:?}");
        prop_assert_eq!(wire::encode(&decoded), bytes);
    }

    #[test]
    fn span_maps_round_trip(list in list()) {
        let spans = list.spans();
        let bytes = wire::encode_spans(&spans);
        prop_assert_eq!(wire::decode(&bytes), Ok(Message::Spans(spans.clone())));
        let mut other = list.clone();
        other.set_spans(spans).unwrap();
        prop_assert!(same(&other, &list));
    }

    /// Damaged messages fail to decode, or decode to something, but never
    /// panic.
    #[test]
    fn damage_never_panics(list in list(), cut in any::<prop::sample::Index>(), flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..4)) {
        let mut bytes = wire::encode(&list);
        for (at, value) in flips {
            let at = at.index(bytes.len());
            bytes[at] ^= value;
        }
        let cut = cut.index(bytes.len() + 1);
        let _ = wire::decode(&bytes[..cut]);
        let _ = wire::decode(&bytes);
    }
}

fn sample() -> DisplayList {
    DisplayList {
        width: 100.0,
        height: 50.0,
        span: Span::DETACHED,
        items: vec![Item {
            command: Command::Box(Rect {
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 4.0,
            }),
            span: Span {
                source: SourceRef(7),
                instance: 1,
            },
        }],
    }
}

#[test]
fn header_is_checked() {
    let bytes = wire::encode(&sample());
    assert_eq!(&bytes[..3], MAGIC);
    assert_eq!(bytes[3], VERSION);
    assert_eq!(wire::decode(b"PNG"), Err(DecodeError::Magic));
    assert_eq!(wire::decode(&[]), Err(DecodeError::Truncated));
    let mut other = bytes.clone();
    other[3] = VERSION + 1;
    assert_eq!(wire::decode(&other), Err(DecodeError::Version(VERSION + 1)));
    let mut other = bytes.clone();
    other[4] = 9;
    assert_eq!(wire::decode(&other), Err(DecodeError::Kind(9)));
    let mut other = bytes.clone();
    other.push(0);
    assert_eq!(wire::decode(&other), Err(DecodeError::Trailing));
    for cut in 0..bytes.len() {
        assert!(wire::decode(&bytes[..cut]).is_err(), "cut at {cut}");
    }
}

#[test]
fn lengths_must_be_finite() {
    let mut list = sample();
    list.width = f32::NAN;
    assert_eq!(
        wire::decode(&wire::encode(&list)),
        Err(DecodeError::NotFinite)
    );
    list.width = f32::INFINITY;
    assert_eq!(
        wire::decode(&wire::encode(&list)),
        Err(DecodeError::NotFinite)
    );
}

#[test]
fn counts_cannot_exceed_the_input() {
    // A list header claiming u64::MAX items, with nothing after it.
    let mut bytes = MAGIC.to_vec();
    bytes.extend([VERSION, 0]);
    bytes.extend([0; 8]); // width, height
    bytes.extend([0, 0]); // span
    bytes.extend([0xff; 9]);
    bytes.push(0x01);
    assert_eq!(wire::decode(&bytes), Err(DecodeError::Overflow));
}

/// A later version may add commands and append fields to an item: this
/// decoder keeps an unknown command as a placeholder with its span, and
/// skips appended fields.
#[test]
fn later_additions_are_skipped() {
    let list = sample();
    let bytes = wire::encode(&list);
    // Header (5), width and height (8), span (2), count (1).
    let (head, items) = bytes.split_at(16);
    assert_eq!(items[0], 3, "a box");
    let body_len = items[1] as usize;
    let body = &items[2..2 + body_len];

    let mut extended = head.to_vec();
    extended[15] = 2;
    // An unknown command: its span (node 1, instance 2) and a byte more.
    extended.extend([42, 3, 1, 2, 3]);
    // The box with a field appended.
    extended.extend([3, (body_len + 2) as u8]);
    extended.extend(body);
    extended.extend([0xaa, 0xbb]);
    let Ok(Message::List(decoded)) = wire::decode(&extended) else {
        panic!("the extended list does not decode");
    };
    assert_eq!(decoded.items.len(), 2);
    let Command::Unknown(unknown) = &decoded.items[0].command else {
        panic!("a placeholder for the unknown command");
    };
    assert_eq!(unknown.tag(), 42);
    assert_eq!(
        decoded.items[0].span,
        Span {
            source: SourceRef(1),
            instance: 2
        }
    );
    assert_eq!(decoded.items[1], list.items[0]);
    assert!(decoded.hit(2.0, 3.0).iter().all(|hit| hit.item == 1));
    assert_eq!(
        wire::decode(&wire::encode(&decoded)),
        Ok(Message::List(decoded.clone()))
    );
    // The newer encoder's span map, which counts the unknown item, fits.
    let mut copy = decoded.clone();
    copy.set_spans(decoded.spans()).unwrap();
    assert_eq!(copy, decoded);
}

#[test]
fn text_ranges_are_checked() {
    let mut list = sample();
    list.items = vec![Item {
        command: Command::Glyphs(Glyphs {
            origin: Point::default(),
            font: FontId(0),
            size: 10.0,
            fill: Color::BLACK,
            text: "łą".into(),
            glyphs: vec![GlyphItem {
                id: 1,
                advance: 5.0,
                dx: 0.0,
                dy: 0.0,
                text: 0..1,
                source: (SourceRef(1), 0..2),
            }],
        }),
        span: Span::DETACHED,
    }];
    assert_eq!(wire::decode(&wire::encode(&list)), Err(DecodeError::Text));
}

#[test]
fn spans_must_fit() {
    let mut list = sample();
    let mut spans = list.spans();
    spans.items.push((Span::DETACHED, vec![]));
    assert!(list.set_spans(spans).is_err());
    assert_eq!(list, sample());
}
