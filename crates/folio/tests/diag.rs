//! Diagnostics as front ends show them (SPEC §12.1, §12.3): spans resolved
//! to files, lines and columns, rendered for a terminal with source
//! excerpts, and serialised to JSON.

use folio::diag::{self, Files, Position, TextFiles};
use folio::{Diagnostic, Fix, Name, Target, World};
use folio_syntax::{DiagSpan, Span, SubRange, SyntaxNode};

const MANIFEST: &str = "\
[project]
entry = \"main.fol\"
edition = \"2027\"
compiler = \"0.0.0\"
";

fn world(main: &str) -> World {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("main.fol", main).unwrap();
    world
}

/// The span of the first node in a file whose text is `text`.
fn find(world: &World, path: &str, text: &str) -> Span {
    fn walk(node: &SyntaxNode, text: &str) -> Option<Span> {
        if node.full_text() == text {
            return Some(node.span());
        }
        node.children().find_map(|child| walk(child, text))
    }
    let source = world.source(path).unwrap();
    walk(source.root(), text).unwrap_or_else(|| panic!("no node `{text}` in {path}"))
}

#[test]
fn parse_errors_render_with_excerpts() {
    let world = world("= Forces\n\nA ball is #box(thrown.\n");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    assert!(compiled.has_errors());

    let text = diag::render(&world, &compiled.diagnostics, false);
    assert_eq!(
        text,
        "\
error: unclosed delimiter
  ┌─ main.fol:3:15
  │
3 │ A ball is #box(thrown.
  │               ^

error: expected identifier
  ┌─ main.fol:3:23
  │
3 │ A ball is #box(thrown.
  │                       ^

"
    );

    // With colour, the same text with ANSI escapes.
    let colored = diag::render(&world, &compiled.diagnostics, true);
    assert!(colored.contains("\u{1b}["), "{colored:?}");
}

#[test]
fn parse_errors_serialise_to_json() {
    let world = world("= Forces\n\n#box(\n");
    let compiled = folio::compile(&world, Target::Pdf, None).unwrap();
    let json = diag::to_json(&world, &compiled.diagnostics);
    assert_eq!(
        json,
        r#"[
  {
    "severity": "error",
    "code": null,
    "message": "unclosed delimiter",
    "location": {
      "path": "main.fol",
      "bytes": {
        "start": 14,
        "end": 15
      },
      "start": {
        "line": 3,
        "column": 5
      },
      "end": {
        "line": 3,
        "column": 6
      }
    },
    "related": [],
    "hints": [],
    "fixes": []
  }
]"#
    );
}

/// A field value of the wrong type, as the evaluator will report it: the
/// value, the declaration it contradicts, a hint and a quick fix.
fn type_error(world: &World) -> Diagnostic {
    let value = find(world, "main.fol", "\"three\"");
    let declared = find(world, "main.fol", "int");
    Diagnostic::error(value, "expected `int`, found `str`")
        .with_related(declared, "the field `marks` is declared as `int` here")
        .with_hint("marks are whole numbers")
        .with_fix(Fix::replace("Write the marks as a number", value, "3"))
}

const TYPED: &str = "\
#element question(marks: int = 1) { kind: block }

#question(marks: \"three\")[A ball is thrown.]
";

#[test]
fn a_type_error_renders_with_its_declaration() {
    let world = world(TYPED);
    let text = diag::render(&world, &[type_error(&world)], false);
    assert_eq!(
        text,
        "\
error: expected `int`, found `str`
  ┌─ main.fol:3:18
  │
1 │ #element question(marks: int = 1) { kind: block }
  │                          --- the field `marks` is declared as `int` here
2 │\x20
3 │ #question(marks: \"three\")[A ball is thrown.]
  │                  ^^^^^^^
  │
  = hint: marks are whole numbers
  = fix: Write the marks as a number

"
    );
}

#[test]
fn a_type_error_serialises_with_related_spans_and_fixes() {
    let world = world(TYPED);
    let resolved = diag::resolve(&world, &type_error(&world));
    let location = resolved.location.as_ref().unwrap();
    assert_eq!(&TYPED[location.bytes.clone()], "\"three\"");
    assert_eq!(
        location.start,
        Position {
            line: 3,
            column: 18
        }
    );
    assert_eq!(resolved.related.len(), 1);
    assert_eq!(
        &TYPED[resolved.related[0].location.bytes.clone()],
        "int",
        "{resolved:?}"
    );
    assert_eq!(resolved.fixes[0].edits[0].location, *location);
    assert_eq!(resolved.fixes[0].edits[0].replacement, "3");

    let json = diag::to_json(&world, &[type_error(&world)]);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let diag = &value[0];
    assert_eq!(diag["severity"], "error");
    assert_eq!(diag["location"]["path"], "main.fol");
    assert_eq!(diag["location"]["start"]["column"], 18);
    assert_eq!(diag["related"][0]["location"]["start"]["line"], 1);
    assert_eq!(
        diag["related"][0]["message"],
        "the field `marks` is declared as `int` here"
    );
    assert_eq!(diag["hints"][0], "marks are whole numbers");
    assert_eq!(diag["fixes"][0]["title"], "Write the marks as a number");
    assert_eq!(diag["fixes"][0]["edits"][0]["replacement"], "3");
    assert_eq!(
        diag["fixes"][0]["edits"][0]["location"]["bytes"]["start"],
        68
    );
}

#[test]
fn a_lint_shows_its_code() {
    let world = world("#styled(color: rgb(\"d62728\"))[read carefully]\n");
    let literal = find(&world, "main.fol", "rgb(\"d62728\")");
    let lint = Diagnostic::warning(literal, "a colour literal in content")
        .with_code("override-literal")
        .with_fix(Fix::replace(
            "Use the token `tok.warn`",
            literal,
            "tok.warn",
        ));
    assert_eq!(
        lint.to_string(),
        "warning[override-literal]: a colour literal in content"
    );

    let text = diag::render(&world, std::slice::from_ref(&lint), false);
    assert!(
        text.starts_with("warning[override-literal]: a colour literal in content\n"),
        "{text}"
    );
    let json = diag::to_json(&world, &[lint]);
    assert!(json.contains(r#""code": "override-literal""#), "{json}");
}

#[test]
fn spans_with_a_sub_range_resolve_to_part_of_a_node() {
    let world = world(TYPED);
    let declared = find(&world, "main.fol", "int");
    let span = DiagSpan::from_span(declared, SubRange::new(1, 3));
    let location = diag::locate(&world, span).unwrap();
    assert_eq!(&TYPED[location.bytes], "nt");

    // A sub-range past the end of its node is from another revision.
    let span = DiagSpan::from_span(declared, SubRange::new(1, 9));
    assert_eq!(diag::locate(&world, span), None);
}

#[test]
fn unresolvable_spans_are_left_out() {
    let world = world(TYPED);
    let value = find(&world, "main.fol", "\"three\"");
    let diagnostic = Diagnostic::error(DiagSpan::detached(), "no source")
        .with_related(DiagSpan::detached(), "nowhere")
        .with_fix(
            Fix::new("Half a fix")
                .with_edit(value, "3")
                .with_edit(DiagSpan::detached(), "nothing"),
        );
    let resolved = diag::resolve(&world, &diagnostic);
    assert_eq!(resolved.location, None);
    assert!(resolved.related.is_empty());
    assert!(resolved.fixes.is_empty());
    assert_eq!(
        diag::render(&world, &[diagnostic], false),
        "error: no source\n = fix: Half a fix\n\n"
    );
}

#[test]
fn manifest_errors_render_from_text() {
    let manifest = "[project]\nentry = 3\nedition = \"2027\"\ncompiler = \"0.0.0\"\n";
    let errors = World::new(manifest).unwrap_err();
    let diagnostics: Vec<Diagnostic> = errors.into_iter().map(Into::into).collect();

    let mut files = TextFiles::new();
    files.insert(World::MANIFEST, "folio.toml", manifest);
    let text = diag::render(&files, &diagnostics, false);
    assert!(text.contains("┌─ folio.toml:2:9"), "{text}");
    assert!(text.contains("2 │ entry = 3"), "{text}");
}

#[test]
fn the_world_resolves_the_manifest() {
    let mut world = World::new(
        "[project]\nentry = \"main.fol\"\nedition = \"2027\"\ncompiler = \"0.0.0\"\n\
         [variants.exam]\ntargets = [\"pdf\"]\n",
    )
    .unwrap();
    let exam = Name::new("exam").unwrap();
    let compiled = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    let text = diag::render(&world, &compiled.diagnostics, false);
    assert!(text.contains("error: file `main.fol` not found"), "{text}");
    assert!(text.contains("┌─ folio.toml:2:9"), "{text}");

    // A new manifest moves the entry; the world resolves against its text.
    world
        .set_manifest(
            "# Physics\n[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
             compiler = \"0.0.0\"\n[variants.exam]\ntargets = [\"pdf\"]\n",
        )
        .unwrap();
    let compiled = folio::compile(&world, Target::Pdf, Some(&exam)).unwrap();
    let text = diag::render(&world, &compiled.diagnostics, false);
    assert!(text.contains("┌─ folio.toml:3:9"), "{text}");
    assert_eq!(
        Files::path(&world, World::MANIFEST).as_deref(),
        Some("folio.toml")
    );
}
