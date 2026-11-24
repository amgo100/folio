//! Prints the concrete syntax tree of a file and its syntax errors:
//! `cargo run -p folio-syntax --example cst -- FILE [--errors]`.

#![forbid(unsafe_code)]

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: cst FILE [--errors]");
    let errors_only = args.next().as_deref() == Some("--errors");
    let text = std::fs::read_to_string(path).expect("readable file");
    let source = folio_syntax::Source::detached(text);
    if !errors_only {
        println!("{:#?}", source.root());
    }
    let (errors, warnings) = source.root().errors_and_warnings();
    for (label, diagnostics) in [("error", errors), ("warning", warnings)] {
        for diagnostic in diagnostics {
            let range = match diagnostic.span.get() {
                folio_syntax::DiagSpanKind::Number { num, sub_range, .. } => {
                    source.range(num, sub_range)
                }
                folio_syntax::DiagSpanKind::Range { range, .. } => Some(range),
                folio_syntax::DiagSpanKind::Detached => None,
            };
            let snippet = range.map(|range| &source.text()[range]).unwrap_or_default();
            println!("{label}: {} at {snippet:?}", diagnostic.message);
        }
    }
}
