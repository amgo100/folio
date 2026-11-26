//! Unit tests for the constructs of the content markup (SPEC §4.2), the label
//! grammar, the sigils and Folio's own constructs.

use folio_syntax::ast::{self, AstNode, Expr};
use folio_syntax::{SyntaxKind, SyntaxNode, is_valid_label_literal_id, parse};

/// Parses `text`, checks the round trip and returns the root.
#[track_caller]
fn root(text: &str) -> SyntaxNode {
    let root = parse(text);
    assert_eq!(root.full_text(), text);
    root
}

/// The messages of all syntax errors in `text`.
fn errors(text: &str) -> Vec<String> {
    let source = folio_syntax::Source::detached(text);
    let (errors, _) = source.root().errors_and_warnings();
    errors
        .into_iter()
        .map(|error| error.message.to_string())
        .collect()
}

/// Asserts that `text` parses without syntax errors.
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

/// The kinds of the top-level children.
fn top(text: &str) -> Vec<SyntaxKind> {
    root(text).children().map(SyntaxNode::kind).collect()
}

// --- Paragraphs and headings ------------------------------------------------

#[test]
fn paragraphs_are_separated_by_blank_lines() {
    use SyntaxKind::*;
    assert_eq!(top("one\ntwo"), [Text, Space, Text]);
    assert_eq!(top("one\n\ntwo"), [Text, Parbreak, Text]);
    assert_eq!(top("one\n \n\n two"), [Text, Parbreak, Text]);
}

#[test]
fn headings() {
    for (text, depth) in [("= One", 1), ("== Two", 2), ("=== Three", 3)] {
        let root = root(text);
        let heading: ast::Heading<'_> = first(&root);
        assert_eq!(heading.depth().get(), depth);
        assert_eq!(
            heading.body().to_untyped().full_text().trim(),
            text.trim_start_matches('=').trim()
        );
    }
    // A heading marker needs a space and must start a line.
    assert_eq!(count(&root("=no"), SyntaxKind::Heading), 0);
    assert_eq!(count(&root("a = b"), SyntaxKind::Heading), 0);
    // A label after a heading ends the heading.
    let root = root("= Kinematics <sec:kin>");
    assert_eq!(
        top("= Kinematics <sec:kin>"),
        [SyntaxKind::Heading, SyntaxKind::Space, SyntaxKind::Label]
    );
    let label: ast::Label<'_> = first(&root);
    assert_eq!(label.get(), "sec:kin");
}

// --- Emphasis and strong ------------------------------------------------------

#[test]
fn emphasis_and_strong() {
    let root = root("_emph_ and *strong*");
    let emph: ast::Emph<'_> = first(&root);
    let strong: ast::Strong<'_> = first(&root);
    assert_eq!(emph.body().to_untyped().full_text(), "emph");
    assert_eq!(strong.body().to_untyped().full_text(), "strong");
    ok("_emph *nested strong* emph_");
    // Within a word, underscores and stars are text.
    assert_eq!(count(&self::root("snake_case_name"), SyntaxKind::Emph), 0);
    assert_eq!(count(&self::root("2*3*4"), SyntaxKind::Strong), 0);
    // An unclosed delimiter is an error that ends at the paragraph.
    assert_eq!(errors("*unclosed\n\nnext"), ["unclosed delimiter"]);
}

// --- Lists ------------------------------------------------------------------------

#[test]
fn lists() {
    let root = root("- bullet\n  - nested\n+ numbered\n3. explicit\n/ Velocity: rate of change");
    assert_eq!(count(&root, SyntaxKind::ListItem), 2);
    assert_eq!(count(&root, SyntaxKind::EnumItem), 2);
    let items: Vec<ast::EnumItem<'_>> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast)
        .collect();
    assert_eq!(items[0].number(), None);
    assert_eq!(items[1].number(), Some(3));
    let term: ast::TermItem<'_> = first(&root);
    assert_eq!(term.term().to_untyped().full_text(), "Velocity");
    assert_eq!(
        term.description().to_untyped().full_text().trim(),
        "rate of change"
    );
    // Markers only count at the start of a line.
    assert_eq!(
        count(&self::root("a - b + c / d: e"), SyntaxKind::ListItem),
        0
    );
}

// --- Math ---------------------------------------------------------------------------

#[test]
fn math_inline_and_display() {
    let root = root("$v^2 = u^2 + 2 a s$ and $ E_k = 1/2 m v^2 $");
    let equations: Vec<ast::Equation<'_>> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast)
        .collect();
    assert_eq!(equations.len(), 2);
    assert!(!equations[0].block());
    assert!(equations[1].block());
    ok("$sum_(i=1)^n x_i$ $integral_0^T P(t) dif t$ $vec(F)$ $qty(9.81, \"m s^-2\")$");
    assert_eq!(errors("$unclosed"), ["unclosed delimiter"]);
}

#[test]
fn math_labels_end_lines() {
    // A label ends a line: before `\\`, the closing `$` or the end.
    let text = "$ v^2 = u^2 + 2 a s <eq:suvat> \\\n  s = u t <eq:s>$";
    ok(text);
    let tree = root(text);
    let labels: Vec<&str> = nodes(&tree)
        .into_iter()
        .filter_map(SyntaxNode::cast::<ast::Label<'_>>)
        .map(|label| label.get())
        .collect();
    assert_eq!(labels, ["eq:suvat", "eq:s"]);
    assert_eq!(tree.full_text(), text);
    // Elsewhere `<` compares or starts a shorthand.
    for text in [
        "$a<b>c$",
        "$a < b > c$",
        "$x <= y$",
        "$a <b> + c$",
        "$<a.b>$",
    ] {
        ok(text);
        assert_eq!(count(&root(text), SyntaxKind::Label), 0, "{text}");
    }
    assert_eq!(count(&root("$x <eq:a>$"), SyntaxKind::Label), 1);
    // A label's name is checked as in markup.
    assert_eq!(errors("$ x <eq:> $"), ["labels cannot end with `:`"]);
}

// --- Element calls ------------------------------------------------------------------

#[test]
fn element_calls() {
    let root = root("#question(marks: 3)[Body] text");
    let call: ast::FuncCall<'_> = first(&root);
    let Expr::Ident(callee) = call.callee() else {
        panic!("callee is not a name")
    };
    assert_eq!(callee.as_str(), "question");
    let args: Vec<_> = call.args().items().collect();
    assert_eq!(args.len(), 2);
    let ast::Arg::Named(named) = args[0] else {
        panic!("expected a field")
    };
    assert_eq!(named.name().as_str(), "marks");
    assert!(named.scope().is_none());
    assert!(matches!(args[1], ast::Arg::Pos(Expr::ContentBlock(_))));

    // Several bodies, nesting, and a call that ends a sentence.
    ok("#figure(placement: wrap, width: 1/3)[#image(\"a.svg\")][Caption.] <fig:ball>");
    ok("Negligible.#sidenote[Take $g = 9.81$.] Next.");
    // A space before the argument list ends the call.
    assert_eq!(count(&self::root("#f (x)"), SyntaxKind::Args), 0);
}

#[test]
fn universal_fields_and_scoped_override() {
    let text = "#question(marks: 3, class: \"hard\", id: \"q1\", steps: \"2-\", \
                override@markscheme: (space-before: 24pt))[]";
    ok(text);
    let root = root(text);
    let named: Vec<ast::Named<'_>> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast)
        .collect();
    let scoped = named
        .iter()
        .find(|named| named.name().as_str() == "override")
        .unwrap();
    assert_eq!(scoped.scope().unwrap().as_str(), "markscheme");
    assert!(matches!(scoped.expr(), Expr::Dict(_)));

    // `override` and `override@markscheme` are different fields, but each
    // may appear once.
    ok("#f(override: (a: 1), override@markscheme: (a: 2))");
    assert_eq!(
        errors("#f(override@x: 1, override@x: 2)"),
        ["duplicate argument: override@x"]
    );
    assert_eq!(
        errors("#f(override@: 1)"),
        ["expected variant or target name"]
    );
    // Trivia before the colon does not hide a duplicate.
    assert_eq!(errors("#f(a /* c */: 1, a: 2)"), ["duplicate argument: a"]);
    assert_eq!(
        errors("#f(a@x /* c */: 1, a@x\n: 2)"),
        ["duplicate argument: a@x"]
    );
}

// --- Labels and references -----------------------------------------------------

#[test]
fn labels_after_nodes_and_bodies() {
    let root = root("#figure[x] <fig:ball>\n= Heading <sec:intro>\n$ x $ <eq:suvat>");
    let labels: Vec<&str> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast::<ast::Label<'_>>)
        .map(|label| label.get())
        .collect();
    assert_eq!(labels, ["fig:ball", "sec:intro", "eq:suvat"]);
}

#[test]
fn valid_labels() {
    for name in [
        "a", "a-b", "a_b", "a:b", "sec:kin", "x1:y2:z3", "_private", "1", "łódź", "q-3:a",
    ] {
        let text = format!("<{name}>");
        assert_eq!(top(&text), [SyntaxKind::Label], "{text}");
        let root = root(&text);
        let label: ast::Label<'_> = first(&root);
        assert_eq!(label.get(), name);
        assert!(is_valid_label_literal_id(name), "{name}");
    }
}

#[test]
fn invalid_labels() {
    assert_eq!(errors("<fig:>"), ["labels cannot end with `:`"]);
    assert_eq!(errors("<fig->"), ["labels cannot end with `-`"]);
    assert_eq!(errors("<a:b:>"), ["labels cannot end with `:`"]);
    assert_eq!(errors("<a.b>"), ["labels cannot contain `.`"]);
    assert_eq!(
        errors("<timer:task.remaining>"),
        ["labels cannot contain `.`"]
    );
    assert_eq!(errors("<a."), ["unclosed label"]);
    assert_eq!(errors("<unclosed label"), ["unclosed label"]);
    for name in ["", "fig:", "fig-", "a.b", "-a", ":a", "a b", "a>"] {
        assert!(!is_valid_label_literal_id(name), "{name:?}");
    }
}

#[test]
fn text_that_is_not_a_label() {
    // A label starts with a letter, a digit or an underscore, so arrows and
    // comparisons in prose stay text.
    for text in ["a <- b", "x <: y", "<>", "a < b"] {
        assert_eq!(count(&root(text), SyntaxKind::Label), 0, "{text}");
        ok(text);
    }
}

#[test]
fn references() {
    let root = root("See @fig:ball.");
    let reference: ast::Ref<'_> = first(&root);
    assert_eq!(reference.target(), "fig:ball");
    assert!(reference.supplement().is_none());

    let root = self::root("@fig:ball[Figure]");
    let reference: ast::Ref<'_> = first(&root);
    assert_eq!(
        reference.supplement().unwrap().to_untyped().full_text(),
        "[Figure]"
    );
}

#[test]
fn trailing_colon_and_hyphen_after_a_reference_are_punctuation() {
    for (text, target, rest) in [
        ("see @fig:ball: it shows", "fig:ball", ": it shows"),
        ("@fig:ball- and", "fig:ball", "- and"),
        ("@a:-:", "a", ":-:"),
        ("@sec:kin.", "sec:kin", "."),
        ("(@eq:suvat)", "eq:suvat", ")"),
        ("@x1:y2", "x1:y2", ""),
    ] {
        let root = root(text);
        let reference: ast::Ref<'_> = first(&root);
        assert_eq!(reference.target(), target, "{text}");
        let at = text.find('@').unwrap();
        let after = &text[at + 1 + target.len()..];
        assert_eq!(after, rest, "{text}");
        ok(text);
    }
}

#[test]
fn at_signs_that_are_not_references() {
    for text in ["a lone @ sign", "@-x", "@:x", "@"] {
        assert_eq!(count(&root(text), SyntaxKind::Ref), 0, "{text}");
        ok(text);
    }
}

#[test]
fn published_value_of_a_labelled_element() {
    let text = "#live(bind: (left: <timer:task>.remaining), left => [#left])";
    ok(text);
    let root = root(text);
    let access: ast::FieldAccess<'_> = first(&root);
    let Expr::Label(label) = access.target() else {
        panic!("target is not a label")
    };
    assert_eq!(label.get(), "timer:task");
    assert_eq!(access.field().as_str(), "remaining");
}

// --- Raw text and comments -------------------------------------------------------

#[test]
fn raw_text_and_fences() {
    let root = root("`inline` and\n```rust\nfn main() {}\n```");
    let raws: Vec<ast::Raw<'_>> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast)
        .collect();
    assert_eq!(raws.len(), 2);
    assert!(!raws[0].block());
    assert!(raws[1].block());
    assert_eq!(raws[1].lang().unwrap().get(), "rust");
    let lines: Vec<_> = raws[1].lines().map(|line| line.get().to_string()).collect();
    assert_eq!(lines, ["fn main() {}"]);
    // The language tag runs up to the first space.
    let root = self::root("```c++ int x;```");
    let raw: ast::Raw<'_> = first(&root);
    assert_eq!(raw.lang().unwrap().get(), "c++");
    assert_eq!(errors("`unclosed"), ["unclosed raw text"]);
}

#[test]
fn comments() {
    use SyntaxKind::*;
    assert_eq!(top("a // line\nb"), [Text, Space, LineComment, Space, Text]);
    assert_eq!(
        top("a /* block /* nested */ */ b"),
        [Text, Space, BlockComment, Space, Text]
    );
    ok("#f(1, // a comment\n 2)");
}

// --- Sigils ---------------------------------------------------------------------------

#[test]
fn sigils() {
    use SyntaxKind::*;
    assert_eq!(top("$x$"), [Equation]);
    assert_eq!(top("#f()"), [Hash, FuncCall]);
    assert_eq!(top("@ref"), [Ref]);
    // Stylesheet tokens are written as fields of `tok` in content, and a `$`
    // always starts math.
    let text = "#styled(color: tok.warn, font-weight: 700)[read carefully]";
    ok(text);
    let root = root(text);
    let access: ast::FieldAccess<'_> = first(&root);
    assert_eq!(access.to_untyped().full_text(), "tok.warn");
    ok("#box(fill: rgb(\"1f5fa8\"))[x]");
    assert_eq!(top("$accent"), [Equation]);
}

#[test]
fn escapes_and_shorthands() {
    use SyntaxKind::*;
    assert_eq!(top("\\#"), [Escape]);
    assert_eq!(top("\\$"), [Escape]);
    assert_eq!(top("\\@"), [Escape]);
    assert_eq!(top("--"), [Shorthand]);
    ok("a line break \\\nand \\u{1F600}");
}

// --- Scripting ------------------------------------------------------------------------

#[test]
fn scripting() {
    ok(
        "#let x = 1\n#let f(a, b: 2) = a + b\n#if x > 0 [yes] else [no]\n\
        #for i in range(3) [#i]\n#while false { break }\n\
        #import \"lib/blocks.fol\": example\n#include \"chapter.fol\"",
    );
    ok("#import \"@school/house:1.2.0\"");
    let root = root("#import \"@school/house:1.2.0\"");
    let import: ast::ModuleImport<'_> = first(&root);
    assert_eq!(import.bare_name().unwrap(), "house");
}

#[test]
fn typst_styling_keywords_are_plain_names() {
    // Styling lives in stylesheets: `set`, `show` and `context` are names.
    for word in ["set", "show", "context"] {
        let text = format!("#let {word} = 1\n#{word}");
        ok(&text);
        let root = root(&text);
        assert_eq!(count(&root, SyntaxKind::Ident), 2, "{word}");
    }
}

#[test]
fn durations_and_other_units() {
    use ast::Unit;
    let text = "#f(5min, 2.4s, 250ms, 12pt, 3mm, 1cm, 1in, 90deg, 1rad, 2em, 1fr, 50%)";
    ok(text);
    let root = root(text);
    let units: Vec<Unit> = nodes(&root)
        .into_iter()
        .filter_map(SyntaxNode::cast::<ast::Numeric<'_>>)
        .map(|numeric| numeric.get().1)
        .collect();
    assert_eq!(
        units,
        [
            Unit::Min,
            Unit::S,
            Unit::Ms,
            Unit::Pt,
            Unit::Mm,
            Unit::Cm,
            Unit::In,
            Unit::Deg,
            Unit::Rad,
            Unit::Em,
            Unit::Fr,
            Unit::Percent
        ]
    );
    assert_eq!(errors("#f(5h)"), ["invalid number suffix: `h`"]);
}

// --- Element declarations (SPEC §4.3) ----------------------------------------------

const EXAMPLE_DECL: &str = r#"#element example(
  title: content = none,
  numbered: bool = true,
  difficulty: choice("core", "extension") = "core",
) {
  kind: block                         // block | inline | float | note | slide
  body: slots(statement: blocks, solution: blocks)
  counter: within heading(level: 1)
  ref-key: "example"                  // word looked up in locale files
}"#;

#[test]
fn element_declaration() {
    ok(EXAMPLE_DECL);
    let root = root(EXAMPLE_DECL);
    let decl: ast::ElementDecl<'_> = first(&root);
    assert_eq!(decl.name().as_str(), "example");

    let fields: Vec<_> = decl.fields().collect();
    let names: Vec<_> = fields.iter().map(|field| field.name().as_str()).collect();
    assert_eq!(names, ["title", "numbered", "difficulty"]);
    assert_eq!(fields[0].ty().to_untyped().full_text(), "content");
    assert!(matches!(fields[0].default(), Some(Expr::None(_))));
    assert!(matches!(fields[2].ty(), Expr::FuncCall(_)));
    assert_eq!(
        fields[2].default().unwrap().to_untyped().full_text(),
        "\"core\""
    );

    let entries: Vec<_> = decl.entries().collect();
    let keys: Vec<_> = entries.iter().map(|entry| entry.key().as_str()).collect();
    assert_eq!(keys, ["kind", "body", "counter", "ref-key"]);
    let counter: Vec<_> = entries[2]
        .values()
        .map(|value| value.to_untyped().full_text())
        .collect();
    assert_eq!(counter, ["within", "heading(level: 1)"]);
}

#[test]
fn element_declaration_variants_and_errors() {
    ok("#element note(text: content)");
    ok("#{ element note(text: content) { kind: inline; } }");
    ok("#element note(a: int = 1 + 2, b: array, c: ref)");
    assert_eq!(errors("#element note"), ["expected field list"]);
    assert_eq!(
        errors("#element note(a: int, a: str)"),
        ["duplicate field: a"]
    );
    assert_eq!(errors("#element note(a) {}"), ["expected colon"]);
    assert_eq!(errors("#element note() { kind: }"), ["expected expression"]);
    // A statement ends the line.
    assert_eq!(
        errors("#element note() {} text"),
        ["expected semicolon or line break"]
    );
}

// --- Scoped rules (SPEC §5.4) -------------------------------------------------------

#[test]
fn scoped_rule() {
    let text = "#rule question { numbering: \"(a)\"; }";
    ok(text);
    let root = root(text);
    let rule: ast::ScopedRule<'_> = first(&root);
    assert_eq!(rule.scopes().count(), 0);
    assert_eq!(rule.selectors().to_untyped().full_text(), "question");
    let declarations: Vec<_> = rule.declarations().collect();
    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0].property().as_str(), "numbering");
    let values: Vec<_> = declarations[0]
        .values()
        .map(|value| value.to_untyped().full_text())
        .collect();
    assert_eq!(values, ["\"(a)\""]);
}

#[test]
fn scoped_rule_with_scopes_and_compound_selector() {
    let text = "#rule @target pptx @variant slides, handout heading[level=1] > answer:first {\n  \
                margin-column: outer, width 32mm, gap 6mm;\n  font-weight: 700\n}";
    ok(text);
    let root = root(text);
    let rule: ast::ScopedRule<'_> = first(&root);
    let scopes: Vec<_> = rule
        .scopes()
        .map(|scope| {
            let names: Vec<_> = scope
                .names()
                .map(|name| name.as_str().to_string())
                .collect();
            (scope.kind().to_string(), names)
        })
        .collect();
    assert_eq!(
        scopes,
        [
            ("target".to_string(), vec!["pptx".to_string()]),
            (
                "variant".to_string(),
                vec!["slides".to_string(), "handout".to_string()]
            ),
        ]
    );
    assert_eq!(
        rule.selectors().to_untyped().full_text(),
        "heading[level=1] > answer:first"
    );
    let properties: Vec<_> = rule
        .declarations()
        .map(|decl| decl.property().as_str().to_string())
        .collect();
    assert_eq!(properties, ["margin-column", "font-weight"]);
    let values: Vec<_> = rule
        .declarations()
        .next()
        .unwrap()
        .values()
        .map(|value| value.to_untyped().full_text().to_string())
        .collect();
    assert_eq!(values, ["outer", "width", "32mm", "gap", "6mm"]);
}

#[test]
fn scoped_rule_with_other_selectors() {
    for (text, selector) in [
        ("#rule #{fig:ball} { width: 40%; }", "#{fig:ball}"),
        (
            "#rule figure#{fig:ball} > caption { width: 40%; }",
            "figure#{fig:ball} > caption",
        ),
        ("#rule .hard { numbering: \"(a)\"; }", ".hard"),
        ("#rule :first { space-before: 0pt; }", ":first"),
    ] {
        ok(text);
        let root = root(text);
        let rule: ast::ScopedRule<'_> = first(&root);
        assert_eq!(
            rule.selectors().to_untyped().full_text(),
            selector,
            "{text}"
        );
        assert_eq!(rule.declarations().count(), 1, "{text}");
    }
}

#[test]
fn scoped_rule_errors() {
    assert_eq!(errors("#rule question"), ["expected declaration block"]);
    assert_eq!(
        errors("#rule @bogus pptx figure {}"),
        ["unknown rule scope: `@bogus`"]
    );
    let root = root("#rule @bogus pptx figure {}");
    let scope: ast::RuleScope<'_> = first(&root);
    assert_eq!(scope.kind(), "bogus");
    let names: Vec<_> = scope
        .names()
        .map(|name| name.as_str().to_string())
        .collect();
    assert_eq!(names, ["pptx"]);
    // A semicolon ends the selector, so the next statement still parses.
    let root = self::root("#{ rule q; let x = 1; x }");
    assert_eq!(count(&root, SyntaxKind::LetBinding), 1);
    assert_eq!(
        errors("#{ rule q; let x = 1; x }"),
        ["expected declaration block"]
    );
    assert_eq!(
        errors("#rule question { numbering \"(a)\"; }"),
        ["expected colon"]
    );
    // A missing semicolon before a new line is reported, and the next
    // declaration still parses.
    let text = "#rule q {\n  a: 1\n  b: 2;\n}";
    assert_eq!(errors(text), ["expected semicolon"]);
    let root = self::root(text);
    let rule: ast::ScopedRule<'_> = first(&root);
    assert_eq!(rule.declarations().count(), 2);
    // An unbalanced selector stops at the end of the line.
    assert!(!errors("#rule heading[level=1 { a: 1; }\nNext line.").is_empty());
    assert_eq!(
        count(
            &self::root("#rule heading[level=1 { a: 1; }\nNext."),
            SyntaxKind::Text
        ),
        1
    );
}

#[test]
fn rule_and_element_remain_names_elsewhere() {
    use SyntaxKind::*;
    // The `rule(...)` template primitive and other uses as names.
    for text in [
        "#rule(0.4pt)",
        "#let rule = 1",
        "#rule.field",
        "#rule",
        "#element.fields",
        "#element(x)",
        "#{ let element = 1; element + 1 }",
        "#{ rule\n question }",
    ] {
        ok(text);
        let root = root(text);
        assert_eq!(count(&root, ScopedRule), 0, "{text}");
        assert_eq!(count(&root, ElementDecl), 0, "{text}");
        assert_eq!(count(&root, Rule) + count(&root, Element), 0, "{text}");
    }
    // In a code block, a rule is a statement like any other.
    let root = root("#{\n  rule answer { space-before: 12pt; }\n  let rule = 1\n}");
    assert_eq!(count(&root, ScopedRule), 1);
    assert_eq!(count(&root, LetBinding), 1);
}

// --- LaTeX math (SPEC §8.1) -----------------------------------------------------------

#[test]
fn latex_math() {
    let text = r"$:tex \frac{1}{2} m v^2$";
    ok(text);
    assert_eq!(top(text), [SyntaxKind::TexEquation]);
    let root = root(text);
    let equation: ast::TexEquation<'_> = first(&root);
    assert_eq!(equation.mode(), "tex");
    assert_eq!(equation.body(), r"\frac{1}{2} m v^2");
    assert!(!equation.block());

    // Display, escaped dollars, line breaks, a label and use in code.
    let text = "$:tex\n  v &= u + a t \\\\\n  \\$5 &= x\n$ <eq:suvat>";
    ok(text);
    let root = self::root(text);
    let equation: ast::TexEquation<'_> = first(&root);
    assert!(equation.block());
    assert_eq!(equation.body(), "v &= u + a t \\\\\n  \\$5 &= x");
    assert_eq!(count(&root, SyntaxKind::Label), 1);
    ok(r"#let ke = $:tex \frac{1}{2} m v^2$");
    ok(r"$:tex$");
}

#[test]
fn latex_math_does_not_collide_with_native_math() {
    // A colon that does not start a mode name stays native math.
    for text in ["$:= x$", "$: x$", "$a: b$", "$::= y$"] {
        ok(text);
        assert_eq!(top(text), [SyntaxKind::Equation], "{text}");
    }
}

#[test]
fn latex_math_errors() {
    assert_eq!(errors("$:foo x$"), ["unknown math input mode: `:foo`"]);
    assert_eq!(errors(r"$:tex \frac{1}{2}"), ["unclosed delimiter"]);
    let root = root(r"$:tex x \$ y");
    assert_eq!(root.children().count(), 1);
    let equation: ast::TexEquation<'_> = first(&root);
    assert_eq!(equation.mode(), "tex");
    assert_eq!(equation.body(), r"x \$ y");
    let root = self::root("$:foo x$");
    let equation: ast::TexEquation<'_> = first(&root);
    assert_eq!(equation.mode(), "foo");
}
