//! The patched upstream crates in `third_party/` (SPEC §2.4, D-300-1).
//!
//! SVG figures reach PDF through `usvg`, `svgtypes`, `kurbo`,
//! `tiny-skia-path`, `resvg` and `tiny-skia`, and PDF figures converted to
//! SVG through `hayro-interpret` and `hayro-syntax`, whose floating-point
//! code called std's transcendental functions (`sin`, `atan2`, `powf`, …). Those
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
//!   aarch64 before the patches (#300);
//! - a calculator function's colour in the corpus's PDF figure is not the
//!   one `libm` computes (#304).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// The patched crates whose patch needs no `libm`: `hayro-syntax` only
/// multiplies out a `powi`.
const WITHOUT_LIBM: &[&str] = &["hayro-syntax"];

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

/// The code of a file without its comments, string and character literals
/// and `#[cfg(test)]` items, one string per line.
fn code(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut depth = 0usize;
    let mut skip_from: Option<usize> = None;
    let mut pending_test = false;
    let mut in_block_comment = false;
    // Inside a string literal: `Some(0)` for a plain one, `Some(n + 1)` for a
    // raw one closed by `"` and `n` hashes. Strings may span lines.
    let mut in_str: Option<usize> = None;
    for raw in text.lines() {
        let mut line = String::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if in_block_comment {
                if c == '*' && next == Some('/') {
                    in_block_comment = false;
                    i += 1;
                }
                i += 1;
                continue;
            }
            if let Some(hashes) = in_str {
                if hashes == 0 && c == '\\' {
                    i += 2;
                    continue;
                }
                if c == '"' {
                    let closing = if hashes == 0 { 0 } else { hashes - 1 };
                    if chars[i + 1..].iter().take_while(|&&h| h == '#').count() >= closing {
                        in_str = None;
                        i += 1 + closing;
                        continue;
                    }
                }
                i += 1;
                continue;
            }
            match c {
                '"' => in_str = Some(0),
                'r' if !line.ends_with(|p: char| p.is_alphanumeric() || p == '_')
                    && matches!(next, Some('"' | '#')) =>
                {
                    let hashes = chars[i + 1..].iter().take_while(|&&h| h == '#').count();
                    if chars.get(i + 1 + hashes) == Some(&'"') {
                        in_str = Some(hashes + 1);
                        i += 2 + hashes;
                        continue;
                    }
                    line.push(c);
                }
                '\'' => {
                    // A character literal (`'x'`, `'\n'`, `'\u{7b}'`), or a
                    // lifetime, which has no closing quote.
                    if next == Some('\\') {
                        let close = chars[i + 2..].iter().position(|&q| q == '\'');
                        i += close.map_or(1, |p| p + 3);
                        continue;
                    }
                    if chars.get(i + 2) == Some(&'\'') {
                        i += 3;
                        continue;
                    }
                    line.push(c);
                }
                '/' if next == Some('/') => break,
                '/' if next == Some('*') => {
                    in_block_comment = true;
                    i += 1;
                }
                _ => line.push(c),
            }
            i += 1;
        }
        let mut item = line.as_str();
        if let Some(at) = line.find("#[cfg(test)]") {
            pending_test = true;
            item = &line[at + "#[cfg(test)]".len()..];
        }
        let opens = line.matches('{').count();
        let closes = line.matches('}').count();
        if pending_test && skip_from.is_none() {
            match (item.find('{'), item.find(';')) {
                // An item without a body, such as `use`: it ends here.
                (None, Some(_)) => pending_test = false,
                (Some(brace), Some(semi)) if semi < brace => pending_test = false,
                (Some(_), _) => {
                    skip_from = Some(depth);
                    pending_test = false;
                }
                (None, None) => {}
            }
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

/// The forbidden calls in a line of code: `.sin(…)` and `f64::sin(…)`.
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
        for ty in ["f32", "f64"] {
            found.extend(line.matches(&format!("{ty}::{name}(")).map(|_| *name));
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
                        "third_party/{name}/{rel}:{first}: {count} call(s) of `{method}`"
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
            // Every release of the crate in the lockfile, not only the
            // patched one: `[patch.crates-io]` does not reach another
            // semver-incompatible version that a dependency might pull in.
            let packages: Vec<&str> = lock_text
                .split("[[package]]\n")
                .filter(|p| p.starts_with(&format!("name = \"{name}\"\n")))
                .collect();
            assert!(!packages.is_empty(), "{lock} has no {name}");
            for package in &packages {
                assert!(
                    !package.contains("\nsource = "),
                    "{lock} takes a release of {name} from a registry, not from \
                     {prefix}/{name}:\n{package}"
                );
            }
            let patched = packages
                .iter()
                .find(|p| p.contains(&format!("\nversion = \"{version}\"\n")))
                .unwrap_or_else(|| panic!("{lock} has no {name} {version}"));
            assert!(
                WITHOUT_LIBM.contains(&name.as_str()) || patched.contains("\n \"libm\",\n"),
                "{lock} is stale: {name} {version} does not depend on libm, as \
                 its patched copy does; regenerate the lockfile"
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

#[test]
fn calculator_colours_come_from_libm() {
    // The PDF figure of the figures corpus, converted to SVG as Folio does
    // under PDF/UA (§11.1). Its spot colour's tint transform, a PostScript
    // calculator function, multiplies atan, sin, cos, log, ln and a power by
    // 10^4 or 10^5 and keeps the fraction, so that a one-unit difference in
    // the last place shows in the 8-bit colour. Its two swatches must have
    // the colours that libm gives, step by step as hayro evaluates the
    // program in f32 (#304); at both tints, the C libraries of x86-64 and
    // aarch64 Linux give red channels apart. This checks only the
    // calculator; the scan above guards every patched call.
    let path = root().join("tests/corpus/figures/assets/spectrum.pdf");
    let pdf = hayro_svg::hayro_syntax::Pdf::new(Arc::new(fs::read(path).unwrap())).unwrap();
    let page = &pdf.pages()[0];
    let svg = hayro_svg::convert(
        page,
        &hayro_svg::RenderCache::new(),
        &hayro_svg::hayro_interpret::InterpreterSettings::default(),
        &hayro_svg::SvgRenderSettings::default(),
    );

    // The two swatches of the spot colour.
    for t in [1.0f32, 0.4921875] {
        let fill = spot_fill(t);
        assert!(
            svg.contains(&fill),
            "no swatch with {fill} at tint {t} in:\n{svg}"
        );
    }
}

/// The fill of a swatch of `spectrum.pdf`'s spot colour at tint `t`: its
/// tint transform (`spectrum.py`) computed with libm in f32, operator by
/// operator, then the 8-bit colour hayro writes.
fn spot_fill(t: f32) -> String {
    let frac = |x: f32, scale: f32| {
        let v = x * scale;
        v - v.floor()
    };
    let mut atan = libm::atan2f(t * 5.0, 12.0).to_degrees() % 360.0;
    if atan < 0.0 {
        atan += 360.0;
    }
    let r = frac(atan, 10_000.0);
    let g = frac(
        libm::sinf((t * 360.0).to_radians()) + libm::cosf((t * 180.0).to_radians()),
        100_000.0,
    );
    let b = frac(
        libm::log10f(t * 9.0 + 1.0) + libm::logf(t + 1.0) + libm::powf(t, 0.6),
        100_000.0,
    );
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    format!("fill=\"#{:02x}{:02x}{:02x}\"", byte(r), byte(g), byte(b))
}
