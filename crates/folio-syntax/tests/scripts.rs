//! A sign after `^` or `_` binds to the operand directly after it (SPEC §8.1,
//! D-323-1, #323): `10^-3` raises `-3`, as `10^(-3)` does.

use folio_syntax::ast::{AstNode, Expr, MathAttach};
use folio_syntax::{SyntaxKind, SyntaxNode, parse};

/// The first attachment of the equation in `text`.
fn attach(root: &SyntaxNode) -> MathAttach<'_> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if let Some(attach) = node.cast::<MathAttach<'_>>() {
            return attach;
        }
        stack.extend(node.children().rev());
    }
    panic!("no attachment");
}

/// The source text of a script, without the trivia around it.
fn text(expr: Option<Expr<'_>>) -> String {
    expr.map(|e| e.to_untyped().full_text().trim().to_string())
        .unwrap_or_default()
}

/// The top and bottom scripts of the first attachment in `$math$`.
#[track_caller]
fn scripts(math: &str) -> (String, String) {
    let source = format!("${math}$\n");
    let root = parse(&source);
    assert_eq!(root.full_text(), source);
    let (errors, _) = root.errors_and_warnings();
    assert!(errors.is_empty(), "{math}: {errors:?}");
    let attach = attach(&root);
    (text(attach.top()), text(attach.bottom()))
}

#[test]
fn a_sign_binds_to_the_operand_after_it() {
    assert_eq!(scripts("10^-3").0, "-3");
    assert_eq!(scripts("3.0 times 10^-3").0, "-3");
    assert_eq!(scripts("e^-x").0, "-x");
    assert_eq!(scripts("x^-1 y").0, "-1");
    assert_eq!(scripts("x^+1").0, "+1");
    assert_eq!(scripts("x^−1").0, "−1");
    assert_eq!(scripts("x_-1").1, "-1");
    assert_eq!(scripts("x^-alpha").0, "-alpha");
    assert_eq!(scripts("x^-sqrt(2)").0, "-sqrt(2)");
    assert_eq!(scripts("x^-(a+b)").0, "-(a+b)");
    assert_eq!(scripts("x^-#n").0, "-#n");
    assert_eq!(scripts("x^-∞").0, "-∞");
    // In a chain of scripts, each sign binds to its own operand.
    assert_eq!(scripts("x_1^-2"), ("-2".into(), "1".into()));
    assert_eq!(scripts("x^-2_-1"), ("-2".into(), "-1".into()));
}

#[test]
fn a_signed_script_is_a_sequence_of_the_sign_and_its_operand() {
    let root = parse("$10^-3$");
    let top = attach(&root).top().unwrap().to_untyped().clone();
    assert_eq!(top.kind(), SyntaxKind::Math);
    let kinds: Vec<_> = top.children().map(SyntaxNode::kind).collect();
    assert_eq!(kinds, [SyntaxKind::MathShorthand, SyntaxKind::MathText]);
    // The sign takes one operand, as an unsigned script does: `x^-a b`
    // raises `-a` only, and `x^-a^b` is `x^(-(a^b))`.
    assert_eq!(scripts("x^-a b").0, "-a");
    assert_eq!(scripts("x^-a^b").0, "-a^b");
}

#[test]
fn a_sign_alone_is_the_script() {
    // Nothing, a space or a closing bracket after the sign: the sign alone
    // is the script, as for an electron, a positron or an ion.
    assert_eq!(scripts("e^-").0, "-");
    assert_eq!(scripts("e^- + p").0, "-");
    assert_eq!(scripts("Cl^- + Na^+").0, "-");
    assert_eq!(scripts("e^+ e^-").0, "+");
    assert_eq!(scripts("(x^-)").0, "-");
    assert_eq!(scripts("x^- 1").0, "-");
    // Before an operator, a relation or punctuation, too.
    assert_eq!(scripts("x^-= y").0, "-");
    assert_eq!(scripts("x^-, y").0, "-");
    // Other symbols than signs keep taking one atom.
    assert_eq!(scripts("x^*1").0, "*");
    assert_eq!(scripts("x^±1").0, "±");
}

#[test]
fn bracketed_scripts_are_unchanged() {
    // The parentheses stay in the tree, as tokens the evaluator skips.
    assert_eq!(scripts("10^(-3)").0, "(-3)");
    assert_eq!(scripts("x^(a+b)").0, "(a+b)");
}
