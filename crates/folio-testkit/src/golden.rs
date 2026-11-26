//! Golden files (SPEC §14.2): output compared with a file recorded in
//! `tests/golden`, and written there instead when `FOLIO_BLESS` is set.
//!
//! After a change meant to alter output, run `scripts/bless.sh` (or a
//! golden test with `FOLIO_BLESS=1`) and review the diff before
//! committing it; a difference nobody intended is a bug.
//!
//! Layout goldens are fragment dumps ([`dump_pages`]): every page, box,
//! glyph run and shape with its position and size in points, rounded to
//! hundredths. A change smaller than that is not a difference. Once the
//! rasteriser exists (M1), page images compared with a perceptual
//! threshold join them.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use folio::layout::{Cache, ObjectMetrics, PageCache, Regions, TextObject};
use folio::{Abs, Fragment, FragmentItem, Pages, Point, Size, Target, TextBlock, World};
use folio_component::{Color, Paint, PathItem};

/// The environment variable that makes [`check`] write golden files
/// instead of comparing with them.
pub const BLESS: &str = "FOLIO_BLESS";

/// The golden-file directory.
pub fn dir() -> PathBuf {
    crate::root().join("tests/golden")
}

/// Whether golden files are being written: `FOLIO_BLESS` is set.
pub fn blessing() -> bool {
    std::env::var_os(BLESS).is_some()
}

/// Compares `actual` with the golden file at `path`, or writes it there
/// when blessing.
///
/// # Errors
///
/// When the file is missing or differs, with a message that shows both.
pub fn check(path: &Path, actual: &str) -> Result<(), String> {
    if blessing() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        if fs::read_to_string(path).ok().as_deref() != Some(actual) {
            fs::write(path, actual).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        return Ok(());
    }
    let expected = fs::read_to_string(path).map_err(|_| {
        format!(
            "{} is missing: run scripts/bless.sh to write it",
            path.display()
        )
    })?;
    if expected == actual {
        return Ok(());
    }
    Err(format!(
        "{} differs; if the change is meant, run scripts/bless.sh and review the diff\n{}",
        path.display(),
        diff(&expected, actual)
    ))
}

/// The first lines where two texts differ, with a little context.
fn diff(expected: &str, actual: &str) -> String {
    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    let first = expected
        .iter()
        .zip(&actual)
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));
    if first == expected.len() && first == actual.len() {
        return "the lines are equal: the texts differ in line endings or a final newline\n".into();
    }
    let from = first.saturating_sub(3);
    let mut out = format!("first difference at line {}\n", first + 1);
    for (sign, lines) in [("-", &expected), ("+", &actual)] {
        for line in lines.iter().skip(from).take(first - from + 12) {
            let _ = writeln!(out, "{sign} {line}");
        }
    }
    out
}

/// Removes the files of `dir` with the extension `ext` that are not in
/// `expected` when blessing, and otherwise lists them as stale.
///
/// # Errors
///
/// When not blessing and there are stale files.
pub fn prune(dir: &Path, ext: &str, expected: &[PathBuf]) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    let mut stale = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|e| e == ext) && !expected.contains(&path) {
            stale.push(path);
        }
    }
    stale.sort();
    if stale.is_empty() {
        return Ok(());
    }
    if blessing() {
        for path in &stale {
            fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        return Ok(());
    }
    let names: Vec<String> = stale.iter().map(|p| p.display().to_string()).collect();
    Err(format!(
        "golden files without a build: {}; run scripts/bless.sh to remove them",
        names.join(", ")
    ))
}

/// A length in points, rounded to hundredths, without a negative zero.
fn len(abs: Abs) -> String {
    let text = format!("{:.2}", abs.get());
    if text == "-0.00" { "0.00".into() } else { text }
}

/// A point as `(x, y)`.
fn point(point: Point) -> String {
    format!("({}, {})", len(point.x), len(point.y))
}

/// Dumps a build's pages as text: each page and, indented beneath it, the
/// fragments, glyph runs and shapes it holds, in painting order, with their
/// positions relative to the fragment that holds them.
///
/// A fragment shows its size, the source it is the box of (as
/// `path:line:column`), its baselines when they are not at its bottom, and
/// its keep and break penalty when it has them. A glyph run shows its font
/// (by PostScript name), size, width, number of glyphs and text. An object
/// shows its size, its baseline from its top and its element's source.
/// Fonts and sources are named by what is stable across builds, not by
/// ids.
pub fn dump_pages(world: &World, pages: &Pages) -> String {
    let fonts = folio::fonts(world).ok();
    let font_name = |id: u32| -> String {
        fonts
            .as_ref()
            .and_then(|fonts| fonts.faces().get(id as usize))
            .map(|face| face.postscript_name.to_string())
            .unwrap_or_else(|| format!("font#{id}"))
    };
    let mut out = String::from(
        "# Folio layout golden file (lengths in points): scripts/bless.sh rewrites it.\n",
    );
    let master = &pages.master;
    let _ = writeln!(
        out,
        "master {} × {}, {} page(s)",
        len(master.size.width),
        len(master.size.height),
        pages.pages.len()
    );
    for (i, page) in pages.pages.iter().enumerate() {
        let _ = writeln!(
            out,
            "page {} {} × {}, {} line(s)",
            i + 1,
            len(page.fragment.size.width),
            len(page.fragment.size.height),
            page.lines.len()
        );
        dump_items(world, &page.fragment, 1, &font_name, &mut out);
    }
    out
}

/// Dumps the items of a fragment at an indentation depth.
fn dump_items(
    world: &World,
    fragment: &Fragment,
    depth: usize,
    font_name: &dyn Fn(u32) -> String,
    out: &mut String,
) {
    let indent = "  ".repeat(depth);
    for (pos, item) in &fragment.items {
        match item {
            FragmentItem::Group(inner) => {
                let _ = write!(
                    out,
                    "{indent}box {} {} × {}",
                    point(*pos),
                    len(inner.size.width),
                    len(inner.size.height)
                );
                match folio::span_map::locate_fragment(world, inner) {
                    Some(at) => {
                        let _ = write!(out, " {}:{}:{}", at.path, at.start.line, at.start.column);
                    }
                    None if !inner.source.is_detached() => out.push_str(" unresolved"),
                    None => {}
                }
                if inner.first_baseline != inner.size.height
                    || inner.last_baseline != inner.size.height
                {
                    let _ = write!(
                        out,
                        " baselines {}..{}",
                        len(inner.first_baseline),
                        len(inner.last_baseline)
                    );
                }
                if inner.keep_with_next {
                    out.push_str(" keep");
                }
                if inner.break_penalty_after != 0 {
                    let _ = write!(out, " penalty {}", inner.break_penalty_after);
                }
                out.push('\n');
                dump_items(world, inner, depth + 1, font_name, out);
            }
            FragmentItem::Text(run) => {
                let Paint::Solid(fill) = run.fill;
                let _ = write!(
                    out,
                    "{indent}text {} {} {} w {} {} glyph(s)",
                    point(*pos),
                    font_name(run.font.0),
                    len(run.size),
                    len(run.width()),
                    run.glyphs.len(),
                );
                if fill != Color::BLACK {
                    let _ = write!(out, " fill {}", hex(fill));
                }
                let _ = writeln!(out, " {:?}", run.text.as_str());
            }
            FragmentItem::Shape { shape, source } => {
                let _ = write!(out, "{indent}shape {}", point(*pos));
                for item in shape.path.0.iter() {
                    match item {
                        PathItem::MoveTo(p) => {
                            let _ = write!(out, " M{}", point(*p));
                        }
                        PathItem::LineTo(p) => {
                            let _ = write!(out, " L{}", point(*p));
                        }
                        PathItem::CubicTo(a, b, c) => {
                            let _ = write!(out, " C{}{}{}", point(*a), point(*b), point(*c));
                        }
                        PathItem::ClosePath => out.push_str(" Z"),
                    }
                }
                if let Some(Paint::Solid(fill)) = shape.fill {
                    let _ = write!(out, " fill {} {:?}", hex(fill), shape.fill_rule);
                }
                if let Some(stroke) = &shape.stroke {
                    let Paint::Solid(paint) = stroke.paint;
                    let _ = write!(
                        out,
                        " stroke {} {} {:?} {:?}",
                        hex(paint),
                        len(stroke.thickness),
                        stroke.cap,
                        stroke.join
                    );
                    if let Some(dash) = &stroke.dash {
                        let _ = write!(out, " {dash:?}");
                    }
                }
                if let Some(at) = locate(world, *source) {
                    let _ = write!(out, " {at}");
                }
                out.push('\n');
            }
            FragmentItem::Object(object) => {
                let _ = write!(
                    out,
                    "{indent}object {} {} × {} baseline {}",
                    point(*pos),
                    len(object.size.width),
                    len(object.size.height),
                    len(object.baseline)
                );
                if let Some(at) = locate(world, object.location.source) {
                    let _ = write!(out, " {at}");
                }
                out.push('\n');
            }
        }
    }
}

/// Where a source reference is written, as `path:line:column`.
fn locate(world: &World, source: folio::SourceRef) -> Option<String> {
    let at = folio::diag::locate(world, folio::span_map::span(source).into())?;
    Some(format!("{}:{}:{}", at.path, at.start.line, at.start.column))
}

/// A colour as `#rrggbbaa`.
fn hex(color: Color) -> String {
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        color.r, color.g, color.b, color.a
    )
}

/// The document of the object golden file ([`boxes`]): inline maths and a
/// reference, which stand for objects in a paragraph's text, and a figure
/// with no content, a block that holds no paragraphs.
pub const BOXES: &str = "#doc(title: [Boxes])\n\n= Boxes\n\nA line with a tall box $x$ in \
it, and a raised one, see @fig:box, after it. The rest of the paragraph runs on so that it \
takes a second line and a third, which shows that only the line with the tall box is taller \
than the others.\n\n#figure[] <fig:box>\n\nA paragraph after the block box.\n";

/// The size of the tall test box: inline maths in [`BOXES`].
pub const TALL: ObjectMetrics = ObjectMetrics {
    width: Abs::pt(30.0),
    ascent: Abs::pt(24.0),
    descent: Abs::pt(8.0),
    shift: Abs::zero(),
};

/// The size of the raised test box: a reference in [`BOXES`].
pub const RAISED: ObjectMetrics = ObjectMetrics {
    width: Abs::pt(16.0),
    ascent: Abs::pt(6.0),
    descent: Abs::pt(2.0),
    shift: Abs::pt(5.0),
};

/// The size of the block test box: the empty figure of [`BOXES`].
pub const BLOCK: Size = Size::new(Abs::pt(120.0), Abs::pt(60.0));

/// The world of [`BOXES`], and its pages with fixed-size test boxes for
/// its objects and its empty figure, through the page builder's test hooks
/// ([`folio::layout::Regions`]). Until maths and images are laid out
/// (#43, #47), nothing else sizes objects: a normal build of the same
/// document leaves them without a size.
pub fn boxes() -> (World, Pages) {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en-GB\"\n";
    let mut world = World::new(manifest).expect("a valid manifest");
    world.set_source("main.fol", BOXES).expect("a source");
    let built = |what: &str| format!("the box document has {what}");
    let text = folio::text(&world, Target::Pdf, None)
        .expect("nothing cancels the build")
        .unwrap_or_else(|| panic!("{}", built("text")));
    let lines = folio::lines(&world, Target::Pdf, None)
        .expect("nothing cancels the build")
        .unwrap_or_else(|| panic!("{}", built("lines")));
    let styled = folio::styles(&world, Target::Pdf, None)
        .expect("nothing cancels the build")
        .unwrap_or_else(|| panic!("{}", built("styles")));
    let fonts = folio::fonts(&world).expect("nothing cancels the build");
    let object = |object: &TextObject| match object.element.name.as_str() {
        "math" => Some(TALL),
        "ref" => Some(RAISED),
        _ => None,
    };
    let block = |block: &TextBlock| {
        (block.element.name.as_str() == "figure" && block.children.is_empty()).then_some(BLOCK)
    };
    let regions = Regions {
        object: Some(&object),
        block: Some(&block),
        ..Regions::default()
    };
    let pages = folio::layout::paginate(
        &text,
        &lines,
        &styled,
        &fonts,
        &Cache::new(),
        &PageCache::new(),
        regions,
    );
    (world, pages)
}
