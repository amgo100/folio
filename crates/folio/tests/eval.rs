//! Evaluation (SPEC §2.1, §4.4): the language, the content tree, memoised
//! calls, the variant as a tracked key, the sandbox and resource limits.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ecow::EcoVec;
use folio::diag;
use folio::eval::{Content, Value};
use folio::{Date, Event, Limits, Name, Registry, Target, World};
use folio_component::{
    Body, Component, ComponentManifest, ComponentName, Declarations, ElementKind, ElementSchema,
    FieldDecl, FieldType,
};

const MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
";

const VARIANTS: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"

[variants.exam]
targets = [\"pdf\"]

[variants.markscheme]
targets = [\"pdf\"]
";

/// A world with the logged names of the queries it executed.
struct Logged {
    world: World,
    log: Arc<Mutex<Vec<String>>>,
}

impl Logged {
    fn new(manifest: &str, files: &[(&str, &str)]) -> Self {
        Self::with(World::builder(), manifest, files)
    }

    fn with(builder: folio::WorldBuilder, manifest: &str, files: &[(&str, &str)]) -> Self {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        let mut world = builder
            .on_event(move |event| {
                if let Event::Execute { query } = event {
                    sink.lock().unwrap().push(query.to_string());
                }
            })
            .build(manifest, None)
            .unwrap();
        for (path, text) in files {
            if path.ends_with(".fol") || path.ends_with(".fsty") {
                world.set_source(path, text).unwrap();
            } else {
                world.set_file(path, text.as_bytes()).unwrap();
            }
        }
        Self { world, log }
    }

    /// How often each query ran since the log was last taken.
    fn take(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for query in std::mem::take(&mut *self.log.lock().unwrap()) {
            *counts.entry(query).or_insert(0) += 1;
        }
        counts
    }

    fn calls(&self) -> usize {
        self.take().get("call_closure").copied().unwrap_or(0)
    }

    /// The plain text of a variant's content; panics with the diagnostics
    /// if there are errors.
    fn text(&self, variant: Option<&str>) -> String {
        plain(&self.content(variant))
    }

    fn content(&self, variant: Option<&str>) -> Content {
        let variant = variant.map(|v| Name::new(v).unwrap());
        let compiled = folio::compile(&self.world, Target::Pdf, variant.as_ref()).unwrap();
        // Warnings that a feature is not laid out or implemented yet do
        // not concern evaluation.
        let diagnostics: Vec<_> = compiled
            .diagnostics
            .iter()
            .filter(|d| {
                d.severity != folio::Severity::Warning
                    || !(d
                        .message
                        .ends_with("is not laid out yet, so the output leaves it out")
                        || d.message
                            .ends_with("are not laid out yet, so the output leaves them out")
                        || d.message
                            .ends_with("is not laid out yet, so the output shows at most its box")
                        || d.message
                            .ends_with("is not implemented yet, so this declaration has no effect"))
            })
            .cloned()
            .collect();
        assert!(
            diagnostics.is_empty(),
            "{}",
            diag::render(&self.world, &diagnostics, false)
        );
        folio::evaluate(&self.world, variant.as_ref())
            .unwrap()
            .expect("the document evaluates")
    }

    /// The rendered diagnostics of the build without variants.
    fn errors(&self) -> String {
        let compiled = folio::compile(&self.world, Target::Pdf, None).unwrap();
        assert!(compiled.has_errors(), "expected errors");
        diag::render(&self.world, &compiled.diagnostics, false)
    }
}

/// The text of content with runs of spaces collapsed and the ends trimmed.
fn plain(content: &Content) -> String {
    content
        .plain_text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn text(main: &str) -> String {
    Logged::new(MANIFEST, &[("main.fol", main)]).text(None)
}

fn repr(main: &str) -> String {
    Logged::new(MANIFEST, &[("main.fol", main)])
        .content(None)
        .repr()
        .to_string()
}

fn errors(main: &str) -> String {
    Logged::new(MANIFEST, &[("main.fol", main)]).errors()
}

/// The first line of each diagnostic: `error: ...`.
fn messages(main: &str) -> Vec<String> {
    errors(main)
        .lines()
        .filter(|l| l.starts_with("error") || l.starts_with("warning"))
        .map(str::to_owned)
        .collect()
}

// ---- The language ----

#[test]
fn expressions() {
    assert_eq!(text("#(1 + 2 * 3) #(7 / 2) #(-(2 - 5))"), "7 3.5 3");
    assert_eq!(
        text("#(\"ab\" + \"c\") #(\"ab\" * 2) #(1 == 1.0) #(2 < 1)"),
        "abc abab true false"
    );
    assert_eq!(
        text("#(true and not false) #(false or true) #(2 in (1, 2))"),
        "true true true"
    );
    assert_eq!(
        text("#(2pt + 1pt) #(1cm / 1cm) #(50% * 2) #(90deg / 2) #(1fr + 1fr)"),
        "3.0pt 1.0 100.0% 45.0deg 2.0fr"
    );
    assert_eq!(
        text("#(1em + 2pt) #(2.4s + 600ms) #(5min / 2)"),
        "2.0pt + 1.0em 3.0s 150.0s"
    );
    assert_eq!(
        text("#calc.pow(2, 10) #calc.sqrt(16) #calc.max(1, 5, 3) #calc.rem(7, 3)"),
        "1024 4.0 5 1"
    );
    assert_eq!(
        text("#type(1) #type(\"a\") #type(none) #repr((1,)) #repr((a: 1))"),
        "int str none (1,) (a: 1)"
    );
    assert_eq!(
        text("#str(12) #int(\"7\") #float(3) #int(2.9)"),
        "12 7 3.0 2"
    );
    assert_eq!(
        text("#rgb(\"1f5fa8\") #rgb(255, 0, 0)"),
        "rgb(\"1f5fa8\") rgb(\"ff0000\")"
    );
}

#[test]
fn bindings_functions_and_control_flow() {
    let main = "\
#let add(a, b: 1) = a + b
#let total(..xs) = xs.pos().sum()
#let sum = { let c = 0; for i in range(5) { c += i }; c }
#add(1) #add(1, b: 2) #total(1, 2, 3) #sum

#let first-even(xs) = { for x in xs { if calc.even(x) { return x } }; none }
#first-even((1, 3, 4, 6))
#{ let n = 0; while true { n += 1; if n == 3 { break } }; n }
#for x in range(5) { if x == 2 { continue }; str(x) }
#let fact(n) = if n <= 1 { 1 } else { n * fact(n - 1) }
#fact(10)
#let twice(f, x) = f(f(x))
#twice(x => x * 3, 2)
#let add5 = add.with(5)
#add5(b: 10)
";
    assert_eq!(text(main), "2 3 6 10 4 3 0134 3628800 18 15");
}

#[test]
fn collections_and_destructuring() {
    let main = "\
#let d = (a: 1, b: 2)
#d.insert(\"c\", 3)
#d.keys().join(\",\") #d.at(\"b\") #d.c #d.at(\"z\", default: 0)
#(1, 2, 3).map(x => x * 2).fold(0, (a, b) => a + b)
#((3, 1, 2).sorted().map(str).join()) #(1, 2, 3).filter(calc.odd).len()
#let (a, .., z) = (1, 2, 3, 4)
#(a + z)
#let (x, y: w) = (x: 1, y: 2)
#(x + w)
#let arr = (1, 2)
#{ arr.at(0) = 5; arr.push(7) }
#arr.at(0) #arr.len() #arr.last()
#{ let (p, q) = (1, 2); (p, q) = (q, p); str(p) + str(q) }
#(\"a,b\".split(\",\").rev().join()) #\"Hello\".slice(1, 3) #\"Hello\".upper()
#for (k, v) in (x: 1, y: 2) [#k=#v ]
#(1, (2, (3,))).flatten().len() #(\"a\", \"b\").enumerate().at(1).at(0)
";
    assert_eq!(
        text(main),
        "a,b,c 2 3 0 12 123 2 5 3 5 3 7 21 ba el HELLO x=1 y=2 3 1"
    );
}

#[test]
fn edge_cases_of_operators_and_places() {
    // Repeating an empty value does not loop, however large the count.
    assert_eq!(
        text(
            "#(\"\" * 1000000000000000000).len() #(() * 9223372036854775807).len()#([] * 1000000000)"
        ),
        "0 0"
    );
    // Functions named like mutating methods, in modules and dictionaries.
    let logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#import \"lib.fol\" as lib\n#lib.insert(1) #let d = (push: x => x * 2)\n#d.push(3)\n",
            ),
            ("lib.fol", "#let insert(x) = x + 1\n"),
        ],
    );
    assert_eq!(logged.text(None), "2 6");
    // A plain assignment adds a dictionary key; a compound one needs it.
    assert_eq!(text("#let d = (:)\n#{ d.x = 1; d.x += 1 }#d.x"), "2");
    assert_eq!(
        messages("#let d = (:)\n#{ d.x += 1 }"),
        ["error: dictionary does not contain key \"x\""]
    );
    // Division rounds down, the remainder keeps the dividend's sign.
    assert_eq!(
        text("#calc.quo(-7, 2) #calc.quo(7, -2) #calc.quo(7, 2) #calc.rem(-7, 2)"),
        "-4 -4 3 -1"
    );
    // A result that is not a number is an error, not zero.
    assert_eq!(
        messages("#calc.pow(-8, 0.5)"),
        ["error: the result is not a number"]
    );
    assert_eq!(
        messages("#(calc.exp(1000) - calc.exp(1000))"),
        ["error: the result is not a number"]
    );
}

#[test]
fn markup_becomes_a_content_tree() {
    assert_eq!(
        repr("= Forces <sec:forces>\n\nA *ball* is _thrown_. See @sec:forces.\n"),
        "[heading(level: 1, body: [\"Forces\"]) <sec:forces>, parbreak, \"A\", space, \
         strong(body: [\"ball\"]), space, \"is\", space, emph(body: [\"thrown\"]), \". See\", \
         space, ref(target: <sec:forces>), \".\", space]"
    );
    assert_eq!(
        repr("- one\n+ two\n/ Term: desc\n"),
        "[list-item(body: [\"one\"]), space, enum-item(body: [\"two\"]), space, \
         terms-item(term: [\"Term\"], body: [\"desc\"]), space]"
    );
    assert_eq!(
        repr("`code` $x^2$\n```rust\nfn main() {}\n```"),
        "[raw(text: \"code\", block: false), space, \
         math(block: false, tree: formula([attach([x], sup: [num(2)])])), \
         space, raw(text: \"fn main() {}\", block: true, lang: \"rust\")]"
    );
    // Values in markup show as text; content blocks and functions join.
    assert_eq!(
        repr("#let greet(name) = [Hello *#name*]\n#greet(\"Ada\")"),
        "[\"Hello\", space, strong(body: [\"Ada\"])]"
    );
    assert_eq!(repr("#none#(1, 2)"), "[\"(1, 2)\"]");
}

#[test]
fn labels_attach_to_the_element_before_them() {
    assert_eq!(
        repr("*A* <first>\n"),
        "[strong(body: [\"A\"]) <first>, space]"
    );
    assert_eq!(
        messages("Some text <lbl>\n"),
        ["error: a label must follow an element"]
    );
    assert_eq!(
        messages("*a* <x> <y>\n"),
        ["error: this element already has a label"]
    );
}

#[test]
fn modules_import_and_include() {
    let logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#import \"lib/util.fol\": inc, base as b\n\
                 #import \"lib/util.fol\"\n\
                 #import \"lib/util.fol\" as u\n\
                 #inc(1) #b #util.base #u.inc(b)\n\
                 #{ import \"lib/util.fol\": *; double(base) }\n\
                 #include \"chapters/one.fol\"\n",
            ),
            (
                "lib/util.fol",
                "#let base = 10\n#let inc(x) = x + 1\n#let double(x) = 2 * x\n",
            ),
            (
                "chapters/one.fol",
                "#import \"/lib/util.fol\": base\nChapter #base.\n",
            ),
        ],
    );
    assert_eq!(logged.text(None), "2 10 10 11 20 Chapter 10.");
}

// ---- Errors ----

#[test]
fn errors_point_at_their_source_and_calls() {
    assert_eq!(messages("#x"), ["error: unknown variable: x"]);
    assert_eq!(
        messages("#(1 + \"a\")"),
        ["error: cannot add an int and a str"]
    );
    assert_eq!(
        messages("#let f(x) = x\n#f()"),
        ["error: missing argument: x"]
    );
    assert_eq!(
        messages("#let f(x) = x\n#f(1, y: 2)"),
        ["error: unexpected argument: y"]
    );
    assert_eq!(
        messages("#let x = 1\n#let f() = { x = 2 }\n#f()"),
        ["error: variables from outside the function are read-only and cannot be changed"]
    );
    assert_eq!(
        messages("#break"),
        ["error: cannot break outside of a loop"]
    );
    assert_eq!(
        messages("#assert(1 > 2, message: \"no\")"),
        ["error: assertion failed: no"]
    );

    // An error in a function points at its body, and at the call.
    let logged = Logged::new(
        MANIFEST,
        &[
            ("main.fol", "#import \"lib.fol\": f\n#f(0)\n"),
            ("lib.fol", "#let f(x) = 1 / x\n"),
        ],
    );
    assert_eq!(
        logged.errors(),
        "\
error: cannot divide by zero
  ┌─ lib.fol:1:13
  │
1 │ #let f(x) = 1 / x
  │             ^^^^^
  │
  ┌─ main.fol:2:2
  │
2 │ #f(0)
  │  ---- in this call of `f`

"
    );
}

#[test]
fn syntax_errors_add_no_errors_of_their_own() {
    // Only the syntax error is reported, not errors that follow from it.
    let messages = messages("#let f(x = \n#f(1)\n");
    assert!(
        messages.iter().all(|m| !m.contains("unknown variable")),
        "{messages:?}"
    );
}

/// The content of the build without variants, which has errors, and the
/// first lines of its diagnostics.
fn partial(files: &[(&str, &str)]) -> (String, Vec<String>) {
    let logged = Logged::new(MANIFEST, files);
    let messages = logged
        .errors()
        .lines()
        .filter(|l| l.starts_with("error") || l.starts_with("warning"))
        .map(str::to_owned)
        .collect();
    let content = folio::evaluate(&logged.world, None)
        .unwrap()
        .expect("the document evaluates around its errors");
    (content.repr().to_string(), messages)
}

#[test]
fn failing_constructs_become_error_markers() {
    // An error in a paragraph: a marker in its place, and the rest of the
    // file still evaluates (SPEC §12.2).
    let (repr, messages) = partial(&[("main.fol", "A #(1 + \"a\") b.\n\nNext.\n")]);
    assert_eq!(
        repr,
        "[\"A\", space, error(message: \"cannot add an int and a str\", body: \
         [\"[error]\"]), space, \"b.\", parbreak, \"Next.\", space]"
    );
    assert_eq!(messages, ["error: cannot add an int and a str"]);

    // A syntax error in code, which runs on to the stray bracket, and the
    // bracket: a marker each, and only the syntax errors are reported.
    let (repr, messages) = partial(&[("main.fol", "A #f(1, b.\n\nNext ] here.\n")]);
    assert_eq!(
        repr,
        "[\"A\", space, error(message: \"this does not parse\", body: [\"[error]\"]), \
         space, error(message: \"this does not parse\", body: [\"[error]\"]), space, \
         \"here.\", space]"
    );
    assert!(messages.iter().all(|m| !m.contains("unknown variable")));

    // A heading keeps its marker inside it.
    let (repr, _) = partial(&[("main.fol", "= Forces #x\n")]);
    assert!(
        repr.starts_with("[heading(level: 1, body: [\"Forces\", space, error("),
        "{repr}"
    );
}

#[test]
fn a_failed_compound_assignment_keeps_the_variable() {
    // The operation fails and is reported; the variable keeps its value,
    // so its later uses see it rather than `none`.
    let (repr, messages) = partial(&[(
        "main.fol",
        "#let x = 1\n#let s = \"ab\"\n#let a = (1,)\n\
         #{ x += \"a\" } #{ s *= -1 } #{ a -= 1 } #{ x /= 0 }\n\
         #x #s #a.len()\n",
    )]);
    assert_eq!(
        messages,
        [
            "error: cannot add an int and a str",
            "error: cannot repeat a negative number of times",
            "error: cannot subtract an array and an int",
            "error: cannot divide by zero",
        ]
    );
    assert!(
        repr.ends_with("\"1\", space, \"ab\", space, \"1\", space]"),
        "{repr}"
    );
    // A sum that grows the place in place still works.
    assert_eq!(
        text("#let s = \"a\"\n#{ s += \"b\" }\n#let c = [x]\n#{ c += \"y\" }\n#s #c"),
        "ab xy"
    );
}

#[test]
fn a_stray_break_or_return_in_a_file_fails_only_itself() {
    // Each is an error at its own span, the construct it came from becomes
    // a marker, and the rest of the file evaluates (SPEC §12.2).
    let main = "A #break B\n\n#if true { return } C\n\n#continue\n\n\
                #{ (return) + 1 } D\n";
    let (repr, messages) = partial(&[("main.fol", main)]);
    assert_eq!(
        messages,
        [
            "error: cannot break outside of a loop",
            "error: cannot return outside of a function",
            "error: cannot continue outside of a loop",
            "error: cannot add a none and an int",
        ]
    );
    assert_eq!(repr.matches("error(").count(), 4, "{repr}");
    for text in ["\"A\"", "\"B\"", "\"C\"", "\"D\""] {
        assert!(repr.contains(text), "{text} in {repr}");
    }
    // The return points at itself, not at the file.
    let errors = errors("#if true { return }\n");
    assert!(errors.contains("^^^^^^"), "{errors}");
    // In a loop or a function they still leave it.
    assert_eq!(
        text("#for i in range(5) [#i #if i == 2 { break }]\n#let f() = { return 7; 8 }\n#f()"),
        "0 1 2 7"
    );
}

#[test]
fn data_files_keep_the_order_of_their_keys() {
    let logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#json(\"d.json\").keys().join(\",\") \
                 #json(\"d.json\").m.keys().join(\",\") \
                 #toml(\"d.toml\").keys().join(\",\") \
                 #toml(\"d.toml\").b.keys().join(\",\")\n",
            ),
            (
                "d.json",
                "{\"z\": 1, \"a\": 2, \"m\": {\"y\": 1, \"b\": 2}}",
            ),
            ("d.toml", "z = 1\na = 2\n[b]\ny = 1\nc = 2\n[a2]\nq = 1\n"),
            ("d.yaml", "z: 1\na: 2\nm:\n  y: 1\n  b: 2\n"),
        ],
    );
    assert_eq!(logged.text(None), "z,a,m y,b z,a,b,a2 y,c");
    let mut logged = logged;
    logged
        .world
        .set_source(
            "main.fol",
            "#yaml(\"d.yaml\").keys().join(\",\") #yaml(\"d.yaml\").m.keys().join(\",\")\n",
        )
        .unwrap();
    assert_eq!(logged.text(None), "z,a,m y,b");
}

#[test]
fn a_failed_binding_fails_its_uses_quietly() {
    // The `let` fails and is reported; its uses become markers without
    // errors of their own, and the other bindings still work.
    let (repr, messages) = partial(&[(
        "main.fol",
        "#let x = 1 + \"a\"\n#let y = 2\n#x #y #(x + 1) #{ x = 3 }\n",
    )]);
    assert_eq!(messages, ["error: cannot add an int and a str"]);
    assert_eq!(repr.matches("error(").count(), 4, "{repr}");
    assert!(repr.contains("\"2\""), "{repr}");

    // A function that uses it fails where it is made and where it is
    // called, quietly; a failed binding shadows a global of its name.
    let (repr, messages) = partial(&[(
        "main.fol",
        "#let x = 1 / 0\n#let f() = x\n#f() #let calc = 1 / 0\n#calc.max(1, 2)\n",
    )]);
    assert_eq!(
        messages,
        [
            "error: cannot divide by zero",
            "error: cannot divide by zero"
        ]
    );
    assert_eq!(repr.matches("error(").count(), 5, "{repr}");
    assert!(!repr.contains("\"2\""), "{repr}");

    // A label after a construct that failed is dropped, not given to the
    // marker.
    let (repr, messages) = partial(&[("main.fol", "#f(1) <a>\n")]);
    assert_eq!(messages, ["error: unknown variable: f"]);
    assert!(!repr.contains("<a>"), "{repr}");

    // Across files: the importer neither reports the missing binding nor
    // loses the ones that work.
    let (repr, messages) = partial(&[
        (
            "main.fol",
            "#import \"lib.fol\": a, b\n#import \"lib.fol\"\n#a #b #lib.a\n",
        ),
        ("lib.fol", "#let a = 1 / 0\n#let b = 2\n"),
    ]);
    assert_eq!(messages, ["error: cannot divide by zero"]);
    assert!(repr.contains("\"2\""), "{repr}");
    assert_eq!(repr.matches("error(").count(), 2, "{repr}");
}

#[test]
fn errors_in_functions_fail_the_markup_that_calls_them() {
    // Markup inside code does not recover: the error reaches the call in
    // the file's markup, with its trace, and the call becomes the marker.
    let logged = Logged::new(
        MANIFEST,
        &[
            ("main.fol", "#import \"lib.fol\": f\nBefore #f(0) after.\n"),
            ("lib.fol", "#let f(x) = [Result: #(1 / x)]\n"),
        ],
    );
    let compiled = folio::compile(&logged.world, Target::Pdf, None).unwrap();
    assert_eq!(compiled.diagnostics.len(), 1);
    let error = &compiled.diagnostics[0];
    assert_eq!(error.message, "cannot divide by zero");
    assert_eq!(error.related.len(), 1);
    assert!(compiled.output.is_some());
    let content = folio::evaluate(&logged.world, None).unwrap().unwrap();
    assert_eq!(
        plain(&content),
        "Before [error] after.",
        "{}",
        content.repr()
    );

    // A label that cannot attach is reported, and the markup goes on.
    let (repr, messages) = partial(&[("main.fol", "Text <a> more *b* <b> <c>\n")]);
    assert_eq!(
        messages,
        [
            "error: a label must follow an element",
            "error: this element already has a label"
        ]
    );
    assert!(repr.contains("strong(body: [\"b\"]) <b>"), "{repr}");
}

#[test]
fn going_too_deep_is_recovered_from() {
    let (repr, messages) = partial(&[("main.fol", "#let f(n) = f(n + 1)\n#f(0) More.\n")]);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("nested more than"), "{messages:?}");
    assert!(repr.contains("\"More.\""), "{repr}");
}

#[test]
fn exceeding_a_limit_is_not_recovered_from() {
    // Everything after the loop would exceed the limit too.
    let logged = limited(
        SMALL,
        &[("main.fol", "#while true {}\nMore text #(1 + 1).\n")],
    );
    let compiled = folio::compile(&logged.world, Target::Pdf, None).unwrap();
    assert_eq!(compiled.diagnostics.len(), 1, "{:?}", compiled.diagnostics);
    let content = folio::evaluate(&logged.world, None).unwrap();
    assert!(content.is_none(), "{:?}", content.map(|c| c.repr()));
    assert!(compiled.output.is_none());
}

#[test]
fn missing_imports_are_reported_once() {
    assert_eq!(
        messages("#import \"nope.fol\": x\n"),
        ["error: file `nope.fol` not found"]
    );
    assert_eq!(
        messages("#import \"../outside.fol\": x\n"),
        ["error: `../outside.fol` is not a path in the project"]
    );
    assert_eq!(
        messages("#let p = \"lib.fol\"\n#import p: x\n"),
        ["error: the path must be written as a string"]
    );
}

#[test]
fn cyclic_imports_are_errors() {
    let logged = Logged::new(
        MANIFEST,
        &[
            ("main.fol", "#import \"a.fol\": x\n#x\n"),
            ("a.fol", "#import \"b.fol\": y\n#let x = y\n"),
            ("b.fol", "#import \"a.fol\": x\n#let y = 1\n"),
        ],
    );
    let errors = logged.errors();
    assert!(errors.starts_with("error: cyclic import\n"), "{errors}");
    // The import that closes the cycle is the error.
    assert!(errors.contains("a.fol:1:9"), "{errors}");

    let logged = Logged::new(MANIFEST, &[("main.fol", "#include \"main.fol\"\n")]);
    assert!(logged.errors().starts_with("error: cyclic import\n"));
}

// ---- Memoisation ----

#[test]
fn editing_a_function_reevaluates_only_its_callers() {
    let mut logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#import \"lib.fol\": f, g\n\
                 #let h(x) = { g(x) + 1 }\n\
                 = Results\n\n\
                 #f(1) #g(2) #h(3)\n",
            ),
            ("lib.fol", "#let f(x) = { x + 1 }\n#let g(x) = { x * 2 }\n"),
        ],
    );
    assert_eq!(logged.text(None), "Results 2 4 7");
    // f(1), g(2), h(3) and the g(3) that h makes.
    assert_eq!(logged.calls(), 4);

    // Editing g's body re-runs the calls of g and of h, which captures g,
    // but not the call of f.
    let lib = logged.world.source("lib.fol").unwrap();
    let at = lib.text().find("x * 2").unwrap() + 4;
    logged
        .world
        .edit_source("lib.fol", at..at + 1, "3")
        .unwrap();
    assert_eq!(logged.text(None), "Results 2 6 10");
    assert_eq!(logged.calls(), 3);

    // Editing f's body re-runs only the call of f.
    let at = logged
        .world
        .source("lib.fol")
        .unwrap()
        .text()
        .find("x + 1")
        .unwrap()
        + 4;
    logged
        .world
        .edit_source("lib.fol", at..at + 1, "5")
        .unwrap();
    assert_eq!(logged.text(None), "Results 6 6 10");
    assert_eq!(logged.calls(), 1);

    // Editing text in the document re-evaluates it, but every call is
    // reused.
    let at = logged
        .world
        .source("main.fol")
        .unwrap()
        .text()
        .find("Results")
        .unwrap();
    logged
        .world
        .edit_source("main.fol", at..at + 7, "Answers")
        .unwrap();
    assert_eq!(logged.text(None), "Answers 6 6 10");
    let executed = logged.take();
    assert_eq!(executed.get("call_closure"), None, "{executed:?}");
    assert_eq!(executed.get("eval_file"), Some(&1), "{executed:?}");

    // Compiling again without an edit runs nothing.
    logged.text(None);
    assert_eq!(logged.take(), BTreeMap::new());
}

#[test]
fn a_library_imported_from_two_depths_is_evaluated_once() {
    // `lib` is imported by the entry file and by `mid`, which the entry
    // file imports: at depths 1 and 2.
    let logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#import \"mid.fol\": twice\n#import \"lib.fol\": double\n\
                 #twice(1) #double(5)\n",
            ),
            (
                "mid.fol",
                "#import \"lib.fol\": double\n#let twice(x) = double(double(x))\n",
            ),
            ("lib.fol", "#let double(x) = x * 2\n"),
        ],
    );
    assert_eq!(logged.text(None), "4 10");
    let executed = logged.take();
    assert_eq!(executed.get("eval_file"), Some(&3), "{executed:?}");
}

#[test]
fn the_same_call_at_two_sites_is_evaluated_once() {
    // Two sites in the entry file, and one in a file it imports, which
    // evaluates at a greater depth.
    let logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#import \"lib.fol\": double, four\n#double(2) #double(2) #four\n",
            ),
            ("lib.fol", "#let double(x) = x * 2\n#let four = double(2)\n"),
        ],
    );
    assert_eq!(logged.text(None), "4 4 4");
    assert_eq!(logged.calls(), 1);

    // An error still points at the site that made it.
    let report = errors("#let f(x) = x\n#f(1)\n#f(1, 2)\n");
    assert!(report.contains("unexpected argument"), "{report}");
    assert!(report.contains("main.fol:3:7"), "{report}");

    // Arguments taken by a sink keep their spans: an error about them
    // points at the call that passed them.
    let report = errors("#let f(..a) = a\n#let g(x) = x\n#g(..f(1))\n#g(..f(1, 2))\n");
    assert!(report.contains("main.fol:4:"), "{report}");
    assert!(!report.contains("main.fol:3:"), "{report}");
}

#[test]
fn calls_near_the_depth_limit_are_exact_at_any_depth() {
    // `down(n)` recurses n calls deep. A call that fits at one depth is
    // reused there, and the same call from a deeper import fails where the
    // limit is reached, as it would without memoisation.
    let main = "#import \"a.fol\": r\n#let down(n) = if n == 0 { 0 } else { down(n - 1) + 1 }\n\
                #down(60) #r\n";
    let (repr, messages) = partial(&[
        ("main.fol", main),
        ("a.fol", "#import \"b.fol\": r\n"),
        ("b.fol", "#import \"c.fol\": r\n"),
        ("c.fol", "#import \"d.fol\": r\n"),
        (
            "d.fol",
            "#let down(n) = if n == 0 { 0 } else { down(n - 1) + 1 }\n#let r = down(60)\n",
        ),
    ]);
    assert!(repr.starts_with("[\"60\""), "{repr}");
    assert_eq!(
        messages,
        ["error: calls and imports are nested more than 64 deep"],
        "{repr}"
    );

    // At depth 1 the library's call reaches an error the limit stops it
    // from reaching at its real depth. That evaluation is not used, and
    // its error is not reported.
    let (_, messages) = partial(&[
        ("main.fol", "#import \"a.fol\": r\n"),
        ("a.fol", "#import \"b.fol\": r\n"),
        ("b.fol", "#import \"c.fol\": r\n"),
        ("c.fol", "#import \"d.fol\": r\n"),
        (
            "d.fol",
            "#let down(n) = if n == 0 { nope } else { down(n - 1) }\n#let r = down(62)\n",
        ),
    ]);
    assert_eq!(
        messages,
        ["error: calls and imports are nested more than 64 deep"]
    );
}

#[test]
fn an_error_brought_by_many_imports_is_kept_once() {
    // Each file imports the next one twice, so the last one's error is
    // reached along 2^18 paths. It is kept once at each file, not once per
    // path.
    let mut files: Vec<(String, String)> = (0..18)
        .map(|i| {
            let path = if i == 0 {
                "main.fol".to_owned()
            } else {
                format!("f{i}.fol")
            };
            let next = format!("f{}.fol", i + 1);
            (
                path,
                format!("#import \"{next}\": x\n#import \"{next}\": y\n"),
            )
        })
        .collect();
    files.push((
        "f18.fol".into(),
        "#let x = 1\n#let y = 2\n#(1 + \"a\")\n".into(),
    ));
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        send.send(partial(&files).1)
    });
    let messages = receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("evaluation finishes in time");
    assert_eq!(messages, ["error: cannot add an int and a str"]);
}

#[test]
fn only_code_that_reads_the_variant_is_evaluated_per_variant() {
    let logged = Logged::new(
        VARIANTS,
        &[
            (
                "main.fol",
                "#import \"lib.fol\": shared, per\n#shared(1) #per(1)\n",
            ),
            (
                "lib.fol",
                "#let shared(x) = { x + 1 }\n\
                 #let per(x) = { if variant == \"exam\" [E#x] else [M#x] }\n",
            ),
        ],
    );
    assert_eq!(logged.text(Some("exam")), "2 E1");
    logged.take();

    // The mark scheme reuses the library file and the call that does not
    // read `variant`, and evaluates again only the call that does, and the
    // file that makes it.
    assert_eq!(logged.text(Some("markscheme")), "2 M1");
    let executed = logged.take();
    assert_eq!(executed.get("call_closure"), Some(&1), "{executed:?}");
    assert_eq!(executed.get("eval_file"), Some(&1), "{executed:?}");

    // `variant` is `none` in a project without variants.
    assert_eq!(text("#repr(variant)"), "none");
}

// ---- Sandbox ----

#[test]
fn scripts_read_only_project_files() {
    let logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#import \"q/bank.fol\": first\n#read(\"/data/note.txt\") #first\n",
            ),
            (
                "q/bank.fol",
                "#let first = json(\"../data/bank.json\").questions.at(0).marks\n",
            ),
            ("data/note.txt", "Hello"),
            ("data/bank.json", "{\"questions\": [{\"marks\": 3}]}"),
        ],
    );
    assert_eq!(logged.text(None), "Hello 3");

    assert_eq!(
        messages("#read(\"../../etc/passwd\")"),
        ["error: `../../etc/passwd` is not a path in the project"]
    );
    // A path from the root means the project's root.
    assert_eq!(
        messages("#read(\"/etc/passwd\")"),
        ["error: file `etc/passwd` not found"]
    );
}

#[test]
fn data_files() {
    let logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#let t = toml(\"d.toml\")\n#let rows = csv(\"d.csv\")\n\
                 #t.title #t.list.len() #t.date.year #rows.len() #rows.at(1).at(1)\n",
            ),
            (
                "d.toml",
                "title = \"Mechanics\"\nlist = [1, 2_000, 3.5]\ndate = 2026-11-24\n",
            ),
            ("d.csv", "name,mark\nAda,\"7, of 8\"\n"),
        ],
    );
    assert_eq!(logged.text(None), "Mechanics 3 2026 2 7, of 8");
    assert_eq!(
        messages("#json(\"main.fol\")")[0],
        "error: the file is not valid JSON: expected value (line 1, column 1)"
    );
    // Errors in a data file say where in it they are.
    let logged = Logged::new(
        MANIFEST,
        &[
            ("main.fol", "#toml(\"d.toml\")\n#yaml(\"d.yaml\")\n"),
            ("d.toml", "a = 1\nb = \n"),
            ("d.yaml", "questions:\n  - text: A ball\n  marks: 3\n"),
        ],
    );
    let compiled = folio::compile(&logged.world, Target::Pdf, None).unwrap();
    let errors: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[0].starts_with("the file is not valid TOML: ")
            && errors[0].ends_with("(line 2, column 5)"),
        "{errors:?}"
    );
    assert!(
        errors[1].starts_with("the file is not valid YAML: ")
            && errors[1].contains("(line 3, column "),
        "{errors:?}"
    );
    // Each points into its data file too, which the terminal shows, and so
    // does a snapshot taken after the build.
    let errors: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .cloned()
        .collect();
    for (error, path) in errors.iter().zip(["d.toml", "d.yaml"]) {
        let place = diag::locate(&logged.world, error.related[0].span).unwrap();
        assert_eq!(place.path, path);
    }
    let text = diag::render(&logged.world, &errors, false);
    assert!(text.contains("b = "), "{text}");
    assert!(text.contains("here in the file"), "{text}");
    let snapshot = logged.world.snapshot();
    assert_eq!(diag::render(&snapshot, &errors, false), text);
}

/// A YAML file whose aliases repeat each other counts as the items it
/// stands for against the memory limit.
#[test]
fn yaml_aliases_count_against_the_memory_limit() {
    let mut laughs = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for i in 1..9 {
        let refs = vec![format!("*a{}", i - 1); 10].join(", ");
        laughs.push_str(&format!("a{i}: &a{i} [{refs}]\n"));
    }
    let logged = Logged::new(
        MANIFEST,
        &[
            ("main.fol", "#yaml(\"l.yaml\").len()\n"),
            ("l.yaml", &laughs),
        ],
    );
    let compiled = folio::compile(&logged.world, Target::Pdf, None).unwrap();
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.is_error() && d.message.contains("memory")),
        "{:?}",
        compiled.diagnostics
    );
}

/// A question bank in a data file drives generated content; an edit to
/// the file evaluates again only the calls whose arguments changed, and
/// not the files that do not read it (SPEC §4.4, §2.3).
#[test]
fn an_edit_to_a_question_bank_reevaluates_only_what_depends_on_it() {
    let mut logged = Logged::new(
        MANIFEST,
        &[
            (
                "main.fol",
                "#import \"card.fol\": card\n\
                 #for q in yaml(\"bank.yaml\").questions [#card(q.text, q.marks) ]\n\
                 #include \"intro.fol\"\n",
            ),
            ("card.fol", "#let card(text, marks) = [#text (#marks)]\n"),
            ("intro.fol", "#let n = 2\nIntro #n.\n"),
            (
                "bank.yaml",
                "questions:\n  - {text: A ball is thrown., marks: 3}\n  - {text: A car brakes., marks: 4}\n",
            ),
        ],
    );
    assert_eq!(
        logged.text(None),
        "A ball is thrown. (3) A car brakes. (4) Intro 2."
    );
    logged.take();
    logged
        .world
        .set_file(
            "bank.yaml",
            b"questions:\n  - {text: A ball is thrown., marks: 3}\n  - {text: A car stops., marks: 4}\n"
                .to_vec(),
        )
        .unwrap();
    assert_eq!(
        logged.text(None),
        "A ball is thrown. (3) A car stops. (4) Intro 2."
    );
    let executed = logged.take();
    // The file that reads the bank, and the one call with new arguments.
    assert_eq!(executed.get("call_closure"), Some(&1), "{executed:?}");
    assert_eq!(executed.get("eval_file"), Some(&1), "{executed:?}");
}

#[test]
fn today_comes_from_the_manifest_or_the_host() {
    assert_eq!(messages("#today"), ["error: no date is set for `today`"]);
    let manifest = format!("{MANIFEST}today = 2026-11-24\n");
    let mut logged = Logged::new(&manifest, &[("main.fol", "#today.display() #today.year")]);
    assert_eq!(logged.text(None), "2026-11-24 2026");
    logged.world.set_today(Some(Date {
        year: 2027,
        month: 1,
        day: 5,
    }));
    assert_eq!(logged.text(None), "2027-01-05 2027");
}

// ---- Limits ----

fn limited(limits: Limits, files: &[(&str, &str)]) -> Logged {
    Logged::with(World::builder().limits(limits), MANIFEST, files)
}

const SMALL: Limits = Limits {
    steps: 10_000,
    memory: 1024 * 1024,
    depth: 20,
};

#[test]
fn steps_are_limited() {
    let logged = limited(SMALL, &[("main.fol", "#while true {}\n")]);
    assert_eq!(
        logged.errors(),
        "\
error: evaluation exceeded the limit of 10000 steps
  ┌─ main.fol:1:2
  │
1 │ #while true {}
  │  ^^^^^^^^^^^^^
  │
  = hint: check for a loop that does not end

"
    );

    // The default limit stops a loop that does not end, too.
    let errors = errors("#{ let i = 0; while true { i += 1 } }");
    assert!(errors.contains("exceeded the limit of"), "{errors}");
}

#[test]
fn step_counts_do_not_depend_on_memoisation() {
    // One call of `work` takes about 3,000 steps: it fits in the limit,
    // two do not, though the second is reused from the first.
    let work = "#let work() = { let n = 0; for i in range(1000) { n += i }; n }\n";
    let once = format!("{work}#work()\n");
    let twice = format!("{work}#work() #work()\n");
    let limits = Limits {
        steps: 5_000,
        ..SMALL
    };

    let cold = limited(limits, &[("main.fol", &twice)]).errors();
    assert!(cold.contains("exceeded the limit of 5000 steps"), "{cold}");

    let mut warm = limited(limits, &[("main.fol", &once)]);
    assert_eq!(warm.text(None), "499500");
    warm.world.set_source("main.fol", &twice).unwrap();
    assert_eq!(warm.errors(), cold);
}

#[test]
fn memory_is_limited() {
    let logged = limited(
        SMALL,
        &[("main.fol", "#{ let s = \"x\"; while true { s += s } }\n")],
    );
    let errors = logged.errors();
    assert!(
        errors.starts_with("error: evaluation exceeded the memory limit of 1 MiB"),
        "{errors}"
    );

    // A repetition is counted before it allocates.
    let logged = limited(SMALL, &[("main.fol", "#(\"x\" * 1000000000)\n")]);
    assert!(logged.errors().contains("memory limit"));
    let logged = limited(SMALL, &[("main.fol", "#range(1000000000).len()\n")]);
    assert!(logged.errors().contains("memory limit"));
}

#[test]
fn recursion_is_limited_and_points_at_the_call() {
    let logged = limited(SMALL, &[("main.fol", "#let f(n) = f(n + 1)\n#f(0)\n")]);
    assert_eq!(
        logged.errors(),
        "\
error: calls and imports are nested more than 20 deep
  ┌─ main.fol:1:13
  │
1 │ #let f(n) = f(n + 1)
  │             ^^^^^^^^
  │             │
  │             in this call of `f`
2 │ #f(0)
  │  ---- in this call of `f`
  │
  = hint: check for a function that calls itself without end

"
    );

    // A call that repeats its own arguments is not a query cycle: the depth
    // is part of the key.
    let logged = limited(SMALL, &[("main.fol", "#let f() = f()\n#f()\n")]);
    assert!(logged.errors().contains("nested more than 20 deep"));

    // With the default limits, deep recursion stops at the limit.
    let errors = errors("#let f(n) = if n == 0 { 0 } else { 1 + f(n - 1) }\n#f(100000)\n");
    assert!(errors.contains("nested more than 64 deep"), "{errors}");
}

#[test]
fn deep_expressions_in_deep_calls_do_not_exhaust_the_stack() {
    // Each call nests 200 expressions, the most the parser allows is 256,
    // and the calls nest to the default limit. The evaluator grows its stack
    // instead of overflowing a thread's small one.
    let nested = format!("{}f(n - 1){}", "(".repeat(200), ")".repeat(200));
    let main = format!("#let f(n) = if n == 0 {{ 0 }} else {{ {nested} }}\n#f(60)\n");
    let text = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || Logged::new(MANIFEST, &[("main.fol", &main)]).text(None))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(text, "0");
}

/// The text of `main`, which must finish within a time no exponential or
/// quadratic evaluation of it would.
fn quick_text(main: &str, seconds: u64) -> String {
    let main = main.to_owned();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || send.send(text(&main)));
    receive
        .recv_timeout(std::time::Duration::from_secs(seconds))
        .expect("evaluation finishes in time")
}

#[test]
fn values_that_share_parts_are_hashed_and_compared_once_per_part() {
    // Each loop doubles a value by holding it twice, which makes a tree of
    // 2^60 nodes from 60 distinct ones. A call hashes its arguments, and
    // `==` compares two such values built apart.
    let cases = [
        "#let a = (1,)\n#for i in range(60) { a = (a, a) }\n#f(a)",
        "#let a = (x: 1)\n#for i in range(60) { a = (x: a, y: a) }\n#f(a)",
        "#let a = sink()\n#for i in range(60) { a = sink(a, a) }\n#f(a)",
        "#let g = strong\n#for i in range(45) { g = g.with(g) }\n#f(g)",
        "#let c = [x]\n#for i in range(60) { c = strong(c) + strong(c) }\n#f(c)",
        "#let a = (1,)\n#for i in range(60) { a = (a, a) }\n#let g() = a\n#f(g)",
    ];
    for case in cases {
        let main = format!("#let f(x) = 1\n#let sink(..a) = a\n{case}\n");
        assert_eq!(quick_text(&main, 30), "1", "{case}");
    }
    let compare = |init_a: &str, init_b: &str, wrap: &str| {
        let main = format!(
            "#let a = {init_a}\n#let b = {init_b}\n\
             #for i in range(60) {{ a = {wa}; b = {wb} }}\n#(a == b)\n",
            wa = wrap.replace('v', "a"),
            wb = wrap.replace('v', "b"),
        );
        quick_text(&main, 30)
    };
    assert_eq!(compare("(1,)", "(1,)", "(v, v)"), "true");
    assert_eq!(compare("(1,)", "(2,)", "(v, v)"), "false");
    // `==` compares `1` and `1.0` as equal, and content strictly.
    assert_eq!(compare("(1,)", "(1.0,)", "(x: v, y: v)"), "true");
    assert_eq!(compare("[x]", "[y]", "strong(v) + strong(v)"), "false");
    // Within one comparison, arrays found equal by `==` are not therefore
    // equal where content holds them, which compares them strictly. The
    // contents are made by one call site, so only the arrays differ.
    let main = "#let w(v) = strong(override: (x: v))[x]\n\
                #let a = ((1,), (1,))\n#let b = ((1.0,), (1.0,))\n\
                #(a == b) #(w(a) == w(a)) #((a, w(a)) == (b, w(b)))\n";
    assert_eq!(text(main), "true true false");
}

#[test]
fn changing_an_array_in_a_loop_takes_linear_time() {
    // The method's target is not held while the array changes in place,
    // which would copy all of it on every call.
    let main = "#let a = ()\n#for i in range(20000) { a.push((i,)) }\n\
                #while a.len() > 1 { let _ = a.pop() }\n\
                #let d = (:)\n#for i in range(20000) { d.insert(\"k\", (i,)) }\n\
                #a.len() #d.len()\n";
    assert_eq!(quick_text(main, 30), "1 1");
}

#[test]
fn removing_dictionary_keys_in_insertion_order_takes_linear_time() {
    // Removing the first key does not shift every key after it. The order
    // of the others stays visible after each removal and after re-insertion.
    let main = "#let d = (:)\n#for i in range(100000) { d.insert(str(i), i) }\n\
                #for i in range(99997) { let _ = d.remove(str(i)) }\n\
                #d.insert(\"99997\", 0)\n#d.insert(\"a\", 1)\n\
                #let _ = d.remove(\"99998\")\n#d.keys().join(\",\")\n";
    assert_eq!(quick_text(main, 30), "99997,99999,a");
}

// ---- Components ----

/// A component that provides one element, `question`.
#[derive(Debug)]
struct Exam(&'static str);

impl Component for Exam {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest::new(
            ComponentName::new(self.0).unwrap(),
            semver::Version::new(0, 0, 0),
            [Target::Pdf],
        )
    }

    fn declarations(&self) -> Declarations {
        let mut question = ElementSchema::new(Name::new("question").unwrap(), ElementKind::Block);
        question.fields = EcoVec::from([FieldDecl::required(
            Name::new("marks").unwrap(),
            FieldType::Int,
        )]);
        question.body = Body::Content(folio_component::ContentKind::Blocks);
        let mut declarations = Declarations::new();
        declarations.elements = EcoVec::from([question]);
        declarations
    }
}

#[test]
fn component_elements_make_instances() {
    let mut registry = Registry::new();
    registry.register(Exam("exam")).unwrap();
    let logged = Logged::with(
        World::builder().components(registry),
        MANIFEST,
        &[(
            "main.fol",
            "#question(marks: 3, override@pptx: (space-before: 24pt))[Find $v$.] <q:1>\n\
             #exam.question(marks: 1)[Why?]\n",
        )],
    );
    let content = logged.content(None);
    assert_eq!(
        content.repr(),
        "[exam.question(marks: 3, \
         body: [\"Find\", space, math(block: false, tree: formula([v])), \".\"], \
         override@pptx: (space-before: 24.0pt)) <q:1>, space, \
         exam.question(marks: 1, body: [\"Why?\"]), space]"
    );
    let element = content.nodes()[0].clone();
    let folio::eval::Node::Element(element) = element else {
        panic!("not an element");
    };
    assert_eq!(element.field("marks"), Some(&Value::Int(3)));
    assert_eq!(element.label.as_deref(), Some("q:1"));
}

#[test]
fn element_calls_report_why_a_name_does_not_resolve() {
    let mut registry = Registry::new();
    registry.register(Exam("exam")).unwrap();
    registry.register(Exam("quiz")).unwrap();
    let manifest = format!("{MANIFEST}\n[components]\nquiz = {{ version = \"0.0.0\" }}\n");
    let logged = Logged::with(
        World::builder().components(registry.clone()),
        &manifest,
        &[("main.fol", "#question(marks: 1)[A]\n")],
    );
    let errors = logged.errors();
    assert!(
        errors.starts_with(
            "error: element `question` is ambiguous: `exam` and `quiz` provide it; \
             qualify it with a namespace\n"
        ),
        "{errors}"
    );

    // Qualified, it resolves; a body given twice is an error.
    let logged = Logged::with(
        World::builder().components(registry),
        &manifest,
        &[("main.fol", "#quiz.question(marks: 1, body: [A])[B]\n")],
    );
    assert!(
        logged
            .errors()
            .starts_with("error: duplicate field `body`: it is given by name and as the body\n")
    );
}
