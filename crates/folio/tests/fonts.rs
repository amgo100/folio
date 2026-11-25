//! Fonts (SPEC §2.4, §2.5): the bundled families, project fonts, locking by
//! hash, fallback, system fonts by build mode, and embedding permissions,
//! checked for every character a build sets.

use std::fs;
use std::path::Path;

use ecow::EcoVec;
use folio::{
    BUNDLED_FAMILIES, BuildMode, Compiled, Diagnostic, FaceKey, FontCache, FontHash, FontOrigin,
    FontVariant, Pin, Severity, SystemFont, Target, World, bundled_fonts,
};

const MANIFEST: &str = r#"
[project]
entry = "main.fol"
edition = "2027"
compiler = "0.0.0"
styles = ["styles/main.fsty"]
"#;

const STRICT: &str = "[formatting]\nmode = \"strict\"\n";

const SERIF: &str = "SourceSerif4-Regular.otf";

/// The contents of a bundled file.
fn bundled(name: &str) -> Vec<u8> {
    let file = bundled_fonts()
        .into_iter()
        .find(|f| f.origin() == &FontOrigin::Bundled(name.into()))
        .unwrap();
    file.data().to_vec()
}

fn lock(postscript_name: &str, hash: FontHash) -> String {
    format!(
        "version = 1\n\n[[font]]\nfamily = \"Source Serif 4\"\n\
         postscript-name = \"{postscript_name}\"\nhash = \"{hash}\"\n"
    )
}

/// A world with a stylesheet and text, and only the bundled fonts.
fn world(style: &str, text: &str) -> World {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("styles/main.fsty", style).unwrap();
    world.set_source("main.fol", text).unwrap();
    world
}

/// A world without bundled fonts, for a project that brings its own.
fn bare(manifest: &str, lockfile: Option<&str>, mode: BuildMode) -> World {
    let mut world = World::builder()
        .bundled_fonts([])
        .mode(mode)
        .build(manifest, lockfile)
        .unwrap();
    world
        .set_source("styles/main.fsty", "doc { font: \"Source Serif 4\"; }\n")
        .unwrap();
    world
        .set_source("main.fol", "= Forces\n\nA ball is thrown upwards.\n")
        .unwrap();
    world
}

fn compile(world: &World) -> Compiled {
    folio::compile(world, Target::Pdf, None).unwrap()
}

fn messages(compiled: &Compiled) -> Vec<(Severity, String)> {
    compiled
        .diagnostics
        .iter()
        .map(|d| (d.severity, d.message.to_string()))
        .collect()
}

fn only(compiled: &Compiled) -> &Diagnostic {
    assert_eq!(compiled.diagnostics.len(), 1, "{:?}", compiled.diagnostics);
    &compiled.diagnostics[0]
}

#[test]
fn builds_with_only_bundled_fonts() {
    let text = "= Forces\n\nA *ball* is _thrown_ upwards at 12 m/s; ŁÓDŹ, αβγ, жёлтый.\n\n\
                ```\nlet v = 12;\n```\n";
    for family in BUNDLED_FAMILIES {
        let style = format!(
            "doc {{ font: \"{family}\"; }}\nstrong {{ font-weight: bold; }}\n\
             emph {{ font-style: italic; }}\n"
        );
        // The math fonts have Latin and Greek, but not Cyrillic.
        let text = if family.ends_with("Math") {
            "A ball, αβγ.\n"
        } else {
            text
        };
        let compiled = compile(&world(&style, text));
        assert!(
            !compiled.has_errors(),
            "{family}: {:?}",
            compiled.diagnostics
        );
        assert!(
            compiled.diagnostics.is_empty(),
            "{family}: {:?}",
            compiled.diagnostics
        );
    }

    // The default font is the bundled New Computer Modern.
    let world = world("", text);
    assert!(compile(&world).diagnostics.is_empty());
    let fonts = folio::fonts(&world).unwrap();
    assert!(fonts.faces().iter().all(|f| f.pin == Pin::Bundled));
    let face = fonts
        .select("new computer modern", FontVariant::NORMAL)
        .unwrap();
    assert_eq!(fonts.faces()[face].postscript_name, "NewCM10-Regular");
}

#[test]
fn builds_with_a_locked_project_font() {
    let root = tempfile::tempdir().unwrap();
    let data = bundled(SERIF);
    fs::create_dir_all(root.path().join("fonts")).unwrap();
    fs::write(root.path().join("fonts").join(SERIF), &data).unwrap();
    let lockfile = lock("SourceSerif4-Regular", FontHash::of(&data));

    let mut world = bare(MANIFEST, Some(&lockfile), BuildMode::Ci);
    world.load_fonts(root.path(), None).unwrap();
    let compiled = compile(&world);
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let fonts = folio::fonts(&world).unwrap();
    assert_eq!(fonts.faces().len(), 1);
    assert_eq!(fonts.faces()[0].pin, Pin::Project);
    assert_eq!(
        fonts.faces()[0].origin,
        FontOrigin::Project("fonts/SourceSerif4-Regular.otf".into())
    );
}

#[test]
fn a_hash_mismatch_fails_with_a_clear_error() {
    let root = tempfile::tempdir().unwrap();
    let data = bundled(SERIF);
    fs::create_dir_all(root.path().join("fonts")).unwrap();
    fs::write(root.path().join("fonts").join(SERIF), &data).unwrap();
    let locked = FontHash::of(b"the file that was locked");
    let lockfile = lock("SourceSerif4-Regular", locked);

    let mut world = bare(MANIFEST, Some(&lockfile), BuildMode::Build);
    world.load_fonts(root.path(), None).unwrap();
    let compiled = compile(&world);
    let error = only(&compiled);
    assert!(error.is_error());
    assert_eq!(
        error.message,
        "the font `SourceSerif4-Regular` does not have the hash it is locked by"
    );
    assert_eq!(
        error.hints[0],
        format!(
            "`folio.lock` locks {locked}, but the project font \
             `fonts/SourceSerif4-Regular.otf` has {}",
            FontHash::of(&data)
        )
    );
    // It points at the hash in `folio.lock`.
    let location = folio::diag::locate(&world, error.span).unwrap();
    assert_eq!(location.path, "folio.lock");
    assert_eq!(location.start.line, 6);
}

#[test]
fn a_missing_locked_font_fails() {
    let lockfile = lock("SourceSerif4-Regular", FontHash::of(b"gone"));
    let world = bare(MANIFEST, Some(&lockfile), BuildMode::Build);
    let compiled = compile(&world);
    let messages = messages(&compiled);
    assert!(
        messages.contains(&(
            Severity::Error,
            "the locked font `SourceSerif4-Regular` (family `Source Serif 4`) is not available"
                .into()
        )),
        "{messages:?}"
    );
}

#[test]
fn takes_locked_fonts_from_the_font_cache() {
    let root = tempfile::tempdir().unwrap();
    let cache = FontCache::new(root.path().join("cache"));
    let hash = cache.insert(&bundled(SERIF)).unwrap();
    let lockfile = lock("SourceSerif4-Regular", hash);

    let mut world = bare(MANIFEST, Some(&lockfile), BuildMode::Ci);
    world.load_fonts(root.path(), Some(&cache)).unwrap();
    let compiled = compile(&world);
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let fonts = folio::fonts(&world).unwrap();
    assert_eq!(fonts.faces()[0].origin, FontOrigin::Cache);
    assert_eq!(fonts.faces()[0].pin, Pin::Locked);

    // Without the cache, the locked font is missing.
    world.load_fonts(root.path(), None).unwrap();
    assert!(compile(&world).has_errors());
}

/// A system font that is a copy of a bundled file in a directory of its own.
fn system_font(dir: &Path, name: &str, postscript_name: &str) -> SystemFont {
    let path = dir.join(name);
    fs::write(&path, bundled(name)).unwrap();
    SystemFont {
        path: path.to_str().unwrap().into(),
        index: 0,
        postscript_name: postscript_name.into(),
        key: FaceKey {
            families: EcoVec::from(["Source Serif 4".into()]),
            variant: FontVariant::NORMAL,
        },
    }
}

#[test]
fn system_fonts_depend_on_the_build_mode() {
    let dir = tempfile::tempdir().unwrap();
    let system = system_font(dir.path(), SERIF, "SourceSerif4-Regular");
    let message = "`SourceSerif4-Regular` is a system font, so the output depends on this machine";

    let cases = [
        (BuildMode::Watch, "", None),
        (BuildMode::Build, "", Some(Severity::Warning)),
        (BuildMode::Ci, "", Some(Severity::Error)),
        (BuildMode::Build, STRICT, Some(Severity::Error)),
    ];
    for (mode, extra, expected) in cases {
        let mut world = bare(&format!("{MANIFEST}{extra}"), None, mode);
        world.set_system_fonts([system.clone()]);
        let compiled = compile(&world);
        let expected: Vec<_> = expected
            .map(|s| (s, message.to_string()))
            .into_iter()
            .collect();
        assert_eq!(messages(&compiled), expected, "{mode:?} {extra}");
    }

    // A system font that `folio.lock` locks pins the build.
    let hash = FontHash::of(&bundled(SERIF));
    let lockfile = lock("SourceSerif4-Regular", hash);
    let mut world = bare(MANIFEST, Some(&lockfile), BuildMode::Ci);
    world.set_system_fonts([system]);
    let compiled = compile(&world);
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    assert_eq!(folio::fonts(&world).unwrap().faces()[0].pin, Pin::Locked);
}

#[test]
fn pinned_faces_are_preferred_to_system_faces() {
    let dir = tempfile::tempdir().unwrap();
    let system = system_font(dir.path(), SERIF, "SourceSerif4-Regular");
    let mut world = world("doc { font: \"Source Serif 4\"; }\n", "Text.\n");
    world.set_system_fonts([system]);
    let compiled = compile(&world);
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let fonts = folio::fonts(&world).unwrap();
    let face = fonts.select("Source Serif 4", FontVariant::NORMAL).unwrap();
    assert_eq!(fonts.faces()[face].pin, Pin::Bundled);
}

#[test]
fn falls_back_along_the_chain_by_script() {
    // Source Serif 4 has no Hebrew; New Computer Modern has.
    let text = "Shalom, שלום עולם, said the ball.\n";
    let chain = "doc { font: \"Source Serif 4\", \"New Computer Modern\"; }\n";
    let world = world(chain, text);
    let compiled = compile(&world);
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );

    let fonts = folio::fonts(&world).unwrap();
    let serif = fonts.select("Source Serif 4", FontVariant::NORMAL).unwrap();
    let modern = fonts
        .select("New Computer Modern", FontVariant::NORMAL)
        .unwrap();
    let text = text.trim_end();
    let runs: Vec<(&str, usize)> = fonts
        .resolve(&[serif, modern], text)
        .into_iter()
        .map(|run| (&text[run.range], run.face.unwrap()))
        .collect();
    assert_eq!(
        runs,
        [
            ("Shalom, ", serif),
            ("שלום עולם, ", modern),
            ("said the ball.", serif)
        ]
    );
}

#[test]
fn a_glyph_missing_after_fallback_is_an_error() {
    let compiled = compile(&world(
        "doc { font: \"Source Serif 4\", \"Source Sans 3\"; }\n",
        "The symbol 漢 and שלום are missing.\n",
    ));
    let error = only(&compiled);
    assert!(error.is_error());
    assert_eq!(
        error.message,
        "no font of the `font` chain has glyphs for `漢` (U+6F22), `ש` (U+05E9), \
         `ל` (U+05DC), `ו` (U+05D5), `ם` (U+05DD)"
    );
    assert_eq!(
        error.hints[0],
        "the fonts tried are `SourceSerif4-Regular`, `SourceSans3-Regular`"
    );
}

#[test]
fn an_unknown_family_is_a_warning() {
    let compiled = compile(&world(
        "doc { font: \"Source Serif\", \"New Computer Modern\"; }\n",
        "Text.\n",
    ));
    let warning = only(&compiled);
    assert_eq!(warning.severity, Severity::Warning);
    assert_eq!(
        warning.message,
        "the font family `Source Serif` is not available"
    );
    assert_eq!(warning.hints[0], "did you mean `Source Serif 4`?");
}

/// A copy of a font file whose OS/2 `fsType` forbids embedding.
fn restricted(mut data: Vec<u8>) -> Vec<u8> {
    let tables = usize::from(u16::from_be_bytes([data[4], data[5]]));
    let os2 = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&at| &data[at..at + 4] == b"OS/2")
        .unwrap();
    let offset = u32::from_be_bytes(data[os2 + 8..os2 + 12].try_into().unwrap()) as usize;
    data[offset + 8..offset + 10].copy_from_slice(&2u16.to_be_bytes());
    data
}

#[test]
fn a_font_that_forbids_embedding_is_an_error_in_a_pdf() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("fonts")).unwrap();
    fs::write(
        root.path().join("fonts").join(SERIF),
        restricted(bundled(SERIF)),
    )
    .unwrap();
    let manifest = format!("{MANIFEST}\n[variants.exam]\ntargets = [\"pdf\", \"docx\"]\n");
    let mut world = bare(&manifest, None, BuildMode::Build);
    world.load_fonts(root.path(), None).unwrap();

    let exam = folio::Name::new("exam").unwrap();
    let pdf = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    assert_eq!(
        messages(&pdf),
        [(
            Severity::Error,
            "the licence of `SourceSerif4-Regular` forbids embedding it, so a PDF cannot use it"
                .into()
        )]
    );
    // DOCX embeds no fonts by default; its build only says that it has no
    // writer yet.
    let docx = folio::compile(&world, Target::Docx, Some(&exam)).unwrap();
    assert_eq!(
        messages(&docx),
        [(
            Severity::Warning,
            "DOCX output is not implemented yet: this build checks the document but writes \
             no file"
                .into()
        )]
    );
}

#[test]
fn a_project_file_that_is_not_a_font_is_a_warning() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("fonts")).unwrap();
    fs::write(root.path().join("fonts/broken.ttf"), b"not a font").unwrap();
    let mut world = world("", "Text.\n");
    world.load_fonts(root.path(), None).unwrap();
    assert_eq!(
        messages(&compile(&world)),
        [(
            Severity::Warning,
            "the project font `fonts/broken.ttf` is not used: it is not a font file Folio can read"
                .into()
        )]
    );
}

#[test]
fn a_chain_with_no_available_family_is_an_error() {
    let compiled = compile(&world("doc { font: \"Nonexistent\"; }\n", "Text.\n"));
    let messages = messages(&compiled);
    assert_eq!(
        messages,
        [
            (
                Severity::Warning,
                "the font family `Nonexistent` is not available".into()
            ),
            (
                Severity::Error,
                "no font of the `font` chain is available: `Nonexistent`".into()
            ),
        ]
    );
}

#[test]
fn leading_spaces_join_the_first_run() {
    let world = world("", "Text.\n");
    let fonts = folio::fonts(&world).unwrap();
    let serif = fonts.select("Source Serif 4", FontVariant::NORMAL).unwrap();
    let modern = fonts
        .select("New Computer Modern", FontVariant::NORMAL)
        .unwrap();
    // Source Serif 4 lacks the Hebrew, so the run is split by character.
    let text = " \u{5e9} x";
    let runs: Vec<_> = fonts
        .resolve(&[serif, modern], text)
        .into_iter()
        .map(|r| (&text[r.range], r.face))
        .collect();
    assert_eq!(runs, [(" \u{5e9} ", Some(modern)), ("x", Some(serif))]);
}

/// A system font of a family no bundled font has, so that text asks for it
/// by name and is set in it.
fn local_serif(dir: &Path) -> SystemFont {
    let mut font = system_font(dir, SERIF, "SourceSerif4-Regular");
    font.key.families = EcoVec::from(["Local Serif".into()]);
    font
}

/// Text that no walk of the document's text nodes sees: the text of `raw`,
/// a link's destination shown as text, and the quotation marks the text
/// stage writes. Each is set in a font of its own style.
const UNSEEN: [(&str, &str, &str); 3] = [
    (
        "raw",
        "raw { font: \"{font}\"; }\n",
        "Code `{text}` here.\n",
    ),
    (
        "link",
        "link { font: \"{font}\"; }\n",
        "See https://example.com/{text} now.\n",
    ),
    (
        "quotation marks",
        "doc { font: \"{font}\"; }\nstrong { font: \"Source Serif 4\"; }\n",
        "\"*Hi*\"\n",
    ),
];

#[test]
fn checks_the_fonts_of_raw_text_links_and_quotation_marks() {
    let dir = tempfile::tempdir().unwrap();
    let system = local_serif(dir.path());
    let message = "`SourceSerif4-Regular` is a system font, so the output depends on this machine";
    for (what, style, text) in UNSEEN {
        let style = style.replace("{font}", "Local Serif");
        let text = text.replace("{text}", "x");
        for (mode, severity) in [
            (BuildMode::Watch, None),
            (BuildMode::Build, Some(Severity::Warning)),
            (BuildMode::Ci, Some(Severity::Error)),
        ] {
            let mut world = World::builder().mode(mode).build(MANIFEST, None).unwrap();
            world.set_source("styles/main.fsty", &style).unwrap();
            world.set_source("main.fol", &text).unwrap();
            world.set_system_fonts([system.clone()]);
            let expected: Vec<_> = severity
                .map(|s| (s, message.to_string()))
                .into_iter()
                .collect();
            assert_eq!(messages(&compile(&world)), expected, "{what}, {mode:?}");
        }
    }

    // A glyph that no font of the chain has.
    for (what, style, text) in &UNSEEN[..2] {
        let style = style.replace("{font}", "Source Serif 4");
        let text = text.replace("{text}", "漢");
        let compiled = compile(&world(&style, &text));
        assert_eq!(
            messages(&compiled),
            [(
                Severity::Error,
                "no font of the `font` chain has a glyph for `漢` (U+6F22)".into()
            )],
            "{what}"
        );
    }
    // Every bundled font has the quotation marks, so the marks are set in
    // a chain that has no font at all, while the words they quote have one.
    let (_, style, text) = UNSEEN[2];
    let compiled = compile(&world(&style.replace("{font}", "Nonexistent"), text));
    assert_eq!(
        messages(&compiled),
        [
            (
                Severity::Warning,
                "the font family `Nonexistent` is not available".into()
            ),
            (
                Severity::Error,
                "no font of the `font` chain is available: `Nonexistent`".into()
            ),
        ]
    );
}
