# Diagnostics as JSON

`folio build --diagnostics json` writes one JSON document to standard error when the command ends, in place of the source excerpts and the `wrote …` lines it writes by default (SPEC §12, §12.1). `folio check --diagnostics json` writes the same document: the diagnostics that `folio build` would report, and each build it checked with the status `"checked"` and no path, as a check writes nothing. Apart from the cases below, standard error then holds that document and nothing else, so a program can parse all of it. The exit status is the same as without the flag.

Some messages are still plain text, with no document or before it:

- Arguments that do not parse, and commands or flags that are not supported yet, end with a plain message and exit status 2.
- Handing the build to the compiler version that `folio.toml` pins (SPEC §2.4) happens before the arguments are read. A failure to run that version ends with a plain message and exit status 1. A warning that the pinned version has an unfixed security problem is plain text before the pinned version's own output.
- A crash of the compiler (exit status 101) writes no document.

A reader that closes standard error before the document ends, as `head` does, gets what it read; the command writes nothing more and exits with the status it would have had.

## Version 1

```json
{
  "version": 1,
  "diagnostics": [
    {
      "severity": "error",
      "code": null,
      "message": "unclosed delimiter",
      "location": {
        "path": "main.fol",
        "bytes": { "start": 3385, "end": 3386 },
        "start": { "line": 56, "column": 5 },
        "end": { "line": 56, "column": 6 }
      },
      "related": [],
      "hints": [],
      "fixes": []
    }
  ],
  "builds": [
    { "variant": null, "version": null, "target": "pdf", "path": "out/main.pdf", "status": "skipped" }
  ]
}
```

### The document

| Field | Type | Meaning |
| --- | --- | --- |
| `version` | integer | The version of this shape: `1`. |
| `diagnostics` | array of diagnostics | Every diagnostic of the command, in the order the terminal would show them. A diagnostic that several builds report appears once. |
| `builds` | array of builds | The builds that were compiled or checked, in the manifest's order, each once. It is empty when the command stopped before compiling, for example because the manifest has errors. |

### A diagnostic

| Field | Type | Meaning |
| --- | --- | --- |
| `severity` | `"error"` or `"warning"` | An error stops the build's output from being written. |
| `code` | string or `null` | The lint or check that reported it, such as `"override-literal"`. |
| `message` | string | What is wrong. |
| `location` | location or `null` | The source it is about; `null` when it points at no file. |
| `related` | array of `{ "location", "message" }` | Other source that explains it, each with what it has to do with the diagnostic. |
| `hints` | array of strings | How to fix it. |
| `fixes` | array of `{ "title", "edits" }` | Quick fixes. Each edit is `{ "location", "replacement" }`: replace the source at `location` with `replacement`. The edits of a fix do not overlap. |

A related location or a fix whose span no longer resolves is left out, and so is a fix with any such edit.

### Codes

Most diagnostics have no code yet. These have one:

| Code | Severity | Reported when |
| --- | --- | --- |
| `internal-error` | error | A query panicked in a long-lived process; it has no location. |
| `namespace-element` | warning | A selector `name.` where `name` is both a component namespace and an element, so it reads as the namespace. |
| `overfull-line` | warning | A line is wider than its space: no break fits. At the text the line ends with, or at the object (an inline formula or image) that makes the line overflow. |
| `underfull-line` | warning | A justified line's spaces stretch further than its badness allows (TeX's limit), or it has no spaces to stretch. At the text the line ends with. |
| `widow` | warning | A page break leaves fewer lines of a paragraph at the top of a page than `widows` asks for. At the paragraph. |
| `orphan` | warning | A page break leaves fewer lines of a paragraph at the foot of a page than `orphans` asks for. At the paragraph. |
| `broken-keep-with-next` | warning | A page break separates a block from the next despite `keep-with-next`. At the block. |
| `broken-keep-together` | warning | A page break falls inside a block despite `keep-together`. At the block. |
| `broken-unbreakable` | warning | A page break falls inside a block despite `breakable: false`. At the block. |
| `page-overflow` | warning | Something is taller than the space a page has for it. At what does not fit. |

`folio build` and `folio check` leave out `underfull-line`, `widow`, `orphan`, `broken-keep-with-next`, `broken-keep-together` and `broken-unbreakable`; `folio check --layout` includes them (SPEC §7.9).

### A location

| Field | Type | Meaning |
| --- | --- | --- |
| `path` | string | The file, relative to the project directory, with `/` between directories. |
| `bytes` | `{ "start", "end" }` | The byte range in the file's text, UTF-8, end exclusive. |
| `start`, `end` | `{ "line", "column" }` | Where the range starts and ends. Lines and columns count from 1; columns count Unicode scalar values, not bytes or UTF-16 code units. The byte range is exact for any other measure. |

### A build

| Field | Type | Meaning |
| --- | --- | --- |
| `variant` | string or `null` | The variant; `null` for a project without variants. |
| `version` | string or `null` | The version of a shuffled paper (`project.versions`); `null` for a project without versions. |
| `target` | string | The target, as `folio.toml` names it: `"pdf"`, `"docx"`, `"pptx"` or `"live"`. |
| `path` | string or `null` | The output's path, relative to the project directory, when the build has one; always `null` from `folio check`. |
| `status` | string | `"written"`: the output was written. `"skipped"`: it was not, because the build has errors. `"none"`: the target writes no file, or has no writer yet. `"failed"`: the build or the write failed, and a diagnostic says why. `"checked"`: `folio check` checked the build, which writes nothing; the path is then `null`. From `folio check`, `"failed"` means the check failed, or the output could not be placed where `[output] path` puts it. |

## Compatibility

- A program should ignore fields it does not know. New fields, and new values of `status`, may appear without a change of version.
- Removing or renaming a field, or changing the meaning or type of one, raises `version`.
