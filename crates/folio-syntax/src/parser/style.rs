//! The stylesheet grammar (SPEC §5.1, §5.3, §10.2, §11.2, §11.3).
//!
//! Stylesheets are parsed into the same kind of lossless syntax tree as
//! content. Rule structure (selectors, at-rules, blocks and property names) is
//! fixed syntax, and expressions appear only in property values and render
//! templates (design C of SPEC Appendix A). Values use the expression grammar
//! of scripts with three additions for templates: an argument or a value can
//! be several items written one after another (`term() " " counter()`), a
//! braced block holds such items (`{ ": " field(title) }`), and an item can be
//! guarded by a condition (`badge(..) if field(difficulty) == "extension"`).
//!
//! Scoped rules in content (`#rule question { .. }`, SPEC §5.4) use the
//! selectors and declarations of this module too.

use ecow::eco_format;
use rustc_hash::FxHashSet;

use super::{AtNewline, Marker, Parser, code_expr, code_expr_prec};
use crate::set::{SyntaxSet, syntax_set};
use crate::{SyntaxKind, SyntaxMode, SyntaxNode, ast};

/// Parses a stylesheet (`.fsty`) file.
pub fn parse_stylesheet(text: &str) -> SyntaxNode {
    let mut p = Parser::new_in(text, 0, SyntaxMode::Code, true);
    stylesheet_items(&mut p, syntax_set!(End), true);
    p.finish_into(SyntaxKind::Stylesheet)
}

/// Reparses a range of top-level stylesheet items. Succeeds only if parsing
/// ends exactly at the end of the range.
pub(crate) fn reparse_stylesheet_items(
    text: &str,
    range: std::ops::Range<usize>,
) -> Option<Vec<SyntaxNode>> {
    let mut p = Parser::new_in(text, range.start, SyntaxMode::Code, true);
    while !p.end() && p.current_start() < range.end {
        stylesheet_item(&mut p, true);
    }
    (p.balanced && p.current_start() == range.end).then(|| p.finish())
}

/// Reparses a declaration block, a slide master's body or an overlay's body.
/// Succeeds only if the new block has balanced delimiters and ends exactly at
/// the end of the range. `style` tells whether the block is in a stylesheet;
/// a declaration block is lexed as one either way, even in a scoped rule in
/// content.
pub(crate) fn reparse_style_block(
    text: &str,
    range: std::ops::Range<usize>,
    kind: SyntaxKind,
    style: bool,
) -> Option<SyntaxNode> {
    let mut p = Parser::new_in(text, range.start, SyntaxMode::Code, style);
    if !p.at(SyntaxKind::LeftBrace) {
        return None;
    }
    match kind {
        SyntaxKind::RuleBlock => rule_block(&mut p),
        SyntaxKind::MasterBlock if style => master_block(&mut p),
        SyntaxKind::OverlayBody if style => overlay_body(&mut p),
        _ => return None,
    }
    (p.balanced && p.prev_end() == range.end).then(|| p.finish().into_iter().next())?
}

/// Parses stylesheet items until the stop set. Imports are allowed only at
/// the top level.
fn stylesheet_items(p: &mut Parser<'_>, stop: SyntaxSet, top_level: bool) {
    debug_assert!(stop.contains(SyntaxKind::End));
    let Some(p) = p.check_depth_until(stop) else {
        return;
    };
    while !p.at_set(stop) {
        stylesheet_item(p, top_level);
    }
}

/// Parses one stylesheet item: a rule, an at-rule, a token definition, a
/// render template, a slide master or a slide layout.
fn stylesheet_item(p: &mut Parser<'_>, top_level: bool) {
    let Some(p) = &mut p.increase_depth() else {
        return;
    };
    match p.current() {
        SyntaxKind::At => at_rule(p, top_level),
        SyntaxKind::TokenName => token_decl(p),
        SyntaxKind::Ident
            if p.current_text() == "render" && p.peek() == Some(SyntaxKind::Ident) =>
        {
            render_block(p)
        }
        SyntaxKind::Ident
            if matches!(p.current_text(), "slide-master" | "slide-layout")
                && p.peek() == Some(SyntaxKind::Str) =>
        {
            slide_block(p)
        }
        _ if at_compound_start(p) => style_rule(p),
        _ => p.unexpected(),
    }
}

/// Parses an at-rule: `@import`, `@variant`, `@target` or `@export`.
fn at_rule(p: &mut Parser<'_>, top_level: bool) {
    let m = p.marker();
    // The name directly after the `@`.
    let (kind, node) = p.lexer.clone().next();
    let name = if kind == SyntaxKind::Ident {
        node.leaf_text().clone()
    } else {
        "".into()
    };
    match name.as_str() {
        "import" => {
            p.assert(SyntaxKind::At);
            p.eat();
            p.expect(SyntaxKind::Str);
            if !p.eat_if(SyntaxKind::Semicolon) {
                p.expected("semicolon");
            }
            p.wrap(m, SyntaxKind::StyleImport);
            if !top_level {
                p[m].convert_to_error("`@import` is allowed only at the top level");
                p[m].hint("imports apply to the whole stylesheet, not to an overlay");
            }
        }
        "variant" | "target" => {
            while p.at(SyntaxKind::At) {
                super::rule_scope(p);
            }
            if p.at(SyntaxKind::LeftBrace) {
                overlay_body(p);
            } else {
                p.expected("block of rules");
            }
            p.wrap(m, SyntaxKind::Overlay);
        }
        "export" => {
            p.assert(SyntaxKind::At);
            p.eat();
            p.expect(SyntaxKind::Ident);
            if p.at(SyntaxKind::LeftBrace) {
                rule_block(p);
            } else {
                p.expected("declaration block");
            }
            p.wrap(m, SyntaxKind::ExportBlock);
        }
        _ => {
            // Skip the unknown at-rule up to a semicolon or past its block.
            p.assert(SyntaxKind::At);
            let mut depth: usize = 0;
            while !p.end() {
                match p.current() {
                    SyntaxKind::Semicolon if depth == 0 => {
                        p.eat();
                        break;
                    }
                    SyntaxKind::LeftBrace => depth += 1,
                    SyntaxKind::RightBrace if depth == 0 => break,
                    SyntaxKind::RightBrace => {
                        depth -= 1;
                        if depth == 0 {
                            p.eat();
                            break;
                        }
                    }
                    kind if kind.is_grouping() => p.balanced = false,
                    _ => {}
                }
                p.eat();
            }
            if depth > 0 {
                p.balanced = false;
            }
            let message = if name.is_empty() {
                eco_format!("expected at-rule after `@`")
            } else {
                eco_format!("unknown at-rule: `@{name}`")
            };
            p.wrap_error(m, message);
            p[m].hint("stylesheets have `@import`, `@variant`, `@target` and `@export`");
        }
    }
}

/// Parses the body of an overlay: `{ answer { display: block; } }`.
fn overlay_body(p: &mut Parser<'_>) {
    let m = p.marker();
    p.with_nl_mode(AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftBrace);
        stylesheet_items(p, syntax_set!(RightBrace, End), false);
        p.expect_closing_delimiter(m, SyntaxKind::RightBrace);
    });
    p.wrap(m, SyntaxKind::OverlayBody);
}

/// Parses a token definition: `$accent: #1f5fa8;`.
fn token_decl(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::TokenName);
    colon_and_value(p);
    if !p.eat_if(SyntaxKind::Semicolon) && !p.at_set(syntax_set!(RightBrace, End)) {
        p.expected("semicolon");
    }
    p.wrap(m, SyntaxKind::TokenDecl);
}

/// Parses a render template: `render example { .. }`,
/// `render example as compact { .. }`.
fn render_block(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Ident);
    qualified_name(p);
    if p.eat_if(SyntaxKind::As) {
        p.expect(SyntaxKind::Ident);
    }
    if p.at(SyntaxKind::LeftBrace) {
        rule_block(p);
    } else {
        p.expected("block of template parts");
    }
    p.wrap(m, SyntaxKind::RenderBlock);
}

/// Parses an element name that may be qualified by its component's
/// namespace: `example`, `exam.question`.
fn qualified_name(p: &mut Parser<'_>) {
    if p.expect(SyntaxKind::Ident) {
        while p.directly_at(SyntaxKind::Dot) {
            p.eat();
            if !p.directly_at(SyntaxKind::Ident) {
                p.expected("identifier");
                break;
            }
            p.eat();
        }
    }
}

/// Parses a slide master or a slide layout (SPEC §11.3):
/// `slide-master "Akademeia" { .. }`, `slide-layout "Title and Content" { .. }`.
fn slide_block(p: &mut Parser<'_>) {
    let m = p.marker();
    let master = p.current_text() == "slide-master";
    p.assert(SyntaxKind::Ident);
    p.assert(SyntaxKind::Str);
    if !p.at(SyntaxKind::LeftBrace) {
        p.expected("block");
    } else if master {
        master_block(p);
    } else {
        rule_block(p);
    }
    p.wrap(
        m,
        if master {
            SyntaxKind::SlideMaster
        } else {
            SyntaxKind::SlideLayout
        },
    );
}

/// Parses a style rule: `heading + paragraph { indent: 0; }`.
fn style_rule(p: &mut Parser<'_>) {
    let m = p.marker();
    selector_list(p);
    if p.at(SyntaxKind::LeftBrace) {
        rule_block(p);
    } else {
        p.expected("declaration block");
    }
    p.wrap(m, SyntaxKind::StyleRule);
}

/// Parses a scoped rule in content after its `rule` keyword and scopes:
/// the selector list and the declaration block.
pub(super) fn scoped_rule_body(p: &mut Parser<'_>) {
    if at_compound_start(p) {
        selector_list(p);
    } else {
        p.expected("selector");
    }
    if p.at(SyntaxKind::LeftBrace) {
        rule_block(p);
    } else {
        p.expected("declaration block");
    }
}

// --- Selectors (SPEC §5.1) ---------------------------------------------------

/// Whether the parser is at an id selector in content, where `#` lexes as an
/// error in code: `#{fig:ball}`.
fn at_content_id_selector(p: &Parser<'_>) -> bool {
    p.at(SyntaxKind::Error) && p.current_text() == "#" && p.text[p.current_end()..].starts_with('{')
}

/// Parses a comma-separated list of selectors: `series.answer, fit`.
fn selector_list(p: &mut Parser<'_>) {
    let m = p.marker();
    loop {
        selector(p);
        if !p.eat_if(SyntaxKind::Comma) {
            break;
        }
    }
    p.wrap(m, SyntaxKind::SelectorList);
}

/// Parses a selector: compound selectors joined by combinators. Whitespace is
/// the descendant combinator, `>` the child and `+` the adjacent sibling
/// combinator. The general sibling combinator `~` is not supported.
fn selector(p: &mut Parser<'_>) {
    let m = p.marker();
    let mut first = true;
    loop {
        if !first {
            if p.at(SyntaxKind::Gt) || p.at(SyntaxKind::Plus) {
                p.eat();
            } else if at_tilde(p) {
                tilde(p);
            } else if !(p.had_trivia() && at_compound_start(p)) {
                break;
            }
        }
        if at_compound_start(p) {
            compound_selector(p, false);
        } else if at_tilde(p) {
            tilde(p);
            continue;
        } else {
            p.expected("selector");
            break;
        }
        first = false;
    }
    p.wrap(m, SyntaxKind::Selector);
}

/// Whether the parser is at a part that can start a compound selector.
fn at_compound_start(p: &Parser<'_>) -> bool {
    match p.current() {
        SyntaxKind::Ident
        | SyntaxKind::Dot
        | SyntaxKind::LeftBracket
        | SyntaxKind::IdSelector
        | SyntaxKind::Colon => true,
        _ => at_content_id_selector(p),
    }
}

/// Whether the parser is at a `~`, which lexes as an error.
fn at_tilde(p: &Parser<'_>) -> bool {
    p.at(SyntaxKind::Error) && p.current_text() == "~"
}

/// Reports the general sibling combinator `~`, which Folio does not have.
fn tilde(p: &mut Parser<'_>) {
    p.token.node = SyntaxNode::error("the general sibling combinator `~` is not supported", "~")
        .with_hints([
            "use `+` for the next sibling, or give the elements a class".into(),
            "a rule may depend only on its node's direct siblings and children".into(),
        ]);
    p.eat();
}

/// Parses a compound selector: an optional element name followed by classes,
/// id selectors, attribute tests and pseudo-classes, with no space between
/// them: `heading[level=1]:first`, `.hard`, `figure#{fig:ball}`.
///
/// `exam.question` is parsed as the name `exam` followed by the class
/// `question`; the style stage reads it as a qualified name when `exam` is a
/// component namespace (SPEC §3.6).
fn compound_selector(p: &mut Parser<'_>, in_has: bool) {
    let m = p.marker();
    p.eat_if(SyntaxKind::Ident);
    compound_selector_parts(p, m, in_has);
}

/// Parses the parts of a compound selector after its element name, if any,
/// and wraps everything from the marker.
fn compound_selector_parts(p: &mut Parser<'_>, m: Marker, in_has: bool) {
    loop {
        if p.marker() != m && p.had_trivia() {
            break;
        }
        match p.current() {
            SyntaxKind::Dot => class_selector(p),
            SyntaxKind::IdSelector => p.eat(),
            SyntaxKind::LeftBracket => attr_selector(p),
            SyntaxKind::Colon => pseudo_class(p, in_has),
            _ if at_content_id_selector(p) => {
                p.relex_id_selector();
                p.eat();
            }
            _ => break,
        }
    }
    if p.marker() == m {
        p.expected("selector");
    } else {
        p.wrap(m, SyntaxKind::CompoundSelector);
    }
}

/// Parses a compound selector that continues an element name already parsed
/// at the marker, in a value: `heading[level=2]` in
/// `slide-break: heading[level=2], slide;`.
pub(super) fn value_selector(p: &mut Parser<'_>, m: Marker) {
    compound_selector_parts(p, m, false);
}

/// Parses a class selector: `.hard`.
fn class_selector(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Dot);
    if p.directly_at(SyntaxKind::Ident) {
        p.eat();
    } else {
        p.expected("class name");
    }
    p.wrap(m, SyntaxKind::ClassSelector);
}

/// Parses an attribute test: `[level=1]`, `[marks>3]`, `[title]`.
fn attr_selector(p: &mut Parser<'_>) {
    let m = p.marker();
    p.with_nl_mode(AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftBracket);
        p.expect(SyntaxKind::Ident);
        if p.at_set(syntax_set!(Eq, ExclEq, Lt, LtEq, Gt, GtEq)) {
            p.eat();
            attr_value(p);
        } else if p.at(SyntaxKind::EqEq) {
            p.token.node.convert_to_error("expected `=`");
            p.token
                .node
                .hint("an attribute test is written `[level=1]`");
            p.eat();
            attr_value(p);
        }
        while !p.at_set(syntax_set!(
            RightBracket,
            LeftBrace,
            RightBrace,
            Semicolon,
            Comma,
            End
        )) {
            p.unexpected();
        }
        if !p.expect_closing_delimiter(m, SyntaxKind::RightBracket) {
            p.balanced = false;
        }
    });
    p.wrap(m, SyntaxKind::AttrSelector);
}

/// Parses the value of an attribute test: a literal or a name.
fn attr_value(p: &mut Parser<'_>) {
    const LITERAL: SyntaxSet = syntax_set!(
        Int, Float, Numeric, Str, Bool, None, Auto, Ident, TokenName, Color, Ratio
    );
    if p.at(SyntaxKind::Minus) || p.at(SyntaxKind::Plus) {
        let m = p.marker();
        p.eat();
        if p.at_set(syntax_set!(Int, Float, Numeric)) {
            p.eat();
        } else {
            p.expected("number");
        }
        p.wrap(m, SyntaxKind::Unary);
    } else if p.at_set(LITERAL) {
        p.eat();
    } else {
        p.expected("value");
    }
}

/// Parses a pseudo-class: `:first`, `:nth(2)`, `:has(answer)`, `:step(2-)`,
/// or a component state such as `:expired`.
fn pseudo_class(p: &mut Parser<'_>, in_has: bool) {
    let Some(p) = &mut p.increase_depth() else {
        return;
    };
    let m = p.marker();
    p.assert(SyntaxKind::Colon);
    if p.current().is_keyword() && !p.had_trivia() {
        p.convert_current(SyntaxKind::Ident);
    }
    if !p.directly_at(SyntaxKind::Ident) {
        p.expected("pseudo-class");
        p.wrap(m, SyntaxKind::PseudoClass);
        return;
    }

    let m_name = p.marker();
    let name = p.current_text();
    p.eat();
    let takes_args = matches!(name, "nth" | "nth-col" | "has" | "step");
    if p.directly_at(SyntaxKind::LeftParen) {
        let m_args = p.marker();
        p.with_nl_mode(AtNewline::Continue, |p| {
            p.assert(SyntaxKind::LeftParen);
            match name {
                "nth" | "nth-col" => nth_arg(p),
                "has" => has_arg(p, in_has),
                "step" => step_spec(p),
                _ => {}
            }
            let m_rest = p.marker();
            skip_to_close(p);
            if takes_args && p.marker() != m_rest {
                p.wrap_error(m_rest, "unexpected tokens in pseudo-class argument");
            }
            if !p.expect_closing_delimiter(m_args, SyntaxKind::RightParen) {
                p.balanced = false;
            }
        });
        p.wrap(m_args, SyntaxKind::PseudoArgs);
        if !takes_args && name != "not" {
            p[m_args]
                .convert_to_error(eco_format!("the pseudo-class `:{name}` takes no arguments"));
        }
    } else if takes_args {
        let example = match name {
            "nth" => "`:nth(2)`",
            "nth-col" => "`:nth-col(1)`",
            "has" => "`:has(answer)`",
            _ => "`:step(2-)`",
        };
        p.expected("argument list");
        p.hint(&eco_format!("write {example}"));
    }
    if name == "not" {
        p[m_name].convert_to_error("the pseudo-class `:not` is not supported");
        p[m_name].hint("give the elements to exclude a class, or use `:has`");
    }
    p.wrap(m, SyntaxKind::PseudoClass);
}

/// Eats tokens up to the closing parenthesis of a pseudo-class argument (or a
/// token that ends the selector).
fn skip_to_close(p: &mut Parser<'_>) {
    while !p.at_set(syntax_set!(
        RightParen, LeftBrace, RightBrace, Semicolon, End
    )) {
        p.balanced &= !p.current().is_grouping();
        p.eat();
    }
}

/// Parses the argument of `:nth` and `:nth-col`: a position counted from 1.
fn nth_arg(p: &mut Parser<'_>) {
    if p.at(SyntaxKind::Int) {
        let m = p.marker();
        let zero = p.current_text().parse::<u64>() == Ok(0);
        p.eat();
        if zero {
            p[m].convert_to_error("positions are counted from 1");
        }
    } else {
        p.expected("position");
    }
}

/// Parses the argument of `:has`: one compound selector, which tests the
/// node's direct children. Deep tests (`:has(a b)`, `:has(a > b)`, nested
/// `:has`) are rejected, because a rule may depend only on a node's direct
/// children (SPEC §5.1).
fn has_arg(p: &mut Parser<'_>, in_has: bool) {
    if at_compound_start(p) {
        let m = p.marker();
        compound_selector(p, true);
        if in_has {
            p[m].convert_to_error("`:has` cannot be nested");
            p[m].hint("`:has` tests direct children only");
        }
    } else if !p.at_set(syntax_set!(Gt, Plus)) {
        p.expected("selector");
        return;
    }
    if p.at_set(syntax_set!(Gt, Plus)) || at_compound_start(p) || at_tilde(p) {
        let m = p.marker();
        skip_to_close(p);
        p.wrap_error(m, "`:has` is limited to direct children");
        p.hint("write `:has(answer)` to test for a child `answer`");
    }
}

/// Parses a step specification in `:step(..)` with the step grammar of
/// SPEC §10.1: a step `3`, ranges `2-4`, open ranges `2-` and `-3`, and lists
/// `1,3-4`.
fn step_spec(p: &mut Parser<'_>) {
    let m = p.marker();
    loop {
        step_range(p);
        if !p.eat_if(SyntaxKind::Comma) {
            break;
        }
    }
    p.wrap(m, SyntaxKind::StepSpec);
}

/// Parses a step or a range of steps: `3`, `2-4`, `2-`, `-3`.
fn step_range(p: &mut Parser<'_>) {
    let m = p.marker();
    let start = step_bound(p);
    let mut end = None;
    if p.at(SyntaxKind::Minus) && (start.is_none() || !p.had_trivia()) {
        p.eat();
        if !p.had_trivia() {
            end = step_bound(p);
        }
    }
    if p.marker() == m {
        p.expected("step");
        return;
    }
    p.wrap(m, SyntaxKind::StepRange);
    if start.is_none() && end.is_none() {
        p[m].convert_to_error("expected step");
    } else if let (Some(Some(a)), Some(Some(b))) = (start, end)
        && a > b
    {
        p[m].convert_to_error(eco_format!("the step range `{a}-{b}` is empty"));
    }
}

/// Parses a step number. Returns `None` if there is none, and `Some(None)`
/// for a number that is not a valid step.
fn step_bound(p: &mut Parser<'_>) -> Option<Option<u64>> {
    match p.current() {
        SyntaxKind::Int => {
            let m = p.marker();
            let value = p.current_text().parse::<u64>().ok().filter(|&n| n > 0);
            p.eat();
            if value.is_none() {
                p[m].convert_to_error("steps are counted from 1");
            }
            Some(value)
        }
        SyntaxKind::Plus | SyntaxKind::Dot => {
            p.token
                .node
                .convert_to_error("the pause counter cannot be used in a selector");
            p.token
                .node
                .hint("`+` and `.` refer to positions in content: `#uncover(\"+-\")`");
            p.eat();
            Some(None)
        }
        _ => None,
    }
}

// --- Declarations and values -------------------------------------------------

/// Parses a declaration block: `{ numbering: "(a)"; }`.
pub(super) fn rule_block(p: &mut Parser<'_>) {
    block_of_declarations(p, false);
}

/// Parses the body of a slide master: declarations and placeholder rules.
fn master_block(p: &mut Parser<'_>) {
    block_of_declarations(p, true);
}

/// Parses a block of declarations, which in a slide master may also contain
/// placeholder rules. The block is lexed as a stylesheet wherever it is, so
/// the declarations of a scoped rule in content are written as in a
/// stylesheet (SPEC §5.4): `#d62728` is a colour and `$size` a token.
fn block_of_declarations(p: &mut Parser<'_>, master: bool) {
    let m = p.marker();
    p.enter_lexer(SyntaxMode::Code, true, AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftBrace);
        while !p.at_set(syntax_set!(RightBrace, RightBracket, RightParen, End)) {
            if p.at(SyntaxKind::Ident) && p.peek() == Some(SyntaxKind::LeftBrace) {
                let m_rule = p.marker();
                placeholder_rule(p);
                if !master {
                    p[m_rule].convert_to_error("nested rules are not supported here");
                    p[m_rule].hint("only slide masters contain rules, for their placeholders");
                }
            } else if p.at(SyntaxKind::Ident) {
                declaration(p);
            } else if !p.eat_if(SyntaxKind::Semicolon) {
                p.unexpected();
            }
        }
        p.expect_closing_delimiter(m, SyntaxKind::RightBrace);
    });
    p.wrap(
        m,
        if master {
            SyntaxKind::MasterBlock
        } else {
            SyntaxKind::RuleBlock
        },
    );
}

/// Parses a placeholder rule in a slide master: `title { font-size: 32pt; }`.
fn placeholder_rule(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Ident);
    rule_block(p);
    p.wrap(m, SyntaxKind::PlaceholderRule);
}

/// Parses a declaration: `space-before: 24pt;`, or with a qualified property
/// name `multicol.columns: 2;`. The value is a sequence of items, optionally
/// separated by commas: `margin-column: outer, width 32mm, gap 6mm`.
fn declaration(p: &mut Parser<'_>) {
    let m = p.marker();
    p.assert(SyntaxKind::Ident);
    while p.directly_at(SyntaxKind::Dot) {
        p.eat();
        if !p.directly_at(SyntaxKind::Ident) {
            p.expected("property name");
            break;
        }
        p.eat();
    }
    colon_and_value(p);
    p.eat_if(SyntaxKind::Semicolon);
    p.wrap(m, SyntaxKind::Declaration);
}

/// Parses the colon and the value of a declaration or token definition. A
/// value after a missing colon is still parsed: `numbering "(a)";`.
fn colon_and_value(p: &mut Parser<'_>) {
    if p.expect(SyntaxKind::Colon) || p.at_set(ITEM_START) {
        value(p);
    }
}

/// The tokens that end a value.
const VALUE_END: SyntaxSet = syntax_set!(Semicolon, RightBrace, RightBracket, RightParen, End);

/// The tokens that can start an item of a value or a template.
const ITEM_START: SyntaxSet = syntax_set!(
    Ident,
    LeftBrace,
    LeftBracket,
    LeftParen,
    Dollar,
    TexEquation,
    None,
    Auto,
    Int,
    Float,
    Bool,
    Numeric,
    Str,
    Label,
    Raw,
    TokenName,
    Color,
    Ratio,
    Plus,
    Minus,
    Not,
    If,
);

/// Parses the value of a declaration or a token definition: items, which may
/// be separated by commas, up to a semicolon or the end of the block. A new
/// line that starts with `name:` begins the next declaration, so a missing
/// semicolon there is reported without losing that declaration.
fn value(p: &mut Parser<'_>) {
    let was = std::mem::replace(&mut p.style_values, true);
    let split = std::mem::replace(&mut p.split_signs, true);
    let m = p.marker();
    let mut last = None;
    while !p.at_set(VALUE_END) {
        if p.had_newline()
            && ((p.at(SyntaxKind::Ident) && p.peek() == Some(SyntaxKind::Colon))
                || at_rule_start(p))
        {
            p.expected("semicolon");
            break;
        }
        if p.at_set(ITEM_START) {
            item(p, &mut last);
        } else if p.eat_if(SyntaxKind::Comma) {
            last = None;
        } else if p.at(SyntaxKind::Error) && p.current_text() == "!" {
            important(p);
        } else {
            p.unexpected();
        }
    }
    if p.marker() == m {
        p.expected("value");
    }
    p.style_values = was;
    p.split_signs = split;
}

/// Whether a line of a value looks like the start of a rule: a selector
/// followed by `{` on the same line, as in `heading { .. }` after a token
/// definition that lacks its semicolon. Lines with a template conditional
/// (`slot(a) if c { .. }`) do not count.
fn at_rule_start(p: &Parser<'_>) -> bool {
    if !p.lexer.style() || !at_compound_start(p) {
        return false;
    }
    let mut lexer = p.lexer.clone();
    loop {
        let (kind, _) = lexer.next();
        match kind {
            SyntaxKind::LeftBrace => return true,
            SyntaxKind::Semicolon
            | SyntaxKind::RightBrace
            | SyntaxKind::If
            | SyntaxKind::Else
            | SyntaxKind::End => return false,
            kind if kind.is_trivia() && lexer.newline() => return false,
            _ => {}
        }
    }
}

/// Reports `!important`, which Folio does not have.
fn important(p: &mut Parser<'_>) {
    let m = p.marker();
    p.eat();
    if p.directly_at(SyntaxKind::Ident) && p.current_text() == "important" {
        p.eat();
        p.wrap_error(m, "`!important` does not exist");
        p.hint("rules are ordered by origin, specificity and source order (SPEC §5.1)");
    }
}

/// Parses one item of a value or a template, or a condition: an expression;
/// `if cond { .. } else { .. }`, which shows one block of items or the other;
/// or `.. if cond` after an item, which shows that item only if the condition
/// holds.
fn item(p: &mut Parser<'_>, last: &mut Option<Marker>) {
    let Some(p) = &mut p.increase_depth() else {
        return;
    };
    let m = p.marker();
    if !p.at(SyntaxKind::If) {
        code_expr(p);
        *last = Some(m);
        return;
    }

    p.assert(SyntaxKind::If);
    condition(p);
    if p.at(SyntaxKind::LeftBrace) {
        conditional_rest(p, m);
        *last = Some(m);
    } else if let Some(m_item) = last.take() {
        p.wrap(m_item, SyntaxKind::Guarded);
        *last = Some(m_item);
    } else {
        p.wrap(m, SyntaxKind::Guarded);
        p[m].convert_to_error("expected an item before `if`");
        p[m].hint("write `item if condition`, or `if condition { items }`");
        *last = Some(m);
    }
}

/// Parses the condition of a template conditional or a guarded item.
fn condition(p: &mut Parser<'_>) {
    let split = std::mem::replace(&mut p.split_signs, false);
    code_expr_prec(p, false, ast::BinOp::Assign.precedence() + 1);
    p.split_signs = split;
}

/// Parses the blocks of a template conditional after its condition:
/// `{ ": " field(title) } else { .. }`.
fn conditional_rest(p: &mut Parser<'_>, m: Marker) {
    let Some(p) = &mut p.increase_depth() else {
        return;
    };
    template_block(p);
    if p.eat_if(SyntaxKind::Else) {
        if p.at(SyntaxKind::If) {
            let m_else = p.marker();
            p.assert(SyntaxKind::If);
            condition(p);
            if p.at(SyntaxKind::LeftBrace) {
                conditional_rest(p, m_else);
            } else {
                p.expected("block");
                p.wrap(m_else, SyntaxKind::Conditional);
            }
        } else if p.at(SyntaxKind::LeftBrace) {
            template_block(p);
        } else {
            p.expected("block");
        }
    }
    p.wrap(m, SyntaxKind::Conditional);
}

/// Parses a block of template items: `{ ": " field(title) }`.
pub(super) fn template_block(p: &mut Parser<'_>) {
    let m = p.marker();
    let split = std::mem::replace(&mut p.split_signs, true);
    p.with_nl_mode(AtNewline::Continue, |p| {
        p.assert(SyntaxKind::LeftBrace);
        let mut last = None;
        while !p.at_set(syntax_set!(RightBrace, RightBracket, RightParen, End)) {
            if p.at_set(ITEM_START) {
                item(p, &mut last);
            } else {
                p.unexpected();
            }
        }
        p.expect_closing_delimiter(m, SyntaxKind::RightBrace);
    });
    p.split_signs = split;
    p.wrap(m, SyntaxKind::TemplateBlock);
}

/// The tokens that can start a function call's argument in a value.
pub(super) const ARG_START: SyntaxSet = ITEM_START.add(SyntaxKind::Dots);

/// Parses a function call's argument in a value or a template. A positional
/// argument, and the value of a named one, can be several items written one
/// after another: `strong(term() " " counter())`.
pub(super) fn template_arg(p: &mut Parser<'_>, seen: &mut FxHashSet<ecow::EcoString>) {
    let m = p.marker();
    if p.eat_if(SyntaxKind::Dots) {
        code_expr(p);
        p.wrap(m, SyntaxKind::Spread);
        return;
    }

    let named = p.at(SyntaxKind::Ident) && p.peek() == Some(SyntaxKind::Colon);
    if named {
        let name: ecow::EcoString = p.current_text().into();
        p.eat();
        if !seen.insert(name.clone()) {
            p[m].convert_to_error(eco_format!("duplicate argument: {name}"));
        }
        p.assert(SyntaxKind::Colon);
    }

    let m_value = p.marker();
    let mut last = None;
    let mut count = 0;
    let split = std::mem::replace(&mut p.split_signs, true);
    while p.at_set(ITEM_START) {
        item(p, &mut last);
        count += 1;
    }
    p.split_signs = split;
    if count == 0 {
        p.expected("expression");
    } else if p.nodes[m_value.0..]
        .iter()
        .filter(|node| !node.kind().is_trivia())
        .count()
        > 1
    {
        p.wrap(m_value, SyntaxKind::Sequence);
    }
    if named {
        p.wrap(m, SyntaxKind::Named);
    }
}
