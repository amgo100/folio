//! The patched upstream crates in `third_party/` (SPEC §2.4, D-300-1).
//!
//! SVG figures reach PDF through `usvg`, `svgtypes`, `kurbo`,
//! `tiny-skia-path`, `resvg` and `tiny-skia`, whose floating-point code
//! called std's transcendental functions (`sin`, `atan2`, `powf`, …). Those
//! come from the platform's maths library, so the same drawing could give
//! other PDF bytes on another machine. Folio builds patched copies that call
//! the `libm` crate instead (`scripts/patch-third-party.sh`,
//! `third_party/README.md`). This test fails when:
//!
//! - a patched copy calls one of the functions `clippy.toml` disallows in
//!   Folio's own code, outside comments and test modules, except at the
//!   call sites listed in [`ALLOWED`];
//! - the `[patch.crates-io]` tables of the two workspaces, or their
//!   lockfiles, do not point at every copy in `third_party/crates.txt`, so
//!   that a build would take the unpatched release;
//! - an arrowhead that usvg turns along its line does not get the rotation
//!   that `libm` computes, the case that differed between x86-64 and
//!   aarch64 before the patches (#300).

use std::fs;
use std::path::{Path, PathBuf};

/// The functions whose results depend on the platform's maths library, or
/// for `powi` are left to the language: the methods `clippy.toml` disallows.
const FORBIDDEN: &[&str] = &[
    "powi", "powf", "exp", "exp2", "exp_m1", "ln", "ln_1p", "log", "log2", "log10", "cbrt",
    "hypot", "sin", "cos", "tan", "sin_cos", "asin", "acos", "atan", "atan2", "sinh", "cosh",
    "tanh", "asinh", "acosh", "atanh",
];

/// Methods that take no argument under these names belong to the crates'
/// own types (kurbo's `Vec2::atan2` and `Vec2::hypot`), not to `f32` or
/// `f64`, whose methods of these names take one.
const BINARY: &[&str] = &["powi", "powf", "log", "hypot", "atan2"];

/// Call sites that look like std calls but are not: `(crate, file, method,
/// count, why)`.
const ALLOWED: &[(&str, &str, &str, usize, &str)] = &[(
    "tiny-skia",
    "src/pipeline/highp.rs",
    "powf",
    11,
    "`f32x8::powf`, tiny-skia's own polynomial approximation in IEEE operations",
)];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The patched crates: name and version, from `third_party/crates.txt`.
fn crates() -> Vec<(String, String)> {
    let list = fs::read_to_string(root().join("third_party/crates.txt")).unwrap();
    list.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut parts = l.split_whitespace();
            let name = parts.next().unwrap().to_string();
            let version = parts.next().unwrap().to_string();
            (name, version)
        })
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The code of a file without its comments, string literals and
/// `#[cfg(test)]` items, one string per line.
fn code(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut depth = 0usize;
    let mut skip_from: Option<usize> = None;
    let mut pending_test = false;
    let mut in_block_comment = false;
    for raw in text.lines() {
        let mut line = String::new();
        let mut chars = raw.chars().peekable();
        let mut in_str = false;
        while let Some(c) = chars.next() {
            if in_block_comment {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    in_block_comment = false;
                }
                continue;
            }
            if in_str {
                if c == '\\' {
                    chars.next();
                } else if c == '"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '/' if chars.peek() == Some(&'/') => break,
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    in_block_comment = true;
                }
                _ => line.push(c),
            }
        }
        if line.trim_start().starts_with("#[cfg(test)]") {
            pending_test = true;
        }
        let opens = line.matches('{').count();
        let closes = line.matches('}').count();
        if pending_test && skip_from.is_none() && opens > 0 {
            skip_from = Some(depth);
            pending_test = false;
        }
        let skipping = skip_from.is_some();
        depth = (depth + opens).saturating_sub(closes);
        if let Some(from) = skip_from
            && depth <= from
        {
            skip_from = None;
        }
        lines.push(if skipping { String::new() } else { line });
    }
    lines
}

/// The forbidden calls in a line of code: the methods, each with the
/// character after its opening parenthesis.
fn calls(line: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    for name in FORBIDDEN {
        let needle = format!(".{name}(");
        let mut from = 0;
        while let Some(at) = line[from..].find(&needle) {
            let after = line[from + at + needle.len()..].trim_start();
            if !(BINARY.contains(name) && after.starts_with(')')) {
                found.push(*name);
            }
            from += at + needle.len();
        }
    }
    found
}

#[test]
fn patched_crates_call_no_platform_maths() {
    let mut failures = Vec::new();
    for (name, _) in crates() {
        let dir = root().join("third_party").join(&name);
        let mut files = Vec::new();
        rust_files(&dir.join("src"), &mut files);
        assert!(!files.is_empty(), "third_party/{name} has no sources");
        for file in files {
            let rel = file
                .strip_prefix(&dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&file).unwrap();
            let mut counts: Vec<(&str, usize, usize)> = Vec::new();
            for (i, line) in code(&text).iter().enumerate() {
                for method in calls(line) {
                    match counts.iter_mut().find(|(m, _, _)| *m == method) {
                        Some(entry) => entry.1 += 1,
                        None => counts.push((method, 1, i + 1)),
                    }
                }
            }
            for (method, count, first) in counts {
                let allowed = ALLOWED
                    .iter()
                    .find(|(c, f, m, _, _)| *c == name && *f == rel && *m == method);
                match allowed {
                    Some((_, _, _, n, _)) if *n == count => {}
                    _ => failures.push(format!(
                        "third_party/{name}/{rel}:{first}: {count} call(s) of `.{method}(`"
                    )),
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "patched crates call std's float functions, whose results depend on the \
         platform (SPEC §2.4); call `libm` instead, or list a call that is not \
         std's in ALLOWED:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_build_uses_the_patched_crates() {
    for (manifest, lock, prefix) in [
        ("Cargo.toml", "Cargo.lock", "third_party"),
        ("fuzz/Cargo.toml", "fuzz/Cargo.lock", "../third_party"),
    ] {
        let manifest_text = fs::read_to_string(root().join(manifest)).unwrap();
        let lock_text = fs::read_to_string(root().join(lock)).unwrap();
        let table = manifest_text
            .split("[patch.crates-io]")
            .nth(1)
            .unwrap_or_else(|| panic!("{manifest} has no [patch.crates-io] table"));
        for (name, version) in crates() {
            let entry = format!("{name} = {{ path = \"{prefix}/{name}\" }}");
            assert!(
                table.contains(&entry),
                "{manifest} does not patch {name}: `{entry}`"
            );
            let package = format!("name = \"{name}\"\nversion = \"{version}\"\n");
            let at = lock_text
                .find(&package)
                .unwrap_or_else(|| panic!("{lock} has no {name} {version}"));
            let rest = &lock_text[at + package.len()..];
            let fields = rest.split("\n\n").next().unwrap();
            assert!(
                !fields.contains("source = "),
                "{lock} takes {name} {version} from a registry, not from {prefix}/{name}"
            );
        }
    }
}

#[test]
fn arrowheads_turn_by_libm() {
    // An arrow along (12, 5): glibc's `atan2f` gives it another angle on
    // aarch64 than on x86-64. The marker's scale is 1, so the group usvg
    // makes for it is a rotation and a translation.
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <marker id="m" viewBox="0 0 10 10" refX="5" refY="5" markerWidth="10"
                markerHeight="10" markerUnits="userSpaceOnUse" orient="auto">
            <path d="M 0 0 L 10 5 L 0 10 z"/>
        </marker>
        <path d="M 10 10 l 48 20" stroke="black" marker-end="url(#m)"/>
    </svg>"##;
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).unwrap();
    let transform = rotated(tree.root()).expect("a marker group turned along its line");
    let degrees = libm::atan2f(5.0, 12.0).to_degrees();
    let radians = degrees.to_radians();
    assert_eq!(transform.sx.to_bits(), libm::cosf(radians).to_bits());
    assert_eq!(transform.ky.to_bits(), libm::sinf(radians).to_bits());
}

/// The transform of the first group in the tree that rotates.
fn rotated(group: &usvg::Group) -> Option<usvg::Transform> {
    if group.transform().ky != 0.0 {
        return Some(group.transform());
    }
    group.children().iter().find_map(|node| match node {
        usvg::Node::Group(g) => rotated(g),
        _ => None,
    })
}
