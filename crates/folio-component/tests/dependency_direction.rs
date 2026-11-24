//! Components reach the compiler only through `folio-component`, so it must not
//! depend on the core, the writers or the `folio` library (SPEC §2.2, §3).
//!
//! The check walks the normal and build dependencies of `folio-component`, on
//! every target and with all features, and fails on any workspace crate that
//! is not listed in [`ALLOWED`]. Dev-dependencies are exempt: the conformance
//! suite runs against `folio`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Workspace crates that `folio-component` may depend on. SPEC §2.2 allows
/// `folio-text` and `folio-draw` (glyph runs and graphic primitives) once they
/// are split out of the core crate; their own dependencies are checked too.
const ALLOWED: &[&str] = &["folio-text", "folio-draw"];

#[test]
fn folio_component_depends_on_no_core_crate() {
    let root = workspace_root();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(&root)
        .args([
            "tree",
            "--locked",
            "--package",
            "folio-component",
            "--edges",
            "normal,build",
            "--target",
            "all",
            "--all-features",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .expect("failed to run `cargo tree`");
    assert!(
        output.status.success(),
        "`cargo tree` failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8(output.stdout).expect("`cargo tree` printed invalid UTF-8");
    let packages: Vec<(&str, Option<PathBuf>)> = tree.lines().filter_map(parse_line).collect();
    assert!(
        packages.iter().any(|(name, _)| *name == "folio-component"),
        "`cargo tree` output did not list folio-component itself:\n{tree}"
    );

    let mut forbidden: Vec<&str> = packages
        .iter()
        .filter(|(name, path)| {
            *name != "folio-component"
                && !ALLOWED.contains(name)
                && path
                    .as_deref()
                    .is_some_and(|p| canonical(p).starts_with(&root))
        })
        .map(|(name, _)| *name)
        .collect();
    forbidden.sort_unstable();
    forbidden.dedup();
    assert!(
        forbidden.is_empty(),
        "folio-component must not depend on these workspace crates: {forbidden:?}\n\
         Move the shared types into folio-component instead (SPEC §2.2)."
    );
}

/// Splits a `cargo tree` line such as `name v1.2.3 (/path/to/crate) (*)` into
/// the package name and, for path dependencies, the crate directory.
fn parse_line(line: &str) -> Option<(&str, Option<PathBuf>)> {
    let line = line.trim().trim_end_matches(" (*)");
    let (name, rest) = line.split_once(' ')?;
    // `rest` is `v1.2.3`, optionally followed by ` (<source>)`. The source runs
    // to the final `)`, so a path that itself contains parentheses survives.
    let path = rest
        .split_once(" (")
        .and_then(|(_, source)| source.strip_suffix(')'))
        .map(Path::new)
        .filter(|candidate| candidate.is_absolute())
        .map(Path::to_path_buf);
    Some((name, path))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("folio-component lives in <workspace>/crates/folio-component");
    canonical(root)
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
