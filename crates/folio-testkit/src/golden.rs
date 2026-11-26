//! Golden files (SPEC §14.2): output compared with a file recorded in
//! `tests/golden`, and written there instead when `FOLIO_BLESS` is set.
//!
//! After a change meant to alter output, run `scripts/bless.sh` (or a
//! golden test with `FOLIO_BLESS=1`) and review the diff before
//! committing it; a difference nobody intended is a bug.
//!
//! Layout goldens come in two forms. Fragment dumps ([`dump_pages`]) hold
//! every page, box, glyph run and shape with its position and size in
//! points, rounded to hundredths, and where its source is; a change
//! smaller than that is not a difference. Page images ([`page_images`])
//! hold what `folio-render` draws of each page, at [`SCALE`] pixels per
//! point, as PNG; [`check_image`] compares them pixel by pixel within
//! [`TOLERANCE`], and writes the image it got beside the build's other
//! outputs when they differ. The dumps say exactly what moved, and where
//! its source is; the images show what a reader sees, and catch what the
//! renderer gets wrong.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use folio::layout::{Cache, ObjectMetrics, PageCache, Regions, TextObject};
use folio::{Abs, Fragment, FragmentItem, Pages, Point, Size, Target, TextBlock, World};
use folio_component::{Color, FontId, Paint, PathItem};
use folio_render::{DisplayList, FontBytes, Image, Options, Outlines};

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

/// Pixels per point of the page images: 72 dpi, so that a page of the
/// corpus is a small file.
pub const SCALE: f32 = 1.0;

/// The largest difference, in levels of 255, that a channel of a pixel of
/// a page image may have from the golden file's.
///
/// `folio-render` draws with scalar float code that IEEE 754 fixes, so a
/// page should come out the same on every machine; the tolerance absorbs
/// the rounding of a renderer update or another platform's float
/// conversions, not a change anyone could see: a glyph that moves by a
/// tenth of a pixel changes the pixels at its edges by far more.
pub const TOLERANCE: u8 = 2;

/// The fonts of a build, by the ids that glyph runs name them by, for
/// `folio-render`.
#[derive(Debug)]
pub struct FontTable(Vec<Option<folio_fonts::Font>>);

impl FontTable {
    /// The faces of a world's fonts.
    pub fn new(world: &World) -> Self {
        let fonts = folio::fonts(world).expect("nothing cancels the build");
        Self(fonts.faces().iter().map(|face| face.font()).collect())
    }
}

impl folio_render::Fonts for FontTable {
    fn font(&self, id: FontId) -> Option<FontBytes<'_>> {
        let font = self.0.get(usize::try_from(id.0).ok()?)?.as_ref()?;
        Some(FontBytes {
            data: font.data(),
            index: font.index(),
        })
    }
}

/// The display list of each page of a build.
pub fn display_lists(pages: &Pages) -> Vec<DisplayList> {
    pages
        .pages
        .iter()
        .map(|page| DisplayList::from_fragment(&page.fragment))
        .collect()
}

/// What `folio-render` draws of each page of a build, at [`SCALE`], on
/// white.
pub fn page_images(world: &World, pages: &Pages) -> Vec<Image> {
    let fonts = FontTable::new(world);
    let mut outlines = Outlines::new();
    let options = Options {
        scale: SCALE,
        background: Some(Color::WHITE),
    };
    display_lists(pages)
        .iter()
        .map(|list| {
            folio_render::render(list, &fonts, &mut outlines, &options)
                .expect("a page of the corpus has a size that renders")
        })
        .collect()
}

/// Where an image that differs from its golden file is written for
/// review: `target/golden/` and the golden file's name, under
/// `CARGO_TARGET_DIR` when it is set.
pub fn actual_path(golden: &Path) -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::root().join("target"));
    target
        .join("golden")
        .join(golden.file_name().unwrap_or_default())
}

/// Compares a page image with the golden PNG at `path`, pixel by pixel
/// within [`TOLERANCE`], or writes it there when blessing and it differs
/// at all.
///
/// # Errors
///
/// When the file is missing, is not a PNG, or differs in size or in a
/// pixel by more than the tolerance; the message counts the pixels that
/// differ and names where the image was written ([`actual_path`]).
pub fn check_image(path: &Path, actual: &Image) -> Result<(), String> {
    let expected = fs::read(path).ok().map(|bytes| Image::from_png(&bytes));
    let png = || actual.png().map_err(|e| format!("{}: {e}", path.display()));
    if blessing() {
        if !matches!(&expected, Some(Ok(image)) if image == actual) {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            fs::write(path, png()?).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        return Ok(());
    }
    let expected = match expected {
        None => {
            return Err(format!(
                "{} is missing: run scripts/bless.sh to write it",
                path.display()
            ));
        }
        Some(Err(e)) => return Err(format!("{}: {e}", path.display())),
        Some(Ok(image)) => image,
    };
    let problem = if (expected.width, expected.height) != (actual.width, actual.height) {
        Some(format!(
            "it is {} × {} pixels, not {} × {}",
            actual.width, actual.height, expected.width, expected.height
        ))
    } else {
        let mut count = 0usize;
        let mut worst = 0u8;
        for (a, b) in actual
            .rgba
            .chunks_exact(4)
            .zip(expected.rgba.chunks_exact(4))
        {
            let diff = a
                .iter()
                .zip(b)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            if diff > TOLERANCE {
                count += 1;
                worst = worst.max(diff);
            }
        }
        (count > 0)
            .then(|| format!("{count} pixel(s) differ by more than {TOLERANCE}, by up to {worst}"))
    };
    let Some(problem) = problem else {
        return Ok(());
    };
    let out = actual_path(path);
    if let Some(parent) = out.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let written = match fs::write(&out, png()?) {
        Ok(()) => format!("the new image is {}", out.display()),
        Err(e) => format!(
            "the new image could not be written to {}: {e}",
            out.display()
        ),
    };
    Err(format!(
        "{} differs: {problem}; {written}; if the change is meant, run scripts/bless.sh and \
         review the images",
        path.display()
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

/// The document of the object golden file ([`boxes`]): inline maths, which
/// stands for objects in a paragraph's text, a figure with
/// no content, a block that holds no paragraphs, and a note's anchor, an
/// object left without a size beside a sized one.
pub const BOXES: &str = "#doc(title: [Boxes])\n\n= Boxes\n\nA line with a tall box $x$ in \
it, and a raised one, see $z$, after it. The rest of the paragraph runs on so that it \
takes a second line and a third, which shows that only the line with the tall box is taller \
than the others.\n\n#figure[] <fig:box>\n\nA note's anchor#sidenote[A note.] has no size and a \
sized box $y$ follows it; the justified line ends where its space does.\n";

/// The stylesheet of [`BOXES`]: a note's anchor takes tracking, which an
/// object without a size takes as its only space, and paragraphs are
/// justified.
pub const BOXES_STYLE: &str = "sidenote { tracking: 2pt; }\nparagraph { justify: true; }\n";

/// The size of the tall test box: inline maths in [`BOXES`].
pub const TALL: ObjectMetrics = ObjectMetrics {
    width: Abs::pt(30.0),
    ascent: Abs::pt(24.0),
    descent: Abs::pt(8.0),
    shift: Abs::zero(),
    after: None,
    binds: false,
};

/// The size of the raised test box: the second inline maths of [`BOXES`],
/// the object at [`RAISED_AT`] in its paragraph.
pub const RAISED: ObjectMetrics = ObjectMetrics {
    width: Abs::pt(16.0),
    ascent: Abs::pt(6.0),
    descent: Abs::pt(2.0),
    shift: Abs::pt(5.0),
    after: None,
    binds: false,
};

/// Where the raised test box stands in its paragraph's text: after "A line
/// with a tall box ", the tall box's object character and " in it, and a
/// raised one, see ".
pub const RAISED_AT: usize = 23 + 3 + 30;

/// The size of the block test box: the empty figure of [`BOXES`].
pub const BLOCK: Size = Size::new(Abs::pt(120.0), Abs::pt(60.0));

/// The world of [`BOXES`], and its pages with fixed-size test boxes for
/// its objects and its empty figure, through the page builder's test hooks
/// ([`folio::layout::Regions`]). The hooks stand in for the sizes math
/// layout gives the formulas, so that the golden file tests the page
/// builder's handling of object sizes on its own; until images are laid
/// out (#47), nothing else sizes the empty figure.
pub fn boxes() -> (World, Pages) {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en-GB\"\nstyles = [\"styles/boxes.fsty\"]\n";
    let mut world = World::new(manifest).expect("a valid manifest");
    world.set_source("main.fol", BOXES).expect("a source");
    world
        .set_source("styles/boxes.fsty", BOXES_STYLE)
        .expect("a stylesheet");
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
        "math" if object.at == RAISED_AT => Some(RAISED),
        "math" => Some(TALL),
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

/// The document of the footnote golden file ([`footnotes`]): notes on
/// small pages, numbered from 1 on each page, one of them long enough to
/// continue on the next page, which starts with what is left of it before
/// the notes anchored there (SPEC §7.8).
pub const FOOTNOTES: &str = "#doc(title: [Projectile motion])\n\n= Projectile motion\n\n\
A ball is thrown horizontally from the top of a cliff.#footnote[Air resistance is \
negligible throughout.] Its horizontal velocity stays the same while its vertical velocity \
grows at a steady rate,#footnote[The acceleration of free fall, $g = qty(9.81, \"m s^-2\")$.] so \
its path is a parabola.\n\n\
The time of flight depends only on the height of the cliff and not on the speed of the \
throw. A faster ball lands further away, but it lands at the same moment as a slower one \
thrown at the same time, because both fall the same height from rest in the vertical \
direction. This surprises many students when they first meet it, and a simple \
demonstration with two coins shows it well.\n\n\
The range is the horizontal speed multiplied by the time of flight.#footnote[This holds \
only while the ground below the cliff is level. On a slope the ball lands earlier or later, \
and the range must be found from where the path meets the ground. The same method works \
for a ball thrown at an angle, once its velocity has been resolved into horizontal and \
vertical components, each of which is treated on its own as before. A worked example \
follows in the next lesson, with a table of results to compare with the experiment, and a \
short set of exam questions. The questions start with a ball thrown from a table, which is \
easy to film with a phone held at the side, and end with a ball thrown up a slope, which \
needs both components at once. Bring a calculator, a ruler and squared paper.] A ball thrown at twice the speed travels twice as far before \
it lands.\n\n\
== Questions\n\n\
Find the time of flight of a ball thrown from a cliff 20 m high.#footnote[Give your answer \
to two significant figures.] Then find how far from the foot of the cliff it lands when it \
is thrown at $qty(5, \"m s^-1\")$.\n";

/// The stylesheet of [`FOOTNOTES`]: small pages whose footnotes are
/// numbered from 1 on each.
pub const FOOTNOTES_STYLE: &str = "page { page-size: 105mm 148mm; margin-top: 12mm; margin-bottom: 12mm; \
margin-inner: 12mm; margin-outer: 12mm; counter-reset: footnote; }\n";

/// The world of [`FOOTNOTES`], and its pages.
pub fn footnotes() -> (World, Pages) {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en-GB\"\nstyles = [\"styles/footnotes.fsty\"]\n";
    let mut world = World::new(manifest).expect("a valid manifest");
    world.set_source("main.fol", FOOTNOTES).expect("a source");
    world
        .set_source("styles/footnotes.fsty", FOOTNOTES_STYLE)
        .expect("a stylesheet");
    let pages = folio::pages(&world, Target::Pdf, None)
        .expect("nothing cancels the build")
        .expect("the footnote document has pages");
    (world, pages)
}
