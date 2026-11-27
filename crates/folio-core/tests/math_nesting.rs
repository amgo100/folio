//! A chain of a left-associative math operator nests as deeply as it is
//! long, up to the parser's depth limit (256), so the evaluator must stop
//! it at its own nesting limit (100) on the way down (SPEC §4.4, §8.1;
//! #322). Longer chains are a syntax error (#380, `folio-syntax`'s
//! `tests/chains.rs`).

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
        let chain = vec![term.as_str(); 200].join("/");
        // In an equation of the file, and in one a function makes.
        for script in [
            format!("A ${chain}$\n"),
            format!("#let f() = ${chain}$\n#f()\n"),
        ] {
            let errors = evaluate_on_small_stack(script);
            assert_eq!(errors, ["math is nested more than 100 deep"]);
        }
    }

    // A line of 98 nested fractions is 99 rows deep, and the equation
    // that holds it 100 levels, the most there may be. Two more fractions
    // are an error of the formula, once.
    let chain = |terms: usize| vec!["x"; terms].join("/");
    assert_eq!(
        evaluate_on_small_stack(format!("A ${}$\n", chain(99))),
        [""; 0]
    );
    assert_eq!(
        evaluate_on_small_stack(format!("A ${}$\n", chain(101))),
        ["math is nested more than 100 deep"]
    );
}

#[test]
fn an_equation_in_code_in_an_equation_counts_from_its_own_top() {
    // Each formula is 61 rows deep; the inner one is never placed in the
    // outer one, so neither is too deep.
    let inner = vec!["x"; 61].join("/");
    let outer = vec!["x"; 60].join("/");
    let script = format!("A ${outer}/#{{ let a = ${inner}$; none }}$\n");
    assert_eq!(evaluate_on_small_stack(script), [""; 0]);
}
