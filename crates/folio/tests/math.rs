//! Native math (SPEC §8.1): equations evaluate to formulas of the math tree,
//! and errors in them point at their source. The trees are snapshots in
//! the compact form of `folio_math::Formula::repr`.

use folio::eval::{Content, Node, Value};
use folio::{Target, World, diag};

const MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
";

fn world(main: &str) -> World {
    let mut world = World::builder().build(MANIFEST, None).unwrap();
    world.set_source("main.fol", main).unwrap();
    world
}

/// The formulas of the equations in `main`, in order, with `display` for a
/// display equation.
fn formulas(main: &str) -> Vec<String> {
    fn walk(content: &Content, out: &mut Vec<String>) {
        for node in content.nodes() {
            let Node::Element(element) = node else {
                continue;
            };
            if element.name.to_string() == "math" {
                let Some(Value::Math(formula)) = element.field("tree") else {
                    panic!("an equation without a tree: {}", content.repr());
                };
                let block = element.field("block") == Some(&Value::Bool(true));
                out.push(if block {
                    format!("display {}", formula.repr())
                } else {
                    formula.repr()
                });
            }
            for (_, value) in element.fields.iter() {
                if let Value::Content(inner) = value {
                    walk(inner, out);
                }
            }
        }
    }
    let world = world(main);
    let content = folio::evaluate(&world, None).unwrap().unwrap();
    let errors: Vec<String> = errors_in(&world)
        .into_iter()
        .map(|(message, _)| message)
        .collect();
    assert!(errors.is_empty(), "{main}: {errors:?}");
    let mut out = Vec::new();
    walk(&content, &mut out);
    out
}

/// The formula of the one equation in `main`.
fn tree(main: &str) -> String {
    let mut formulas = formulas(main);
    assert_eq!(formulas.len(), 1, "{main}");
    formulas.remove(0)
}

/// The errors of a build, each with the source text its span points at.
fn errors_in(world: &World) -> Vec<(String, String)> {
    let compiled = folio::compile(world, Target::Pdf, None).unwrap();
    let main = world.source("main.fol").unwrap();
    compiled
        .diagnostics
        .iter()
        .filter(|d| d.severity == folio::Severity::Error)
        .map(|d| {
            let at = diag::locate(world, d.span)
                .map(|location| main.text()[location.bytes].to_string())
                .unwrap_or_default();
            (d.message.to_string(), at)
        })
        .collect()
}

fn errors(main: &str) -> Vec<(String, String)> {
    errors_in(&world(main))
}

// ---- The examples of SPEC §8.1 ----

#[test]
fn spec_examples() {
    assert_eq!(
        tree("$v^2 = u^2 + 2 a s$"),
        "[attach([v], sup: [num(2)]), rel(=), attach([u], sup: [num(2)]), vary(+), num(2), a, s]"
    );
    assert_eq!(
        tree("$F = (m v - m u) / t$"),
        "[F, rel(=), frac([m, v, vary(−), m, u], [t])]"
    );
    assert_eq!(
        tree("$sum_(i=1)^n x_i$"),
        "[nary(∑, sub: [i, rel(=), num(1)], sup: [n]), attach([x], sub: [i])]"
    );
    assert_eq!(tree("$vec(F)$"), "[vec([F])]");
    assert_eq!(tree("$hat(i)$"), "[unit-vec([i])]");
    assert_eq!(
        tree("$integral_0^T P(t) dif t$"),
        "[nary(∫, sub: [num(0)], sup: [T]), P, delim('(', [t], ')'), dif, t]"
    );
}

#[test]
fn inline_and_display() {
    // Spaces inside the dollars make display math; they are not part of it.
    assert_eq!(tree("$ 2 a s $"), "display [num(2), a, s]");
    assert_eq!(tree("$2 a s$"), "[num(2), a, s]");
    // One space is no display equation, and holds no lines.
    assert_eq!(tree("$ $"), "");
}

#[test]
fn lines_alignment_points_and_labels() {
    assert_eq!(
        tree(
            "$ v^2 = u^2 + 2 a s <eq:suvat> \\\n  \
             s &= u t + 1/2 a t^2 <eq:s> \\\n  \
             v &= u + a t $"
        ),
        "display [attach([v], sup: [num(2)]), rel(=), attach([u], sup: [num(2)]), vary(+), \
         num(2), a, s] <eq:suvat> \\ [s, &, rel(=), u, t, vary(+), frac([num(1)], [num(2)]), a, \
         attach([t], sup: [num(2)])] <eq:s> \\ [v, &, rel(=), u, vary(+), a, t]"
    );
    // A trailing line break makes no empty line; an inline formula may be
    // labelled too.
    assert_eq!(tree("$a \\ b \\ $"), "[a] \\ [b]");
    assert_eq!(tree("$x <eq:x>$"), "[x] <eq:x>");
    // The closing `$` may stand on a line of its own.
    assert_eq!(
        tree("$\n  v = u + a t <eq:v>\n$"),
        "display [v, rel(=), u, vary(+), a, t] <eq:v>"
    );
}

#[test]
fn line_labels_are_labels() {
    // A line's label is checked with every other label of the document.
    let main = "$ x <eq:a> \\ y <eq:b> $ and #figure[F] <eq:a>";
    assert_eq!(
        errors(main),
        [(
            "label `<eq:a>` is used twice".to_string(),
            "figure[F]".to_string()
        )]
    );
    assert_eq!(
        errors("$ x <eq:a> \\ y <eq:a> $"),
        [(
            "label `<eq:a>` is used twice".to_string(),
            "<eq:a>".to_string()
        )]
    );
}

// ---- The node set ----

#[test]
fn scripts_and_operators() {
    assert_eq!(
        tree("$f'(x) = lim_(h -> 0) (f(x + h) - f(x)) / h$"),
        "[attach([f], primes: 1), delim('(', [x], ')'), rel(=), \
         attach([op(lim, limits)], sub: [h, rel(→), num(0)]), \
         frac([f, delim('(', [x, vary(+), h], ')'), vary(−), f, delim('(', [x], ')')], [h])]"
    );
    assert_eq!(
        tree("$sin theta + cos^2 theta$"),
        "[op(sin), θ, vary(+), attach([op(cos)], sup: [num(2)]), θ]"
    );
    assert_eq!(
        tree("$x_1^2 a'' n!$"),
        "[attach([x], sub: [num(1)], sup: [num(2)]), attach([a], primes: 2), n, sym(!)]"
    );
    assert_eq!(
        tree("$sum_i x_i + integral x dif x$"),
        "[nary(∑, sub: [i]), attach([x], sub: [i]), vary(+), nary(∫), x, dif, x]"
    );
}

#[test]
fn roots_delimiters_matrices_and_cases() {
    assert_eq!(
        tree("$sqrt(x) root(3, x) √x ∛y$"),
        "[root([x]), root(index: [num(3)], [x]), root([x]), root(index: [num(3)], [y])]"
    );
    assert_eq!(
        tree("$[0, 1) [|x|] abs(x) norm(v) floor(x)$"),
        "[delim('[', [num(0), punct(,), num(1)], ')'), delim('⟦', [x], '⟧'), \
         delim('|', [x], '|'), delim('‖', [v], '‖'), delim('⌊', [x], '⌋')]"
    );
    assert_eq!(
        tree("$mat(1, 2; 3, 4) mat(delim: \"[\", a, b) mat(delim: #none, a; b)$"),
        "[mat('(', [[num(1)], [num(2)]], [[num(3)], [num(4)]], ')'), \
         mat('[', [[a], [b]], ']'), mat(none, [[a]], [[b]], none)]"
    );
    assert_eq!(
        tree("$cases(x &\"if\" x > 0, -x &\"otherwise\")$"),
        "[cases([x, &, text(\"if\"), x, rel(>), num(0)], \
         [vary(−), x, &, text(\"otherwise\")])]"
    );
}

#[test]
fn accents_decorations_styles_and_symbols() {
    assert_eq!(
        tree("$tilde(x) dot.double(x) accent(x, ->) overbrace(a + b, n) underline(y)$"),
        "[accent(\u{303}, [x]), accent(\u{308}, [x]), accent(\u{20D7}, [x]), \
         overbrace([a, vary(+), b], annotation: [n]), underline([y])]"
    );
    assert_eq!(
        tree("$bold(F) upright(d) cal(L) bb(R)$"),
        "[bold([F]), upright([d]), cal([L]), bb([R])]"
    );
    assert_eq!(
        tree("$alpha Omega arrow.r.long arrow.long.r <= != dots.h oo$"),
        "[α, Ω, rel(⟶), rel(⟶), rel(≤), rel(≠), sym(…), sym(∞)]"
    );
    // `hbar` is LaTeX's name for `planck.reduce`; `dotless` letters (#353).
    assert_eq!(
        tree("$hbar planck.reduce dotless.i dotless.j$"),
        "[ℏ, ℏ, ı, ȷ]"
    );
    // `dot` is the accent when called and the operator otherwise.
    assert_eq!(tree("$a dot b$"), "[a, bin(⋅), b]");
}

#[test]
fn text_numbers_and_embedded_values() {
    assert_eq!(
        tree("$\"kg\" #(1 + 2) #(-3.5) #\"text\" #none$"),
        "[text(\"kg\"), num(3), vary(−), num(3.5), text(\"text\")]"
    );
    // Floats that Rust writes with an exponent are a power of ten.
    assert_eq!(
        tree("$#(6.02e23) #(-1e-7) #float(\"inf\")$"),
        "[num(6.02), bin(×), attach([num(10)], sup: [num(23)]), vary(−), num(1), bin(×), \
         attach([num(10)], sup: [vary(−), num(7)]), sym(∞)]"
    );
    // A float literal embedded with `#` keeps its spelling (#421).
    assert_eq!(
        tree("$#3.00e8 #(-1.60e-19) #(-2.50) #300000000.0$"),
        "[num(3.00), bin(×), attach([num(10)], sup: [num(8)]), vary(−), num(1.60), bin(×), \
         attach([num(10)], sup: [vary(−), num(19)]), vary(−), num(2.50), num(300000000.0)]"
    );
    // Under a sign in parentheses too; a literal out of range says so.
    assert_eq!(tree("$#(-(2.50))$"), "[vary(−), num(2.50)]");
    assert_eq!(
        errors("$#1e99999$"),
        [(
            "a power of ten can be at most 9999 in either direction".to_string(),
            "1e99999".to_string()
        )]
    );
    // A computed or bound float is written in the shorter of its plain and
    // scientific forms (D-324-2), without the `.0` of a whole float.
    assert_eq!(
        tree(
            "#let c = 3.00e8\n$c = #c, #(2.0 * 1.5e8), #(0.5 * 3000.0), #(1.0 / 20.0), #(1.0 / 1000.0)$"
        ),
        "[c, rel(=), num(3), bin(×), attach([num(10)], sup: [num(8)]), punct(,), num(3), bin(×), \
         attach([num(10)], sup: [num(8)]), punct(,), num(1500), punct(,), num(0.05), punct(,), num(1), \
         bin(×), attach([num(10)], sup: [vary(−), num(3)])]"
    );
    // Content that is only text is one text, with its spaces.
    assert_eq!(
        tree("#let t = [speed of light]\n$#t = c$"),
        "[text(\"speed of light\"), rel(=), c]"
    );
    // A formula shown in markup is an inline equation again.
    assert_eq!(
        formulas("#let e = $x^2$\n#e.tree"),
        ["[attach([x], sup: [num(2)])]"]
    );
    // A formula is placed as its row, from a binding or embedded as code.
    assert_eq!(
        formulas("#let k = $1/2 m v^2$\n$E_k = #k$ $x = #$a + b$$"),
        [
            "[attach([E], sub: [k]), rel(=), frac([num(1)], [num(2)]), m, \
             attach([v], sup: [num(2)])]",
            "[x, rel(=), a, vary(+), b]",
        ]
    );
}

#[test]
fn names_read_the_scripts_bindings() {
    // A longer name is a binding before it is a name of math; a function
    // of the script gets its arguments as formulas.
    assert_eq!(
        formulas(
            "#let mass = $m$\n#let sq(x) = $#x^2$\n#let pi = 3\n\
             $mass v + sq(y) + pi$"
        ),
        ["[m, v, vary(+), attach([y], sup: [num(2)]), vary(+), num(3)]"]
    );
    // A closure captures what its equations name.
    assert_eq!(
        formulas("#let mass = $M$\n#let f() = $mass g$\n#f()"),
        ["[M, g]"]
    );
}

// ---- Errors ----

/// An error and the source its span points at.
fn error(message: &str, at: &str) -> (String, String) {
    (message.to_string(), at.to_string())
}

#[test]
fn unknown_names_and_symbols() {
    let world = world("$2 as$");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let error_diag = compiled
        .diagnostics
        .iter()
        .find(|d| d.severity == folio::Severity::Error)
        .unwrap();
    assert_eq!(error_diag.message, "unknown name `as` in an equation");
    assert_eq!(
        error_diag
            .hints
            .iter()
            .map(|h| h.as_str())
            .collect::<Vec<_>>(),
        [
            "a product of variables is written with spaces: `a s`",
            "text in an equation is written in quotes: `\"as\"`",
        ]
    );
    assert_eq!(
        errors("$arrow.q$ $alpha.beta$ $frac$"),
        [
            error("unknown symbol `arrow.q`", "arrow.q"),
            error("unknown symbol `alpha.beta`", "alpha.beta"),
            error("`frac` is a function", "frac"),
        ]
    );
    assert_eq!(
        errors("$dot.double$"),
        [error("`dot.double` is a function", "dot.double")]
    );
}

#[test]
fn misplaced_breaks_alignment_points_and_labels() {
    assert_eq!(
        errors("$(a & b)$ $frac(a \\ b, c)$ #let a = $x & y$\n$sqrt(#a)$ $#a$"),
        [
            error(
                "an alignment point can only be at the top level of a line or a case",
                "&"
            ),
            error(
                "a line break can only be at the top level of an equation",
                "\\"
            ),
            // A formula with an alignment point can be placed only where
            // one can stand.
            error(
                "an alignment point can only be at the top level of a line or a case",
                "a"
            ),
        ]
    );
}

#[test]
fn wrong_arguments() {
    assert_eq!(
        errors(
            "$frac(a)$ $vec(1, 2)$ $mat(1, 2; 3)$ $mat(delim: \"x\", a)$ \
             $sqrt(x, y: 1)$ $frac(a; b)$ $accent(x, y)$"
        ),
        [
            error("`frac` takes a numerator and a denominator", "frac(a)"),
            error("`vec` takes the vector's symbol", "vec(1, 2)"),
            error("this row has 1 cell, but the first has 2 cells", "3"),
            error(
                "expected an opening delimiter or `none`, found \"x\"",
                "delim: \"x\""
            ),
            error("`sqrt` has no argument `y`", "y: 1"),
            error(
                "a semicolon separates the rows of `mat(..)`, not the arguments of `frac`",
                "a"
            ),
            error("this is not an accent", "y"),
        ]
    );
}

#[test]
fn content_that_is_not_math() {
    assert_eq!(
        errors("$x = #strong[a]$ $#(1, 2)$"),
        [
            error("this content cannot be placed in an equation", "strong[a]"),
            error("an array cannot be placed in an equation", "(1, 2)"),
        ]
    );
}

#[test]
fn deep_formulas_stop_at_the_nesting_limit() {
    let main = "#let x = $a$\n#for i in range(200) { x = $(#x)/2$ }\n$#x$";
    let errors = errors(main);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].0.contains("nested more than 100 deep"),
        "{errors:?}"
    );
}
