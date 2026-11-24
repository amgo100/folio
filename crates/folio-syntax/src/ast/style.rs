//! Typed views of stylesheets (SPEC §5.1, §5.3, §10.2, §11.2, §11.3): items,
//! selectors, declarations and the template forms of values.

use ecow::EcoString;

use super::{AstNode, Expr, Ident, Str};
use crate::{SyntaxKind, SyntaxNode};

node! {
    /// The root of a stylesheet file.
    struct Stylesheet
}

impl<'a> Stylesheet<'a> {
    /// The items of the stylesheet, in source order.
    pub fn items(self) -> impl DoubleEndedIterator<Item = StyleItem<'a>> {
        self.0.children().filter_map(SyntaxNode::cast)
    }
}

/// An item of a stylesheet or of an overlay.
#[derive(Debug, Copy, Clone, Hash)]
pub enum StyleItem<'a> {
    /// An import: `@import "school-base.fsty";`.
    Import(StyleImport<'a>),
    /// A token definition: `$accent: #1f5fa8;`.
    Token(TokenDecl<'a>),
    /// A style rule: `heading + paragraph { indent: 0; }`.
    Rule(StyleRule<'a>),
    /// A variant or target overlay: `@variant markscheme { .. }`.
    Overlay(Overlay<'a>),
    /// An export block: `@export theme { accent1: $accent; }`.
    Export(ExportBlock<'a>),
    /// A render template: `render example { .. }`.
    Render(RenderBlock<'a>),
    /// A slide master: `slide-master "Akademeia" { .. }`.
    SlideMaster(SlideMaster<'a>),
    /// A slide layout: `slide-layout "Title and Content" { .. }`.
    SlideLayout(SlideLayout<'a>),
}

impl<'a> AstNode<'a> for StyleItem<'a> {
    fn from_untyped(node: &'a SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::StyleImport => node.cast().map(Self::Import),
            SyntaxKind::TokenDecl => node.cast().map(Self::Token),
            SyntaxKind::StyleRule => node.cast().map(Self::Rule),
            SyntaxKind::Overlay => node.cast().map(Self::Overlay),
            SyntaxKind::ExportBlock => node.cast().map(Self::Export),
            SyntaxKind::RenderBlock => node.cast().map(Self::Render),
            SyntaxKind::SlideMaster => node.cast().map(Self::SlideMaster),
            SyntaxKind::SlideLayout => node.cast().map(Self::SlideLayout),
            _ => None,
        }
    }

    fn to_untyped(self) -> &'a SyntaxNode {
        match self {
            Self::Import(v) => v.to_untyped(),
            Self::Token(v) => v.to_untyped(),
            Self::Rule(v) => v.to_untyped(),
            Self::Overlay(v) => v.to_untyped(),
            Self::Export(v) => v.to_untyped(),
            Self::Render(v) => v.to_untyped(),
            Self::SlideMaster(v) => v.to_untyped(),
            Self::SlideLayout(v) => v.to_untyped(),
        }
    }

    fn placeholder() -> Self {
        Self::Rule(StyleRule::placeholder())
    }
}

node! {
    /// A stylesheet import: `@import "school-base.fsty";`.
    struct StyleImport
}

impl<'a> StyleImport<'a> {
    /// The path of the imported stylesheet.
    pub fn path(self) -> Option<Str<'a>> {
        self.0.try_cast_first()
    }
}

node! {
    /// A token definition: `$accent: #1f5fa8;`.
    struct TokenDecl
}

impl<'a> TokenDecl<'a> {
    /// The name of the token.
    pub fn name(self) -> TokenName<'a> {
        self.0.cast_first()
    }

    /// The items of the value, in order.
    pub fn values(self) -> impl Iterator<Item = Expr<'a>> {
        values(self.0)
    }
}

node! {
    /// A token name: `$accent`.
    struct TokenName
}

impl<'a> TokenName<'a> {
    /// The name without its `$`: `accent`.
    pub fn get(self) -> &'a str {
        let text = self.0.leaf_text();
        text.strip_prefix('$').unwrap_or(text)
    }
}

node! {
    /// A colour: `#1f5fa8`, `#fff`, `#1f5fa880`.
    struct Color
}

impl Color<'_> {
    /// The colour's red, green, blue and alpha components. Short forms
    /// repeat each digit (`#fff` is `#ffffff`); the alpha is 255 when it is
    /// not written.
    pub fn rgba(self) -> [u8; 4] {
        let text = self.0.leaf_text();
        let digits = text.strip_prefix('#').unwrap_or(text);
        let value = |s: &str| u8::from_str_radix(s, 16).unwrap_or(0);
        let mut out = [0, 0, 0, 255];
        match digits.len() {
            3 | 4 => {
                for (slot, i) in out.iter_mut().zip(0..digits.len()) {
                    *slot = value(&digits[i..i + 1]) * 17;
                }
            }
            6 | 8 => {
                for (slot, i) in out.iter_mut().zip((0..digits.len()).step_by(2)) {
                    *slot = value(&digits[i..i + 2]);
                }
            }
            _ => {}
        }
        out
    }
}

node! {
    /// An aspect ratio: `16:9`.
    struct Ratio
}

impl Ratio<'_> {
    /// The two sides of the ratio: `(16.0, 9.0)`.
    pub fn get(self) -> (f64, f64) {
        let text = self.0.leaf_text();
        let (a, b) = text.split_once(':').unwrap_or((text, "1"));
        (a.parse().unwrap_or(1.0), b.parse().unwrap_or(1.0))
    }
}

node! {
    /// A style rule: `heading + paragraph { indent: 0; }`.
    struct StyleRule
}

impl<'a> StyleRule<'a> {
    /// The selectors.
    pub fn selectors(self) -> SelectorList<'a> {
        self.0.cast_first()
    }

    /// The declarations.
    pub fn declarations(self) -> impl DoubleEndedIterator<Item = Declaration<'a>> {
        block_declarations(self.0)
    }
}

node! {
    /// A variant or target overlay: `@variant markscheme { .. }`,
    /// `@target pdf, pptx { .. }`.
    struct Overlay
}

impl<'a> Overlay<'a> {
    /// The scopes: `@variant markscheme`.
    pub fn scopes(self) -> impl DoubleEndedIterator<Item = super::RuleScope<'a>> {
        self.0.children().filter_map(SyntaxNode::cast)
    }

    /// The items inside the overlay.
    pub fn items(self) -> impl DoubleEndedIterator<Item = StyleItem<'a>> {
        self.0
            .try_cast_first::<OverlayBody<'a>>()
            .into_iter()
            .flat_map(|body| body.0.children().filter_map(SyntaxNode::cast))
    }
}

node! {
    /// The body of an overlay: `{ answer { display: block; } }`.
    struct OverlayBody
}

node! {
    /// An export block (SPEC §11.2): `@export theme { accent1: $accent; }`.
    struct ExportBlock
}

impl<'a> ExportBlock<'a> {
    /// What is exported: `theme`.
    pub fn target(self) -> Ident<'a> {
        // The first name is `export` itself.
        self.0
            .children()
            .filter_map(SyntaxNode::cast)
            .nth(1)
            .unwrap_or_else(Ident::placeholder)
    }

    /// The declarations.
    pub fn declarations(self) -> impl DoubleEndedIterator<Item = Declaration<'a>> {
        block_declarations(self.0)
    }
}

node! {
    /// A render template (SPEC §5.3): `render example { .. }`,
    /// `render example as compact { .. }`.
    struct RenderBlock
}

impl<'a> RenderBlock<'a> {
    /// The element name, with its namespace if it is qualified:
    /// `["example"]`, `["exam", "question"]`.
    pub fn element(self) -> impl Iterator<Item = Ident<'a>> {
        self.0
            .children()
            .skip(1)
            .take_while(|node| node.kind() != SyntaxKind::As)
            .filter_map(SyntaxNode::cast)
    }

    /// The name of a named template: `compact` in `render example as compact`.
    pub fn name(self) -> Option<Ident<'a>> {
        self.0
            .children()
            .skip_while(|node| node.kind() != SyntaxKind::As)
            .find_map(SyntaxNode::cast)
    }

    /// The parts of the template: `header: row(..)`, `body: slot(..)`.
    pub fn parts(self) -> impl DoubleEndedIterator<Item = Declaration<'a>> {
        block_declarations(self.0)
    }
}

node! {
    /// A slide master (SPEC §11.3): `slide-master "Akademeia" { .. }`.
    struct SlideMaster
}

impl<'a> SlideMaster<'a> {
    /// The name of the master.
    pub fn name(self) -> Str<'a> {
        self.0.cast_first()
    }

    /// The declarations for the master itself: `slide-size: 16:9`.
    pub fn declarations(self) -> impl DoubleEndedIterator<Item = Declaration<'a>> {
        self.body()
            .flat_map(|body| body.children().filter_map(SyntaxNode::cast))
    }

    /// The rules for its placeholders: `title { .. }`.
    pub fn placeholders(self) -> impl DoubleEndedIterator<Item = PlaceholderRule<'a>> {
        self.body()
            .flat_map(|body| body.children().filter_map(SyntaxNode::cast))
    }

    /// The body.
    fn body(self) -> impl DoubleEndedIterator<Item = &'a SyntaxNode> {
        self.0
            .children()
            .find(|node| node.kind() == SyntaxKind::MasterBlock)
            .into_iter()
    }
}

node! {
    /// The body of a slide master.
    struct MasterBlock
}

node! {
    /// A placeholder rule in a slide master: `title { font-size: 32pt; }`.
    struct PlaceholderRule
}

impl<'a> PlaceholderRule<'a> {
    /// The placeholder: `title`, `body`, `footer`.
    pub fn name(self) -> Ident<'a> {
        self.0.cast_first()
    }

    /// The declarations.
    pub fn declarations(self) -> impl DoubleEndedIterator<Item = Declaration<'a>> {
        block_declarations(self.0)
    }
}

node! {
    /// A slide layout (SPEC §11.3): `slide-layout "Title and Content" { .. }`.
    struct SlideLayout
}

impl<'a> SlideLayout<'a> {
    /// The name of the layout.
    pub fn name(self) -> Str<'a> {
        self.0.cast_first()
    }

    /// The declarations: `title: area(5%, 5%, 90%, 15%)`.
    pub fn declarations(self) -> impl DoubleEndedIterator<Item = Declaration<'a>> {
        block_declarations(self.0)
    }
}

// --- Selectors ---------------------------------------------------------------

node! {
    /// A comma-separated list of selectors: `series.answer, fit`.
    struct SelectorList
}

impl<'a> SelectorList<'a> {
    /// The selectors.
    pub fn iter(self) -> impl DoubleEndedIterator<Item = Selector<'a>> {
        self.0.children().filter_map(SyntaxNode::cast)
    }
}

node! {
    /// A selector: compound selectors joined by combinators, such as
    /// `question answer` or `list > item`.
    struct Selector
}

impl<'a> Selector<'a> {
    /// The compound selectors from left to right, each with the combinator
    /// that joins it to the one before (`None` for the first).
    pub fn steps(self) -> impl Iterator<Item = (Option<Combinator>, CompoundSelector<'a>)> {
        let mut pending = None;
        let mut first = true;
        self.0.children().filter_map(move |node| match node.kind() {
            SyntaxKind::Gt => {
                pending = Some(Combinator::Child);
                None
            }
            SyntaxKind::Plus => {
                pending = Some(Combinator::NextSibling);
                None
            }
            _ => {
                let compound = node.cast::<CompoundSelector<'a>>()?;
                let combinator = if first {
                    None
                } else {
                    Some(pending.take().unwrap_or(Combinator::Descendant))
                };
                first = false;
                Some((combinator, compound))
            }
        })
    }

    /// The source text of the selector.
    pub fn text(self) -> EcoString {
        self.0.full_text()
    }
}

/// How a compound selector relates to the one before it.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum Combinator {
    /// A descendant, written as whitespace: `question answer`.
    Descendant,
    /// A child: `list > item`.
    Child,
    /// The next sibling: `heading + paragraph`.
    NextSibling,
}

node! {
    /// A compound selector: `heading[level=1]:first`, `.hard`,
    /// `figure#{fig:ball}`.
    ///
    /// `exam.question` is the name `exam` with the class `question` here; the
    /// style stage reads it as a qualified element name when `exam` is a
    /// component namespace (SPEC §3.6).
    struct CompoundSelector
}

impl<'a> CompoundSelector<'a> {
    /// The element name, if there is one.
    pub fn element(self) -> Option<Ident<'a>> {
        self.0.children().next().and_then(SyntaxNode::cast)
    }

    /// The classes, id selectors, attribute tests and pseudo-classes.
    pub fn parts(self) -> impl DoubleEndedIterator<Item = SimpleSelector<'a>> {
        self.0.children().filter_map(SyntaxNode::cast)
    }
}

/// A part of a compound selector after its element name.
#[derive(Debug, Copy, Clone, Hash)]
pub enum SimpleSelector<'a> {
    /// A class: `.hard`.
    Class(ClassSelector<'a>),
    /// A labelled element: `#{fig:ball}`.
    Id(IdSelector<'a>),
    /// An attribute test: `[level=1]`.
    Attr(AttrSelector<'a>),
    /// A pseudo-class: `:first`, `:step(2-)`.
    Pseudo(PseudoClass<'a>),
}

impl<'a> AstNode<'a> for SimpleSelector<'a> {
    fn from_untyped(node: &'a SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::ClassSelector => node.cast().map(Self::Class),
            SyntaxKind::IdSelector => node.cast().map(Self::Id),
            SyntaxKind::AttrSelector => node.cast().map(Self::Attr),
            SyntaxKind::PseudoClass => node.cast().map(Self::Pseudo),
            _ => None,
        }
    }

    fn to_untyped(self) -> &'a SyntaxNode {
        match self {
            Self::Class(v) => v.to_untyped(),
            Self::Id(v) => v.to_untyped(),
            Self::Attr(v) => v.to_untyped(),
            Self::Pseudo(v) => v.to_untyped(),
        }
    }

    fn placeholder() -> Self {
        Self::Class(ClassSelector::placeholder())
    }
}

node! {
    /// A class selector: `.hard`.
    struct ClassSelector
}

impl<'a> ClassSelector<'a> {
    /// The class name.
    pub fn name(self) -> Ident<'a> {
        self.0.cast_first()
    }
}

node! {
    /// An id selector: `#{fig:ball}`.
    struct IdSelector
}

impl<'a> IdSelector<'a> {
    /// The label: `fig:ball`.
    pub fn label(self) -> &'a str {
        self.0
            .children()
            .find(|node| node.kind() == SyntaxKind::LabelName)
            .map_or("", |node| node.leaf_text())
    }
}

node! {
    /// An attribute test: `[level=1]`, `[marks>3]`, `[title]`.
    struct AttrSelector
}

impl<'a> AttrSelector<'a> {
    /// The field that is tested.
    pub fn field(self) -> Ident<'a> {
        self.0.cast_first()
    }

    /// The comparison, if any. A test without one checks that the field is
    /// set.
    pub fn op(self) -> Option<AttrOp> {
        self.0.children().find_map(|node| {
            Some(match node.kind() {
                SyntaxKind::Eq => AttrOp::Eq,
                SyntaxKind::ExclEq => AttrOp::Neq,
                SyntaxKind::Lt => AttrOp::Lt,
                SyntaxKind::LtEq => AttrOp::Leq,
                SyntaxKind::Gt => AttrOp::Gt,
                SyntaxKind::GtEq => AttrOp::Geq,
                _ => return None,
            })
        })
    }

    /// The value compared with.
    pub fn value(self) -> Option<Expr<'a>> {
        self.0.children().filter_map(SyntaxNode::cast).nth(1)
    }
}

/// The comparison of an attribute test.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum AttrOp {
    /// `=`
    Eq,
    /// `!=`
    Neq,
    /// `<`
    Lt,
    /// `<=`
    Leq,
    /// `>`
    Gt,
    /// `>=`
    Geq,
}

node! {
    /// A pseudo-class: `:first`, `:nth(2)`, `:has(answer)`, `:step(2-)`, or a
    /// component state such as `:expired`.
    struct PseudoClass
}

impl<'a> PseudoClass<'a> {
    /// The name: `first`, `nth-col`, `expired`.
    pub fn name(self) -> Ident<'a> {
        self.0.cast_first()
    }

    /// The position of `:nth(n)` and `:nth-col(n)`.
    pub fn position(self) -> Option<u64> {
        self.args()?
            .children()
            .find(|node| node.kind() == SyntaxKind::Int)?
            .leaf_text()
            .parse()
            .ok()
    }

    /// The child tested by `:has(..)`.
    pub fn has(self) -> Option<CompoundSelector<'a>> {
        self.args()?.try_cast_first()
    }

    /// The steps of `:step(..)`.
    pub fn steps(self) -> Option<StepSpec<'a>> {
        self.args()?.try_cast_first()
    }

    /// The argument list.
    fn args(self) -> Option<&'a SyntaxNode> {
        self.0
            .children()
            .find(|node| node.kind() == SyntaxKind::PseudoArgs)
    }
}

node! {
    /// The argument list of a pseudo-class.
    struct PseudoArgs
}

node! {
    /// A step specification (SPEC §10.1): `2-4`, `2-`, `-3`, `1,3-4`.
    struct StepSpec
}

impl<'a> StepSpec<'a> {
    /// The steps and ranges.
    pub fn ranges(self) -> impl DoubleEndedIterator<Item = StepRange<'a>> {
        self.0.children().filter_map(SyntaxNode::cast)
    }
}

node! {
    /// A step or a range of steps: `3`, `2-4`, `2-`, `-3`.
    struct StepRange
}

impl StepRange<'_> {
    /// The first and last step; `None` is open. A single step `3` gives
    /// `(Some(3), Some(3))`, `2-` gives `(Some(2), None)`.
    pub fn bounds(self) -> (Option<u64>, Option<u64>) {
        let mut start = None;
        let mut end = None;
        let mut dash = false;
        for node in self.0.children() {
            match node.kind() {
                SyntaxKind::Minus => dash = true,
                SyntaxKind::Int => {
                    let value = node.leaf_text().parse().ok();
                    if dash {
                        end = value;
                    } else {
                        start = value;
                    }
                }
                _ => {}
            }
        }
        if !dash {
            end = start;
        }
        (start, end)
    }
}

// --- Declarations and values -------------------------------------------------

node! {
    /// A declaration block: `{ numbering: "(a)"; }`.
    struct RuleBlock
}

impl<'a> RuleBlock<'a> {
    /// The declarations.
    pub fn declarations(self) -> impl DoubleEndedIterator<Item = Declaration<'a>> {
        self.0.children().filter_map(SyntaxNode::cast)
    }
}

node! {
    /// A declaration: `numbering: "(a)";`, `margin-column: outer, width 32mm;`,
    /// or with a qualified property `multicol.columns: 2;`.
    struct Declaration
}

impl<'a> Declaration<'a> {
    /// The property: `numbering`, `space-before`.
    pub fn property(self) -> Ident<'a> {
        self.0
            .children()
            .take_while(|node| node.kind() != SyntaxKind::Colon)
            .filter_map(SyntaxNode::cast)
            .last()
            .unwrap_or_else(Ident::placeholder)
    }

    /// The namespace of a qualified property: `multicol` in
    /// `multicol.columns`.
    pub fn namespace(self) -> Option<Ident<'a>> {
        let mut names = self
            .0
            .children()
            .take_while(|node| node.kind() != SyntaxKind::Colon)
            .filter_map(SyntaxNode::cast::<Ident<'a>>);
        let first = names.next()?;
        names.next().map(|_| first)
    }

    /// The items of the value, in order: `outer`, `width`, `32mm`, ...
    pub fn values(self) -> impl Iterator<Item = Expr<'a>> {
        values(self.0)
    }

    /// The items of the value grouped by the commas between them:
    /// `[[outer], [width, 32mm], [gap, 6mm]]`.
    pub fn groups(self) -> Vec<Vec<Expr<'a>>> {
        let mut groups = vec![vec![]];
        for node in self
            .0
            .children()
            .skip_while(|node| node.kind() != SyntaxKind::Colon)
        {
            if node.kind() == SyntaxKind::Comma {
                groups.push(vec![]);
            } else if let Some(expr) = node.cast()
                && let Some(group) = groups.last_mut()
            {
                group.push(expr);
            }
        }
        groups
    }
}

node! {
    /// A block of template items: `{ ": " field(title) }`. A template
    /// conditional (`if field(title) { .. }`) is a
    /// [`Conditional`](super::Conditional) with template blocks as bodies.
    struct TemplateBlock
}

impl<'a> TemplateBlock<'a> {
    /// The items.
    pub fn items(self) -> impl DoubleEndedIterator<Item = Expr<'a>> {
        self.0.children().filter_map(SyntaxNode::cast)
    }
}

node! {
    /// Several template items in one argument: `term() " " counter()`.
    struct Sequence
}

impl<'a> Sequence<'a> {
    /// The items.
    pub fn items(self) -> impl DoubleEndedIterator<Item = Expr<'a>> {
        self.0.children().filter_map(SyntaxNode::cast)
    }
}

node! {
    /// A template item that is shown only if a condition holds:
    /// `align-end(badge(field(difficulty))) if field(difficulty) == "extension"`.
    struct Guarded
}

impl<'a> Guarded<'a> {
    /// The item.
    pub fn item(self) -> Expr<'a> {
        self.0.cast_first()
    }

    /// The condition.
    pub fn condition(self) -> Expr<'a> {
        self.0
            .children()
            .skip_while(|node| node.kind() != SyntaxKind::If)
            .find_map(SyntaxNode::cast)
            .unwrap_or_else(Expr::placeholder)
    }
}

/// The value items after the colon of a declaration or token definition.
fn values<'a>(node: &'a SyntaxNode) -> impl Iterator<Item = Expr<'a>> {
    node.children()
        .skip_while(|node| node.kind() != SyntaxKind::Colon)
        .filter_map(SyntaxNode::cast)
}

/// The declarations in the declaration block of a node.
fn block_declarations<'a>(
    node: &'a SyntaxNode,
) -> impl DoubleEndedIterator<Item = Declaration<'a>> {
    node.try_cast_first::<RuleBlock<'a>>()
        .into_iter()
        .flat_map(RuleBlock::declarations)
}
