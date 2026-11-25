//! Loading a project from disk with `folio::loader`, and keeping a world in
//! step with the disk as file-system events name paths (SPEC §12.2).

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use folio::loader::{LoadError, Loader};
use folio::{Event, FileKind, Target, World};

const MANIFEST: &str =
    "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A project directory with a manifest and the given files, and a world
/// loaded from it whose executed queries are logged.
fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, Loader, World, Arc<Mutex<Vec<String>>>) {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "folio.toml", MANIFEST);
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let builder = World::builder().on_event(move |event| {
        if let Event::Execute { query } = event {
            sink.lock().unwrap().push(query.to_string());
        }
    });
    let mut loader = Loader::new(dir.path()).unwrap();
    let mut world = loader.world(builder).unwrap();
    let mut warnings = Vec::new();
    loader.load(&mut world, &mut warnings).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    (dir, loader, world, log)
}

fn changed(update: &folio::loader::Update) -> Vec<&str> {
    update.changed.iter().map(String::as_str).collect()
}

/// A file read with `json(..)` is listed with the build's files, and an
/// edit to it on disk rebuilds the document.
#[test]
fn a_data_file_is_listed_and_an_edit_rebuilds() {
    let (dir, mut loader, mut world, log) = project(&[
        (
            "main.fol",
            "#let bank = json(\"data/bank.json\")\n= Forces\n\n#bank.title\n",
        ),
        ("data/bank.json", "{\"title\": \"A ball is thrown.\"}"),
        ("data/unused.csv", "x,y\n"),
    ]);
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let files: Vec<_> = compiled
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.kind))
        .collect();
    assert_eq!(
        files,
        [
            ("main.fol", FileKind::Content),
            ("data/bank.json", FileKind::Data)
        ]
    );

    // An event for a file the world already has as it is changes nothing.
    let update = loader
        .update(
            &mut world,
            [dir.path().join("data/bank.json")],
            &mut Vec::new(),
        )
        .unwrap();
    assert!(update.changed.is_empty());

    write(
        dir.path(),
        "data/bank.json",
        "{\"title\": \"A ball falls.\"}",
    );
    let update = loader
        .update(
            &mut world,
            [dir.path().join("data/bank.json")],
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(changed(&update), ["data/bank.json"]);
    // Every changed path is one the build read, so it rebuilds.
    assert!(
        compiled
            .files
            .iter()
            .any(|f| f.path.as_str() == "data/bank.json")
    );

    log.lock().unwrap().clear();
    let again = folio::compile(&world, Target::Pdf, None).unwrap();
    assert_ne!(again.output, compiled.output);
    let ran = log.lock().unwrap().clone();
    assert!(ran.iter().any(|q| q == "eval_file"), "{ran:?}");
    let text = folio::text(&world, Target::Pdf, None).unwrap().unwrap();
    let words: String = text
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().to_owned())
        .collect();
    assert!(words.contains("A ball falls."), "{words}");
}

/// A file named but missing is listed too, so that creating it rebuilds.
#[test]
fn a_missing_file_is_listed() {
    let (dir, mut loader, mut world, _) = project(&[(
        "main.fol",
        "#include \"q1.fol\"\n#image(\"figure.png\")\n#read(\"notes.txt\")\n",
    )]);
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(compiled.has_errors());
    let files: Vec<_> = compiled
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.kind))
        .collect();
    assert_eq!(
        files,
        [
            ("main.fol", FileKind::Content),
            ("q1.fol", FileKind::Content),
            ("figure.png", FileKind::Image),
            ("notes.txt", FileKind::Data)
        ]
    );

    write(dir.path(), "notes.txt", "Notes.");
    let update = loader
        .update(&mut world, [dir.path().join("notes.txt")], &mut Vec::new())
        .unwrap();
    assert_eq!(changed(&update), ["notes.txt"]);
    assert_eq!(&*world.file("notes.txt").unwrap(), b"Notes.");
}

/// Events for files and directories that are added, removed and renamed.
#[test]
fn updates_follow_the_disk() {
    let (dir, mut loader, mut world, _) = project(&[
        ("main.fol", "= Forces\n"),
        ("questions/q1.fol", "A question.\n"),
        ("questions/q2.fol", "Another.\n"),
        ("data/points.csv", "x,y\n"),
    ]);
    let root = dir.path();
    let sources = |world: &World| -> Vec<String> {
        world
            .source_paths()
            .chain(world.file_paths())
            .map(|p| p.as_str().to_owned())
            .collect()
    };

    // A directory removed takes its files with it.
    fs::remove_dir_all(root.join("questions")).unwrap();
    let update = loader
        .update(&mut world, [root.join("questions")], &mut Vec::new())
        .unwrap();
    assert_eq!(changed(&update), ["questions/q1.fol", "questions/q2.fol"]);
    assert_eq!(sources(&world), ["main.fol", "data/points.csv"]);

    // A directory renamed: the event names both paths.
    fs::rename(root.join("data"), root.join("tables")).unwrap();
    let update = loader
        .update(
            &mut world,
            [root.join("data"), root.join("tables")],
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(changed(&update), ["data/points.csv", "tables/points.csv"]);
    assert_eq!(sources(&world), ["main.fol", "tables/points.csv"]);

    // A directory created with files in it, named by a relative path.
    write(root, "new/a.fol", "A.\n");
    write(root, "new/.hidden.fol", "Hidden.\n");
    let update = loader.update(&mut world, ["new"], &mut Vec::new()).unwrap();
    assert_eq!(changed(&update), ["new/a.fol"]);

    // Hidden files, paths outside the project and the font directory's
    // files that are not fonts change nothing.
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "x.fol", "");
    write(root, ".git/config", "");
    let update = loader
        .update(
            &mut world,
            [
                root.join(".git/config"),
                outside.path().join("x.fol"),
                root.join("../elsewhere.fol"),
            ],
            &mut Vec::new(),
        )
        .unwrap();
    assert!(update.changed.is_empty());

    // An event for the project directory reads it all again.
    fs::remove_file(root.join("new/a.fol")).unwrap();
    write(root, "main.fol", "= Energy\n");
    let update = loader.update(&mut world, [root], &mut Vec::new()).unwrap();
    assert_eq!(changed(&update), ["main.fol", "new/a.fol"]);
    assert_eq!(world.source("main.fol").unwrap().text(), "= Energy\n");

    // A source that is no longer UTF-8 is removed, with a warning.
    fs::write(root.join("main.fol"), [0xff, 0xfe]).unwrap();
    let mut warnings = Vec::new();
    let update = loader
        .update(&mut world, [root.join("main.fol")], &mut warnings)
        .unwrap();
    assert_eq!(changed(&update), ["main.fol"]);
    assert!(world.source("main.fol").is_none());
    assert_eq!(warnings.len(), 1);
}

/// A new manifest replaces the world's, and its outputs are no longer
/// read; one with errors is reported with its text, and the world keeps
/// the old one.
#[test]
fn updates_the_manifest() {
    let (dir, mut loader, mut world, _) =
        project(&[("main.fol", "= Forces\n"), ("out/main-exam.pdf", "%PDF-")]);
    let root = dir.path();
    assert!(world.file("out/main-exam.pdf").is_some());

    write(
        root,
        "folio.toml",
        &format!("{MANIFEST}[variants.exam]\ntargets = [\"pdf\"]\n"),
    );
    let update = loader
        .update(&mut world, [root.join("folio.toml")], &mut Vec::new())
        .unwrap();
    assert_eq!(world.manifest().variants.len(), 1);
    assert_eq!(changed(&update), ["folio.toml", "out/main-exam.pdf"]);
    assert!(world.file("out/main-exam.pdf").is_none());

    // An event for the project directory reads the fonts again too.
    let font = folio::bundled_fonts()[0].clone();
    fs::create_dir(root.join("fonts")).unwrap();
    fs::write(root.join("fonts/serif.ttf"), font.data().as_ref()).unwrap();
    let update = loader
        .update(
            &mut world,
            [root.to_path_buf(), root.join("fonts/serif.ttf")],
            &mut Vec::new(),
        )
        .unwrap();
    // The root stands for the font directory.
    assert_eq!(changed(&update), ["fonts"]);
    assert_eq!(world.font_files().len(), 1);

    // An event for the project directory reads the manifest again too.
    write(root, "folio.toml", MANIFEST);
    let update = loader.update(&mut world, [root], &mut Vec::new()).unwrap();
    assert_eq!(changed(&update), ["folio.toml", "out/main-exam.pdf"]);
    assert!(world.manifest().variants.is_empty());
    assert!(world.file("out/main-exam.pdf").is_some());

    write(root, "folio.toml", "[project]\n");
    let update = loader
        .update(&mut world, [root.join("folio.toml")], &mut Vec::new())
        .unwrap();
    assert!(update.changed.is_empty());
    let errors = update.errors.unwrap();
    assert!(!errors.diagnostics.is_empty());
    let shown = folio::diag::render(&errors.files, &errors.diagnostics, false);
    assert!(shown.contains("folio.toml"), "{shown}");
    assert!(world.manifest().variants.is_empty());
}

/// A change to a file that a link inside the project leads to updates the
/// link's path too.
#[cfg(unix)]
#[test]
fn updates_paths_through_links() {
    use std::os::unix::fs::symlink;

    let (dir, mut loader, mut world, _) = project(&[
        ("main.fol", "= Forces\n"),
        ("shared/questions/q1.fol", "A question.\n"),
    ]);
    let root = dir.path();
    symlink(root.join("shared/questions"), root.join("questions")).unwrap();
    let update = loader
        .update(&mut world, [root.join("questions")], &mut Vec::new())
        .unwrap();
    assert_eq!(changed(&update), ["questions/q1.fol"]);

    write(root, "shared/questions/q1.fol", "Changed.\n");
    let update = loader
        .update(
            &mut world,
            [root.join("shared/questions/q1.fol")],
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(
        changed(&update),
        ["questions/q1.fol", "shared/questions/q1.fol"]
    );
    assert_eq!(
        world.source("questions/q1.fol").unwrap().text(),
        "Changed.\n"
    );

    // A directory replaced by a link to a directory it is in is no longer
    // read, and its files leave the world.
    fs::remove_dir_all(root.join("shared/questions")).unwrap();
    symlink(root.join("shared"), root.join("shared/questions")).unwrap();
    let update = loader
        .update(&mut world, [root.join("shared/questions")], &mut Vec::new())
        .unwrap();
    assert_eq!(
        changed(&update),
        ["questions/q1.fol", "shared/questions/q1.fol"]
    );
    assert!(world.source("shared/questions/q1.fol").is_none());

    // A link to a directory the link is in is not followed.
    symlink(root, root.join("shared/up")).unwrap();
    let update = loader
        .update(
            &mut world,
            [root.join("shared/up"), root.join("shared/up/main.fol")],
            &mut Vec::new(),
        )
        .unwrap();
    assert!(update.changed.is_empty(), "{:?}", update.changed);
}

#[test]
fn reports_a_missing_project() {
    let dir = tempfile::tempdir().unwrap();
    let error = folio::loader::find_root(Some(dir.path())).unwrap_err();
    assert!(matches!(error, LoadError::NoManifest { above: false, .. }));
    assert!(error.to_string().ends_with("holds no folio.toml"));
}
