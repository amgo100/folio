# Fuzz regressions

Inputs that once made a fuzz target fail, one directory per target
(`markup/`, `stylesheet/`, `reparse/`, `style/`). Commit the input that
`cargo fuzz` wrote to `fuzz/artifacts/<target>/` here together with its fix;
`cargo test -p folio-testkit --test fuzz` replays every file on stable Rust,
so the failure cannot come back unnoticed. Name a file after what it caught,
such as `unclosed-raw-in-heading`.
