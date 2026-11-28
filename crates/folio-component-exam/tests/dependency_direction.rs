//! A first-party component reaches the compiler only through
//! `folio-component` (SPEC §2.2, §3), so the exam component depends on no
//! other workspace crate but those `folio-component` may depend on.
//!
//! The check walks the normal and build dependencies of
//! `folio-component-exam`, on every target and with all features, and fails
//! on any workspace crate that is not listed in [`ALLOWED`].
//! Dev-dependencies are exempt: the tests build papers with `folio`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Workspace crates that may be in the exam component's dependency tree:
/// the component interface, and the crates it may itself depend on (SPEC
/// §2.2).
const ALLOWED: &[&str] = &["folio-component", "folio-text", "folio-draw"];

/// The one workspace crate the exam component may depend on directly.
const DIRECT: &str = "folio-component";

#[test]
fn the_exam_component_depends_only_on_the_interface() {
    let root = workspace_root();

    // Its whole tree holds no workspace crate but the interface's.
    let tree = cargo_tree(&root, &[]);
    let packages: Vec<(&str, Option<PathBuf>)> = tree.lines().filter_map(parse_line).collect();
    assert!(
        packages
            .iter()
            .any(|(name, _)| *name == "folio-component-exam"),
        "`cargo tree` output did not list folio-component-exam itself:\n{tree}"
    );
    let mut forbidden = workspace_crates(&root, &packages, ALLOWED);
    forbidden.sort_unstable();
    forbidden.dedup();
    assert!(
        forbidden.is_empty(),
        "folio-component-exam must not depend on these workspace crates: {forbidden:?}\n\
         A component uses only the interface of folio-component (SPEC §2.2)."
    );

    // And it reaches `folio-text` and `folio-draw`, if at all, only
    // through `folio-component`.
    let direct = cargo_tree(&root, &["--depth", "1"]);
    let packages: Vec<(&str, Option<PathBuf>)> = direct.lines().filter_map(parse_line).collect();
    let mut forbidden = workspace_crates(&root, &packages, &[DIRECT]);
    forbidden.sort_unstable();
    forbidden.dedup();
    assert!(
        forbidden.is_empty(),
        "folio-component-exam must depend directly on no workspace crate but \
         folio-component: {forbidden:?} (SPEC §2.2)"
    );
}

/// The workspace crates among `packages`, other than the exam component
/// itself and those `allowed`.
fn workspace_crates<'a>(
    root: &Path,
    packages: &[(&'a str, Option<PathBuf>)],
    allowed: &[&str],
) -> Vec<&'a str> {
    packages
        .iter()
        .filter(|(name, path)| {
            *name != "folio-component-exam"
                && !allowed.contains(name)
                && path
                    .as_deref()
                    .is_some_and(|p| canonical(p).starts_with(root))
        })
        .map(|(name, _)| *name)
        .collect()
}

/// The normal and build dependencies of the exam component, on every
/// target and with all features, one package per line.
fn cargo_tree(root: &Path, extra: &[&str]) -> String {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(root)
        .args([
            "tree",
            "--locked",
            "--package",
            "folio-component-exam",
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
        .args(extra)
        .output()
        .expect("failed to run `cargo tree`");
    assert!(
        output.status.success(),
        "`cargo tree` failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("`cargo tree` printed invalid UTF-8")
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
        .expect("folio-component-exam lives in <workspace>/crates/folio-component-exam");
    canonical(root)
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
