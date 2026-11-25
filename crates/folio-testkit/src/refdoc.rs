//! The reference document of SPEC §14.1 (100 A4 pages with 30 figures, 400
//! display equations and 20 tables) and smaller documents of the same
//! make-up, generated deterministically.
//!
//! The generator grows as features land. Until then, an item whose feature
//! does not exist is a stand-in of about its size made of what does:
//!
//! - display equations (math, M1): a short paragraph that states the
//!   equation in words;
//! - tables (M2): a term list, one term per row.
//!
//! Figures are real figures with a caption; their bodies are text until
//! graphics land (M4). When a feature lands, replace its stand-in in
//! `Generator::equation` or `Generator::table` and re-tune
//! [`CHARS_PER_PAGE`] so that the page count stays on target
//! (`tests/refdoc.rs` checks it).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::corpus::Project;

/// The make-up of a generated document.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct Spec {
    /// The number of A4 pages to fill.
    pub pages: usize,
    /// The number of figures.
    pub figures: usize,
    /// The number of display equations.
    pub equations: usize,
    /// The number of tables.
    pub tables: usize,
}

impl Spec {
    /// The reference document of SPEC §14.1.
    pub const REFERENCE: Self = Self {
        pages: 100,
        figures: 30,
        equations: 400,
        tables: 20,
    };

    /// The document of the M0 exit check (SPEC §15): ten pages, with the
    /// reference document's proportions.
    pub const M0_EXIT: Self = Self::pages(10);

    /// A document of a number of pages with the reference document's
    /// proportions of figures, equations and tables, rounded down.
    pub const fn pages(pages: usize) -> Self {
        let r = Self::REFERENCE;
        Self {
            pages,
            figures: r.figures * pages / r.pages,
            equations: r.equations * pages / r.pages,
            tables: r.tables * pages / r.pages,
        }
    }
}

/// About how many characters of running text, with the paragraph breaks
/// and headings, fill one page of the generated document's page set-up,
/// net of its figures, equations and tables. Tuned against the page count
/// that `tests/refdoc.rs` checks.
pub const CHARS_PER_PAGE: usize = 3100;

/// The characters of running text that an item displaces from its page.
const FIGURE_CHARS: usize = 900;
const EQUATION_CHARS: usize = 300;
const TABLE_CHARS: usize = 700;

/// The project's manifest: one document, built as PDF.
const MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
lang = \"en-GB\"
styles = [\"styles/reference.fsty\"]
";

/// The project's stylesheet: A4, as the exam papers of the corpus.
const STYLESHEET: &str = "\
page { page-size: A4; margin-top: 20mm; margin-bottom: 20mm; margin-inner: 20mm; margin-outer: 20mm; }
doc { font: \"STIX Two Text\"; font-size: 11pt; justify: true; hyphenate: true; }
heading { font: \"Source Sans 3\"; space-before: 12pt; space-after: 6pt; keep-with-next: true; }
heading[level=1] { font-size: 17pt; }
heading[level=2] { font-size: 13pt; }
heading[level=3] { font-size: 11pt; }
figure { space-before: 8pt; space-after: 8pt; }
terms-item { space-before: 2pt; }
";

/// The variants of [`generate_variants`]'s project, as the exam and mark
/// scheme of SPEC §6.1 with a third that shares the mark scheme's answers.
pub const VARIANTS: [&str; 3] = ["exam", "markscheme", "teacher"];

/// The manifest of [`generate_variants`]'s project: [`VARIANTS`], each
/// built as PDF, the mark scheme with a stylesheet overlay.
const VARIANTS_MANIFEST: &str = "\
[variants.exam]
targets = [\"pdf\"]

[variants.markscheme]
targets = [\"pdf\"]
styles = [\"styles/markscheme.fsty\"]

[variants.teacher]
targets = [\"pdf\"]
";

/// The mark scheme's overlay: the whole mark scheme set smaller, so that
/// it is shaped and paginates apart from the other two variants.
const MARKSCHEME_STYLESHEET: &str = "doc { font-size: 10pt; }\n";

/// Generates a document: its project, named `reference` for
/// [`Spec::REFERENCE`] and `refdoc-{pages}` otherwise.
pub fn generate(spec: &Spec) -> Project {
    let name = if *spec == Spec::REFERENCE {
        "reference".to_string()
    } else {
        format!("refdoc-{}", spec.pages)
    };
    Project {
        name,
        manifest: MANIFEST.into(),
        sources: BTreeMap::from([
            ("main.fol".to_string(), Generator::new(false).document(spec)),
            ("styles/reference.fsty".to_string(), STYLESHEET.to_string()),
        ]),
    }
}

/// Generates a document with the three [`VARIANTS`] (SPEC §6): the
/// document of [`generate`], which the exam variant shows unchanged, with
/// an answer after the running text of every page that only the mark
/// scheme and teacher variants show. Its project
/// is named as [`generate`]'s with `-variants` appended.
pub fn generate_variants(spec: &Spec) -> Project {
    let mut project = generate(spec);
    project.name.push_str("-variants");
    project.manifest.push('\n');
    project.manifest.push_str(VARIANTS_MANIFEST);
    project
        .sources
        .insert("main.fol".into(), Generator::new(true).document(spec));
    project.sources.insert(
        "styles/markscheme.fsty".into(),
        MARKSCHEME_STYLESHEET.into(),
    );
    project
}

/// How many of `total` items fall on page `page` of `pages`, spread evenly.
fn share(total: usize, page: usize, pages: usize) -> usize {
    (page + 1) * total / pages - page * total / pages
}

/// A small deterministic random number generator (xorshift64*): the
/// document must be the same on every machine and in every version.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number in `range`.
    fn range(&mut self, range: std::ops::Range<usize>) -> usize {
        range.start + (self.next() % (range.end - range.start) as u64) as usize
    }

    /// One of `items`.
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.range(0..items.len())]
    }
}

const NOUNS: &[&str] = &[
    "pendulum",
    "oscillator",
    "wave",
    "field",
    "charge",
    "current",
    "resistor",
    "capacitor",
    "photon",
    "electron",
    "nucleus",
    "spring",
    "trolley",
    "projectile",
    "satellite",
    "lens",
    "mirror",
    "gas",
    "piston",
    "conductor",
    "magnet",
    "coil",
    "frequency",
    "amplitude",
    "wavelength",
    "momentum",
    "energy",
    "force",
    "velocity",
    "acceleration",
    "displacement",
    "temperature",
    "pressure",
    "density",
    "potential",
    "resistance",
];

const VERBS: &[&str] = &[
    "depends on",
    "determines",
    "is proportional to",
    "changes",
    "reduces",
    "increases",
    "balances",
    "transfers energy to",
    "acts on",
    "measures",
    "limits",
    "follows from",
];

const ADJECTIVES: &[&str] = &[
    "uniform",
    "constant",
    "resultant",
    "horizontal",
    "vertical",
    "electric",
    "magnetic",
    "thermal",
    "kinetic",
    "internal",
    "terminal",
    "maximum",
    "effective",
    "ideal",
    "isolated",
    "transverse",
    "longitudinal",
    "stationary",
];

const CLAUSES: &[&str] = &[
    "when the system is in equilibrium",
    "as long as no external force acts",
    "provided that the temperature stays constant",
    "which can be checked by experiment",
    "within the uncertainty of the measurement",
    "for small displacements from equilibrium",
    "if air resistance is negligible",
    "throughout the motion",
];

/// Writes the document.
#[derive(Debug)]
struct Generator {
    rng: Rng,
    /// The answers' own generator, so that they leave the rest of the
    /// document as [`generate`] writes it.
    answer_rng: Rng,
    out: String,
    figure: usize,
    equation: usize,
    table: usize,
    /// Whether every page ends with an answer that the exam variant hides.
    answers: bool,
}

impl Generator {
    fn new(answers: bool) -> Self {
        Self {
            rng: Rng(0x5eed_f011_0b00_00c5),
            answer_rng: Rng(0xa115_3e45_0b00_00c5),
            out: String::new(),
            figure: 0,
            equation: 0,
            table: 0,
            answers,
        }
    }

    /// Writes the document and returns its markup.
    fn document(mut self, spec: &Spec) -> String {
        let _ = writeln!(
            self.out,
            "#doc(title: [Reference document], subject: \"{} pages, generated for benchmarks\")\n",
            spec.pages
        );
        for page in 0..spec.pages {
            if page % 10 == 0 {
                self.heading(1);
            }
            self.heading(2);
            let figures = share(spec.figures, page, spec.pages);
            let equations = share(spec.equations, page, spec.pages);
            let tables = share(spec.tables, page, spec.pages);
            let mut items: Vec<u8> = [(b'f', figures), (b'e', equations), (b't', tables)]
                .into_iter()
                .flat_map(|(kind, n)| std::iter::repeat_n(kind, n))
                .collect();
            // Shuffle the page's items so their order varies.
            for i in (1..items.len()).rev() {
                let j = self.rng.range(0..i + 1);
                items.swap(i, j);
            }
            let displaced =
                figures * FIGURE_CHARS + equations * EQUATION_CHARS + tables * TABLE_CHARS;
            let mut budget = CHARS_PER_PAGE.saturating_sub(displaced).max(400);
            // Split the running text into as many chunks as there are
            // items, plus one, and put an item after each chunk but the
            // last.
            let chunks = items.len() + 1;
            for k in 0..chunks {
                let share = budget / (chunks - k);
                budget -= share;
                self.text(share);
                if k == chunks / 2 && page % 3 == 1 {
                    self.heading(3);
                }
                match items.get(k) {
                    Some(b'f') => self.figure(),
                    Some(b'e') => self.equation(),
                    Some(b't') => self.table(),
                    _ => {}
                }
            }
            if self.answers {
                self.answer(page);
            }
        }
        self.out
    }

    /// An answer to the page's questions, shown by every variant but the
    /// exam.
    fn answer(&mut self, page: usize) {
        std::mem::swap(&mut self.rng, &mut self.answer_rng);
        let sentences = [self.sentence(), self.sentence()];
        std::mem::swap(&mut self.rng, &mut self.answer_rng);
        let _ = writeln!(
            self.out,
            "#if variant != \"exam\" [\n  Answer {}: {}\n]\n",
            page + 1,
            sentences.join(" ")
        );
    }

    /// A heading of a level.
    fn heading(&mut self, level: usize) {
        let title = self.title();
        let _ = writeln!(self.out, "{} {title}\n", "=".repeat(level));
    }

    /// A heading's text.
    fn title(&mut self) -> String {
        let adjective = self.rng.pick(ADJECTIVES);
        let noun = self.rng.pick(NOUNS);
        let mut title = format!("The {adjective} {noun}");
        if self.rng.range(0..3) == 0 {
            let _ = write!(title, " and its {}", self.rng.pick(NOUNS));
        }
        title
    }

    /// A sentence of running text, with some emphasis.
    fn sentence(&mut self) -> String {
        let mut s = String::new();
        let adjective = self.rng.pick(ADJECTIVES);
        let noun = self.rng.pick(NOUNS);
        let verb = self.rng.pick(VERBS);
        let object = self.rng.pick(NOUNS);
        let _ = write!(s, "The {adjective} {noun} {verb} the ");
        match self.rng.range(0..8) {
            0 => {
                let _ = write!(s, "_{object}_");
            }
            1 => {
                let _ = write!(s, "*{object}*");
            }
            _ => s.push_str(object),
        }
        if self.rng.range(0..2) == 0 {
            let _ = write!(s, " {}", self.rng.pick(CLAUSES));
        }
        if self.rng.range(0..4) == 0 {
            let value = self.rng.range(1..999);
            let _ = write!(s, ", and its value is {}.{} units", value / 10, value % 10);
        }
        s.push('.');
        s
    }

    /// Paragraphs, and sometimes a list, of about `chars` characters.
    fn text(&mut self, chars: usize) {
        let start = self.out.len();
        while self.out.len() - start < chars {
            if self.rng.range(0..9) == 0 {
                let marker = if self.rng.range(0..2) == 0 { "-" } else { "+" };
                for _ in 0..self.rng.range(2..5) {
                    let sentence = self.sentence();
                    let _ = writeln!(self.out, "{marker} {sentence}");
                }
                self.out.push('\n');
                continue;
            }
            let sentences = self.rng.range(3..7);
            let mut paragraph = Vec::with_capacity(sentences);
            for _ in 0..sentences {
                paragraph.push(self.sentence());
            }
            let _ = writeln!(self.out, "{}\n", paragraph.join(" "));
        }
    }

    /// A figure with a caption. Its body is text until graphics land.
    fn figure(&mut self) {
        self.figure += 1;
        let mut body = Vec::new();
        for _ in 0..6 {
            body.push(self.sentence());
        }
        let noun = self.rng.pick(NOUNS);
        let _ = writeln!(
            self.out,
            "#figure[{}][Figure {}: the {noun} in its set-up]\n",
            body.join(" "),
            self.figure
        );
    }

    /// A display equation's stand-in until math lands (M1): the equation
    /// in words, as a paragraph of its own.
    fn equation(&mut self) {
        self.equation += 1;
        let a = self.rng.pick(NOUNS);
        let b = self.rng.pick(NOUNS);
        let c = self.rng.pick(NOUNS);
        let _ = writeln!(
            self.out,
            "Equation {}: {a} equals {b} times {c} over two.\n",
            self.equation
        );
    }

    /// A table's stand-in until tables land (M2): a term list, a row per
    /// term.
    fn table(&mut self) {
        self.table += 1;
        let _ = writeln!(self.out, "Table {}:\n", self.table);
        for _ in 0..self.rng.range(5..8) {
            let term = self.rng.pick(NOUNS);
            let value = self.rng.range(10..9999);
            let unit = self.rng.pick(&["m", "s", "kg", "N", "J", "W", "V", "Ω"]);
            let _ = writeln!(
                self.out,
                "/ {term}: {}.{:02} {unit}",
                value / 100,
                value % 100
            );
        }
        self.out.push('\n');
    }
}
