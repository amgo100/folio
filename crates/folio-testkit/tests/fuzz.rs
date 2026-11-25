//! Replays the fuzz targets' inputs on stable Rust: the seed corpus
//! ([`folio_testkit::fuzz::seeds`]) and the regression inputs in
//! `fuzz/regressions/<target>/`, which once made a target fail and must
//! not again. `scripts/fuzz.sh` runs the fuzzers themselves.

use std::fs;

use folio_testkit::fuzz::{self, Edit, TARGETS};

/// The function of a target, by name.
fn target(name: &str) -> fn(&[u8]) {
    TARGETS
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("no fuzz target {name}"))
        .1
}

#[test]
fn seeds() {
    let seeds = fuzz::seeds();
    for (name, _) in TARGETS {
        assert!(
            seeds.iter().any(|(t, _, _)| t == name),
            "the fuzz target {name} has no seeds"
        );
    }
    for (name, file, bytes) in &seeds {
        let run = target(name);
        if let Err(panic) = std::panic::catch_unwind(|| run(bytes)) {
            panic!("the seed {name}/{file} fails: {panic:?}");
        }
    }
}

#[test]
fn regressions() {
    for (name, run) in TARGETS {
        let Ok(entries) = fs::read_dir(fuzz::regressions_dir(name)) else {
            continue;
        };
        let mut paths: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
        paths.sort();
        for path in paths {
            if path.file_name().is_some_and(|n| n == "README.md") {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            if let Err(panic) = std::panic::catch_unwind(|| run(&bytes)) {
                panic!("{} fails again: {panic:?}", path.display());
            }
        }
    }
}

#[test]
fn edits_round_trip() {
    let edit = Edit {
        stylesheet: true,
        text: "doc { font-size: 11pt; }\nłódź {}".into(),
        range: 25..29,
        with: "#{".into(),
    };
    assert_eq!(Edit::decode(&edit.encode().unwrap()), Some(edit.clone()));
    edit.check();
    // Too few bytes, and bytes that are not UTF-8.
    assert_eq!(Edit::decode(&[1, 2, 3]), None);
    assert_eq!(Edit::decode(&[0, 0, 0, 0, 0xff]), None);
    // A start past the end wraps, and a range stops at the end.
    let edit = Edit::decode(&[0, 9, 0, 15, b'a', b'b']).unwrap();
    assert_eq!(edit.range, 0..2);
}

#[test]
fn style_splits_content_and_stylesheet() {
    assert_eq!(
        fuzz::split_style(b"= A\0heading { x: 1; }"),
        Some(("= A", "heading { x: 1; }"))
    );
    assert_eq!(fuzz::split_style(b"= A"), Some(("= A", "")));
    fuzz::style(
        b"= Title\n\nText *strong*.\0heading { font-size: 20pt; }\nstrong { font-weight: bold; }",
    );
}

#[test]
fn nesting_scripts_apply_wrappers_in_loops() {
    let script = fuzz::nesting_script(&[0, 3, 7, 2, 9]);
    assert_eq!(
        script,
        "#let sink(..a) = a\n#let v = [x]\n\
         #for i in range(3) { v = [*#v*] }\n\
         #for i in range(2) { v = (v,) }\n\
         #v\n"
    );
    // A wrapper index wraps around.
    assert!(fuzz::nesting_script(&[255, 1]).contains(fuzz::WRAPPERS[255 % fuzz::WRAPPERS.len()]));
}

/// The fuzz crate, which only nightly Rust builds, has a target for each
/// harness, and each calls its harness.
#[test]
fn fuzz_crate_matches_harnesses() {
    let fuzz = folio_testkit::root().join("fuzz");
    let manifest = fs::read_to_string(fuzz.join("Cargo.toml")).unwrap();
    let mut files: Vec<String> = fs::read_dir(fuzz.join("fuzz_targets"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    let mut expected: Vec<String> = TARGETS.iter().map(|(n, _)| format!("{n}.rs")).collect();
    expected.sort();
    assert_eq!(files, expected);
    for (name, _) in TARGETS {
        // Cargo forbids binaries named after its own directories.
        assert!(
            !["build", "deps", "examples", "incremental"].contains(name),
            "cargo forbids a binary named {name}"
        );
        let body =
            fs::read_to_string(fuzz.join("fuzz_targets").join(format!("{name}.rs"))).unwrap();
        assert!(
            body.contains(&format!("folio_testkit::fuzz::{name}(data)")),
            "fuzz_targets/{name}.rs does not call its harness"
        );
        assert!(
            manifest.contains(&format!(
                "name = \"{name}\"\npath = \"fuzz_targets/{name}.rs\""
            )),
            "fuzz/Cargo.toml has no binary {name}"
        );
    }
}

#[test]
fn edits_with_a_nul_do_not_encode() {
    let edit = Edit {
        stylesheet: false,
        text: "a\0b".into(),
        range: 0..1,
        with: String::new(),
    };
    assert_eq!(edit.encode(), None);
}

#[test]
fn incremental_edits_decode_within_the_text() {
    // No project byte, or no edit: nothing to do.
    assert!(fuzz::Edits::decode(&[]).is_none());
    assert!(fuzz::Edits::decode(&[0, 1, 2, 3, 4]).is_none());
    // A project and two edits; trailing bytes are ignored.
    let edits = fuzz::Edits::decode(&[0, 0, 0xff, 0xff, 15, 0, 1, 3, 0, 2, 200, 9]).unwrap();
    assert_eq!(edits.ops.len(), 2);
    let sources = &edits.project.sources;
    for op in &edits.ops {
        let edit = fuzz::Edits::edit(sources, *op);
        let text = &sources[&edit.path];
        assert!(text.is_char_boundary(edit.range.start));
        assert!(text.is_char_boundary(edit.range.end));
        assert!(edit.range.end - edit.range.start <= 15 * 4);
    }
    // A snippet index wraps; one past the last snippet inserts nothing.
    let mut op = edits.ops[1];
    op[0] = 0;
    let path = fuzz::Edits::edit(sources, op).path;
    let snippets = if path.ends_with(".fsty") {
        fuzz::SHEET_SNIPPETS
    } else {
        fuzz::CONTENT_SNIPPETS
    };
    op[4] = u8::try_from(snippets.len()).unwrap();
    assert_eq!(fuzz::Edits::edit(sources, op).with, "");
    op[4] = u8::try_from(snippets.len() + 1).unwrap();
    assert_eq!(fuzz::Edits::edit(sources, op).with, snippets[0]);
    // At most MAX_EDITS edits.
    let long = vec![1; 1 + 5 * (fuzz::MAX_EDITS + 3)];
    assert_eq!(
        fuzz::Edits::decode(&long).unwrap().ops.len(),
        fuzz::MAX_EDITS
    );
    // The same seed gives the same input.
    assert_eq!(fuzz::differential_input(7), fuzz::differential_input(7));
    assert_ne!(fuzz::differential_input(7), fuzz::differential_input(8));
}
