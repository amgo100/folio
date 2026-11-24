//! Golden tests (SPEC §14.2) for shaping, bidi, itemisation and break
//! opportunities, with the bundled fonts.
//!
//! Each case writes the shaped paragraph ([`Shaped::repr`]) and the visual
//! order of its runs as one line, and compares the result with
//! `tests/golden/<case>.txt`. Run with `FOLIO_BLESS=1` to write the files
//! after a change that is meant to alter them, and review the diff.

use std::fs;
use std::ops::Range;
use std::path::PathBuf;

use ecow::EcoVec;
use folio_fonts::{FontOrigin, bundled};
use folio_text::{
    BreakKind, Cache, Feature, Lang, Paragraph, Segment, SegmentKind, Shaped, TextFont, shape,
};

/// A bundled font, by file name.
fn font(cache: &Cache, file: &str) -> TextFont {
    let file = bundled()
        .into_iter()
        .find(|f| f.origin() == &FontOrigin::Bundled(file.into()))
        .unwrap_or_else(|| panic!("no bundled font {file}"));
    cache.font(file.data(), 0).unwrap()
}

/// A segment of text in a font.
fn seg(range: Range<usize>, font: Option<&TextFont>, lang: &str, hyphenate: bool) -> Segment {
    Segment {
        range,
        kind: SegmentKind::Text {
            font: font.cloned(),
            features: EcoVec::new(),
        },
        lang: Lang::new(lang),
        hyphenate,
    }
}

/// A paragraph of one segment.
fn simple(t: &str, font: Option<&TextFont>, lang: &str, hyphenate: bool) -> Paragraph {
    Paragraph {
        text: t.into(),
        segments: EcoVec::from([seg(0..t.len(), font, lang, hyphenate)]),
    }
}

/// The shaped paragraph as the golden file holds it: its description, then
/// the visual order of the runs of the paragraph set as one line.
fn describe(shaped: &Shaped) -> String {
    let mut out = shaped.repr();
    let runs: Vec<String> = shaped
        .visual_runs(0..shaped.text().len())
        .into_iter()
        .map(|(range, level)| format!("{:?}@{level}", &shaped.text()[range]))
        .collect();
    out.push_str(&format!("visual {}\n", runs.join(" ")));
    out
}

fn check(name: &str, paragraph: &Paragraph) -> Shaped {
    let shaped = shape(paragraph);
    let actual = describe(&shaped);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("FOLIO_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &actual).unwrap();
    } else {
        let expected = fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("{} is missing: run with FOLIO_BLESS=1", path.display()));
        assert!(
            expected == actual,
            "{name} differs from {}; run with FOLIO_BLESS=1 to update it and review the diff\n\
             --- expected\n{expected}\n--- actual\n{actual}",
            path.display()
        );
    }
    shaped
}

#[test]
fn mixed_script() {
    let cache = Cache::new();
    let ncm = font(&cache, "NewCM10-Regular.otf");
    let shaped = check(
        "mixed-script",
        &simple(
            "Office (Физика, Φυσική, פיזיקה) fit.",
            Some(&ncm),
            "en",
            false,
        ),
    );
    let scripts: Vec<&str> = shaped.items.iter().map(|i| i.script.short_name()).collect();
    assert_eq!(scripts, ["Latn", "Cyrl", "Grek", "Hebr", "Latn"]);
}

#[test]
fn rtl_paragraph() {
    let cache = Cache::new();
    let ncm = font(&cache, "NewCM10-Regular.otf");
    // Hebrew first, so the paragraph runs right to left, with English and
    // a number embedded.
    let shaped = check(
        "rtl-paragraph",
        &simple("שלום, world 2026! עולם", Some(&ncm), "he", false),
    );
    assert_eq!(shaped.level, 1);
}

#[test]
fn rtl_in_ltr() {
    let cache = Cache::new();
    let ncm = font(&cache, "NewCM10-Regular.otf");
    let shaped = check(
        "rtl-in-ltr",
        &simple("The word שלום עולם means peace.", Some(&ncm), "en", false),
    );
    assert_eq!(shaped.level, 0);
}

#[test]
fn neutral_text_follows_the_language() {
    let cache = Cache::new();
    let ncm = font(&cache, "NewCM10-Regular.otf");
    assert_eq!(shape(&simple("(1) 2026", Some(&ncm), "he", false)).level, 1);
    assert_eq!(shape(&simple("(1) 2026", Some(&ncm), "pl", false)).level, 0);
}

const POLISH: &str = "Wczoraj w Warszawie i w Krakowie odbył się biało-czerwony \
     marsz. Uczniowie z klasy a nauczyciel o tym wiedział, u nas zaś nikt. \
     O wszystkim dowiedzieliśmy się z gazety.";

#[test]
fn polish() {
    let cache = Cache::new();
    let serif = font(&cache, "SourceSerif4-Regular.otf");
    check("polish", &simple(POLISH, Some(&serif), "pl", true));
}

/// SPEC §7.2: in Polish, a word of one letter never ends a line, so no
/// break opportunity follows it.
#[test]
fn polish_single_letter_words_never_end_a_line() {
    let cache = Cache::new();
    let serif = font(&cache, "SourceSerif4-Regular.otf");
    let shaped = shape(&simple(POLISH, Some(&serif), "pl", true));
    let text = shaped.text();
    for b in shaped
        .breaks
        .iter()
        .filter(|b| b.kind != BreakKind::Mandatory)
    {
        let line = text[..b.offset].trim_end();
        let last = line.rsplit(|c: char| !c.is_alphanumeric()).next().unwrap();
        assert!(
            !matches!(
                last,
                "a" | "i" | "o" | "u" | "w" | "z" | "A" | "I" | "O" | "U" | "W" | "Z"
            ),
            "a line may end after `{last}`: {:?}",
            &text[..b.offset]
        );
    }
    // The rule holds across segments: `w` in one segment, its next word in
    // another (as in `w *Warszawie*`).
    let t = "Jestem w Warszawie";
    let split = "Jestem w ".len();
    let paragraph = Paragraph {
        text: t.into(),
        segments: EcoVec::from([
            seg(0..split, Some(&serif), "pl", false),
            seg(split..t.len(), Some(&serif), "pl", false),
        ]),
    };
    let offsets: Vec<usize> = shape(&paragraph).breaks.iter().map(|b| b.offset).collect();
    assert_eq!(offsets, ["Jestem ".len(), t.len()]);
}

#[test]
fn scripts_without_spaces() {
    // No bundled font has these scripts: the items are missing, but break
    // opportunities do not depend on fonts.
    check(
        "thai",
        &simple("ภาษาไทยเขียนติดกันโดยไม่เว้นวรรค", None, "th", false),
    );
    check(
        "japanese",
        &simple(
            "日本語の文章は、単語の間に空白を入れない。",
            None,
            "ja",
            false,
        ),
    );
}

#[test]
fn features_and_segments() {
    let cache = Cache::new();
    let serif = font(&cache, "SourceSerif4-Regular.otf");
    let t = "office office";
    let mut off = seg(7..t.len(), Some(&serif), "en", false);
    off.kind = SegmentKind::Text {
        font: Some(serif.clone()),
        features: EcoVec::from([Feature::parse("-liga").unwrap()]),
    };
    let paragraph = Paragraph {
        text: t.into(),
        segments: EcoVec::from([seg(0..7, Some(&serif), "en", false), off]),
    };
    check("features", &paragraph);
}

#[test]
fn objects_and_forced_breaks() {
    let cache = Cache::new();
    let serif = font(&cache, "SourceSerif4-Regular.otf");
    let t = "See \u{FFFC} here.\u{2028}Next line";
    let obj = t.find('\u{FFFC}').unwrap();
    let paragraph = Paragraph {
        text: t.into(),
        segments: EcoVec::from([
            seg(0..obj, Some(&serif), "en", false),
            Segment {
                range: obj..obj + 3,
                kind: SegmentKind::Object,
                lang: Lang::new("en"),
                hyphenate: false,
            },
            seg(obj + 3..t.len(), Some(&serif), "en", false),
        ]),
    };
    check("objects-and-breaks", &paragraph);
}

#[test]
fn cache_memoises_and_shapes_in_parallel() {
    let cache = Cache::new();
    let serif = font(&cache, "SourceSerif4-Regular.otf");
    let paragraphs: Vec<Paragraph> = (0..64)
        .map(|i| {
            simple(
                &format!("Paragraph {} of a long text.", i % 16),
                Some(&serif),
                "en",
                true,
            )
        })
        .collect();
    let first = cache.shape_all(&paragraphs);
    assert_eq!(cache.len(), 16);
    // Equal paragraphs share one result, and match shaping without the
    // cache.
    assert!(std::sync::Arc::ptr_eq(&first[0], &first[16]));
    for (p, s) in paragraphs.iter().zip(&first) {
        assert_eq!(**s, shape(p));
    }
    let again = cache.shape_all(&paragraphs);
    assert!(std::sync::Arc::ptr_eq(&first[3], &again[3]));
    // A font handle is made once.
    assert_eq!(font(&cache, "SourceSerif4-Regular.otf"), serif);
    // Paragraphs no call asks for are forgotten.
    for _ in 0..64 {
        cache.shape_all(&[]);
    }
    assert!(cache.is_empty());
}
