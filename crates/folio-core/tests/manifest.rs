//! `folio.toml` and `folio.lock`: the examples of the specification parse into
//! the model, and invalid files give errors that point at the key or value at
//! fault (SPEC §4.1).

use std::num::NonZeroU16;

use folio_core::project::{
    ComponentSource, FormattingMode, Lockfile, LockfileInput, Manifest, ManifestInput,
    ProjectError, Target,
};
use folio_syntax::{DiagSpanKind, FileId};
use salsa::Setter;

fn file() -> FileId {
    FileId::from_raw(NonZeroU16::new(1).unwrap())
}

const PROJECT: &str = r#"[project]
entry = "main.fol"
edition = "2027"
compiler = "1.2.0"
"#;

/// The TOML blocks of the specification, in order.
fn spec_blocks() -> Vec<String> {
    let spec = include_str!("../../../docs/SPEC.md");
    let mut blocks = Vec::new();
    let mut lines = spec.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "```toml" {
            let block: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "```").collect();
            blocks.push(block.join("\n") + "\n");
        }
    }
    blocks
}

fn is_lockfile(block: &str) -> bool {
    block.starts_with("version =")
}

/// A manifest fragment from the specification, completed with `[project]`.
fn spec_manifest(containing: &str) -> Manifest {
    let block = spec_blocks()
        .into_iter()
        .find(|b| b.contains(containing))
        .unwrap_or_else(|| panic!("no TOML block in SPEC.md contains {containing:?}"));
    let text = if block.contains("[project]") {
        block
    } else {
        format!("{PROJECT}\n{block}")
    };
    parse(&text)
}

fn parse(text: &str) -> Manifest {
    match Manifest::parse(text, file()) {
        Ok(manifest) => manifest,
        Err(errors) => panic!("{}", render(text, &errors)),
    }
}

/// Errors as `message @ source text` lines, with hints indented.
fn render(text: &str, errors: &[ProjectError]) -> String {
    let mut out = String::new();
    for error in errors {
        let at = match error.span.get() {
            DiagSpanKind::Range { id, range } => {
                assert_eq!(id, file());
                &text[range]
            }
            other => panic!("unexpected span {other:?}"),
        };
        out.push_str(&format!("{} @ {at}\n", error.message));
        for hint in &error.hints {
            out.push_str(&format!("  hint: {hint}\n"));
        }
    }
    out
}

/// The errors of an invalid manifest; `[project]` is prepended unless given.
fn errors(text: &str) -> String {
    let text = if text.contains("[project]") {
        text.to_owned()
    } else {
        format!("{PROJECT}{text}")
    };
    match Manifest::parse(&text, file()) {
        Ok(manifest) => panic!("expected errors, got {manifest:#?}"),
        Err(errors) => render(&text, &errors),
    }
}

fn lock_errors(text: &str) -> String {
    match Lockfile::parse(text, file()) {
        Ok(lock) => panic!("expected errors, got {lock:#?}"),
        Err(errors) => render(text, &errors),
    }
}

#[test]
fn every_spec_example_parses() {
    let blocks = spec_blocks();
    assert!(blocks.len() >= 5, "found only {} TOML blocks", blocks.len());
    for block in blocks {
        if is_lockfile(&block) {
            if let Err(errors) = Lockfile::parse(&block, file()) {
                panic!("{block}\n{}", render(&block, &errors));
            }
        } else if block.contains("[project]") {
            parse(&block);
        } else {
            parse(&format!("{PROJECT}\n{block}"));
        }
    }
}

#[test]
fn spec_4_1_project() {
    let m = spec_manifest("[project]");
    let p = &m.project;
    assert_eq!(p.entry.v.as_str(), "main.fol");
    assert_eq!(p.edition.v.as_str(), "2027");
    assert_eq!(p.compiler.v, semver::Version::new(1, 2, 0));
    assert_eq!(p.lang.v.as_str(), "en");
    assert_eq!(format!("{:?}", p.styles), r#"["styles/exam.fsty"]"#);
    assert_eq!(p.today, None);

    let [house] = &m.components.declared[..] else {
        panic!()
    };
    assert_eq!(house.name.v.as_str(), "@school/house");
    assert!(matches!(&house.source, ComponentSource::Path(p) if p.v.as_str() == "../house-style"));

    let names: Vec<_> = m.variants.iter().map(|v| v.name.v.as_str()).collect();
    assert_eq!(names, ["exam", "markscheme"]);
    assert_eq!(format!("{:?}", m.variants[0].targets), "[Pdf, Docx]");
    assert_eq!(
        format!("{:?}", m.variants[1].styles),
        r#"["styles/markscheme.fsty"]"#
    );
    assert_eq!(m.formatting.mode.v, FormattingMode::Local);
}

#[test]
fn spec_3_6_components() {
    let m = spec_manifest("disable = ");
    let c = &m.components;
    assert_eq!(c.declared.len(), 2);
    let circuits = c
        .declared
        .iter()
        .find(|d| d.name.v.as_str() == "circuits")
        .unwrap();
    let ComponentSource::Version(req) = &circuits.source else {
        panic!()
    };
    assert!(req.v.matches(&semver::Version::new(0, 3, 2)));
    assert!(!req.v.matches(&semver::Version::new(0, 4, 0)));
    assert_eq!(format!("{:?}", c.disabled), r#"["plot"]"#);
    assert!(!c.is_enabled("plot"));
    assert!(c.is_enabled("exam"));
    assert!(c.is_enabled("circuits"));
    assert!(!c.is_enabled("unknown"));
}

#[test]
fn spec_5_5_strict_mode() {
    let m = spec_manifest("strict-paths");
    let f = &m.formatting;
    assert_eq!(f.mode.v, FormattingMode::Strict);
    assert_eq!(format!("{:?}", f.strict_paths), r#"["questions/**"]"#);
    assert_eq!(
        f.overrides_sheet.as_ref().unwrap().v.as_str(),
        "styles/overrides.fsty"
    );
    assert!(f.allow_overrides_sheet.v);
    assert!(!f.allow_local_packages.v);
}

#[test]
fn spec_6_1_variants_and_numbering_groups() {
    let m = spec_manifest("[numbering-groups.paper]");
    let names: Vec<_> = m.variants.iter().map(|v| v.name.v.as_str()).collect();
    assert_eq!(names, ["exam", "markscheme", "slides", "handout"]);
    let slides = m.variant("slides").unwrap();
    assert_eq!(
        slides.targets.iter().map(|t| t.v).collect::<Vec<_>>(),
        [Target::Pptx, Target::Live]
    );
    assert_eq!(
        format!("{:?}", m.variant("handout").unwrap().styles),
        r#"["styles/slides.fsty", "styles/handout.fsty"]"#
    );

    let [paper] = &m.numbering_groups[..] else {
        panic!()
    };
    assert_eq!(paper.name.v.as_str(), "paper");
    assert_eq!(format!("{:?}", paper.variants), r#"["exam", "markscheme"]"#);
    assert_eq!(
        format!("{:?}", paper.shared_counters),
        r#"["question", "part", "figure", "equation"]"#
    );
    assert_eq!(m.numbering_group("markscheme"), Some(paper));
    assert_eq!(m.numbering_group("slides"), None);
}

#[test]
fn spec_lockfile() {
    let block = spec_blocks().into_iter().find(|b| is_lockfile(b)).unwrap();
    let lock = Lockfile::parse(&block, file()).unwrap();
    let [font] = &lock.fonts[..] else { panic!() };
    assert_eq!(font.family.v, "Source Serif 4");
    assert_eq!(font.postscript_name.v, "SourceSerif4-Regular");
    let circuits = lock.component("circuits").unwrap();
    assert_eq!(circuits.version.v, semver::Version::new(0, 3, 2));
    assert!(circuits.hash.v.to_string().starts_with("sha256:e5895263"));
    assert!(lock.packages.is_empty());
    assert_eq!(lock.bindings.element("question").unwrap().as_str(), "exam");
    assert_eq!(
        lock.bindings.element("circuit").unwrap().as_str(),
        "circuits"
    );
    assert_eq!(lock.bindings.element("figure"), None);
    assert_eq!(lock.bindings.property("question"), None);
}

#[test]
fn defaults() {
    let m = parse(PROJECT);
    assert_eq!(m.project.lang.v.as_str(), "en");
    assert!(m.project.lang.span.is_detached());
    assert!(m.project.styles.is_empty());
    assert!(m.components.declared.is_empty() && m.components.disabled.is_empty());
    assert!(m.variants.is_empty() && m.numbering_groups.is_empty());
    assert_eq!(m.formatting.mode.v, FormattingMode::Local);
    assert!(m.formatting.allow_overrides_sheet.v);
    assert!(!m.formatting.allow_local_packages.v);
    assert_eq!(m.output.path.v.as_str(), "out/{stem}-{variant}.{ext}");
    assert_eq!(
        m.output.path.v.resolve("main", "exam", "pdf"),
        "out/main-exam.pdf"
    );
}

#[test]
fn spans_point_into_the_file() {
    let text = format!("{PROJECT}lang = \"pl\"\n");
    let m = parse(&text);
    let DiagSpanKind::Range { range, .. } = m.project.lang.span.get() else {
        panic!()
    };
    assert_eq!(&text[range], "\"pl\"");
}

#[test]
fn today_and_output() {
    let m = parse(&format!(
        "{PROJECT}today = 2026-11-24\n\n[variants.exam]\ntargets = [\"pdf\"]\n\n\
         [output]\npath = \"./build//{{stem}}.{{ext}}\"\n"
    ));
    assert_eq!(m.project.today.unwrap().v.to_string(), "2026-11-24");
    assert_eq!(m.output.path.v.as_str(), "build/{stem}.{ext}");
    assert_eq!(
        m.output.path.v.resolve("main", "exam", "pdf"),
        "build/main.pdf"
    );
}

#[test]
fn unknown_keys() {
    assert_eq!(
        errors("[projcet]\nentry = \"main.fol\"\n"),
        "unknown key `projcet` in the top level @ projcet\n  hint: did you mean `project`?\n"
    );
    assert_eq!(
        errors("[project]\nentyr = \"main.fol\"\nedition = \"2027\"\ncompiler = \"1.2.0\"\n"),
        "missing key `entry` in `[project]` @ [project]\n\
         unknown key `entyr` in `[project]` @ entyr\n  hint: did you mean `entry`?\n"
    );
    assert_eq!(
        errors("colour = \"red\"\n"),
        "unknown key `colour` in `[project]` @ colour\n  hint: expected `entry`, `edition`, \
         `compiler`, `lang`, `styles` or `today`\n"
    );
    assert_eq!(
        errors("\n[variants.exam]\ntargets = [\"pdf\"]\nstyle = [\"a.fsty\"]\n"),
        "unknown key `style` in `[variants.exam]` @ style\n  hint: did you mean `styles`?\n"
    );
    assert_eq!(
        errors("\n[components]\nc = { path = \"../c\", rev = \"1\" }\n"),
        "unknown key `rev` in component `c` @ rev\n  hint: expected `path` or `version`\n"
    );
}

#[test]
fn missing_project() {
    assert_eq!(
        errors("[project]\n").lines().collect::<Vec<_>>(),
        [
            "missing key `entry` in `[project]` @ [project]",
            "missing key `edition` in `[project]` @ [project]",
            "missing key `compiler` in `[project]` @ [project]",
        ]
    );
    let text = "[variants.exam]\ntargets = [\"pdf\"]\n";
    let errors = Manifest::parse(text, file()).unwrap_err();
    assert_eq!(errors[0].message, "missing table `[project]`");
}

#[test]
fn invalid_project_values() {
    let project = |body: &str| errors(&format!("[project]\n{body}"));
    assert_eq!(
        project("entry = \"main.fol\"\nedition = 2027\ncompiler = \"1.2.0\"\n"),
        "`edition` must be a string, not an integer @ 2027\n  hint: write it in quotes: `\"2027\"`\n"
    );
    assert_eq!(
        project("entry = \"main.fol\"\nedition = \"2031\"\ncompiler = \"1.2.0\"\n"),
        "unknown edition `2031` @ \"2031\"\n  hint: this compiler supports the editions `2027`\n  \
         hint: a newer edition needs a newer compiler\n"
    );
    assert_eq!(
        project("entry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"1.2\"\n"),
        "`1.2` is not a compiler version @ \"1.2\"\n  hint: pin an exact version, such as `1.2.0`\n"
    );
    assert_eq!(
        project("entry = \"../main.fol\"\nedition = \"2027\"\ncompiler = \"1.2.0\"\n"),
        "`../main.fol` leaves the project @ \"../main.fol\"\n  \
         hint: Folio reads files only inside the project directory\n"
    );
    assert_eq!(
        project("entry = \"main.typ\"\nedition = \"2027\"\ncompiler = \"1.2.0\"\n"),
        "`main.typ` is not a content file @ \"main.typ\"\n  \
         hint: a content file has the extension `.fol`\n"
    );
    assert_eq!(
        errors("lang = \"english\"\n"),
        "`english` is not a language tag @ \"english\"\n  \
         hint: write a language tag such as `en`, `pl` or `en-GB`\n"
    );
    assert_eq!(
        errors("styles = [\"a.fsty\", \"styles\\\\b.fsty\", \"./a.fsty\"]\n"),
        "`styles\\b.fsty` contains `\\` @ \"styles\\\\b.fsty\"\n  \
         hint: separate directories with `/`: `styles/b.fsty`\n\
         duplicate item in `styles` @ \"./a.fsty\"\n"
    );
    assert_eq!(
        errors("today = \"2026-11-24\"\n"),
        "`today` must be a date such as `2026-11-24`, not a string @ \"2026-11-24\"\n  \
         hint: write the date without quotes\n"
    );
    assert_eq!(
        errors("today = 2026-11-24T08:00:00\n"),
        "`today` must be a date such as `2026-11-24`, without a time, not a date or time \
         @ 2026-11-24T08:00:00\n"
    );
}

#[test]
fn invalid_variants() {
    assert_eq!(
        errors("\n[variants.exam]\ntargets = [\"pdf\", \"html\"]\n"),
        "unknown target `html` @ \"html\"\n  hint: the targets are `pdf`, `docx`, `pptx` or `live`\n"
    );
    assert_eq!(
        errors("\n[variants.exam]\ntargets = []\n"),
        "variant `exam` has no targets @ []\n  \
         hint: list at least one of `pdf`, `docx`, `pptx` and `live`\n"
    );
    assert_eq!(
        errors("\n[variants.exam]\nstyles = [\"a.fsty\"]\n"),
        "missing key `targets` in `[variants.exam]` @ [variants.exam]\n"
    );
    assert_eq!(
        errors("\n[variants.\"mark scheme\"]\ntargets = [\"pdf\"]\n"),
        "`mark scheme` is not a valid name @ \"mark scheme\"\n  hint: a name starts with a \
         letter or `_` and continues with letters, digits, `-` or `_`\n"
    );
    assert_eq!(
        errors("\n[variants]\nexam = \"pdf\"\n"),
        "`exam` must be a table, not a string @ \"pdf\"\n"
    );
}

const TWO_VARIANTS: &str = "\n[variants.exam]\ntargets = [\"pdf\"]\n\n\
                            [variants.markscheme]\ntargets = [\"pdf\"]\n";

#[test]
fn invalid_numbering_groups() {
    assert_eq!(
        errors(&format!(
            "{TWO_VARIANTS}\n[numbering-groups.paper]\nvariants = [\"exam\", \"marksheme\"]\n"
        )),
        "no variant `marksheme` is declared @ \"marksheme\"\n  hint: did you mean `markscheme`?\n"
    );
    assert_eq!(
        errors(&format!(
            "{TWO_VARIANTS}\n[numbering-groups.a]\nvariants = [\"exam\", \"markscheme\"]\n\n\
             [numbering-groups.b]\nvariants = [\"exam\"]\n"
        )),
        "variant `exam` is already in numbering group `a` @ \"exam\"\n  \
         hint: a variant belongs to at most one numbering group\n"
    );
    assert_eq!(
        errors(&format!(
            "{TWO_VARIANTS}\n[numbering-groups.paper]\nvariants = [\"exam\"]\n\
             shared-counters = [\"exam.question\", \"a.b.c\"]\n"
        )),
        "`a.b.c` is not a valid name @ \"a.b.c\"\n  hint: write a name such as `question`, or \
         qualify it with its component: `exam.question`\n"
    );
    assert_eq!(
        errors("\n[numbering-groups.paper]\nvariants = []\n"),
        "numbering group `paper` has no variants @ []\n"
    );
}

#[test]
fn invalid_components() {
    assert_eq!(
        errors("\n[components]\nexam = { version = \"1\" }\n"),
        "`exam` is a first-party component and needs no declaration @ exam\n  \
         hint: first-party components are on by default; `disable` switches them off\n"
    );
    assert_eq!(
        errors("\n[components]\ndisable = [\"circuits\"]\n"),
        "`circuits` is not a first-party component @ \"circuits\"\n  hint: `disable` switches \
         off first-party components: `exam`, `multicol`, `plot`, `timer` or `inputs`\n"
    );
    assert_eq!(
        errors("\n[components]\nc = { path = \"../c\", version = \"1\" }\n"),
        "component `c` has both `path` and `version` @ { path = \"../c\", version = \"1\" }\n  \
         hint: a component comes either from a directory or from a published version\n"
    );
    assert_eq!(
        errors("\n[components]\nc = {}\n"),
        "component `c` needs `path` or `version` @ {}\n  hint: write `c = { path = \"../c\" }` \
         or `c = { version = \"1.0\" }`\n"
    );
    assert_eq!(
        errors("\n[components]\nc = { version = \"latest\" }\n"),
        "`latest` is not a version requirement @ \"latest\"\n  \
         hint: write a version such as `0.3` or `1.2.0`\n"
    );
    assert_eq!(
        errors("\n[components]\n\"school/house\" = { path = \"/srv/house\" }\n"),
        "`school/house` is not a valid component name @ \"school/house\"\n  hint: write a name \
         such as `circuits`, or a scoped name such as `@school/house`\n"
    );
    assert_eq!(
        errors("\n[components]\n\"@school/house\" = { path = \"/srv/house\" }\n"),
        "`/srv/house` is an absolute path @ \"/srv/house\"\n  hint: write it relative to the \
         directory that holds `folio.toml`, so the project can move\n"
    );
}

#[test]
fn invalid_formatting() {
    assert_eq!(
        errors("\n[formatting]\nmode = \"strcit\"\n"),
        "unknown formatting mode `strcit` @ \"strcit\"\n  hint: the modes are `local` and `strict`\n"
    );
    assert_eq!(
        errors("\n[formatting]\nstrict-paths = [\"questions/**\"]\n"),
        "`strict-paths` applies only in strict mode @ [\"questions/**\"]\n  \
         hint: set `mode = \"strict\"` to make these paths strict\n"
    );
    assert_eq!(
        errors("\n[formatting]\nmode = \"strict\"\nstrict-paths = [\"../shared/**\"]\n"),
        "`../shared/**` leaves the project @ \"../shared/**\"\n  \
         hint: patterns match files inside the project directory\n"
    );
    assert_eq!(
        errors("\n[formatting]\nallow-local-packages = \"yes\"\n"),
        "`allow-local-packages` must be `true` or `false`, not a string @ \"yes\"\n"
    );
    assert_eq!(
        errors("\n[formatting]\noverrides-sheet = \"styles/overrides.css\"\n"),
        "`styles/overrides.css` is not a stylesheet @ \"styles/overrides.css\"\n  \
         hint: a stylesheet has the extension `.fsty`\n"
    );
}

#[test]
fn invalid_output() {
    assert_eq!(
        errors(&format!(
            "{TWO_VARIANTS}\n[output]\npath = \"out/{{stem}}.{{ext}}\"\n"
        )),
        "every variant writes to the same file @ \"out/{stem}.{ext}\"\n  \
         hint: add `{variant}` to the path\n"
    );
    assert_eq!(
        errors(
            "\n[variants.exam]\ntargets = [\"pdf\", \"docx\", \"live\"]\n\n\
                [output]\npath = \"out/{stem}-{variant}.pdf\"\n"
        ),
        "the targets of variant `exam` write to the same file @ \"out/{stem}-{variant}.pdf\"\n  \
         hint: add `{ext}` to the path\n"
    );
    assert_eq!(
        errors("\n[output]\npath = \"out/{stme}.{ext}\"\n"),
        "unknown placeholder `{stme}` @ \"out/{stme}.{ext}\"\n  hint: did you mean `{stem}`?\n"
    );
    assert_eq!(
        errors("\n[output]\npath = \"out/{stem\"\n"),
        "unmatched brace in `out/{stem` @ \"out/{stem\"\n  \
         hint: placeholders are `{stem}`, `{variant}` or `{ext}`\n"
    );
    assert_eq!(
        errors("\n[output]\npath = \"out/\"\n"),
        "the output path names a directory @ \"out/\"\n  \
         hint: end it with a file name, such as `out/{stem}-{variant}.{ext}`\n"
    );
    assert_eq!(
        errors("\n[output]\npath = \"../{stem}.{ext}\"\n"),
        "`../{stem}.{ext}` leaves the project @ \"../{stem}.{ext}\"\n  \
         hint: Folio reads files only inside the project directory\n  \
         hint: outputs are written inside the project directory\n"
    );
}

#[test]
fn syntax_errors_and_several_errors_at_once() {
    let text = "[project]\nentry = \nedition = \"2027\"\nedition = \"2027\"\ncompiler = \"x\"\n";
    let errors = Manifest::parse(text, file()).unwrap_err();
    let messages: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(errors.len(), 3, "{messages:?}");
    assert!(messages[0].contains("string values must be quoted"));
    assert_eq!(messages[1], "duplicate key");
    assert_eq!(messages[2], "`x` is not a compiler version");
    // In source order.
    let starts: Vec<_> = errors
        .iter()
        .map(|e| match e.span.get() {
            DiagSpanKind::Range { range, .. } => range.start,
            _ => unreachable!(),
        })
        .collect();
    assert!(starts.is_sorted());
}

const HASH: &str = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

#[test]
fn lockfile_defaults() {
    assert_eq!(
        Lockfile::parse("version = 1\n", file()).unwrap(),
        Lockfile::default()
    );
}

#[test]
fn invalid_lockfiles() {
    assert_eq!(
        lock_errors("[[font]]\nfamily = \"A\"\n"),
        "missing key `version` in the lockfile @ \n  hint: this compiler writes `version = 1`\n\
         missing key `postscript-name` in `[[font]]` @ [[font]]\n\
         missing key `hash` in `[[font]]` @ [[font]]\n"
    );
    assert_eq!(
        lock_errors("version = 2\n"),
        "unknown lockfile format 2 @ 2\n  hint: the lockfile was written by a newer Folio \
         compiler\n  hint: this compiler reads format 1\n"
    );
    assert_eq!(
        lock_errors(&format!(
            "version = 1\n[[component]]\nname = \"c\"\nversion = \"0.3\"\nhash = \"{HASH}\"\n"
        )),
        "`0.3` is not an exact version @ \"0.3\"\n  hint: the lockfile records the version that \
         was resolved, such as `0.3.2`\n"
    );
    assert_eq!(
        lock_errors(
            "version = 1\n[[package]]\nname = \"p\"\nversion = \"1.0.0\"\nhash = \"abc\"\n"
        ),
        "`abc` is not a hash @ \"abc\"\n  hint: write `sha256:` followed by 64 lower-case hex \
         digits\n"
    );
    let twice = format!("[[component]]\nname = \"c\"\nversion = \"1.0.0\"\nhash = \"{HASH}\"\n");
    assert_eq!(
        lock_errors(&format!("version = 1\n{twice}{twice}")),
        "the component `c` is locked twice @ \"c\"\n"
    );
    assert_eq!(
        lock_errors("version = 1\n[bindings.elements]\nquestion = \"exma\"\n"),
        "`question` is bound to `exma`, which is not locked @ \"exma\"\n  \
         hint: did you mean `exam`?\n"
    );
    assert_eq!(
        lock_errors("version = 1\n[bindings]\nelement = {}\n"),
        "unknown key `element` in `[bindings]` @ element\n  hint: did you mean `elements`?\n"
    );
    assert_eq!(
        lock_errors("version = 1\n[[fonts]]\nfamily = \"A\"\n"),
        "unknown key `fonts` in the top level @ fonts\n  hint: did you mean `font`?\n"
    );
}

#[test]
fn salsa_inputs() {
    let mut db = salsa::DatabaseImpl::new();
    let manifest = parse(PROJECT);
    let input = ManifestInput::new(&db, manifest.clone());
    assert_eq!(input.manifest(&db), &manifest);

    let changed = parse(&format!("{PROJECT}lang = \"pl\"\n"));
    input.set_manifest(&mut db).to(changed);
    assert_eq!(input.manifest(&db).project.lang.v.as_str(), "pl");

    let lock = LockfileInput::new(&db, Lockfile::default());
    assert!(lock.lockfile(&db).fonts.is_empty());
}
