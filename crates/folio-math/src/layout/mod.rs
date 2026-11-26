// Derived from typst-layout v0.15.1, crates/typst-layout/src/math/mod.rs, and
// typst-library v0.15.1, crates/typst-library/src/math/ir/process.rs and
// crates/typst-library/src/math/ir/resolve.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

//! Math layout (SPEC §8.2): a formula of the math tree set with the
//! OpenType MATH table of its font, following TeX's rules as the font
//! parameterises them.
//!
//! The layout starts from Typst's math layout, adapted to Folio's tree:
//! where Typst resolves an equation's content into an intermediate form of
//! items with classes, sizes and stretch requests, Folio reads them off the
//! tree directly, so the tree is the only input.
//!
//! - **Styles**: display, text, script and scriptscript sizes and cramped
//!   styles, as TeX gives them to the parts of fractions, scripts, roots and
//!   matrices; scripts are set at the font's script sizes with `ssty`.
//! - **Spacing**: by the classes of neighbouring nodes (relations, binary
//!   operators, large operators, punctuation), as TeX spaces them; a
//!   variable operator such as `-` is binary after an operand.
//! - **Glyphs**: letters in their math alphabets, delimiters, radicals,
//!   accents and operators stretched by the font's variants and assemblies.
//! - **Inline formulas** ([`layout_inline`]) are split after top-level
//!   relations and binary operators into chunks, between which a line may
//!   break ([`inline_chunks`] tells the text stage how many); a **display**
//!   ([`layout_display`]) is a block of lines aligned at their alignment
//!   points.
//!
//! Lengths are points (`f64`) inside the layout and [`Abs`] at its
//! interface. Nothing here reads a clock or depends on hash order (SPEC
//! §2.4).

mod accent;
mod alphabet;
mod fenced;
mod font;
mod fraction;
mod fragment;
mod radical;
mod run;
mod scripts;
mod table;

use ecow::{EcoString, EcoVec};
use folio_component::{
    Abs, Em, FontId, FragmentItem, Glyph, GlyphRun, Paint, Path, Point, Shape, SourceRef,
};
use folio_syntax::Span;
use unicode_math_class::MathClass;

use self::alphabet::Alphabet;
use self::font::{Constant, Features};
use self::fragment::{Axis, Frame, FrameFragment, FrameItem, GlyphFragment, MathFragment, Stretch};
use crate::{Formula, Node, Row};

pub use self::font::{FontBytes, MathFont};

/// The space between a thin space's neighbours: 1/6 em.
const THIN: f64 = 1.0 / 6.0;
/// A medium space, around binary operators: 2/9 em.
const MEDIUM: f64 = 2.0 / 9.0;
/// A thick space, around relations: 5/18 em.
const THICK: f64 = 5.0 / 18.0;
/// How much shorter than its target a stretched delimiter may be.
const DELIM_SHORT_FALL: f64 = 0.1;
/// How much narrower than its base an accent may be.
const ACCENT_SHORT_FALL: f64 = 0.5;
/// The cost of a line break after a relation (TeX's `\relpenalty`).
pub const REL_PENALTY: i32 = 500;
/// The cost of a line break after a binary operator (`\binoppenalty`).
pub const BIN_PENALTY: i32 = 700;

/// The size a part of a formula is set at, smallest first.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum MathSize {
    /// Scripts of scripts.
    ScriptScript,
    /// Scripts.
    Script,
    /// Inline formulas.
    Text,
    /// Display formulas.
    Display,
}

/// What a formula is set with.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct MathOptions {
    /// The math font.
    pub font: MathFont,
    /// The font size of the text around the formula.
    pub size: Abs,
    /// The paint of the glyphs and rules.
    pub fill: Paint,
    /// The space between the lines of a formula of several lines.
    pub leading: Abs,
}

/// A character of a formula that the math font has no glyph for, and which
/// is left out.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct MissingGlyph {
    /// The node it was written in.
    pub span: Span,
    /// The text that could not be set.
    pub text: EcoString,
}

/// An inline formula, laid out: its chunks, between which a line may
/// break.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct InlineMath {
    /// The chunks, in order: one for a formula that cannot break.
    pub chunks: Vec<Chunk>,
    /// What the font could not set.
    pub missing: Vec<MissingGlyph>,
}

/// A chunk of an inline formula: what a line holds of it between two
/// places where it may break.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Chunk {
    /// How wide it is.
    pub width: Abs,
    /// How far it reaches above its baseline.
    pub ascent: Abs,
    /// How far it reaches below its baseline.
    pub descent: Abs,
    /// What it draws, positioned from the start of its baseline (y down).
    pub items: EcoVec<(Point, FragmentItem)>,
    /// The break after it, for every chunk but the last.
    pub after: Option<ChunkBreak>,
}

/// A place where a line may break inside an inline formula.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct ChunkBreak {
    /// What breaking there costs: [`REL_PENALTY`] or [`BIN_PENALTY`].
    pub penalty: i32,
    /// The space at the end of the chunk before it, which a line that
    /// breaks there drops.
    pub space: Abs,
}

/// A display formula, laid out.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct DisplayMath {
    /// How wide its widest line is.
    pub width: Abs,
    /// How tall it is.
    pub height: Abs,
    /// The baseline of each line, from the top, in the order of the
    /// formula's lines.
    pub baselines: Vec<Abs>,
    /// What it draws, positioned from its top-left corner (y down).
    pub items: EcoVec<(Point, FragmentItem)>,
    /// What the font could not set.
    pub missing: Vec<MissingGlyph>,
}

/// How many chunks [`layout_inline`] splits an inline formula into: one
/// more than the places after its top-level relations and binary operators
/// where a line may break. A formula of several lines, or with alignment
/// points, does not break.
pub fn inline_chunks(formula: &Formula) -> usize {
    match inline_row(formula) {
        Some(row) => 1 + breaks(&prepare(row.nodes(), MathSize::Text, false)).len(),
        None => 1,
    }
}

/// The row of a formula that may break across lines.
fn inline_row(formula: &Formula) -> Option<&Row> {
    let [line] = formula.lines() else {
        return None;
    };
    let aligned = line
        .row
        .nodes()
        .iter()
        .any(|node| matches!(node, Node::AlignPoint(_)));
    (!aligned).then_some(&line.row)
}

/// Lays out an inline formula in chunks ([`inline_chunks`]). `source`
/// turns a node's span into the reference glyphs and rules carry.
pub fn layout_inline(
    formula: &Formula,
    options: &MathOptions,
    source: &dyn Fn(Span) -> SourceRef,
) -> InlineMath {
    let mut ctx = Ctx::new(options);
    let style = Style::new(MathSize::Text);
    let chunks: Vec<(Frame, Option<(i32, f64)>)> = match inline_row(formula) {
        Some(row) => {
            let prepared = prepare(row.nodes(), MathSize::Text, false);
            let breaks = breaks(&prepared);
            let laid = ctx.layout_prepared(row.nodes(), &prepared, style);
            let mut chunks = Vec::new();
            let mut current: Vec<MathFragment> = Vec::new();
            for (i, fragments) in laid.into_iter().enumerate() {
                current.extend(fragments);
                if let Some(&(_, penalty)) = breaks.iter().find(|(at, _)| *at == i) {
                    let space = prepared[i].rspace * ctx.font_size(style);
                    chunks.push((
                        run::into_line_frame(std::mem::take(&mut current)),
                        Some((penalty, space)),
                    ));
                }
            }
            chunks.push((run::into_line_frame(current), None));
            chunks
        }
        None => {
            let lines = ctx.layout_lines(formula, style);
            vec![(lines.frame, None)]
        }
    };
    let chunks = chunks
        .into_iter()
        .map(|(frame, after)| {
            let mut items = EcoVec::new();
            ctx.flatten(&frame, 0.0, -frame.baseline, source, &mut items);
            Chunk {
                width: Abs::pt(frame.width),
                ascent: Abs::pt(frame.baseline),
                descent: Abs::pt(frame.descent()),
                items,
                after: after.map(|(penalty, space)| ChunkBreak {
                    penalty,
                    space: Abs::pt(space),
                }),
            }
        })
        .collect();
    InlineMath {
        chunks,
        missing: ctx.missing,
    }
}

/// Lays out a display formula: its lines, aligned at their alignment
/// points and otherwise centred.
pub fn layout_display(
    formula: &Formula,
    options: &MathOptions,
    source: &dyn Fn(Span) -> SourceRef,
) -> DisplayMath {
    let mut ctx = Ctx::new(options);
    let lines = ctx.layout_lines(formula, Style::new(MathSize::Display));
    let mut items = EcoVec::new();
    ctx.flatten(&lines.frame, 0.0, 0.0, source, &mut items);
    DisplayMath {
        width: Abs::pt(lines.frame.width),
        height: Abs::pt(lines.frame.height),
        baselines: lines.baselines.into_iter().map(Abs::pt).collect(),
        items,
        missing: ctx.missing,
    }
}

/// Text set in the math font.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SetText {
    /// How wide it is.
    pub width: Abs,
    /// How far it reaches above its baseline.
    pub ascent: Abs,
    /// How far it reaches below its baseline.
    pub descent: Abs,
    /// What it draws, positioned from the start of its baseline (y down).
    pub items: EcoVec<(Point, FragmentItem)>,
}

/// Lays out text in the math font, upright, at the size of the text around
/// formulas: an equation's number. `None` when the font cannot set it.
pub fn layout_text(text: &str, options: &MathOptions, source: SourceRef) -> Option<SetText> {
    let ctx = Ctx::new(options);
    let style = Style::new(MathSize::Text);
    let glyph = GlyphFragment::new(
        &ctx.font,
        text,
        Span::detached(),
        MathClass::Normal,
        style.size,
        ctx.font_size(style),
        Features::default(),
    )?;
    let frame = glyph.into_frame();
    let mut items = EcoVec::new();
    ctx.flatten(&frame, 0.0, -frame.baseline, &|_| source, &mut items);
    Some(SetText {
        width: Abs::pt(frame.width),
        ascent: Abs::pt(frame.baseline),
        descent: Abs::pt(frame.descent()),
        items,
    })
}

/// The style a part of a formula is set in.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) struct Style {
    pub size: MathSize,
    pub cramped: bool,
    /// The alphabet of a styled body, if any.
    pub alphabet: Option<Alphabet>,
    /// Whether `i` and `j` take their dotless forms, under an accent.
    pub dotless: bool,
}

impl Style {
    fn new(size: MathSize) -> Self {
        Self {
            size,
            cramped: false,
            alphabet: None,
            dotless: false,
        }
    }

    /// The style of a superscript or an upper limit.
    pub(crate) fn sup(self) -> Self {
        Self {
            size: match self.size {
                MathSize::Display | MathSize::Text => MathSize::Script,
                _ => MathSize::ScriptScript,
            },
            ..self
        }
    }

    /// The style of a subscript or a lower limit: cramped.
    pub(crate) fn sub(self) -> Self {
        self.sup().cramp()
    }

    /// The style of a numerator.
    pub(crate) fn num(self) -> Self {
        Self {
            size: match self.size {
                MathSize::Display => MathSize::Text,
                MathSize::Text => MathSize::Script,
                _ => MathSize::ScriptScript,
            },
            ..self
        }
    }

    /// The style of a denominator: cramped.
    pub(crate) fn den(self) -> Self {
        self.num().cramp()
    }

    pub(crate) fn cramp(self) -> Self {
        Self {
            cramped: true,
            ..self
        }
    }

    /// The features glyphs are shaped with in this style.
    pub(crate) fn features(self) -> Features {
        Features {
            ssty: match self.size {
                MathSize::Script => 1,
                MathSize::ScriptScript => 2,
                _ => 0,
            },
            dtls: self.dotless,
            ..Features::default()
        }
    }
}

/// A multi-line block, with the baseline of each line.
pub(crate) struct Lines {
    pub frame: Frame,
    pub baselines: Vec<f64>,
}

/// The state of one layout.
pub(crate) struct Ctx {
    pub font: MathFont,
    /// The font size of the text around the formula, in points.
    pub base: f64,
    pub leading: f64,
    id: FontId,
    fill: Paint,
    missing: Vec<MissingGlyph>,
}

impl Ctx {
    fn new(options: &MathOptions) -> Self {
        Self {
            font: options.font.clone(),
            base: options.size.to_pt(),
            leading: options.leading.to_pt(),
            id: options.font.id(),
            fill: options.fill.clone(),
            missing: Vec::new(),
        }
    }

    /// The font size of a style, in points.
    pub(crate) fn font_size(&self, style: Style) -> f64 {
        match style.size {
            MathSize::Script => self.base * self.font.constant(Constant::ScriptPercentScaleDown),
            MathSize::ScriptScript => {
                self.base * self.font.constant(Constant::ScriptScriptPercentScaleDown)
            }
            _ => self.base,
        }
    }

    /// A constant at the font size of a style, in points.
    pub(crate) fn constant(&self, constant: Constant, style: Style) -> f64 {
        self.font.constant(constant) * self.font_size(style)
    }

    /// A glyph fragment of text in a style, or `None`, with the text
    /// recorded as missing, when the font cannot set it.
    pub(crate) fn glyph(
        &mut self,
        text: &str,
        span: Span,
        class: MathClass,
        style: Style,
    ) -> Option<GlyphFragment> {
        // Empty text, such as an empty string, sets nothing.
        if text.is_empty() {
            return None;
        }
        let size = self.font_size(style);
        let glyph = GlyphFragment::new(
            &self.font,
            text,
            span,
            class,
            style.size,
            size,
            style.features(),
        );
        if glyph.is_none() {
            self.missing(text, span);
        }
        glyph
    }

    /// A glyph fragment stretched along an axis.
    pub(crate) fn stretched(
        &mut self,
        text: &str,
        span: Span,
        class: MathClass,
        style: Style,
        stretch: Stretch,
    ) -> Option<GlyphFragment> {
        let size = self.font_size(style);
        let glyph = GlyphFragment::stretched(
            &self.font,
            text,
            span,
            class,
            style.size,
            size,
            style.features(),
            stretch,
        );
        if glyph.is_none() {
            self.missing(text, span);
        }
        glyph
    }

    fn missing(&mut self, text: &str, span: Span) {
        self.missing.push(MissingGlyph {
            span,
            text: text.into(),
        });
    }

    /// Lays out a row into one fragment of a class.
    pub(crate) fn layout_row(&mut self, row: &Row, style: Style, class: MathClass) -> MathFragment {
        let fragments = self.layout_fragments(row.nodes(), style, false);
        self.group(fragments, style, class)
    }

    /// The fragments of a sequence of nodes, spaced.
    pub(crate) fn layout_fragments(
        &mut self,
        nodes: &[Node],
        style: Style,
        closing: bool,
    ) -> Vec<MathFragment> {
        let prepared = prepare(nodes, style.size, closing);
        self.layout_prepared(nodes, &prepared, style)
            .into_iter()
            .flatten()
            .collect()
    }

    /// Fragments as one fragment: itself if there is one, else a frame of
    /// them that is like text where they all are.
    pub(crate) fn group(
        &self,
        mut fragments: Vec<MathFragment>,
        style: Style,
        class: MathClass,
    ) -> MathFragment {
        if fragments.len() == 1 {
            let mut fragment = fragments.pop().expect("one fragment");
            match &mut fragment {
                MathFragment::Glyph(glyph) => glyph.class = class,
                MathFragment::Frame(frame) => frame.class = class,
                MathFragment::Space(_) => {}
            }
            return fragment;
        }
        let text_like = fragments
            .iter()
            .filter(|f| f.math_size().is_some())
            .all(MathFragment::is_text_like);
        let frame = run::into_line_frame(fragments);
        let mut fragment = FrameFragment::new(frame, self.font_size(style), class, style.size);
        fragment.text_like = text_like;
        fragment.into()
    }

    /// The fragments of each node of a prepared sequence, with the spaces
    /// before and after it.
    fn layout_prepared(
        &mut self,
        nodes: &[Node],
        prepared: &[Prepared],
        style: Style,
    ) -> Vec<Vec<MathFragment>> {
        let size = self.font_size(style);
        let mut out: Vec<Vec<MathFragment>> = vec![Vec::new(); nodes.len()];
        for p in prepared {
            let fragments = &mut out[p.index];
            if p.lspace > 0.0 {
                fragments.push(MathFragment::Space(p.lspace * size));
            }
            self.layout_node(&nodes[p.index], style, p, fragments);
            if p.rspace > 0.0 {
                fragments.push(MathFragment::Space(p.rspace * size));
            }
        }
        out
    }

    /// Lays out one node.
    fn layout_node(
        &mut self,
        node: &Node,
        style: Style,
        prepared: &Prepared,
        out: &mut Vec<MathFragment>,
    ) {
        let class = prepared.class;
        match node {
            Node::Ident(atom) => {
                let text = alphabet::style(&atom.text, |c| {
                    style.alphabet.unwrap_or_else(|| alphabet::auto(c))
                });
                out.extend(self.glyph(&text, atom.span, class, style).map(Into::into));
            }
            Node::Number(atom) | Node::Text(atom) => {
                let text =
                    alphabet::style(&atom.text, |_| style.alphabet.unwrap_or(Alphabet::Upright));
                out.extend(self.glyph(&text, atom.span, class, style).map(Into::into));
            }
            Node::Symbol(atom) => {
                let text =
                    alphabet::style(&atom.text, |_| style.alphabet.unwrap_or(Alphabet::Upright));
                let glyph = if class == MathClass::Large && style.size == MathSize::Display {
                    let target = self.constant(Constant::DisplayOperatorMinHeight, style);
                    self.stretched(
                        &text,
                        atom.span,
                        class,
                        style,
                        Stretch {
                            axis: Axis::Y,
                            target,
                            short_fall: 0.0,
                        },
                    )
                } else {
                    self.glyph(&text, atom.span, class, style)
                };
                if let Some(mut glyph) = glyph {
                    if class == MathClass::Large {
                        glyph.center_on_axis(&self.font);
                    }
                    out.push(glyph.into());
                }
            }
            Node::Operator(op) => {
                out.extend(self.glyph(&op.name, op.span, class, style).map(Into::into));
            }
            Node::Differential(span) => {
                out.extend(self.glyph("d", *span, class, style).map(Into::into));
            }
            Node::Frac(frac) => fraction::layout_frac(self, frac, style, out),
            Node::Attach(attach) => scripts::layout_attach(self, attach, style, class, out),
            Node::Nary(nary) => scripts::layout_nary(self, nary, style, out),
            Node::Root(root) => radical::layout_root(self, root, style, out),
            Node::Delimited(delimited) => fenced::layout_delimited(self, delimited, style, out),
            Node::Matrix(matrix) => table::layout_matrix(self, matrix, style, out),
            Node::Cases(cases) => table::layout_cases(self, cases, style, out),
            Node::Accent(accent) => {
                accent::layout_accent(self, &accent.base, accent.accent, accent.span, style, out)
            }
            Node::Vector(vector) => {
                // A vector is set bold upright, and a unit vector with a
                // hat over it too, until a stylesheet chooses (SPEC §8.1).
                let bold = Style {
                    alphabet: Some(Alphabet::Bold),
                    ..style
                };
                if vector.unit {
                    accent::layout_accent(self, &vector.base, '\u{302}', vector.span, bold, out)
                } else {
                    let fragments = self.layout_fragments(vector.base.nodes(), bold, false);
                    let fragment = self.group(fragments, style, class);
                    out.push(fragment);
                }
            }
            Node::Decoration(decoration) => accent::layout_decoration(self, decoration, style, out),
            Node::Styled(styled) => {
                let inner = Style {
                    alphabet: Some(Alphabet::of(styled.variant)),
                    ..style
                };
                let fragments = self.layout_fragments(styled.body.nodes(), inner, false);
                let fragment = self.group(fragments, style, class);
                out.push(fragment);
            }
            Node::AlignPoint(_) => {}
        }
    }

    /// Lays out the lines of a formula, aligned at their alignment points.
    fn layout_lines(&mut self, formula: &Formula, style: Style) -> Lines {
        let rows: Vec<Vec<Vec<MathFragment>>> = formula
            .lines()
            .iter()
            .map(|line| self.layout_aligned(line.row.nodes(), style))
            .collect();
        let leading = if style.size >= MathSize::Text {
            self.leading
        } else {
            0.25 * self.font_size(style)
        };
        run::layout_multiline(rows, leading, true)
    }

    /// The cells of a sequence of nodes split at its alignment points, laid
    /// out with the spacing of the whole sequence. The space before the
    /// first node of a cell aligned at its start goes to the end of the
    /// cell before it, so that what an alignment point separates is spaced
    /// as if it were not there.
    pub(crate) fn layout_aligned(
        &mut self,
        nodes: &[Node],
        style: Style,
    ) -> Vec<Vec<MathFragment>> {
        let mut cells: Vec<Vec<Node>> = vec![Vec::new()];
        for node in nodes {
            match node {
                Node::AlignPoint(_) => cells.push(Vec::new()),
                other => cells.last_mut().expect("a cell").push(other.clone()),
            }
        }
        let starts: Vec<usize> = cells
            .iter()
            .scan(0, |at, cell| {
                let start = *at;
                *at += cell.len();
                Some(start)
            })
            .collect();
        let flat: Vec<Node> = cells.iter().flatten().cloned().collect();
        let mut prepared = prepare(&flat, style.size, false);
        let size = self.font_size(style);
        let mut moved = vec![0.0; cells.len()];
        for (c, &start) in starts.iter().enumerate() {
            if c % 2 == 1
                && !cells[c].is_empty()
                && let Some(p) = prepared.iter_mut().find(|p| p.index == start)
            {
                moved[c - 1] = p.lspace * size;
                p.lspace = 0.0;
            }
        }
        let laid = self.layout_prepared(&flat, &prepared, style);
        let mut out = Vec::with_capacity(cells.len());
        let mut iter = laid.into_iter();
        for (c, cell) in cells.iter().enumerate() {
            let mut fragments: Vec<MathFragment> =
                iter.by_ref().take(cell.len()).flatten().collect();
            if moved[c] > 0.0 {
                fragments.push(MathFragment::Space(moved[c]));
            }
            out.push(fragments);
        }
        out
    }

    /// Adds what a frame draws to `out`, with its top-left corner at
    /// `(dx, dy)`.
    fn flatten(
        &self,
        frame: &Frame,
        dx: f64,
        dy: f64,
        source: &dyn Fn(Span) -> SourceRef,
        out: &mut EcoVec<(Point, FragmentItem)>,
    ) {
        for (x, y, item) in &frame.items {
            let (x, y) = (dx + x, dy + y);
            match item {
                FrameItem::Frame(inner) => self.flatten(inner, x, y, source, out),
                FrameItem::Run(run) => {
                    let node = source(run.span);
                    let glyphs = run
                        .glyphs
                        .iter()
                        .map(|g| Glyph {
                            id: g.id,
                            x_advance: Em::new(g.advance),
                            x_offset: Em::new(g.x_offset),
                            y_offset: Em::new(g.y_offset),
                            range: g.range.0..g.range.1,
                            // A glyph stands for its whole node: the span
                            // map cannot follow a letter to the bytes of a
                            // name such as `alpha`.
                            source: (node, 0..u32::MAX),
                        })
                        .collect();
                    out.push((
                        Point::new(Abs::pt(x), Abs::pt(y)),
                        FragmentItem::Text(GlyphRun {
                            font: self.id,
                            size: Abs::pt(run.size),
                            fill: self.fill.clone(),
                            text: run.text.clone(),
                            glyphs,
                        }),
                    ));
                }
                FrameItem::Rule {
                    width,
                    height,
                    span,
                } => out.push((
                    Point::new(Abs::pt(x), Abs::pt(y)),
                    FragmentItem::Shape {
                        shape: Shape::filled(
                            Path::rect(Abs::pt(*width), Abs::pt(*height)),
                            self.fill.clone(),
                        ),
                        source: source(*span),
                    },
                )),
            }
        }
    }
}

/// A node of a sequence with its classes and the spaces around it, in ems.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    /// Its index in the sequence.
    pub index: usize,
    /// Its class, after a variable operator is resolved.
    pub class: MathClass,
    /// The class its left and right sides space as.
    pub lclass: MathClass,
    pub rclass: MathClass,
    pub lspace: f64,
    pub rspace: f64,
    /// Whether it is spaced from its neighbours like a word: quoted text.
    spaced: bool,
    /// Whether it brings its own space and takes none from its left
    /// neighbour: the differential.
    explicit: bool,
}

/// The classes of a node: its own, and those its left and right sides
/// space as.
fn classes(node: &Node) -> (MathClass, MathClass, MathClass) {
    let same = |class| (class, class, class);
    match node {
        Node::Ident(_) => same(MathClass::Alphabetic),
        Node::Number(_) | Node::Text(_) | Node::Differential(_) => same(MathClass::Normal),
        Node::Symbol(atom) => same(atom.class),
        Node::Operator(_) | Node::Nary(_) => same(MathClass::Large),
        Node::Delimited(d) => fenced_classes(d.open.is_some(), d.close.is_some()),
        Node::Matrix(m) => fenced_classes(m.open.is_some(), m.close.is_some()),
        Node::Cases(_) => fenced_classes(true, false),
        Node::Attach(attach) => match attach.base.nodes() {
            [base] => classes(base),
            _ => same(MathClass::Normal),
        },
        Node::Styled(styled) => match styled.body.nodes() {
            [] => same(MathClass::Normal),
            [one] => classes(one),
            [first, .., last] => (MathClass::Normal, classes(first).1, classes(last).2),
        },
        Node::AlignPoint(_) => same(MathClass::Space),
        _ => same(MathClass::Normal),
    }
}

fn fenced_classes(open: bool, close: bool) -> (MathClass, MathClass, MathClass) {
    (
        MathClass::Normal,
        if open {
            MathClass::Opening
        } else {
            MathClass::Normal
        },
        if close {
            MathClass::Closing
        } else {
            MathClass::Normal
        },
    )
}

/// Resolves the classes of a sequence's nodes and the spaces between them
/// (TeXbook, chapter 18). `closing` says whether a closing delimiter
/// follows the sequence.
pub(crate) fn prepare(nodes: &[Node], size: MathSize, closing: bool) -> Vec<Prepared> {
    let script = size <= MathSize::Script;
    let mut out: Vec<Prepared> = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        if matches!(node, Node::AlignPoint(_)) {
            continue;
        }
        let (mut class, mut lclass, mut rclass) = classes(node);
        // A variable operator after an operand is a binary operator.
        if class == MathClass::Vary
            && out.last().is_some_and(|prev| {
                matches!(
                    prev.class,
                    MathClass::Normal
                        | MathClass::Alphabetic
                        | MathClass::Closing
                        | MathClass::Fence
                )
            })
        {
            class = MathClass::Binary;
            lclass = MathClass::Binary;
            rclass = MathClass::Binary;
        }
        let mut item = Prepared {
            index,
            class,
            lclass,
            rclass,
            lspace: 0.0,
            rspace: 0.0,
            spaced: matches!(node, Node::Text(_)),
            explicit: matches!(node, Node::Differential(_)),
        };
        if let Some(prev) = out.last_mut() {
            if item.explicit {
                item.lspace = THIN;
            } else {
                spacing(prev, &mut item, script);
            }
        }
        out.push(item);
    }
    if closing
        && !script
        && let Some(last) = out.last_mut()
        && last.rclass == MathClass::Punctuation
    {
        last.rspace = THIN;
    }
    out
}

/// The space between two neighbours, set on the one it belongs to.
fn spacing(l: &mut Prepared, r: &mut Prepared, script: bool) {
    use MathClass::*;
    match (l.rclass, r.lclass) {
        // No space before punctuation; a thin space after it, unless in
        // scripts.
        (_, Punctuation) => {}
        (Punctuation, _) if !script => l.rspace = THIN,
        // None after an opening delimiter and before a closing one.
        (Opening, _) | (_, Closing) => {}
        // A thick space around relations, unless two are in a row or in
        // scripts.
        (Relation, Relation) => {}
        (Relation, _) if !script => l.rspace = THICK,
        (_, Relation) if !script => r.lspace = THICK,
        // A medium space around binary operators, unless in scripts.
        (Binary, _) if !script => l.rspace = MEDIUM,
        (_, Binary) if !script => r.lspace = MEDIUM,
        // A thin space around large operators, unless an opening delimiter
        // follows (TeXbook, p. 170).
        (Large, Opening | Fence) => {}
        (Large, _) => l.rspace = THIN,
        (_, Large) => r.lspace = THIN,
        // Words are spaced from what is around them.
        _ if l.spaced || r.spaced => r.lspace = WORD_SPACE,
        _ => {}
    }
}

/// The space around quoted text, in ems.
const WORD_SPACE: f64 = 1.0 / 3.0;

/// The places in a prepared sequence where a line may break: after a
/// relation that no relation or closing delimiter follows, and after a
/// binary operator that no closing delimiter follows, each with its cost.
/// A place is the index of the node a break follows.
fn breaks(prepared: &[Prepared]) -> Vec<(usize, i32)> {
    prepared
        .windows(2)
        .filter_map(|pair| {
            let (here, next) = (&pair[0], &pair[1]);
            let penalty = match here.rclass {
                MathClass::Relation
                    if !matches!(next.lclass, MathClass::Relation | MathClass::Closing) =>
                {
                    REL_PENALTY
                }
                MathClass::Binary if next.lclass != MathClass::Closing => BIN_PENALTY,
                _ => return None,
            };
            Some((here.index, penalty))
        })
        .collect()
}
