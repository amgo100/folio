//! Unit tests for the stylesheet grammar (SPEC §5.1, §5.3, §10.2, §11.2,
//! §11.3): items, selectors, declarations, template values, errors and
//! incremental reparsing.

use folio_syntax::ast::{self, AstNode, AttrOp, Combinator, Expr, SimpleSelector, StyleItem};
use folio_syntax::{Source, SyntaxKind, SyntaxNode, parse, parse_stylesheet};

/// Parses a stylesheet, checks the round trip and returns the root.
#[track_caller]
fn root(text: &str) -> SyntaxNode {
    let root = parse_stylesheet(text);
    assert_eq!(root.full_text(), text);
    assert_eq!(root.kind(), SyntaxKind::Stylesheet);
    root
}

/// The messages of all syntax errors in a stylesheet.
fn errors(text: &str) -> Vec<String> {
    let source = Source::detached_stylesheet(text);
    let (errors, _) = source.root().errors_and_warnings();
    errors
        .into_iter()
        .map(|error| error.message.to_string())
        .collect()
}

/// Asserts that a stylesheet parses without syntax errors.
#[track_caller]
fn ok(text: &str) {
    assert_eq!(errors(text), Vec::<String>::new(), "errors in {text:?}");
}

/// All nodes of the tree in depth-first order.
fn nodes(node: &SyntaxNode) -> Vec<&SyntaxNode> {
    let mut out = vec![node];
    for child in node.children() {
        out.extend(nodes(child));
    }
    out
}

/// The number of nodes of `kind` in the tree of `root`.
fn count(root: &SyntaxNode, kind: SyntaxKind) -> usize {
    nodes(root)
        .into_iter()
        .filter(|node| node.kind() == kind)
        .count()
}

/// The first node of type `T` in the tree of `root`.
#[track_caller]
fn first<'a, T: AstNode<'a>>(root: &'a SyntaxNode) -> T {
    nodes(root)
        .into_iter()
        .find_map(SyntaxNode::cast)
        .unwrap_or_else(|| panic!("no such node in {root:#?}"))
}

/// The items of a stylesheet.
fn items(root: &SyntaxNode) -> Vec<StyleItem<'_>> {
    root.cast::<ast::Stylesheet<'_>>()
        .unwrap()
        .items()
        .collect()
}

/// The source text of a node.
fn src<'a>(node: impl AstNode<'a>) -> String {
    node.to_untyped().full_text().to_string()
}

/// The selector of the first rule, as (combinator, compound) pairs of text.
fn steps(text: &str) -> Vec<(Option<Combinator>, String)> {
    let root = root(text);
    let selector: ast::Selector<'_> = first(&root);
    selector
        .steps()
        .map(|(combinator, compound)| (combinator, src(compound)))
        .collect()
}

// --- Items -------------------------------------------------------------------

#[test]
fn items_of_a_stylesheet() {
    let text = "@import \"school-base.fsty\";\n\
                $accent: #1f5fa8;\n\
                page { page-size: A4; }\n\
                @variant markscheme, solutions { answer { display: block; } }\n\
                @target pptx { slide { transition: fade 300ms; } }\n\
                @export theme { accent1: $accent; }\n\
                render example as compact { body: slot(statement); }\n\
                slide-master \"Akademeia\" { slide-size: 16:9; title { font-size: 32pt; } }\n\
                slide-layout \"Title\" { title: area(5%, 5%, 90%, 15%); }\n";
    ok(text);
    let root = root(text);
    let items = items(&root);
    assert_eq!(items.len(), 9);

    let StyleItem::Import(import) = items[0] else {
        panic!()
    };
    assert_eq!(import.path().unwrap().get(), "school-base.fsty");

    let StyleItem::Token(token) = items[1] else {
        panic!()
    };
    assert_eq!(token.name().get(), "accent");
    let Some(Expr::Color(color)) = token.values().next() else {
        panic!()
    };
    assert_eq!(color.rgba(), [0x1f, 0x5f, 0xa8, 255]);

    let StyleItem::Rule(rule) = items[2] else {
        panic!()
    };
    assert_eq!(text_of_selectors(rule.selectors()), ["page"]);
    assert_eq!(
        rule.declarations().next().unwrap().property().as_str(),
        "page-size"
    );

    let StyleItem::Overlay(overlay) = items[3] else {
        panic!()
    };
    let scope = overlay.scopes().next().unwrap();
    assert_eq!(scope.kind(), "variant");
    let names: Vec<_> = scope.names().map(|n| n.as_str().to_string()).collect();
    assert_eq!(names, ["markscheme", "solutions"]);
    assert_eq!(overlay.items().count(), 1);

    let StyleItem::Overlay(overlay) = items[4] else {
        panic!()
    };
    assert_eq!(overlay.scopes().next().unwrap().kind(), "target");

    let StyleItem::Export(export) = items[5] else {
        panic!()
    };
    assert_eq!(export.target().as_str(), "theme");
    assert_eq!(export.declarations().count(), 1);

    let StyleItem::Render(render) = items[6] else {
        panic!()
    };
    let element: Vec<_> = render.element().map(|n| n.as_str().to_string()).collect();
    assert_eq!(element, ["example"]);
    assert_eq!(render.name().unwrap().as_str(), "compact");
    assert_eq!(render.parts().next().unwrap().property().as_str(), "body");

    let StyleItem::SlideMaster(master) = items[7] else {
        panic!()
    };
    assert_eq!(master.name().get(), "Akademeia");
    assert_eq!(master.declarations().count(), 1);
    let Some(Expr::Ratio(ratio)) = master.declarations().next().unwrap().values().next() else {
        panic!()
    };
    assert_eq!(ratio.get(), (16.0, 9.0));
    let placeholder = master.placeholders().next().unwrap();
    assert_eq!(placeholder.name().as_str(), "title");
    assert_eq!(placeholder.declarations().count(), 1);

    let StyleItem::SlideLayout(layout) = items[8] else {
        panic!()
    };
    assert_eq!(layout.name().get(), "Title");
    assert_eq!(layout.declarations().count(), 1);
}

/// The text of each selector in a list.
fn text_of_selectors(list: ast::SelectorList<'_>) -> Vec<String> {
    list.iter()
        .map(|selector| selector.text().to_string())
        .collect()
}

#[test]
fn render_and_slide_words_are_names_elsewhere() {
    // `render` followed by a name is a template; elsewhere it is an element.
    for text in [
        "render { a: 1; }",
        "render > x { a: 1; }",
        "render.x { a: 1; }",
        "slide-master { a: 1; }",
        "slide-layout x { a: 1; }",
    ] {
        ok(text);
        let root = root(text);
        assert!(matches!(items(&root)[..], [StyleItem::Rule(_)]), "{text}");
    }
    let root = root("render exam.question { body: slot(stem); }");
    let render: ast::RenderBlock<'_> = first(&root);
    let element: Vec<_> = render.element().map(|n| n.as_str().to_string()).collect();
    assert_eq!(element, ["exam", "question"]);
    assert_eq!(render.name(), None);
}

#[test]
fn nested_overlays() {
    let text = "@target docx { @variant markscheme { mark { export-name: \"Mark\"; } } }";
    ok(text);
    let root = root(text);
    assert_eq!(count(&root, SyntaxKind::Overlay), 2);
    // Several scopes on one overlay.
    let text = "@variant slides @target pptx { a { b: 1; } }";
    ok(text);
    let root = self::root(text);
    let overlay: ast::Overlay<'_> = first(&root);
    let kinds: Vec<_> = overlay
        .scopes()
        .map(|scope| scope.kind().to_string())
        .collect();
    assert_eq!(kinds, ["variant", "target"]);
}

#[test]
fn comments() {
    let text =
        "// line\n/* block /* nested */ */\nquestion { /* inside */ numbering: \"1.\"; // end\n}";
    ok(text);
    let root = root(text);
    assert_eq!(count(&root, SyntaxKind::LineComment), 2);
    assert_eq!(count(&root, SyntaxKind::BlockComment), 2);
    assert_eq!(count(&root, SyntaxKind::Declaration), 1);
}

// --- Selectors ---------------------------------------------------------------

#[test]
fn combinators() {
    use Combinator::*;
    assert_eq!(
        steps("question answer {}"),
        [
            (None, "question".into()),
            (Some(Descendant), "answer".into())
        ]
    );
    assert_eq!(
        steps("list > item {}"),
        [(None, "list".into()), (Some(Child), "item".into())]
    );
    assert_eq!(
        steps("heading+paragraph {}"),
        [
            (None, "heading".into()),
            (Some(NextSibling), "paragraph".into())
        ]
    );
    assert_eq!(
        steps("grid.results > cell:nth-col(1) .x {}"),
        [
            (None, "grid.results".into()),
            (Some(Child), "cell:nth-col(1)".into()),
            (Some(Descendant), ".x".into()),
        ]
    );
}

#[test]
fn selector_lists() {
    let root = root("series.answer, fit, plot gridline { display: none; }");
    let rule: ast::StyleRule<'_> = first(&root);
    assert_eq!(
        text_of_selectors(rule.selectors()),
        ["series.answer", "fit", "plot gridline"]
    );
}

#[test]
fn compound_selectors() {
    let text = "figure.wide.hard#{fig:ball}[placement=wrap][width>=30%]:first:has(caption) {}";
    ok(text);
    let root = root(text);
    let compound: ast::CompoundSelector<'_> = first(&root);
    assert_eq!(compound.element().unwrap().as_str(), "figure");
    let parts: Vec<_> = compound.parts().collect();
    assert_eq!(parts.len(), 7);
    let SimpleSelector::Class(class) = parts[0] else {
        panic!()
    };
    assert_eq!(class.name().as_str(), "wide");
    let SimpleSelector::Id(id) = parts[2] else {
        panic!()
    };
    assert_eq!(id.label(), "fig:ball");
    let SimpleSelector::Attr(attr) = parts[3] else {
        panic!()
    };
    assert_eq!(attr.field().as_str(), "placement");
    assert_eq!(attr.op(), Some(AttrOp::Eq));
    assert_eq!(src(attr.value().unwrap()), "wrap");
    let SimpleSelector::Attr(attr) = parts[4] else {
        panic!()
    };
    assert_eq!(attr.op(), Some(AttrOp::Geq));
    let SimpleSelector::Pseudo(pseudo) = parts[5] else {
        panic!()
    };
    assert_eq!(pseudo.name().as_str(), "first");
    let SimpleSelector::Pseudo(pseudo) = parts[6] else {
        panic!()
    };
    assert_eq!(src(pseudo.has().unwrap()), "caption");

    // Without an element name.
    let root = self::root(".hard {} #{sec:kin-1} {} :first {} [level=1] {}");
    assert_eq!(count(&root, SyntaxKind::StyleRule), 4);
    let compound: ast::CompoundSelector<'_> = first(&root);
    assert_eq!(compound.element(), None);
}

#[test]
fn attribute_tests() {
    for (text, op, value) in [
        ("[level=1]", Some(AttrOp::Eq), Some("1")),
        ("[level != 1]", Some(AttrOp::Neq), Some("1")),
        ("[marks>3]", Some(AttrOp::Gt), Some("3")),
        ("[marks<=-1]", Some(AttrOp::Leq), Some("-1")),
        ("[marks<2.5]", Some(AttrOp::Lt), Some("2.5")),
        (
            "[difficulty=\"extension\"]",
            Some(AttrOp::Eq),
            Some("\"extension\""),
        ),
        ("[numbered=false]", Some(AttrOp::Eq), Some("false")),
        ("[title]", None, None),
    ] {
        let text = format!("question{text} {{}}");
        ok(&text);
        let root = root(&text);
        let attr: ast::AttrSelector<'_> = first(&root);
        assert_eq!(attr.op(), op, "{text}");
        assert_eq!(attr.value().map(src).as_deref(), value, "{text}");
    }
}

#[test]
fn qualified_names_parse_as_name_and_class() {
    // The style stage tells `exam.question` (a qualified name) from
    // `example.short` (a class); both parse alike.
    let root = root("exam.question {} example.short {}");
    let compounds: Vec<_> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast::<ast::CompoundSelector<'_>>)
        .map(|compound| {
            let SimpleSelector::Class(class) = compound.parts().next().unwrap() else {
                panic!()
            };
            (
                compound.element().unwrap().as_str().to_string(),
                class.name().as_str().to_string(),
            )
        })
        .collect();
    assert_eq!(
        compounds,
        [
            ("exam".into(), "question".into()),
            ("example".into(), "short".into())
        ]
    );
}

#[test]
fn pseudo_classes() {
    for (text, name) in [
        ("item:first", "first"),
        ("item:last", "last"),
        ("countdown:expired", "expired"),
        ("slider:dragging", "dragging"),
    ] {
        let text = format!("{text} {{}}");
        ok(&text);
        let root = root(&text);
        let pseudo: ast::PseudoClass<'_> = first(&root);
        assert_eq!(pseudo.name().as_str(), name);
    }
    let root = root("item:nth(3) {} cell:nth-col(12) {}");
    let positions: Vec<_> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast::<ast::PseudoClass<'_>>)
        .map(|pseudo| pseudo.position())
        .collect();
    assert_eq!(positions, [Some(3), Some(12)]);
}

#[test]
fn step_specifications() {
    for (spec, ranges) in [
        ("3", vec![(Some(3), Some(3))]),
        ("2-4", vec![(Some(2), Some(4))]),
        ("2-", vec![(Some(2), None)]),
        ("-3", vec![(None, Some(3))]),
        ("1,3-4", vec![(Some(1), Some(1)), (Some(3), Some(4))]),
        (
            "1, 3-, 5",
            vec![(Some(1), Some(1)), (Some(3), None), (Some(5), Some(5))],
        ),
    ] {
        let text = format!("arrow.force:step({spec}) {{ stroke-color: $accent; }}");
        ok(&text);
        let root = root(&text);
        let pseudo: ast::PseudoClass<'_> = first(&root);
        let found: Vec<_> = pseudo
            .steps()
            .unwrap()
            .ranges()
            .map(|r| r.bounds())
            .collect();
        assert_eq!(found, ranges, "{spec}");
    }
}

// --- Declarations and values -------------------------------------------------

#[test]
fn declaration_values() {
    let text =
        "page { margin-column: outer, width 32mm, gap 6mm; border-left: 1.5pt solid $accent; }";
    ok(text);
    let root = root(text);
    let rule: ast::StyleRule<'_> = first(&root);
    let decls: Vec<_> = rule.declarations().collect();
    let groups: Vec<Vec<String>> = decls[0]
        .groups()
        .into_iter()
        .map(|group| group.into_iter().map(src).collect())
        .collect();
    assert_eq!(
        groups,
        [vec!["outer"], vec!["width", "32mm"], vec!["gap", "6mm"]]
    );
    let values: Vec<_> = decls[1].values().map(src).collect();
    assert_eq!(values, ["1.5pt", "solid", "$accent"]);
    assert!(matches!(decls[1].values().nth(2), Some(Expr::TokenName(_))));
}

#[test]
fn signs_start_items_as_in_css() {
    let root = root("q { margin: 0 -1mm; width: 100% - 2mm; inset: 1mm-2mm +3mm; }");
    let counts: Vec<_> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast::<ast::Declaration<'_>>)
        .map(|decl| decl.values().map(src).collect::<Vec<_>>())
        .collect();
    assert_eq!(
        counts,
        [
            vec!["0", "-1mm"],
            vec!["100% - 2mm"],
            vec!["1mm-2mm", "+3mm"]
        ]
    );
}

#[test]
fn qualified_property_names() {
    let text = "glossary { multicol.columns: 3; columns: 2; }";
    ok(text);
    let root = root(text);
    let rule: ast::StyleRule<'_> = first(&root);
    let decls: Vec<_> = rule
        .declarations()
        .map(|decl| {
            (
                decl.namespace().map(|ns| ns.as_str().to_string()),
                decl.property().as_str().to_string(),
            )
        })
        .collect();
    assert_eq!(
        decls,
        [
            (Some("multicol".into()), "columns".into()),
            (None, "columns".into())
        ]
    );
}

#[test]
fn colours_tokens_and_ratios() {
    for (text, rgba) in [
        ("#1f5fa8", [0x1f, 0x5f, 0xa8, 255]),
        ("#fff", [255, 255, 255, 255]),
        ("#1f5fa880", [0x1f, 0x5f, 0xa8, 0x80]),
        ("#0008", [0, 0, 0, 0x88]),
    ] {
        let text = format!("$c: {text};");
        ok(&text);
        let root = root(&text);
        let color: ast::Color<'_> = first(&root);
        assert_eq!(color.rgba(), rgba, "{text}");
    }
    let root = root("$wide: 2.39:1; x { slide-size: 16:9; color: color($accent-strong); }");
    let ratio: ast::Ratio<'_> = first(&root);
    assert_eq!(ratio.get(), (2.39, 1.0));
    let names: Vec<_> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast::<ast::TokenName<'_>>)
        .map(|name| name.get().to_string())
        .collect();
    assert_eq!(names, ["wide", "accent-strong"]);
}

#[test]
fn selectors_in_values() {
    let text = "document { slide-break: heading[level=2], slide; show-only: answer mark; }";
    ok(text);
    let root = root(text);
    let decl: ast::Declaration<'_> = first(&root);
    let values: Vec<_> = decl.values().collect();
    let Expr::CompoundSelector(selector) = values[0] else {
        panic!("{values:?}")
    };
    assert_eq!(selector.element().unwrap().as_str(), "heading");
    assert!(matches!(values[1], Expr::Ident(_)));
}

#[test]
fn template_items() {
    let text = "render example {\n  \
                header: row(\n    \
                strong(term() \" \" counter() if field(title) { \": \" field(title) }),\n    \
                align-end(badge(field(difficulty))) if field(difficulty) == \"extension\",\n  \
                );\n  \
                body: slot(statement) rule(0.4pt) slot(solution);\n\
                }";
    ok(text);
    let root = root(text);
    let render: ast::RenderBlock<'_> = first(&root);
    let parts: Vec<_> = render.parts().collect();
    assert_eq!(parts[0].property().as_str(), "header");
    assert_eq!(parts[1].values().count(), 3);

    let sequence: ast::Sequence<'_> = first(&root);
    let items: Vec<_> = sequence.items().collect();
    assert_eq!(items.len(), 4);
    let Expr::Conditional(conditional) = items[3] else {
        panic!()
    };
    assert_eq!(src(conditional.condition()), "field(title)");
    let Expr::TemplateBlock(block) = conditional.if_body() else {
        panic!()
    };
    assert_eq!(block.items().count(), 2);

    let guarded: ast::Guarded<'_> = first(&root);
    assert_eq!(src(guarded.item()), "align-end(badge(field(difficulty)))");
    assert_eq!(
        src(guarded.condition()),
        "field(difficulty) == \"extension\""
    );
}

#[test]
fn template_conditionals_with_else() {
    let text = "render q { trailer: if field(marks) == 1 { \"[1 mark]\" } \
                else if field(marks) > 1 { \"[\" field(marks) \" marks]\" } else { }; }";
    ok(text);
    let root = root(text);
    let conditional: ast::Conditional<'_> = first(&root);
    let Some(Expr::Conditional(inner)) = conditional.else_body() else {
        panic!()
    };
    assert!(matches!(inner.else_body(), Some(Expr::TemplateBlock(_))));
}

#[test]
fn named_template_arguments() {
    let text = "render q { body: box(slot(stem) rule(1pt), padding: 2mm 3mm, fill: $tint); }";
    ok(text);
    let root = root(text);
    let call: ast::FuncCall<'_> = first(&root);
    let args: Vec<_> = call.args().items().map(|arg| text_of_arg(arg)).collect();
    assert_eq!(
        args,
        ["slot(stem) rule(1pt)", "padding: 2mm 3mm", "fill: $tint"]
    );
    assert_eq!(count(&root, SyntaxKind::Sequence), 2);
}

/// The source text of an argument.
fn text_of_arg(arg: ast::Arg<'_>) -> String {
    match arg {
        ast::Arg::Pos(expr) => src(expr),
        ast::Arg::Named(named) => src(named),
        ast::Arg::Spread(spread) => src(spread),
    }
}

#[test]
fn code_and_content_blocks_in_values_are_script() {
    // Inside a content block, `$` starts math and `#` an embedded expression,
    // as in content files.
    let text = "countdown { fallback: [#strong[Five] minutes, $t = 5$]; }";
    ok(text);
    let root = root(text);
    assert_eq!(count(&root, SyntaxKind::Equation), 1);
    assert_eq!(count(&root, SyntaxKind::TokenName), 0);
    assert_eq!(
        count(&root, SyntaxKind::Strong) + count(&root, SyntaxKind::FuncCall),
        1
    );
}

// --- Errors ------------------------------------------------------------------

#[test]
fn excluded_selectors_are_rejected() {
    assert_eq!(
        errors("heading ~ paragraph { indent: 0; }"),
        ["the general sibling combinator `~` is not supported"]
    );
    assert_eq!(
        errors("question:has(part answer) { a: 1; }"),
        ["`:has` is limited to direct children"]
    );
    assert_eq!(
        errors("question:has(part > answer) { a: 1; }"),
        ["`:has` is limited to direct children"]
    );
    assert_eq!(
        errors("question:has(part:has(answer)) { a: 1; }"),
        ["`:has` cannot be nested"]
    );
    assert_eq!(
        errors("answer:not(.x) { a: 1; }"),
        ["the pseudo-class `:not` is not supported"]
    );
    // The rule still parses after the error.
    let root = root("heading ~ paragraph { indent: 0; }");
    assert_eq!(count(&root, SyntaxKind::Declaration), 1);
}

#[test]
fn pseudo_class_errors() {
    assert_eq!(errors("a:step(4-2) {}"), ["the step range `4-2` is empty"]);
    assert_eq!(errors("a:step(0) {}"), ["steps are counted from 1"]);
    assert_eq!(
        errors("a:step(+-) {}"),
        ["the pause counter cannot be used in a selector"]
    );
    assert_eq!(errors("a:step() {}"), ["expected step"]);
    assert_eq!(errors("a:nth {}"), ["expected argument list"]);
    assert_eq!(errors("a:nth(0) {}"), ["positions are counted from 1"]);
    assert_eq!(
        errors("a:nth(x) {}"),
        [
            "expected position",
            "unexpected tokens in pseudo-class argument"
        ]
    );
    assert_eq!(
        errors("a:first(2) {}"),
        ["the pseudo-class `:first` takes no arguments"]
    );
    assert_eq!(errors("a:step(2-4 {}"), ["unclosed delimiter"]);
}

#[test]
fn value_errors() {
    assert_eq!(
        errors("q { color: red !important; }"),
        ["`!important` does not exist"]
    );
    assert_eq!(errors("q { numbering \"(a)\"; }"), ["expected colon"]);
    assert_eq!(errors("q { a: ; }"), ["expected value"]);
    assert_eq!(errors("q { a: 1\n  b: 2; }"), ["expected semicolon"]);
    assert_eq!(errors("$c: #12345g;"), ["invalid colour: `#12345g`"]);
    assert_eq!(errors("$c: #12;"), ["invalid colour: `#12`"]);
    assert_eq!(
        errors("$c: 16:9pt;"),
        ["a ratio cannot have a suffix: `pt`"]
    );
    assert_eq!(
        errors("render x { a: row(if y); }"),
        ["expected an item before `if`"]
    );
    assert_eq!(errors("$c #fff;"), ["expected colon"]);
}

#[test]
fn structure_errors() {
    assert_eq!(
        errors("@import \"base.fsty\"\nq {}"),
        ["expected semicolon"]
    );
    assert_eq!(
        errors("@bogus thing { a: 1; }"),
        ["unknown at-rule: `@bogus`"]
    );
    assert_eq!(
        errors("@variant m { @import \"x.fsty\"; }"),
        ["`@import` is allowed only at the top level"]
    );
    assert_eq!(
        errors("q { title { a: 1; } }"),
        ["nested rules are not supported here"]
    );
    assert_eq!(errors("q { a: 1; } }"), ["unexpected closing brace"]);
    assert_eq!(errors("q { a: 1;"), ["unclosed delimiter"]);
    assert_eq!(errors("q"), ["expected declaration block"]);
    assert_eq!(errors("heading[level==1] {}"), ["expected `=`"]);
    assert_eq!(errors("heading[level=1 { a: 1; }"), ["unclosed delimiter"]);
    assert_eq!(errors("#{fig:ball.x} {}"), ["labels cannot contain `.`"]);
    assert_eq!(errors("#{fig:} {}"), ["labels cannot end with `:`"]);
    assert_eq!(errors("#{} {}"), ["expected label"]);
    assert_eq!(errors("#{fig {}"), ["unclosed delimiter"]);
    assert_eq!(
        errors("$c: #;"),
        ["expected a colour or an id selector after `#`"]
    );
}

#[test]
fn errors_do_not_hide_later_rules() {
    let text = "@bogus x { a: 1; }\nq { numbering \"(a)\"; }\nheading ~ p { a: 1; }\nr { a: 1; }";
    let root = root(text);
    assert_eq!(count(&root, SyntaxKind::StyleRule), 3);
    let rule = items(&root)
        .into_iter()
        .filter_map(|item| match item {
            StyleItem::Rule(rule) => Some(rule),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert_eq!(text_of_selectors(rule.selectors()), ["r"]);
}

// --- Scoped rules in content (SPEC §5.4) -------------------------------------

#[test]
fn scoped_rules_use_the_stylesheet_grammar() {
    let text = "#rule question:has(answer) > part[marks>2] { trailer: align-end \"[\" field(marks) \"]\"; }";
    let root = parse(text);
    assert_eq!(root.full_text(), text);
    let (errors, _) = Source::detached(text).root().errors_and_warnings();
    assert!(errors.is_empty(), "{errors:?}");
    let rule: ast::ScopedRule<'_> = first(&root);
    let selector = rule.selectors().iter().next().unwrap();
    assert_eq!(selector.steps().count(), 2);
    assert_eq!(count(&root, SyntaxKind::PseudoClass), 1);
    assert_eq!(count(&root, SyntaxKind::AttrSelector), 1);

    // In content, tokens and colours are written as code.
    let text = "#rule .warn { color: tok.warn; background: rgb(\"1f5fa8\"); }";
    let root = parse(text);
    let (errors, _) = Source::detached(text).root().errors_and_warnings();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(count(&root, SyntaxKind::TokenName), 0);
    assert_eq!(count(&root, SyntaxKind::FieldAccess), 1);

    // An id selector and the excluded combinator.
    let (errors, _) = Source::detached("#rule #{fig:ball} ~ x { a: 1; }")
        .root()
        .errors_and_warnings();
    let messages: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        messages,
        ["the general sibling combinator `~` is not supported"]
    );
}

#[test]
fn content_sigils_are_unchanged() {
    // `$` is math and `#` an embedded expression in content, not a token name
    // or a colour.
    let root = parse("#let c = 1 $x$ #c");
    assert_eq!(count(&root, SyntaxKind::Equation), 1);
    assert_eq!(count(&root, SyntaxKind::TokenName), 0);
    assert_eq!(count(&root, SyntaxKind::Color), 0);
}

// --- Incremental reparsing ---------------------------------------------------

#[test]
fn reparse_of_a_declaration_block_is_local() {
    let text = "@import \"base.fsty\";\n\
                $accent: #1f5fa8;\n\
                question { numbering: \"1.\"; trailer: field(marks); }\n\
                answer { display: answer-space; }\n";
    let mut source = Source::detached_stylesheet(text);
    let at = text.find("\"1.\"").unwrap() + 2;
    let reparsed = source.edit(at..at, ")");
    assert_eq!(
        &source.text()[reparsed],
        "{ numbering: \"1).\"; trailer: field(marks); }"
    );
    assert!(source.root().spanless_eq(&parse_stylesheet(source.text())));
}

#[test]
fn reparse_of_top_level_items_is_local() {
    let text = "a { x: 1; }\nb { x: 2; }\nc { x: 3; }\nd { x: 4; }\ne { x: 5; }\nf { x: 6; }\n";
    let mut source = Source::detached_stylesheet(text);
    // Editing a selector reparses only the items around it.
    let at = text.find("c {").unwrap() + 1;
    let reparsed = source.edit(at..at, ".hard");
    assert!(
        reparsed.len() < source.text().len() / 2,
        "reparsed {reparsed:?}"
    );
    assert!(source.root().spanless_eq(&parse_stylesheet(source.text())));
    assert_eq!(source.root().kind(), SyntaxKind::Stylesheet);

    // Removing a closing brace makes the later rules nested rules of the
    // unclosed block, so everything after the edit is reparsed.
    let at = source.text().find("x: 4; }").unwrap() + 6;
    source.edit(at..at + 1, "");
    assert!(source.root().spanless_eq(&parse_stylesheet(source.text())));
    assert_eq!(count(source.root(), SyntaxKind::StyleRule), 4);
}

#[test]
fn reparse_of_a_scoped_rule_in_a_stylesheet_value() {
    // The content block is lexed as content, so its scoped rule must be
    // reparsed as content too: `#fff` is not a colour there.
    let text = "q { fallback: [#rule a { b: #fff; }]; }";
    let mut source = Source::detached_stylesheet(text);
    assert_eq!(count(source.root(), SyntaxKind::Color), 0);
    let at = text.find("b:").unwrap();
    let reparsed = source.edit(at..at, "b");
    assert_eq!(&source.text()[reparsed], "{ bb: #fff; }");
    assert!(source.root().spanless_eq(&parse_stylesheet(source.text())));
    assert_eq!(count(source.root(), SyntaxKind::Color), 0);
}

#[test]
fn reparse_of_a_scoped_rule_block_in_content() {
    let text = "Some text.\n\n#rule question { numbering: \"(a)\"; }\n\nMore text.";
    let mut source = Source::detached(text);
    let at = text.find("(a)").unwrap() + 1;
    let reparsed = source.edit(at..at + 1, "i");
    assert_eq!(&source.text()[reparsed], "{ numbering: \"(i)\"; }");
    assert!(source.root().spanless_eq(&parse(source.text())));
}
