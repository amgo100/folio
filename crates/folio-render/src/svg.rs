//! SVG output of display lists.

use std::collections::HashSet;
use std::fmt::Write as _;

use folio_component::{Color, FillRule, FontId, LineCap, LineJoin};
use tiny_skia::PathSegment;

use crate::list::{Command, DisplayList, FRAME_THICKNESS, PathShape, Segment};
use crate::raster::{Fonts, Outlines};

/// A display list as an SVG document, one unit per point.
///
/// Each glyph's outline is written once, in font units, and placed with
/// `<use>`; a glyph whose font or outline cannot be read is left out, as
/// [`crate::render`] leaves it. Numbers are written to three decimals, so
/// the same list gives the same text everywhere.
pub fn svg(list: &DisplayList, fonts: &dyn Fonts, outlines: &mut Outlines) -> String {
    let mut defs = String::new();
    let mut body = String::new();
    let mut defined: HashSet<(FontId, u16)> = HashSet::new();
    for item in &list.items {
        match &item.command {
            Command::Glyphs(run) => {
                let Some(upem) = outlines.units_per_em(fonts, run.font) else {
                    continue;
                };
                let k = run.size / upem;
                let _ = write!(body, "<g fill=\"{}\"", hex(run.fill));
                if run.fill.a != 255 {
                    let _ = write!(
                        body,
                        " fill-opacity=\"{}\"",
                        num(f32::from(run.fill.a) / 255.0)
                    );
                }
                body.push('>');
                for (at, glyph) in run.positioned() {
                    let Some(outline) = outlines.outline(fonts, run.font, glyph.id) else {
                        continue;
                    };
                    let id = format!("g{}-{}", run.font.0, glyph.id);
                    if defined.insert((run.font, glyph.id)) {
                        let _ = write!(defs, "<path id=\"{id}\" d=\"");
                        outline_data(&mut defs, outline);
                        defs.push_str("\"/>");
                    }
                    let _ = write!(
                        body,
                        "<use href=\"#{id}\" transform=\"matrix({} 0 0 {} {} {})\"/>",
                        num(k),
                        num(-k),
                        num(at.x),
                        num(at.y)
                    );
                }
                body.push_str("</g>");
            }
            Command::Path(shape) => path(&mut body, shape),
            Command::Frame(rect) => {
                let half = FRAME_THICKNESS / 2.0;
                let inner = |side: f32| (side - FRAME_THICKNESS).max(0.0);
                let _ = write!(
                    body,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"none\" \
                     stroke=\"#000000\" stroke-width=\"{}\"/>",
                    num(rect.x + half),
                    num(rect.y + half),
                    num(inner(rect.width)),
                    num(inner(rect.height)),
                    num(FRAME_THICKNESS)
                );
            }
            Command::Box(_) | Command::Unknown(_) => {}
        }
    }
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}pt\" height=\"{h}pt\" \
         viewBox=\"0 0 {w} {h}\">",
        w = num(list.width),
        h = num(list.height)
    );
    if !defs.is_empty() {
        let _ = write!(out, "<defs>{defs}</defs>");
    }
    out.push_str(&body);
    out.push_str("</svg>\n");
    out
}

/// A number with at most three decimals, without trailing zeros or a
/// negative zero.
fn num(value: f32) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

fn hex(color: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

fn outline_data(out: &mut String, path: &tiny_skia::Path) {
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(p) => {
                let _ = write!(out, "M{} {}", num(p.x), num(p.y));
            }
            PathSegment::LineTo(p) => {
                let _ = write!(out, "L{} {}", num(p.x), num(p.y));
            }
            PathSegment::QuadTo(a, p) => {
                let _ = write!(out, "Q{} {} {} {}", num(a.x), num(a.y), num(p.x), num(p.y));
            }
            PathSegment::CubicTo(a, b, p) => {
                let _ = write!(
                    out,
                    "C{} {} {} {} {} {}",
                    num(a.x),
                    num(a.y),
                    num(b.x),
                    num(b.y),
                    num(p.x),
                    num(p.y)
                );
            }
            PathSegment::Close => out.push('Z'),
        }
    }
}

fn path(out: &mut String, shape: &PathShape) {
    out.push_str("<path d=\"");
    for segment in &shape.segments {
        match *segment {
            Segment::MoveTo(p) => {
                let _ = write!(out, "M{} {}", num(p.x), num(p.y));
            }
            Segment::LineTo(p) => {
                let _ = write!(out, "L{} {}", num(p.x), num(p.y));
            }
            Segment::CubicTo(a, b, p) => {
                let _ = write!(
                    out,
                    "C{} {} {} {} {} {}",
                    num(a.x),
                    num(a.y),
                    num(b.x),
                    num(b.y),
                    num(p.x),
                    num(p.y)
                );
            }
            Segment::Close => out.push('Z'),
        }
    }
    out.push('"');
    match shape.fill {
        Some(color) => {
            let _ = write!(out, " fill=\"{}\"", hex(color));
            if color.a != 255 {
                let _ = write!(out, " fill-opacity=\"{}\"", num(f32::from(color.a) / 255.0));
            }
            if shape.fill_rule == FillRule::EvenOdd {
                out.push_str(" fill-rule=\"evenodd\"");
            }
        }
        None => out.push_str(" fill=\"none\""),
    }
    if let Some(stroke) = &shape.stroke {
        let _ = write!(
            out,
            " stroke=\"{}\" stroke-width=\"{}\"",
            hex(stroke.color),
            num(stroke.thickness)
        );
        if stroke.color.a != 255 {
            let _ = write!(
                out,
                " stroke-opacity=\"{}\"",
                num(f32::from(stroke.color.a) / 255.0)
            );
        }
        match stroke.cap {
            LineCap::Butt => {}
            LineCap::Round => out.push_str(" stroke-linecap=\"round\""),
            LineCap::Square => out.push_str(" stroke-linecap=\"square\""),
        }
        match stroke.join {
            LineJoin::Miter => {
                let _ = write!(out, " stroke-miterlimit=\"{}\"", num(stroke.miter_limit));
            }
            LineJoin::Round => out.push_str(" stroke-linejoin=\"round\""),
            LineJoin::Bevel => out.push_str(" stroke-linejoin=\"bevel\""),
        }
        if let Some((array, phase)) = &stroke.dash {
            let array: Vec<String> = array.iter().map(|&d| num(d)).collect();
            let _ = write!(out, " stroke-dasharray=\"{}\"", array.join(" "));
            if *phase != 0.0 {
                let _ = write!(out, " stroke-dashoffset=\"{}\"", num(*phase));
            }
        }
    }
    out.push_str("/>");
}
