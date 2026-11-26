# Test fonts

Math fonts that the math layout's golden test (`tests/golden.rs`) sets its
corpus in besides the two that Folio bundles (New Computer Modern Math and
STIX Two Math, in `crates/folio-fonts/bundled/`). SPEC §8.2 names all four.
They are test fonts only: Folio does not bundle or ship them.

| File | Version | Source | SHA-256 | Licence |
| --- | --- | --- | --- | --- |
| `LibertinusMath-Regular.otf` | Libertinus 7.051 | `static/OTF/` of `Libertinus-7.051.zip`, https://github.com/alerque/libertinus/releases/tag/v7.051 | `ded714b0d3808af527438ed7a85f16933133f66a12104c4a83db4cc248888011` | SIL Open Font License 1.1, `OFL-Libertinus.txt` |
| `FiraMath-Regular.otf` | Fira Math 0.3.4 | https://github.com/firamath/firamath/releases/tag/v0.3.4 | `2028cbd3dd4d8c0cf1608520eb4759956a83a67931d7b6d8e7c313520186e35b` | SIL Open Font License 1.1, `OFL-FiraMath.txt` |

The files are unmodified.
