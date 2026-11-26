//! LaTeX input (SPEC §8.1): `$:tex …$` reads the amsmath core into the
//! same math tree as native syntax. The corpus pairs formulas as they are
//! pasted from LaTeX documents with their native equivalents, and checks
//! that both give the same tree; errors point at the command they are
//! about, inside the formula.

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
/// display equation, after checking that the build has no errors.
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
    let errors = errors_in(&world);
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

/// An error and the source its span points at.
fn error(message: &str, at: &str) -> (String, String) {
    (message.to_string(), at.to_string())
}

/// Asserts that a LaTeX formula and a native one give the same tree, and
/// returns it.
#[track_caller]
fn same(tex: &str, native: &str) -> String {
    let from_tex = tree(&format!("$:tex {tex}$"));
    let from_native = tree(&format!("${native}$"));
    assert_eq!(
        from_tex, from_native,
        "\n  tex:    {tex}\n  native: {native}"
    );
    from_tex
}

// ---- The corpus ----

/// Formulas as they are pasted from physics and mathematics teaching
/// material, with their native equivalents.
const CORPUS: &[(&str, &str)] = &[
    // Mechanics.
    (r"\frac{1}{2} m v^2", "1/2 m v^2"),
    (r"v^2 = u^2 + 2as", "v^2 = u^2 + 2 a s"),
    (r"s = ut + \tfrac{1}{2}at^2", "s = u t + 1/2 a t^2"),
    (r"F = \frac{mv - mu}{t}", "F = (m v - m u) / t"),
    (r"E_k = \dfrac12 m v^{2}", "E_k = 1/2 m v^2"),
    (r"p = mv", "p = m v"),
    (r"\vec{F} = m\vec{a}", "vec(F) = m vec(a)"),
    (r"\omega = \frac{2\pi}{T}", "omega = (2 pi) / T"),
    (r"T = 2\pi\sqrt{\frac{l}{g}}", "T = 2 pi sqrt(l / g)"),
    (r"F = G\frac{m_1 m_2}{r^2}", "F = G (m_1 m_2) / r^2"),
    (r"\Delta E = mc^2", "Delta E = m c^2"),
    (r"\lambda = \frac{h}{p}", "lambda = h / p"),
    (r"E = h f", "E = h f"),
    (r"\hbar\omega", "planck.reduce omega"),
    (
        r"n_1 \sin\theta_1 = n_2 \sin\theta_2",
        "n_1 sin theta_1 = n_2 sin theta_2",
    ),
    (
        r"\sin^2\theta + \cos^2\theta = 1",
        "sin^2 theta + cos^2 theta = 1",
    ),
    (
        r"x(t) = A\cos(\omega t + \phi)",
        "x(t) = A cos(omega t + phi.alt)",
    ),
    (
        r"\varepsilon_0 \mu_0 = \frac{1}{c^2}",
        "epsilon_0 mu_0 = 1 / c^2",
    ),
    (r"pV = nRT", "p V = n R T"),
    (r"R_{\text{total}} = R_1 + R_2", "R_\"total\" = R_1 + R_2"),
    (
        r"V = IR \quad\text{and}\quad P = IV",
        "V = I R quad \"and\" quad P = I V",
    ),
    (r"a \approx 9.81", "a approx 9.81"),
    (r"F \propto \frac{1}{r^2}", "F prop 1 / r^2"),
    (r"x \geq 0, y \le 1, z \neq 2", "x >= 0, y <= 1, z != 2"),
    (r"a \cdot b \times c", "a dot b times c"),
    (r"\pm \mp", "plus.minus minus.plus"),
    (r"A \to B \Rightarrow C \iff D", "A -> B => C <==> D"),
    // Calculus.
    (r"\int_0^T P(t)\,\mathrm{d}t", "integral_0^T P(t) dif t"),
    (r"\int_a^b f(x)\,dx", "integral_a^b f(x) thin d x"),
    (r"\int x \dd x", "integral x dif x"),
    (r"\sum_{i=1}^{n} x_i", "sum_(i=1)^n x_i"),
    (r"\sum^n_{i=1} x_i", "sum_(i=1)^n x_i"),
    (r"\prod_{k=1}^{n} k", "product_(k=1)^n k"),
    (
        r"\lim_{h \to 0} \frac{f(x+h) - f(x)}{h}",
        "lim_(h -> 0) (f(x + h) - f(x)) / h",
    ),
    (r"f'(x) = 2x", "f'(x) = 2 x"),
    (r"f''(x)", "f''(x)"),
    (
        r"\frac{\partial f}{\partial x}",
        "(partial f) / (partial x)",
    ),
    (
        r"\nabla \cdot \vec{E} = \frac{\rho}{\varepsilon_0}",
        "nabla dot vec(E) = rho / epsilon_0",
    ),
    (
        r"\oint \vec{B} \cdot \mathrm{d}\vec{l} = 0",
        "integral.cont vec(B) dot dif vec(l) = 0",
    ),
    (r"\iint_S", "integral.double_S"),
    (r"e^{i\pi} + 1 = 0", "e^(i pi) + 1 = 0"),
    (r"\ln x \log_{10} y \exp(z)", "ln x log_10 y exp(z)"),
    (r"\max_x f \min_{y} g", "max_x f min_y g"),
    (r"\infty", "infinity"),
    // Roots, delimiters and accents.
    (r"\sqrt{x} \sqrt[3]{y}", "sqrt(x) root(3, y)"),
    (r"\sqrt{b^2 - 4ac}", "sqrt(b^2 - 4 a c)"),
    (
        r"x = \frac{-b \pm \sqrt{b^2-4ac}}{2a}",
        "x = (-b plus.minus sqrt(b^2 - 4 a c)) / (2 a)",
    ),
    (r"\left( \frac{a}{b} \right)^2", "(a / b)^2"),
    (r"\left[ x \right]", "[x]"),
    (r"\left\{ x \right\}", "{x}"),
    (r"\{a, b\}", "{a, b}"),
    (r"[0, 1)", "[0, 1)"),
    (r"\langle x \rangle", "⟨x⟩"),
    (r"\left\langle x \right\rangle", "⟨x⟩"),
    (r"\lfloor x \rfloor", "floor(x)"),
    (r"|x| \lvert y \rvert", "|x| bar.v y bar.v"),
    (r"\|v\|", "bar.v.double v bar.v.double"),
    (r"\bigl( a \bigr)", "(a)"),
    (
        r"\hat{x} \bar{y} \dot{z} \ddot{w} \tilde{v}",
        "accent(x, \"^\") macron(y) dot(z) dot.double(w) tilde(v)",
    ),
    (r"\overline{AB}", "overline(A B)"),
    (r"\underbrace{a+b}_{n}", "underbrace(a + b, n)"),
    (r"\overbrace{a+b}^{n}", "overbrace(a + b, n)"),
    (r"\overrightarrow{AB}", "arrow(A B)"),
    // Alphabets and text.
    (
        r"\mathbf{F} \mathrm{kg} \mathcal{L} \mathbb{R}",
        "bold(F) upright(k g) cal(L) bb(R)",
    ),
    (r"\boldsymbol{\alpha}", "bold(alpha)"),
    (r"x \in \mathbb{R}", "x in bb(R)"),
    (r"\text{if } x > 0", "\"if \" x > 0"),
    (r"\textbf{max}", "bold(\"max\")"),
    // Symbols.
    (
        r"\alpha \beta \gamma \Gamma \Omega",
        "alpha beta gamma Gamma Omega",
    ),
    (
        r"\epsilon \varepsilon \phi \varphi",
        "epsilon.alt epsilon phi.alt phi",
    ),
    (r"\forall x \exists y", "forall x exists y"),
    (
        r"A \subseteq B \cup C \cap D",
        "A subset.eq B union C sect D",
    ),
    (r"a \ll b \gg c", "a << b >> c"),
    (
        r"\ldots \cdots \vdots \ddots",
        "dots.h dots.c dots.v dots.down",
    ),
    (r"a \parallel b \perp c", "a parallel b perp c"),
    (r"\therefore x", "therefore x"),
    (r"\not= \not\in", "eq.not in.not"),
    (r"n!", "n!"),
    (r"a - b * c", "a - b * c"),
    (r"x\,y\;z\quad w", "x thin y thick z quad w"),
    (r"\displaystyle\sum\limits_i x", "sum_i x"),
    (r"a \bmod n", "a mod n"),
    (r"\mathrm{H_2O}", "upright(H_2 O)"),
    (r"\rightleftharpoons", "harpoons.rtlb"),
];

#[test]
fn corpus_gives_the_same_trees_as_native_math() {
    let mut differ = Vec::new();
    for &(tex, native) in CORPUS {
        let from_tex = tree(&format!("$:tex {tex}$"));
        let from_native = tree(&format!("${native}$"));
        if from_tex != from_native {
            differ.push(format!(
                "\n  tex:    {tex}\n  native: {native}\n  {from_tex}\n  {from_native}"
            ));
        }
    }
    assert!(differ.is_empty(), "{}", differ.concat());
}

#[test]
fn matrices_and_cases() {
    assert_eq!(
        same(
            r"\begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}",
            "mat(1, 2; 3, 4)"
        ),
        "[mat('(', [[num(1)], [num(2)]], [[num(3)], [num(4)]], ')')]"
    );
    same(
        r"\begin{bmatrix} a & b \end{bmatrix}",
        "mat(delim: \"[\", a, b)",
    );
    same(
        r"\begin{matrix} a \\ b \\ \end{matrix}",
        "mat(delim: #none, a; b)",
    );
    same(
        r"\begin{vmatrix} a & b \\ c & d \end{vmatrix}",
        "mat(delim: \"|\", a, b; c, d)",
    );
    same(
        r"\begin{Vmatrix} a \end{Vmatrix} \begin{Bmatrix} b \end{Bmatrix}",
        "mat(delim: \"‖\", a) mat(delim: \"{\", b)",
    );
    same(
        r"\begin{array}{cc} 1 & 0 \\ 0 & 1 \end{array}",
        "mat(delim: #none, 1, 0; 0, 1)",
    );
    // Short rows are filled with empty cells, as TeX sets them.
    assert_eq!(
        tree(r"$:tex \begin{pmatrix} 1 & 2 \\ 3 \end{pmatrix}$"),
        "[mat('(', [[num(1)], [num(2)]], [[num(3)], []], ')')]"
    );
    assert_eq!(
        same(
            r"|x| = \begin{cases} x & \text{if } x \geq 0 \\ -x & \text{otherwise} \end{cases}",
            "|x| = cases(x &\"if \" x >= 0, -x &\"otherwise\")"
        ),
        "[fence(|), x, fence(|), rel(=), cases([x, &, text(\"if \"), x, rel(≥), num(0)], \
         [vary(−), x, &, text(\"otherwise\")])]"
    );
}

#[test]
fn lines_alignment_points_and_labels() {
    let native = "$ v^2 &= u^2 + 2 a s <eq:suvat> \\\n s &= u t + 1/2 a t^2 <eq:s> $";
    let tex = "$:tex \\begin{align}\n  v^2 &= u^2 + 2as \\label{eq:suvat} \\\\\n  \
               s &= ut + \\frac{1}{2} a t^2 \\label{eq:s}\n\\end{align} $";
    assert_eq!(tree(tex), tree(native));
    // A label may stand anywhere in its line, as in LaTeX.
    assert_eq!(tree("$:tex \\label{eq:x} x = 1$"), tree("$x = 1 <eq:x>$"));
    // The alignment environments hold the lines; so does the top level.
    for tex in [
        r"a &= b \\ c &= d",
        r"\begin{aligned} a &= b \\ c &= d \end{aligned}",
        r"\begin{align*} a &= b \\ c &= d \\ \end{align*}",
        r"\begin{equation}\begin{split} a &= b \\ c &= d \end{split}\end{equation}",
        r"\begin{alignat}{2} a &= b \\[4pt] c &= d \end{alignat}",
    ] {
        assert_eq!(
            tree(&format!("$:tex {tex}$")),
            "[a, &, rel(=), b] \\ [c, &, rel(=), d]",
            "{tex}"
        );
    }
    assert_eq!(
        tree("$:tex \\begin{gather} a \\\\ b \\end{gather} $"),
        "display [a] \\ [b]"
    );
    // Line labels are labels of the document.
    assert_eq!(
        errors("$:tex x \\label{eq:a} \\\\ y \\label{eq:a}$"),
        // The labels of a LaTeX formula point at its source.
        [error(
            "label `<eq:a>` is used twice",
            " x \\label{eq:a} \\\\ y \\label{eq:a}"
        )]
    );
}

#[test]
fn display_and_escapes() {
    assert_eq!(
        tree("$:tex \\frac{1}{2} m v^2 $"),
        "display [frac([num(1)], [num(2)]), m, attach([v], sup: [num(2)])]"
    );
    assert_eq!(tree("$:tex 5\\% \\$ $"), "display [num(5), sym(%), sym($)]");
    assert_eq!(tree("$:tex$"), "");
    // Comments run to the end of their line.
    assert_eq!(tree("$:tex a % the mass\n + b$"), "[a, vary(+), b]");
}

#[test]
fn tex_follows_tex_where_it_differs_from_native_math() {
    // Scripts take one token, and groups are one base.
    assert_eq!(
        tree(r"$:tex x^10 {a+b}^2$"),
        "[attach([x], sup: [num(1)]), num(0), attach([a, vary(+), b], sup: [num(2)])]"
    );
    // A slash is a slash, and spaces separate nothing.
    assert_eq!(tree(r"$:tex a/b$"), "[a, bin(/), b]");
    assert_eq!(tree(r"$:tex ab$"), "[a, b]");
    // Any name can be an operator, which native math writes as text.
    assert_eq!(
        tree(r"$:tex \operatorname{grad} \phi \operatorname*{argmax}_x$"),
        "[op(grad), ϕ, attach([op(argmax, limits)], sub: [x])]"
    );
    // A LaTeX formula is placed in native math as its row.
    assert_eq!(
        formulas("#let k = $:tex \\frac12 m v^2$\n$E = #k$"),
        ["[E, rel(=), frac([num(1)], [num(2)]), m, attach([v], sup: [num(2)])]"]
    );
}

// ---- Errors ----

#[test]
fn unknown_commands_point_into_the_formula() {
    let world = world("Energy: $:tex E = \\frac{1}{2} m \\vel^2$.");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let errors: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.severity == folio::Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].message, "unknown LaTeX command `\\vel`");
    let location = diag::locate(&world, errors[0].span).unwrap();
    assert_eq!(
        &world.source("main.fol").unwrap().text()[location.bytes],
        "\\vel"
    );
    assert_eq!(errors[0].hints.as_slice(), ["did you mean `\\vec`?"]);
}

#[test]
fn errors_point_at_their_source() {
    assert_eq!(
        errors(
            "$:tex \\frac{1}{2$ $:tex x^2^3$ $:tex \\left( x$ $:tex (a & b)$ \
             $:tex \\begin{foo} x \\end{foo}$ $:tex \\binom{n}{k}$ $:tex \\sqrt$ \
             $:tex \\nonumber$ $:tex \\frac{a \\\\ b}{c}$ $:tex x \\label{a.b}$ \
             $:tex \\begin{pmatrix} a $ $:tex x = \\begin{align} a \\end{align}$ \
             $:tex \\begin{align} a $ $:tex x \\label{a} y \\label{b}$ \
             $:tex \\text{a \\alpha}$ $:tex a \\\\ b \\end{align}$"
        ),
        [
            error("unclosed brace", "{"),
            error("double superscript", "^"),
            error("`\\left` without `\\right`", "\\left"),
            error(
                "an alignment point can only be at the top level of a line or a case",
                "&"
            ),
            error("unknown LaTeX environment `foo`", "\\begin{foo}"),
            error("`\\binom` is not supported in LaTeX math", "\\binom"),
            error("missing radicand", "\\sqrt"),
            error("`\\nonumber` is not supported in LaTeX math", "\\nonumber"),
            error(
                "a line break can only be at the top level of an equation",
                "\\\\"
            ),
            error("`a.b` is not a valid label", "a.b"),
            error("missing `\\end{pmatrix}`", "\\begin{pmatrix}"),
            error("`align` can only hold a whole equation", "\\begin{align}"),
            error("missing `\\end{align}`", "\\begin{align}"),
            error("a line can have only one label", "\\label{b}"),
            error("`\\alpha` cannot stand in text", "\\alpha"),
            error("`\\end` without `\\begin`", "\\end"),
        ]
    );
}

#[test]
fn unknown_modes_are_syntax_errors() {
    assert_eq!(
        errors("$:asciimath x$"),
        [error("unknown math input mode: `:asciimath`", ":asciimath")]
    );
}

#[test]
fn deep_formulas_stop_at_the_nesting_limit() {
    let main = format!("$:tex {}x{}$", "{".repeat(150), "}".repeat(150));
    let errors = errors(&main);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].0, "math is nested more than 100 deep");
}
