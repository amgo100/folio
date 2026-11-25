//! PDF output (SPEC §11.1) through the whole pipeline: the documents of
//! the test corpus in `tests/pdf/corpus` are tagged PDF/UA-1, with the
//! structure of their content, their metadata and their bookmarks, and
//! compile to the same bytes every time.
//!
//! With `FOLIO_PDF_OUT` set to a directory, the test also writes each
//! document's PDF there, with the text a reader should extract from it, for
//! `tests/pdf/corpus.py` to check with veraPDF and `pdftotext`.

use std::fs;
use std::path::{Path, PathBuf};

use folio::{Diagnostic, Severity, Target, World};

/// The corpus directory.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/pdf/corpus")
}

/// A world with a corpus project: its manifest and every source file.
fn project(dir: &Path) -> World {
    let manifest = fs::read_to_string(dir.join("folio.toml")).unwrap();
    let mut world = World::new(&manifest).unwrap();
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
                let text = fs::read_to_string(&path).unwrap();
                world
                    .set_source(&relative.replace('\\', "/"), &text)
                    .unwrap();
            }
        }
    }
    world
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

fn build(world: &World) -> (Vec<u8>, Vec<Diagnostic>) {
    let compiled = folio::compile(world, Target::Pdf, None).unwrap();
    let pdf = compiled.output.clone().expect("a PDF");
    (pdf.to_vec(), compiled.diagnostics.to_vec())
}

/// The text of a build's paragraphs as a reader should extract it: in
/// order, one paragraph per line, without the objects that layout does
/// not draw yet.
fn expected_text(world: &World) -> String {
    let text = folio::text(world, Target::Pdf, None).unwrap().unwrap();
    let mut out = String::new();
    for paragraph in text.paragraphs.iter() {
        let line: String = paragraph
            .shaped
            .text()
            .chars()
            .filter(|&c| c != '\u{FFFC}')
            .map(|c| if c == '\u{2028}' { ' ' } else { c })
            .collect();
        out.push_str(line.trim());
        out.push('\n');
    }
    out
}

/// Every corpus document is a PDF/UA-1 file whose structure follows its
/// content, and compiles to the same bytes in a new world.
#[test]
fn corpus_is_pdf_ua_1() {
    let out = std::env::var_os("FOLIO_PDF_OUT").map(PathBuf::from);
    let projects = projects();
    assert!(projects.len() >= 3, "the corpus has its documents");
    for (name, dir) in projects {
        let world = project(&dir);
        let (pdf, diagnostics) = build(&world);
        let problems: Vec<&Diagnostic> = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error || d.message.contains("PDF"))
            .collect();
        assert!(problems.is_empty(), "{name}: {problems:#?}");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("<pdfuaid:part>1</pdfuaid:part>"), "{name}");
        for element in ["/S/Document", "/S/Title", "/S/Sect", "/S/H1", "/S/P"] {
            assert!(text.contains(element), "{name} has no {element}");
        }
        assert!(text.contains("/Type/Outlines"), "{name}");
        assert_eq!(build(&project(&dir)).0, pdf, "{name} is deterministic");
        if let Some(out) = &out {
            fs::create_dir_all(out).unwrap();
            fs::write(out.join(format!("{name}.pdf")), &pdf).unwrap();
            fs::write(out.join(format!("{name}.txt")), expected_text(&world)).unwrap();
        }
    }
}

/// The notes use every structure role that the built-in elements map to.
#[test]
fn notes_have_every_role() {
    let (pdf, _) = build(&project(&corpus().join("notes")));
    let text = String::from_utf8_lossy(&pdf);
    for element in [
        "/S/H2",
        "/S/H3",
        "/S/L",
        "/S/LI",
        "/S/Lbl",
        "/S/LBody",
        "/S/Note",
        "/S/Caption",
        "/S/Code",
        "/S/Div",
    ] {
        assert!(text.contains(element), "the notes have no {element}");
    }
    assert!(text.contains("/Title(Waves and Oscillations)"));
    assert!(text.contains("/Subject(A-level Physics, Year 12 notes)"));
    assert!(text.contains("/Lang(en-GB)"));
    assert!(
        text.contains("/Title(Simple harmonic motion)"),
        "a bookmark"
    );
}

/// The title in the metadata is the title as shown, with the language's
/// quotation marks.
#[test]
fn metadata_title_as_shown() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let mut world = World::new(manifest).unwrap();
    let main = "#doc(title: [The \"ideal\" gas])\n\nA ball falls.\n";
    world.set_source("main.fol", main).unwrap();
    let (pdf, diagnostics) = build(&world);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">The \u{201C}ideal\u{201D} gas</rdf:li>"), "{text}");
}

/// A document without a title is written tagged, but cannot claim
/// PDF/UA-1, which requires one; that is no warning.
#[test]
fn untitled_document() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let mut world = World::new(manifest).unwrap();
    world
        .set_source("main.fol", "= Forces\n\nA ball falls.\n")
        .unwrap();
    let (pdf, diagnostics) = build(&world);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(!text.contains("pdfuaid"));
    assert!(text.contains("/S/H1"));
}

/// A document with a title that cannot meet PDF/UA-1 is written tagged,
/// with a warning that says why, at the heading.
#[test]
fn heading_without_text() {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let mut world = World::new(manifest).unwrap();
    // A no-break space draws a glyph but gives the heading no text.
    let main = "#doc(title: [Forces])\n\n= ~\n\nA ball falls.\n";
    world.set_source("main.fol", main).unwrap();
    let (pdf, diagnostics) = build(&world);
    let warning = diagnostics
        .iter()
        .find(|d| d.message.contains("PDF/UA-1"))
        .unwrap_or_else(|| panic!("a warning: {diagnostics:#?}"));
    assert_eq!(warning.severity, Severity::Warning);
    assert!(warning.message.contains("this heading has no text"));
    let rendered = folio::diag::render(&world, std::slice::from_ref(warning), false);
    assert!(rendered.contains("main.fol:3:1\n"), "{rendered}");
    let text = String::from_utf8_lossy(&pdf);
    assert!(!text.contains("pdfuaid"));
}
