//! Shuffled versions (SPEC §6.4): `shuffle` puts items in an order fixed by
//! its seed while the document is evaluated, so each version is numbered in
//! the order it is printed, and its mark scheme in the same order.

use folio::{Fragment, FragmentItem, Name, Severity, Target, World};

const PLAIN: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                     compiler = \"0.0.0\"\nlang = \"en\"\n";

const GROUPED: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                       compiler = \"0.0.0\"\nlang = \"en\"\nstyles = [\"main.fsty\"]\n\n\
                       [variants.exam]\ntargets = [\"pdf\"]\n\n\
                       [variants.markscheme]\ntargets = [\"pdf\"]\n\n\
                       [numbering-groups.paper]\nvariants = [\"exam\", \"markscheme\"]\n\
                       shared-counters = [\"heading\"]\n";

fn world(manifest: &str, main: &str) -> World {
    let mut world = World::new(manifest).unwrap();
    world
        .set_source("main.fsty", "heading { numbering: \"1.\"; }\n")
        .unwrap();
    world.set_source("main.fol", main).unwrap();
    world
}

/// The plain text of the evaluated document, its spaces collapsed.
fn text(world: &World, variant: Option<&str>) -> String {
    let variant = variant.map(|v| Name::new(v).unwrap());
    folio::evaluate(world, variant.as_ref())
        .unwrap()
        .expect("the document evaluates")
        .plain_text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn errors(world: &World) -> Vec<String> {
    folio::compile(world, Target::Pdf, None)
        .unwrap()
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.to_string())
        .collect()
}

/// The text of every paragraph a variant sets.
fn paragraphs(world: &World, variant: &str) -> Vec<String> {
    let variant = Name::new(variant).unwrap();
    folio::text(world, Target::Pdf, Some(&variant))
        .unwrap()
        .unwrap()
        .paragraphs
        .iter()
        .map(|p| p.shaped.text().replace('\u{a0}', " "))
        .collect()
}

#[test]
fn a_seed_fixes_the_order() {
    let w = world(
        PLAIN,
        "#shuffle(range(6), seed: \"A\").map(str).join(\",\") \
         #shuffle(range(6), seed: \"B\").map(str).join(\",\") \
         #shuffle(range(6), seed: 7).map(str).join(\",\") \
         #(shuffle(range(6), seed: 7) == shuffle(range(6), seed: \"7\")) \
         #shuffle((\"x\", \"y\", \"z\"), seed: none).join()",
    );
    // The orders the language fixes (SPEC §6.4); the same as
    // `permutation`'s unit test in folio-core.
    assert_eq!(
        text(&w, None),
        "2,5,1,3,4,0 1,0,5,2,4,3 5,0,3,1,2,4 true xyz"
    );
    assert!(errors(&w).is_empty(), "{:?}", errors(&w));
}

#[test]
fn bad_seeds_are_errors() {
    let w = world(
        PLAIN,
        "#shuffle((1, 2))\n\n#shuffle((1, 2), seed: 1.5)\n\n#shuffle(3, seed: \"A\")\n",
    );
    assert_eq!(
        errors(&w),
        [
            "missing argument: seed",
            "expected a string, an integer or none as the seed, found a float",
            "expected an array or content to shuffle, found an int",
        ]
    );
}

/// `#shuffle(seed: ..)[..]` reorders the elements of its body and leaves
/// the text between them in place; `version` is `none` without versions,
/// so `seed: version` keeps the order.
#[test]
fn a_body_reorders_its_elements() {
    let main = "#shuffle(seed: \"A\")[\n= One\n\n= Two\n\nBetween.\n\n= Three\n\n= Four\n]\n\n\
                #repr(version)\n\n\
                #shuffle(seed: version)[\n= Five\n\n= Six\n]\n";
    let w = world(PLAIN, main);
    assert_eq!(text(&w, None), "Four Two Between. Three One none Five Six");
    assert!(errors(&w).is_empty(), "{:?}", errors(&w));
}

/// SPEC §6.4: questions shuffled with one seed are numbered in printed
/// order, and the exam and its mark scheme, which share the seed, agree.
#[test]
fn a_shuffled_paper_is_numbered_in_printed_order() {
    let main = "#let bank = (\"Forces\", \"Waves\", \"Fields\", \"Optics\")\n\
                #for q in shuffle(bank, seed: \"B\") [\n\
                = #q\n\n\
                #if variant == \"markscheme\" [Answer to #q.]\n\n\
                ]\n";
    let w = world(GROUPED, main);
    let exam = paragraphs(&w, "exam");
    let scheme = paragraphs(&w, "markscheme");
    assert_eq!(exam, ["1. Forces", "2. Waves", "3. Optics", "4. Fields"]);
    assert_eq!(
        scheme,
        [
            "1. Forces",
            "Answer to Forces.",
            "2. Waves",
            "Answer to Waves.",
            "3. Optics",
            "Answer to Optics.",
            "4. Fields",
            "Answer to Fields.",
        ]
    );
}

/// A paper declared in two versions, each shuffled by its version.
const VERSIONED: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                         compiler = \"0.0.0\"\nlang = \"en\"\nstyles = [\"main.fsty\"]\n\
                         versions = [\"A\", \"B\"]\n\n\
                         [variants.exam]\ntargets = [\"pdf\"]\n\n\
                         [variants.markscheme]\ntargets = [\"pdf\"]\n\n\
                         [numbering-groups.paper]\nvariants = [\"exam\", \"markscheme\"]\n\
                         shared-counters = [\"heading\"]\n";

const BANK: &str = "#let bank = (\"Forces\", \"Waves\", \"Fields\", \"Optics\")\n\
                    #for q in shuffle(bank, seed: version) [\n\
                    = #q\n\n\
                    #if variant == \"markscheme\" [Answer to #q.]\n\n\
                    ]\n";

/// The text of each line a build sets, in order.
fn lines(world: &World, variant: &str, version: Option<&str>) -> Vec<String> {
    fn walk(fragment: &Fragment, out: &mut Vec<String>) {
        for (_, item) in &fragment.items {
            match item {
                FragmentItem::Group(inner) => walk(inner, out),
                FragmentItem::Text(run) => out.push(run.text.as_str().replace('\u{a0}', " ")),
                _ => {}
            }
        }
    }
    let variant = Name::new(variant).unwrap();
    let version = version.map(|v| Name::new(v).unwrap());
    let pages = folio::pages_versioned(world, Target::Pdf, Some(&variant), version.as_ref())
        .unwrap()
        .expect("the build is laid out");
    let mut out = Vec::new();
    for page in &pages.pages {
        walk(&page.fragment, &mut out);
    }
    out
}

/// SPEC §6.4: each version is numbered in its own printed order, and its
/// mark scheme, a numbering group of its own with its exam, agrees.
#[test]
fn each_version_numbers_its_own_order() {
    let w = world(VERSIONED, BANK);
    // The orders that seeds "A" and "B" give four items.
    let a = ["1. Optics", "2. Waves", "3. Fields", "4. Forces"];
    let b = ["1. Forces", "2. Waves", "3. Optics", "4. Fields"];
    assert_eq!(lines(&w, "exam", Some("A")), a);
    assert_eq!(lines(&w, "exam", Some("B")), b);
    let scheme: Vec<String> = lines(&w, "markscheme", Some("B"))
        .into_iter()
        .filter(|l| !l.starts_with("Answer"))
        .collect();
    assert_eq!(scheme, b);
    assert_eq!(
        lines(&w, "markscheme", Some("A"))[..2],
        ["1. Optics", "Answer to Optics."]
    );
    // Without a version, as the editor builds it, the order is the source's.
    assert_eq!(
        lines(&w, "exam", None),
        ["1. Forces", "2. Waves", "3. Fields", "4. Optics"]
    );
    for version in ["A", "B"] {
        for variant in ["exam", "markscheme"] {
            let name = Name::new(variant).unwrap();
            let version = Name::new(version).unwrap();
            let compiled =
                folio::compile_versioned(&w, Target::Pdf, Some(&name), Some(&version)).unwrap();
            assert!(
                !compiled.has_errors(),
                "{variant} {version}: {:?}",
                compiled.diagnostics
            );
            // Rebuilding in a fresh world gives the same bytes.
            let fresh = world(VERSIONED, BANK);
            let again =
                folio::compile_versioned(&fresh, Target::Pdf, Some(&name), Some(&version)).unwrap();
            assert_eq!(compiled.output, again.output, "{variant} {version}");
        }
    }
}

/// A version the manifest does not declare is an error.
#[test]
fn an_undeclared_version_is_an_error() {
    let w = world(VERSIONED, BANK);
    let exam = Name::new("exam").unwrap();
    let c = Name::new("C").unwrap();
    let compiled = folio::compile_versioned(&w, Target::Pdf, Some(&exam), Some(&c)).unwrap();
    let errors: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| (d.message.to_string(), d.hints.clone()))
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].0, "no version `C` is declared");
    assert_eq!(errors[0].1[0], "the manifest declares `A`, `B`");
}
