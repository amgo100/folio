//! A long chain of a left-associative math operator nests as deeply as it
//! is long without nesting the parser's calls, so the evaluator must stop
//! it at the nesting limit on the way down (SPEC §4.4, §8.1; #322).

mod support;

use folio_core::eval::Limits;

/// Evaluates `script` on a thread with the stack of the threads that
/// compile builds in parallel.
fn evaluate_on_small_stack(script: String) -> Vec<String> {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || support::evaluate(&script, Limits::default()))
        .unwrap()
        .join()
        .expect("evaluating does not overflow the stack")
}

#[test]
fn long_fraction_chains_end_at_the_nesting_limit() {
    for (open, close) in [("", ""), ("(", ")")] {
        let term = format!("{open}x{close}");
        let chain = vec![term.as_str(); 5000].join("/");
        // In an equation of the file, and in one a function makes.
        for script in [
            format!("A ${chain}$\n"),
            format!("#let f() = ${chain}$\n#f()\n"),
        ] {
            let errors = evaluate_on_small_stack(script);
            assert_eq!(errors, ["math is nested more than 100 deep"]);
        }
    }

    // A chain within the limit evaluates.
    let chain = vec!["x"; 90].join("/");
    assert_eq!(evaluate_on_small_stack(format!("A ${chain}$\n")), [""; 0]);
}
