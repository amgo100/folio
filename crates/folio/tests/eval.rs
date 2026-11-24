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
    FieldDecl, FieldType, INTERFACE_VERSION,
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
        assert!(
            compiled.diagnostics.is_empty(),
            "{}",
            diag::render(&self.world, &compiled.diagnostics, false)
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
fn markup_becomes_a_content_tree() {
    assert_eq!(
        repr("= Forces <sec:forces>\n\nA *ball* is _thrown_. See @sec:forces.\n"),
        "[heading(level: 1, body: [\"Forces\"]) <sec:forces>, parbreak, \"A\", space, \
         strong(body: [\"ball\"]), space, \"is\", space, emph(body: [\"thrown\"]), \". See\", \
         space, ref(target: \"sec:forces\"), \".\", space]"
    );
    assert_eq!(
        repr("- one\n+ two\n/ Term: desc\n"),
        "[list-item(body: [\"one\"]), space, enum-item(body: [\"two\"]), space, \
         terms-item(term: [\"Term\"], description: [\"desc\"]), space]"
    );
    assert_eq!(
        repr("`code` $x^2$\n```rust\nfn main() {}\n```"),
        "[raw(text: \"code\", block: false), space, math(block: false, body: \"x^2\"), \
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
fn syntax_errors_stop_evaluation_quietly() {
    // Only the syntax error is reported, not errors that follow from it.
    let messages = messages("#let f(x = \n#f(1)\n");
    assert!(
        messages.iter().all(|m| !m.contains("unknown variable")),
        "{messages:?}"
    );
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
        "error: the file is not valid JSON: expected value at line 1 column 1"
    );
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

// ---- Components ----

#[derive(Debug)]
struct Exam;

impl Component for Exam {
    fn manifest(&self) -> ComponentManifest {
        ComponentManifest {
            name: ComponentName::new("exam").unwrap(),
            version: semver::Version::new(0, 0, 0),
            interface: INTERFACE_VERSION,
            targets: EcoVec::from([Target::Pdf]),
        }
    }

    fn declarations(&self) -> Declarations {
        let mut question = ElementSchema::new(Name::new("question").unwrap(), ElementKind::Block);
        question.fields = EcoVec::from([FieldDecl::required(
            Name::new("marks").unwrap(),
            FieldType::Int,
        )]);
        question.body = Body::Content(folio_component::ContentKind::Blocks);
        Declarations {
            elements: EcoVec::from([question]),
            ..Declarations::default()
        }
    }
}

#[test]
fn component_elements_make_instances() {
    let mut registry = Registry::new();
    registry.register(&Exam).unwrap();
    let logged = Logged::with(
        World::builder().components(registry),
        MANIFEST,
        &[(
            "main.fol",
            "#question(marks: 3, override@markscheme: (space-before: 24pt))[Find $v$.] <q:1>\n\
             #exam.question(marks: 1)[Why?]\n",
        )],
    );
    let content = logged.content(None);
    assert_eq!(
        content.repr(),
        "[exam.question(marks: 3, override@markscheme: (space-before: 24.0pt), \
         body: [\"Find\", space, math(block: false, body: \"v\"), \".\"]) <q:1>, space, \
         exam.question(marks: 1, body: [\"Why?\"]), space]"
    );
    let element = content.nodes()[0].clone();
    let folio::eval::Node::Element(element) = element else {
        panic!("not an element");
    };
    assert_eq!(element.field("marks"), Some(&Value::Int(3)));
    assert_eq!(element.label.as_deref(), Some("q:1"));
}
