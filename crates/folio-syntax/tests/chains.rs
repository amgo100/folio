//! Long operator chains (#380): `x/x/…`, `x^x^…` and `x_x_…` stop at the
//! parser's depth limit with one error, and the tree stays shallow enough for
//! the recursive walks over it (numbering, dropping) on a small stack.

use folio_syntax::{Source, SyntaxNode};

/// The stack of the compile threads, the watcher and the language server.
const STACK: usize = 2 * 1024 * 1024;

/// How tall a tree may get: the chain limit (8 × 256) and a few levels per
/// level of the parser's depth limit (256) around it.
const MAX_HEIGHT: usize = 8 * 256 + 6 * 256;

/// The error of what is parsed past the depth limit: a right-associative
/// chain such as `x^x^…`, which nests the parser's calls.
const DEEP: &str = "maximum parsing depth exceeded";

/// The error of a chain that makes the tree too tall: a left-associative or
/// postfix chain such as `x/x/…`.
const LONG: &str = "operator chain is too long";

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
    assert!(h <= MAX_HEIGHT, "the tree is {h} levels deep");
    errors(&source)
}

#[test]
fn long_chains_give_one_error() {
    small_stack(|| {
        for (op, error) in [
            ("/", LONG),
            ("^", DEEP),
            ("_", DEEP),
            (" / ", LONG),
            ("!/", LONG),
        ] {
            let errors = check(chain(op, 100_000));
            assert_eq!(errors, [error], "{op:?} chain of 100,000 terms");
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
        assert!(texts[0].starts_with("x^x"), "{:?}", &texts[0][..20]);
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
            assert_eq!(check(text), [LONG; 2], "{op:?} chains of 100,000");
        }
        // A right-associative chain recurses to the depth limit, where the
        // rest of it becomes the one error.
        let text = format!("#{{x{}}}\n", "+=x".repeat(100_000));
        assert_eq!(check(text), [DEEP]);
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

#[test]
fn nested_chains_stay_shallow() {
    small_stack(|| {
        // Each level puts the chain so far in brackets as the first operand
        // of a new chain: the heights of the levels must not add up.
        for (open, close, op, eq) in [("(", ")", "/x", true), ("(", ")", "+x", false)] {
            let mut s = String::from("x");
            for _ in 0..200 {
                s = format!("{open}{s}{close}{}", op.repeat(300));
            }
            let text = if eq {
                format!("${s}$\n")
            } else {
                format!("#({s})\n")
            };
            let errors = check(text);
            assert!(
                !errors.is_empty() && errors.len() <= 200,
                "{op:?}: {}",
                errors.len()
            );
        }
    });
}

#[test]
fn a_chain_does_not_count_against_its_last_operand() {
    // The last operand of a chain is shallow in the tree, so nesting in it
    // has the depth limit's full room, as it does in an incremental reparse
    // of a block there.
    let nested = |n: usize| format!("{}a{}", "#[".repeat(n), "]".repeat(n));
    for (before, after) in [("$x", "$"), ("#(x", ")")] {
        let op = if before == "$x" { "/x" } else { "+x" };
        let text = format!("{before}{}/#[{} b]{after}\n", op.repeat(212), nested(20))
            .replace("/#[", if before == "$x" { "/#[" } else { "+[" });
        assert_eq!(check(text), Vec::<String>::new());
    }
}

#[test]
fn embedded_code_in_a_bracketed_operand_stays_in_its_group() {
    small_stack(|| {
        let text = format!("$({}(a #f b) + c) / d$\n", "x^".repeat(300));
        let source = Source::detached(text);
        assert_eq!(errors(&source), ["maximum parsing depth exceeded"]);
    });
}

#[test]
fn reparsing_in_the_last_operand_matches_a_full_parse() {
    for (before, op, block, after) in [("$x", "/x", "/#[", "$"), ("#(x", "+x", "+[", ")")] {
        for n in [212, 250, 254] {
            let text = format!("{before}{}{block}a b]{after}\n", op.repeat(n));
            let mut source = Source::detached(text.clone());
            let at = text.find("a b").unwrap();
            let with = format!("{}a{}", "#[".repeat(20), "]".repeat(20));
            source.edit(at..at + 1, &with);
            let fresh = Source::detached(source.text().to_string());
            assert_eq!(errors(&source), errors(&fresh), "{op:?} × {n}");
            assert!(source.root().spanless_eq(fresh.root()), "{op:?} × {n}");
        }
    }
}

#[test]
fn a_chain_on_a_deeply_nested_operand_parses() {
    // Nesting within the depth limit makes operands up to about 766 levels
    // tall, or more with short chains at each level; a short chain on one is
    // no error.
    let mut nested = String::from("x");
    for _ in 0..250 {
        nested = format!("a({nested})^2/x");
    }
    for text in [
        format!("${nested}/y$\n"),
        format!("${}x{}/y/z$\n", "(".repeat(130), ")".repeat(130)),
        format!("#f({}x{}).a.b + 1\n", "f(".repeat(250), ")".repeat(250)),
    ] {
        assert_eq!(check(text), Vec::<String>::new());
    }
}

#[test]
fn code_in_a_bracketed_operand_is_parsed_as_code() {
    small_stack(|| {
        for inner in ["#[a $y$ b]", "#[it's \"so]", "#f b"] {
            let text = format!("A ${}({inner}) + c$ and *strong* text.\n", "x^".repeat(300));
            let source = Source::detached(text);
            assert_eq!(
                errors(&source),
                ["maximum parsing depth exceeded"],
                "{inner:?}"
            );
            let root = source.root();
            assert_eq!(
                root.children()
                    .filter(|n| n.kind() == folio_syntax::SyntaxKind::Equation)
                    .count(),
                1,
                "{inner:?}"
            );
        }
    });
}

#[test]
fn the_error_leaves_looser_operators_to_the_enclosing_expression() {
    small_stack(|| {
        // The `^` chain stops at the depth limit; `/y` belongs to the fraction
        // around it, which takes it without an error.
        let text = format!("${}x/y$\n", "x^".repeat(300));
        let source = Source::detached(text);
        assert_eq!(errors(&source), ["maximum parsing depth exceeded"]);
        let mut stack = vec![source.root()];
        let mut error = None;
        while let Some(node) = stack.pop() {
            if node.kind() == folio_syntax::SyntaxKind::Error {
                error = Some(node.leaf_text().to_string());
            }
            stack.extend(node.children());
        }
        assert!(error.unwrap().ends_with("^x"));
    });
}

#[test]
fn an_operand_at_the_limit_is_what_a_regular_parse_takes() {
    small_stack(|| {
        // `\`, `&` and embedded code are operands of `^` in a regular parse,
        // so at the limit they belong to the chain's error.
        for (operand, error) in [
            ("\\ y", "maximum parsing depth exceeded"),
            ("& y", "maximum parsing depth exceeded"),
            ("#f(x) y", "maximum parsing depth exceeded"),
        ] {
            for n in [254, 255, 256, 300] {
                let text = format!("${}{operand}$\n", "x^".repeat(n));
                let errors = check(text);
                assert!(errors.len() <= 1, "{operand:?} × {n}: {errors:?}");
                if n >= 256 || !errors.is_empty() {
                    assert_eq!(errors, [error], "{operand:?} × {n}");
                }
            }
        }
        // A label is no operand: where the chain reaches the limit right
        // before one, it is an expected expression, as in a regular parse.
        let text = format!("${}<a>$\n", "x^".repeat(255));
        assert_eq!(check(text), ["expected expression"]);
    });
}

#[test]
fn code_in_errors_within_errors_is_parsed_as_code() {
    small_stack(|| {
        let chain = "y^".repeat(300);
        let inner = format!("#[ ${chain}(#[ $ a $ ]) $ ]");
        let text = format!("A ${}({inner}) + c$ and *strong* text.\n", "x^".repeat(300));
        let source = Source::detached(text);
        assert_eq!(errors(&source), ["maximum parsing depth exceeded"]);
        let equations = source
            .root()
            .children()
            .filter(|n| n.kind() == folio_syntax::SyntaxKind::Equation)
            .count();
        assert_eq!(equations, 1);
    });
}

#[test]
fn code_at_the_depth_limit_parses_as_before() {
    small_stack(|| {
        let nested =
            |n: usize, inner: &str| format!("#({}{inner}{})\n", "(".repeat(n), ")".repeat(n));
        for n in 240..260 {
            let plain = check(nested(n, "x"));
            // A call needs no deeper parse for empty arguments: it parses
            // wherever a name does.
            if plain.is_empty() {
                assert_eq!(check(nested(n, "g()")), plain, "g() at {n}");
                assert_eq!(check(nested(n, "g().a")), plain, "g().a at {n}");
                // `not` without `in` keeps its own error.
                let errors = check(nested(n, "x not y"));
                assert!(
                    errors.iter().any(|e| e.contains("keyword `in`"))
                        && !errors.iter().any(|e| e.contains("depth")),
                    "x not y at {n}: {errors:?}"
                );
            }
        }
    });
}

#[test]
fn a_cut_chain_keeps_the_shape_of_its_parent() {
    small_stack(|| {
        // The error of a postfix chain in a denominator stands where the
        // denominator stands: the fraction has its three children.
        for text in [
            format!("$a / x{}$\n", "!".repeat(3000)),
            format!("#(a + f{})\n", "()".repeat(3000)),
        ] {
            let source = Source::detached(text.clone());
            assert_eq!(errors(&source), [LONG], "{text:.12}");
            let mut stack = vec![source.root()];
            while let Some(node) = stack.pop() {
                if matches!(
                    node.kind(),
                    folio_syntax::SyntaxKind::MathFrac | folio_syntax::SyntaxKind::Binary
                ) {
                    let kids = node.children().filter(|c| !c.kind().is_trivia()).count();
                    assert_eq!(kids, 3, "{:?} in {text:.12}", node.kind());
                }
                stack.extend(node.children());
            }
        }
    });
}

#[test]
fn a_root_operand_takes_its_radicand() {
    small_stack(|| {
        let text = format!("${}√y^z + w$\n", "x^".repeat(300));
        let source = Source::detached(text);
        assert_eq!(errors(&source), ["maximum parsing depth exceeded"]);
        let mut stack = vec![source.root()];
        while let Some(node) = stack.pop() {
            if node.kind() == folio_syntax::SyntaxKind::Error {
                assert!(node.leaf_text().ends_with("^√y^z"));
            }
            stack.extend(node.children());
        }
    });
}

#[test]
fn reparsing_in_an_early_operand_matches_a_full_parse() {
    // An edit in the first or a middle operand of a chain near the height
    // limit, making it taller: the operand lies deep in the tree, so the
    // reparser parses in full, and the trees agree.
    for (before, op, after) in [("#([b]", " + x", ")"), ("$#[b]", "/x", "$")] {
        for n in [2000, 2040, 2046, 2047] {
            for at in [0, n / 2, n - 70] {
                let mut text = String::from(before);
                for i in 0..n {
                    text.push_str(op);
                    if i == at && at > 0 {
                        text.push_str(if before.starts_with('$') {
                            "/#[c]"
                        } else {
                            " + [c]"
                        });
                    }
                }
                text.push_str(after);
                text.push('\n');
                let mut source = Source::detached(text.clone());
                let target = if at == 0 { "b]" } else { "c]" };
                let pos = text.find(target).unwrap();
                let with = format!("{}a{}", "#[".repeat(20), "]".repeat(20));
                source.edit(pos..pos + 1, &with);
                let fresh = Source::detached(source.text().to_string());
                assert_eq!(
                    errors(&source),
                    errors(&fresh),
                    "{op:?} × {n}, edit at {at}"
                );
                assert!(
                    source.root().spanless_eq(fresh.root()),
                    "{op:?} × {n}, edit at {at}"
                );
            }
        }
    }
}

#[test]
fn a_chain_cut_by_its_height_leaves_the_enclosing_scripts() {
    small_stack(|| {
        // The subscript's postfix chain is cut; `^c` still attaches to `a`.
        let text = format!("$a_x{}^c$\n", "!".repeat(2100));
        let source = Source::detached(text);
        assert_eq!(errors(&source), [LONG]);
        let mut stack = vec![source.root()];
        while let Some(node) = stack.pop() {
            if node.kind() == folio_syntax::SyntaxKind::Error {
                assert!(node.leaf_text().ends_with('!'));
            }
            stack.extend(node.children());
        }
    });
}

#[test]
fn a_chain_of_signed_scripts_gives_one_error() {
    small_stack(|| {
        let text = format!("${}x$\n", "x^-".repeat(5000));
        assert_eq!(check(text), [DEEP]);
    });
}
