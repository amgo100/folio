//! Deterministic output (SPEC §2.4): the same inputs and the same compiler
//! version give byte-identical outputs.
//!
//! The test builds the whole corpus in `tests/pdf/corpus` (every variant and
//! target each project declares) in two child processes of this test binary
//! that differ in everything that must not matter:
//!
//! - how the builds run: each in a fresh world, one at a time and in
//!   reverse order; or every build of a project in parallel on handles of
//!   one world ([`folio::compile_all`]), after an edit to the entry file
//!   and its revert, so that the memoised queries and the shaping, line
//!   and page caches have been through an incremental recompile;
//! - the number of `rayon` threads, which shape paragraphs and run the
//!   parallel builds;
//! - the hash seeds, which differ between processes;
//! - the time zone, the locale, `SOURCE_DATE_EPOCH` and the working
//!   directory.
//!
//! It compares every output file and the diagnostics of every build byte
//! for byte. Then it compares the SHA-256 digests of the outputs with the
//! ones recorded in `tests/determinism/digests.txt` by the last bless, so
//! that running the test on another machine or operating system compares
//! it with the one that recorded them. After a change meant to alter the
//! output, run `FOLIO_BLESS=1 cargo test -p folio --test determinism` and
//! commit the new digests. The file has the format of `sha256sum`, so
//! outputs written with `FOLIO_DETERMINISM_KEEP=<dir>` can be checked with
//! `sha256sum -c` from that directory.

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use folio::{BuildMode, Name, Target, World};
use sha2::{Digest, Sha256};

/// The directory a child process writes its outputs to; set only in the
/// children.
const OUT: &str = "FOLIO_DETERMINISM_OUT";
/// How a child process runs the builds: [`FRESH`] or [`SHARED`].
const STRATEGY: &str = "FOLIO_DETERMINISM_STRATEGY";
/// Each build in a fresh world, one at a time, in reverse order.
const FRESH: &str = "fresh";
/// Every build of a project in parallel on one world, after an edit and
/// its revert.
const SHARED: &str = "shared";

/// The corpus directory.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/pdf/corpus")
}

/// The recorded digests of the corpus outputs.
fn digests_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/determinism/digests.txt")
}

/// The corpus projects, by name, in order.
fn projects() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(corpus())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("folio.toml").exists())
        .map(|p| (p.file_name().unwrap().to_str().unwrap().to_string(), p))
        .collect();
    out.sort();
    out
}

/// The source files of a project: paths relative to its directory, with
/// their text.
fn sources(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("fol" | "fsty")
            ) {
                let relative = path.strip_prefix(dir).unwrap().to_str().unwrap();
                out.insert(
                    relative.replace('\\', "/"),
                    fs::read_to_string(&path).unwrap(),
                );
            }
        }
    }
    out
}

/// A world with a project, in CI mode, which forbids what is not
/// reproducible (SPEC §2.4).
fn world(dir: &Path, sources: &BTreeMap<String, String>) -> World {
    let manifest = fs::read_to_string(dir.join("folio.toml")).unwrap();
    let mut world = World::builder()
        .mode(BuildMode::Ci)
        .build(&manifest, None)
        .unwrap();
    for (path, text) in sources {
        world.set_source(path, text).unwrap();
    }
    world
}

/// The name of a build's output, as `folio build` names it (SPEC §6.4),
/// with the target as its extension.
fn file_name(project: &str, variant: Option<&Name>, target: Target) -> String {
    match variant {
        Some(variant) => format!("{project}-{variant}.{}", target.as_str()),
        None => format!("{project}.{}", target.as_str()),
    }
}

/// Writes a build's output, if it has one, and its diagnostics as JSON.
fn write_build(out: &Path, name: &str, world: &World, compiled: &folio::Compiled) {
    if let Some(bytes) = &compiled.output {
        fs::write(out.join(name), &bytes[..]).unwrap();
    }
    let json = folio::diag::to_json(world, &compiled.diagnostics);
    fs::write(out.join(format!("{name}.diagnostics.json")), json).unwrap();
}

/// What a child process does: builds the corpus by `strategy` into `out`.
fn build_corpus(out: &Path, strategy: &str) {
    for (project, dir) in projects() {
        let sources = sources(&dir);
        match strategy {
            FRESH => {
                let builds = world(&dir, &sources).builds();
                for (variant, target) in builds.into_iter().rev() {
                    let world = world(&dir, &sources);
                    let compiled = folio::compile(&world, target, variant.as_ref()).unwrap();
                    let name = file_name(&project, variant.as_ref(), target);
                    write_build(out, &name, &world, &compiled);
                }
            }
            SHARED => {
                let mut world = world(&dir, &sources);
                folio::compile_all(&world);
                // An edit and its revert, recompiled after each.
                let entry = world.manifest().project.entry.v.to_string();
                let text = &sources[&entry];
                world
                    .set_source(&entry, &format!("{text}\n\nAn added paragraph.\n"))
                    .unwrap();
                folio::compile_all(&world);
                world.set_source(&entry, text).unwrap();
                for output in folio::compile_all(&world) {
                    let compiled = output.result.unwrap();
                    let name = file_name(&project, output.variant.as_ref(), output.target);
                    write_build(out, &name, &world, &compiled);
                }
            }
            _ => panic!("unknown strategy {strategy}"),
        }
    }
}

/// The files of a directory, by name.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| {
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            (name, fs::read(&p).unwrap())
        })
        .collect()
}

/// Runs this test in a child process that builds the corpus by `strategy`
/// into `out`, with `vars` set and `removed` unset.
fn run_child(out: &Path, strategy: &str, vars: &[(&str, &str)], removed: &[&str]) {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["corpus_is_byte_identical", "--exact", "--test-threads=1"])
        .current_dir(out)
        .env(OUT, out)
        .env(STRATEGY, strategy);
    for (key, value) in vars {
        command.env(key, value);
    }
    for key in removed {
        command.env_remove(key);
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "the {strategy} build failed:\n{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
}

/// Where two byte strings first differ, with the bytes around it.
fn first_difference(a: &[u8], b: &[u8]) -> String {
    let at = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let context = |bytes: &[u8]| {
        let start = at.saturating_sub(32);
        let end = (at + 32).min(bytes.len());
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };
    format!(
        "{} and {} bytes, first difference at byte {at}:\n  {:?}\n  {:?}",
        a.len(),
        b.len(),
        context(a),
        context(b)
    )
}

/// The SHA-256 digests of the output files (not the diagnostics), in the
/// format of `sha256sum`.
fn digests(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut out = String::new();
    for (name, bytes) in files {
        if name.ends_with(".diagnostics.json") {
            continue;
        }
        for byte in Sha256::digest(bytes) {
            write!(out, "{byte:02x}").unwrap();
        }
        writeln!(out, "  {name}").unwrap();
    }
    out
}

/// The whole corpus, every variant and target, builds to the same bytes in
/// processes that differ in strategy, threads, hash seeds, time zone,
/// locale, `SOURCE_DATE_EPOCH` and working directory, and to the bytes
/// recorded in `tests/determinism/digests.txt`.
#[test]
fn corpus_is_byte_identical() {
    if let Some(out) = env::var_os(OUT) {
        let strategy = env::var(STRATEGY).unwrap();
        build_corpus(Path::new(&out), &strategy);
        return;
    }

    let fresh = tempfile::tempdir().unwrap();
    let shared = tempfile::tempdir().unwrap();
    run_child(
        fresh.path(),
        FRESH,
        &[
            ("RAYON_NUM_THREADS", "1"),
            ("TZ", "UTC"),
            ("LC_ALL", "C"),
            ("LANG", "C"),
        ],
        &["SOURCE_DATE_EPOCH"],
    );
    run_child(
        shared.path(),
        SHARED,
        &[
            ("RAYON_NUM_THREADS", "7"),
            ("TZ", "Pacific/Chatham"),
            ("LC_ALL", "pl_PL.UTF-8"),
            ("LANG", "pl_PL.UTF-8"),
            ("SOURCE_DATE_EPOCH", "1767225600"),
        ],
        &[],
    );

    let a = files(fresh.path());
    let b = files(shared.path());
    assert!(
        a.keys().any(|name| name.ends_with(".pdf")),
        "the builds wrote outputs: {:?}",
        a.keys()
    );
    assert_eq!(
        a.keys().collect::<Vec<_>>(),
        b.keys().collect::<Vec<_>>(),
        "both builds write the same files"
    );
    let differ: Vec<String> = a
        .iter()
        .filter(|(name, bytes)| b[*name] != **bytes)
        .map(|(name, bytes)| format!("{name}: {}", first_difference(bytes, &b[name])))
        .collect();
    assert!(
        differ.is_empty(),
        "outputs differ between the builds:\n{}",
        differ.join("\n")
    );

    if let Some(keep) = env::var_os("FOLIO_DETERMINISM_KEEP") {
        let keep = PathBuf::from(keep);
        fs::create_dir_all(&keep).unwrap();
        for (name, bytes) in &a {
            fs::write(keep.join(name), bytes).unwrap();
        }
    }

    let actual = digests(&a);
    let path = digests_file();
    if env::var_os("FOLIO_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &actual).unwrap();
        return;
    }
    let recorded = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        recorded == actual,
        "the corpus outputs differ from the digests recorded in {}.\n\
         If the change to the output is intended, run\n  \
         FOLIO_BLESS=1 cargo test -p folio --test determinism\n\
         and commit the new digests. If it is not, this machine gives other \
         bytes than the one that recorded them, which is a determinism bug.\n\
         recorded:\n{recorded}\nnow:\n{actual}",
        path.display()
    );
}
