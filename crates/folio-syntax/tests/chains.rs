//! Long operator chains (#380): `x/x/…`, `x^x^…` and `x_x_…` stop at the
//! parser's depth limit with one error, and the tree stays shallow enough for
//! the recursive walks over it (numbering, dropping) on a small stack.

use folio_syntax::{Source, SyntaxNode};

/// The stack of the compile threads, the watcher and the language server.
const STACK: usize = 2 * 1024 * 1024;

/// Runs `f` on a thread with a 2 MiB stack.
fn small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// The height of the tree, walked without recursion.
fn height(root: &SyntaxNode) -> usize {
    let mut max = 0;
    let mut stack = vec![(root, 1)];
    while let Some((node, depth)) = stack.pop() {
        max = max.max(depth);
        stack.extend(node.children().map(|c| (c, depth + 1)));
    }
    max
}

/// `$x<op>x<op>…$` with `terms` terms in a paragraph.
fn chain(op: &str, terms: usize) -> String {
    format!("A ${}$ B\n", vec!["x"; terms].join(op))
}

/// The messages of the syntax errors of `source`.
fn errors(source: &Source) -> Vec<String> {
    let (errors, _) = source.root().errors_and_warnings();
    errors.into_iter().map(|e| e.message.to_string()).collect()
}

/// Parses and numbers `text`, checks the round trip, that the tree is
/// shallow, and returns the error messages.
fn check(text: String) -> Vec<String> {
    let source = Source::detached(text.clone());
    assert_eq!(source.root().full_text(), text);
    let h = height(source.root());
    assert!(h < 600, "the tree is {h} levels deep");
    errors(&source)
}

#[test]
fn long_chains_give_one_error() {
    small_stack(|| {
        for op in ["/", "^", "_", " / ", "!/"] {
            let errors = check(chain(op, 100_000));
            assert_eq!(
                errors,
                ["maximum parsing depth exceeded"],
                "{op:?} chain of 100,000 terms"
            );
        }
    });
}

#[test]
fn chains_within_the_limit_parse() {
    for op in ["/", "^", "_"] {
        assert_eq!(check(chain(op, 200)), Vec::<String>::new(), "{op:?}");
    }
}

#[test]
fn the_error_ends_with_the_chain() {
    small_stack(|| {
        // What follows the chain parses as usual: the sum after a space, the
        // next line, the embedded code and the label.
        let text = format!(
            "${}^f(x)^(y)'_[z]! + y \\ z^2 & #f(1) <eq:a>$\n",
            vec!["x"; 5000].join("^")
        );
        let source = Source::detached(text.clone());
        assert_eq!(errors(&source), ["maximum parsing depth exceeded"]);
        let mut stack = vec![source.root()];
        let mut texts = Vec::new();
        while let Some(node) = stack.pop() {
            if node.kind() == folio_syntax::SyntaxKind::Error {
                texts.push(node.leaf_text().to_string());
            }
            stack.extend(node.children());
        }
        assert_eq!(texts.len(), 1);
        assert!(texts[0].starts_with("^x^x"), "{:?}", &texts[0][..20]);
        assert!(
            texts[0].ends_with("^x^f(x)^(y)'_[z]!"),
            "{:?}",
            &texts[0][texts[0].len() - 20..]
        );
    });
}

#[test]
fn long_code_chains_give_one_error() {
    small_stack(|| {
        // Binary operators, calls and field accesses; the statements after
        // the chain parse as usual.
        for (op, operand) in [
            ("+", "x"),
            (" and ", "x"),
            ("()", ""),
            (".f()", ""),
            (".a", ""),
        ] {
            let text = format!(
                "#let a = (x{})\n#b\n#{{\n  x{}\n  y + 1\n}}\n",
                format!("{op}{operand}").repeat(100_000),
                format!("{op}{operand}").repeat(100_000),
            );
            assert_eq!(
                check(text),
                ["maximum parsing depth exceeded"; 2],
                "{op:?} chains of 100,000"
            );
        }
        // A right-associative chain recurses to the limit first: one error
        // there and one for the rest.
        let text = format!("#{{x{}}}\n", "+=x".repeat(100_000));
        assert_eq!(check(text), ["maximum parsing depth exceeded"; 2]);
    });
}

#[test]
fn code_chains_within_the_limit_parse() {
    for op in ["+", "()", ".a"] {
        let text = format!(
            "#(x{})\n",
            format!("{op}x").repeat(200).replace("()x", "()")
        );
        assert_eq!(check(text), Vec::<String>::new(), "{op:?}");
    }
}
