//! The memory limit bounds what evaluation allocates (SPEC §4.4, §14.3):
//! scripts that would allocate far more than the limit, through library
//! functions or through values that hold one part many times, end with a
//! limit error, and the process's peak memory stays within a small multiple
//! of the limit.
//!
//! The file holds one test, so that nothing else runs in its process while
//! it reads the process's peak memory.

#![cfg(target_os = "linux")]

mod support;

use folio_core::eval::Limits;

/// The memory limit of the test: small, so that the scripts reach it fast.
const MEMORY: u64 = 32 * 1024 * 1024;

/// Evaluates `script` with the test's memory limit.
fn evaluate_script(script: &str) -> Vec<String> {
    support::evaluate(
        script,
        Limits {
            memory: MEMORY,
            ..Limits::default()
        },
    )
}

/// The process's peak resident memory in bytes, as Linux reports it.
fn peak_memory() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .expect("Linux reports the peak resident memory");
    let kib: u64 = line
        .trim_start_matches("VmHWM:")
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .unwrap();
    kib * 1024
}

/// Scripts that allocate without end, or walk exponentially large trees of
/// shared parts, each in a way the evaluator once did not count (#321).
const SCRIPTS: &[&str] = &[
    // Library functions that copy an array.
    "#let a = range(200000)\n#let b = ()\n#let i = 0\n\
     #while i < 3000 { b.push(a.rev()); i += 1 }\n",
    "#let a = range(200000)\n#let b = ()\n#let i = 0\n\
     #while i < 3000 { b.push(a.sorted()); i += 1 }\n",
    "#let a = range(200000)\n#let b = ()\n#let i = 0\n\
     #while i < 3000 { b.push(a.slice(1)); i += 1 }\n",
    "#let a = range(200000)\n#let b = ()\n#let i = 0\n\
     #while i < 3000 { b.push(a.filter(x => true)); i += 1 }\n",
    "#let a = range(1000)\n#let i = 0\n\
     #while i < 6 { a = a.map(x => a).flatten(); i += 1 }\n",
    // Flattening an array of shared empty arrays allocates nothing, but
    // walks every one of them.
    "#let a = ()\n#let i = 0\n#while i < 60 { a = (a, a); i += 1 }\n#a.flatten().len()\n",
    // Representations of shared parts.
    "#let a = ()\n#let i = 0\n#while i < 40 { a = (a, a); i += 1 }\n#repr(a).len()\n",
    "#let a = ()\n#let i = 0\n#while i < 40 { a = (a, a); i += 1 }\n#a\n",
    "#let a = ()\n#let i = 0\n#while i < 40 { a = (a, a); i += 1 }\n#panic(a)\n",
    // Content that places content twice.
    "#let c = [x]\n#let i = 0\n#while i < 40 { c = [#c #c]; i += 1 }\n#c\n",
    "#let c = [x]\n#let i = 0\n#while i < 40 { c = [#c] + [#c]; i += 1 }\n#repr(c).len()\n",
    // Spreading an array twice.
    "#let a = (1,)\n#let i = 0\n#while i < 40 { a = (..a, ..a); i += 1 }\n",
    "#let f(..a) = a.pos()\n#let a = (1,)\n#let i = 0\n\
     #while i < 40 { a = f(..a, ..a); i += 1 }\n",
    // A formula whose nodes share one long text.
    "#let s = \"x\" * 1000000\n#let f = $#s$\n#let i = 0\n\
     #while i < 16 { f = $(#f) (#f)$; i += 1 }\n#repr(f).len()\n",
    // A formula whose quantities write long digits.
    "#let d = \"1\" * 1000\n#let f = $#num(d, pm: d)$\n#let i = 0\n\
     #while i < 16 { f = $(#f) (#f)$; i += 1 }\n#repr(f).len()\n",
    // Text runs that placing content in a formula copies.
    "#let s = \"x\" * 1000000\n#let c = [#s]\n#let i = 0\n\
     #while i < 12 { c = [#c #c]; i += 1 }\n$#c$\n",
    // Copies made by `replace` and repetitions made by `product`.
    "#let s = \"a\" * 1000000\n#let b = ()\n#let i = 0\n\
     #while i < 3000 { b.push(s.replace(\"q\", \"\")); i += 1 }\n",
    "#let a = range(1000)\n#let b = (a, 1000000).product()\n",
    // Escaping, which makes a string several times longer.
    "#let s = \"\\u{1}\" * 10000000\n#let r = repr(s)\n",
    "#let d = ((\"\\u{1}\" * 10000000): 1)\n#let r = repr(d)\n",
    // Calls, which the memo tables keep.
    "#let f(x) = x\n#let i = 0\n#while i < 5000000 { let _ = f(i); i += 1 }\n",
    // Strings.
    "#let s = \"aß\"\n#let i = 0\n#while i < 40 { s = (s + s).upper(); i += 1 }\n",
];

#[test]
fn peak_memory_stays_within_a_multiple_of_the_limit() {
    let baseline = peak_memory();
    for script in SCRIPTS {
        let errors = evaluate_script(script);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("exceeded the memory limit")),
            "{script}\n{errors:?}"
        );
    }

    // A formula that doubles at each step ends with the formula's own
    // limit, long before its memory would matter.
    let errors = evaluate_script(
        "#let g(n) = if n == 0 { $x$ } else { let a = g(n - 1); $(#a) / (#a)$ }\n$#g(40)$\n",
    );
    assert!(
        errors
            .iter()
            .any(|error| error.contains("a formula holds more than")),
        "{errors:?}"
    );

    // Comparing arrays that hold one part many times compares each pair of
    // parts once.
    let errors = evaluate_script(
        "#let a = ()\n#let b = ()\n#let i = 0\n\
         #while i < 60 { a = (a, a); b = (b, b); i += 1 }\n\
         #assert(not (a < b))\n#assert(calc.max(a, b) == a)\n#let s = (a, b).sorted()\n",
    );
    assert!(errors.is_empty(), "{errors:?}");

    // The values are counted at least at what they take, and growing a
    // vector can leave up to as much again unused.
    let peak = peak_memory().saturating_sub(baseline);
    assert!(
        peak <= 3 * MEMORY,
        "the peak memory grew by {} MiB, with a limit of {} MiB",
        peak / (1024 * 1024),
        MEMORY / (1024 * 1024)
    );
}
