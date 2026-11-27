//! The body of the `image` fuzz target (SPEC §14.2, §14.3, #333): an image
//! file of any bytes, read, written to PDF and rendered.
//!
//! The first byte of the input says how the figure is placed (bit 0: in a
//! titled document, so that a PDF figure is converted to vector paths
//! under PDF/UA rather than embedded); the rest is the file. It goes
//! through the image reader of every build (`folio_core::image::read_image`),
//! the PDF writer's (`folio_pdf::image::prepare`) and the renderer's
//! (`folio_render::image::decode`, for page images and SVG output), so a
//! file the reader accepts must also be drawn without a panic. A panic, an
//! abort or a run past libFuzzer's time or memory limit is a failure.
//!
//! It is in a module of its own until it is registered with the other
//! targets in [`crate::fuzz`].

use std::fs;

use folio::{Target, World};

use crate::golden;

/// The body of the `image` fuzz target.
pub fn image(data: &[u8]) {
    let Some((&flags, file)) = data.split_first() else {
        return;
    };
    let world = world(flags & 1 == 1, file);
    let Ok(compiled) = folio::compile(&world, Target::Pdf, None) else {
        return;
    };
    let _ = compiled.output;
    let Ok(Some(pages)) = folio::pages(&world, Target::Pdf, None) else {
        return;
    };
    let _ = golden::page_images(&world, &pages);
    let fonts = golden::FontTable::new(&world);
    let mut outlines = folio_render::Outlines::new();
    for list in golden::display_lists(&pages) {
        let _ = folio_render::svg(&list, &fonts, &mut outlines);
    }
}

/// A world with one image, `figure`, of the given bytes, on a small page.
fn world(title: bool, file: &[u8]) -> World {
    let manifest = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
                    lang = \"en-GB\"\nstyles = [\"main.fsty\"]\n";
    let mut world = World::new(manifest).expect("the manifest is valid");
    world
        .set_source(
            "main.fsty",
            "page { page-size: (60mm, 60mm); margin-inner: 5mm; margin-outer: 5mm; }\n",
        )
        .expect("a stylesheet path");
    let doc = if title {
        "#doc(title: [Figure])\n\n"
    } else {
        ""
    };
    world
        .set_source(
            "main.fol",
            &format!("{doc}#image(\"figure\", alt: \"a figure\", width: 2cm)\n"),
        )
        .expect("a source path");
    world
        .set_file("figure", file.to_vec())
        .expect("a file path");
    world
}

/// The seed inputs of the `image` target: every image of the corpus and
/// the hostile figures of #333, each placed both ways. Named for the
/// seed corpus.
pub fn seeds() -> Vec<(String, Vec<u8>)> {
    let root = crate::root();
    let mut files = Vec::new();
    for dir in [
        "tests/corpus/figures/assets",
        "crates/folio-core/tests/untrusted",
    ] {
        let mut paths: Vec<_> = fs::read_dir(root.join(dir))
            .map(|entries| entries.filter_map(|e| Some(e.ok()?.path())).collect())
            .unwrap_or_default();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".py") || name.ends_with(".md") {
                continue;
            }
            if let Ok(bytes) = fs::read(&path) {
                files.push((name.to_string(), bytes));
            }
        }
    }
    let mut out = Vec::new();
    for (name, bytes) in files {
        for (flag, how) in [(0u8, "embedded"), (1, "converted")] {
            let mut input = vec![flag];
            input.extend_from_slice(&bytes);
            out.push((format!("{name}-{how}"), input));
        }
    }
    out
}
