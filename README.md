# Folio

Folio is a typesetting system written in Rust, inspired by LaTeX and Typst. It compiles one semantic source into print-quality PDF, Word and PowerPoint files that use real styles and slide masters, and live presentations with animation and interactive elements. Content states what things are, stylesheets state how they look, and components add new kinds of content and layout without changing the core.

Its primary use is teaching material: exams with mark schemes, notes and handouts, and lesson slides, all from shared sources.

## Status

Early implementation: M0 "Skeleton" and M1 "Math, preview, diagnostics" are done. Folio builds tagged PDF with math, quantities and units, references and footnotes, previews documents in the browser as they are edited, and reports diagnostics on the command line and through a language server. DOCX, PPTX and live presentations are not written yet. The full design is in [docs/SPEC.md](docs/SPEC.md); implementation follows the roadmap in §15. Attribution for third-party code is in [NOTICE](NOTICE).

## Usage

A project is a directory with a `folio.toml` manifest; `tests/corpus` holds examples. Build the `folio` binary from this repository and run it in a project directory, or point it at one with `--root`:

```bash
# Build every variant and target the manifest declares, or only those chosen
folio build
folio build --variant exam --target pdf

# Report diagnostics without writing output; --layout adds the softer
# layout problems (underfull lines, widows and orphans, broken keeps),
# --diagnostics json writes them as one JSON document
folio check
folio check --layout --diagnostics json

# Rebuild on every change and serve a live preview on 127.0.0.1;
# it prints the preview's address
folio watch
folio watch --variant markscheme --port 8080
```

From a checkout, `cargo run --bin folio -- build --root tests/corpus/exam` builds the example exam. `folio --help` and `folio <command> --help` list every flag.
