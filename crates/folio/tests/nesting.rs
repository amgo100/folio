//! The nesting limit (SPEC §4.4): a script cannot make a value nested more
//! deeply than the stages after evaluation can walk. A value deeper than
//! [`MAX_NESTING`] is an error at the expression that makes it, and content
//! at the limit goes through every stage to a PDF on a thread with a small
//! stack, as the threads that compile builds in parallel have.

use folio::eval::MAX_NESTING;
use folio::{Severity, Target, World};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
styles = ["main.fsty"]
"#;

/// A stylesheet whose descendant selectors match at every depth.
const STYLE: &str = "\
strong strong { font-weight: 400; }
emph strong { font-style: normal; }
list-item list-item { space-before: 2pt; }
";

/// Runs `f` on a thread with a 2 MiB stack, the size of the threads that
/// compile builds in parallel.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

/// Compiles `main` to a PDF and returns its error messages, and the text
/// of the PDF's content if it was written.
fn compile(main: &str) -> (Vec<String>, Option<String>) {
    let main = main.to_owned();
    on_small_stack(move || {
        let mut world = World::new(MANIFEST).unwrap();
        world.set_source("main.fol", &main).unwrap();
        world.set_source("main.fsty", STYLE).unwrap();
        let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
        let errors = compiled
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message.to_string())
            .collect();
        let text = compiled.output.is_some().then(|| {
            folio::evaluate(&world, None)
                .unwrap()
                .expect("the document evaluates")
                .plain_text()
                .trim()
                .to_string()
        });
        (errors, text)
    })
}

/// The error messages of compiling `main`, which must have one.
fn errors(main: &str) -> Vec<String> {
    let (errors, _) = compile(main);
    assert!(!errors.is_empty(), "no error for {main}");
    errors
}

#[test]
fn deeply_nested_content_is_an_error_not_a_crash() {
    // The example of the issue: the stages after evaluation used to
    // overflow the stack and abort the process.
    let found = errors("#let c = [x]\n#for i in range(10000) { c = strong(c) }\n#c\n");
    assert_eq!(
        found,
        [format!("content is nested more than {MAX_NESTING} deep")]
    );

    // Markup that nests too deeply is caught where it is made: here, in
    // the markup of a function that wraps its argument.
    let found = errors(&format!(
        "#let w(c) = [*#c*]\n#let c = [x]\n#for i in range({}) {{ c = w(c) }}\n#c\n",
        MAX_NESTING + 1
    ));
    assert_eq!(
        found,
        [format!("content is nested more than {MAX_NESTING} deep")]
    );
}

#[test]
fn other_deeply_nested_values_are_errors() {
    let cases = [
        // Arrays and dictionaries made by literals.
        (
            "#let a = ()\n#for i in range(100000) { a = (a,) }\n",
            "an array is",
        ),
        (
            "#let a = (:)\n#for i in range(100000) { a = (x: a) }\n",
            "a dictionary is",
        ),
        // Values nested by changing a place in them.
        (
            "#let a = ()\n#for i in range(100000) { a.push(a) }\n",
            "would make",
        ),
        (
            "#let a = (x: 1)\n#for i in range(100000) { a.x = (x: a.x) }\n",
            "would make",
        ),
        (
            "#let a = (0,)\n#for i in range(100000) { a.at(0) = (a.at(0),) }\n",
            "would make",
        ),
        (
            "#let a = (x: (y: 1))\n#for i in range(100000) { a.x.y = a }\n",
            "would make",
        ),
        // Closures that capture closures, and functions with arguments.
        (
            "#let f = () => 1\n#for i in range(100000) { let g = f; f = () => g }\n",
            "a function is",
        ),
        (
            "#let f = strong\n#for i in range(100000) { f = f.with(f) }\n",
            "a function is",
        ),
        (
            "#let f(..a) = a\n#let a = f()\n#for i in range(100000) { a = f(a) }\n",
            "arguments are",
        ),
    ];
    for (main, expected) in cases {
        let found = errors(main);
        assert!(
            found.len() == 1
                && found[0].contains(expected)
                && found[0].contains(&format!("nested more than {MAX_NESTING} deep")),
            "{main}: {found:?}"
        );
    }

    // An element is one more than its deepest field: an array that fits in
    // a dictionary that fits makes an element that does not.
    let main = format!(
        "#let a = ()\n#for i in range({}) {{ a = (a,) }}\n#let d = (x: a)\n#strong(override: d)[x]\n",
        MAX_NESTING - 2
    );
    assert_eq!(
        errors(&main),
        [format!("content is nested more than {MAX_NESTING} deep")]
    );
}

#[test]
fn a_change_that_is_too_deep_changes_nothing() {
    // Each `push` nests the array one more. The one that would nest it more
    // than the limit fails and leaves it as it was: after `k` pushes it is
    // `k + 1` deep, and pushing it into itself again would make it `k + 2`.
    let main = format!(
        "#let a = ()\n#for i in range({}) {{ a.push(a) }}\n#a.len()\n",
        MAX_NESTING + 10
    );
    let (errors, text) = compile(&main);
    assert_eq!(errors.len(), 1, "{errors:?}");
    let text = text.expect("the build is partial");
    assert_eq!(text, format!("[error] {}", MAX_NESTING - 1));
}

#[test]
fn content_at_the_limit_goes_through_every_stage() {
    // The deepest content allowed, as inline elements, as blocks, and as
    // elements with labels and references, compiles to a PDF on a small
    // stack.
    let n = MAX_NESTING;
    let mains = [
        format!("#let c = [x]\n#for i in range({n}) {{ c = strong(c) }}\n#c\n"),
        format!(
            "#let c = [x]\n#for i in range({n}) {{ c = if calc.rem(i, 2) == 0 {{ strong(c) }} else {{ emph(c) }} }}\n#c\n"
        ),
        format!("#let c = [x]\n#for i in range({n}) {{ c = [- #c] }}\n#c\n"),
        format!(
            "#let c = [x]\n#for i in range({}) {{ c = [- #c] }}\n#figure(c) <deep>\n\nSee @deep.\n",
            n - 1
        ),
    ];
    for main in mains {
        let (errors, text) = compile(&main);
        assert!(errors.is_empty(), "{main}: {errors:?}");
        assert!(text.is_some(), "{main}");
    }

    // One more level is an error.
    let main = format!(
        "#let c = [x]\n#for i in range({}) {{ c = strong(c) }}\n#c\n",
        n + 1
    );
    assert_eq!(
        errors(&main),
        [format!("content is nested more than {n} deep")]
    );
}

#[test]
fn arrays_at_the_limit_are_hashed_compared_and_dropped() {
    // The deepest array allowed is a key of a memoised call, the result of
    // one, and a binding of the module.
    let main = format!(
        "#let a = ()\n#for i in range({}) {{ a = (a,) }}\n#let f(x) = x\n#let b = f(a)\n#(a == b)\n",
        MAX_NESTING - 1
    );
    let (errors, text) = compile(&main);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(text.as_deref(), Some("true"));
}
