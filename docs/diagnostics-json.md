# Diagnostics as JSON

`folio build --diagnostics json` writes one JSON document to standard error when the command ends, in place of the source excerpts and the `wrote …` lines it writes by default (SPEC §12, §12.1). Standard error then holds that document and nothing else, so a program can parse all of it. The exit status is the same as without the flag.

Arguments that do not parse, and commands or flags that are not supported yet, still end with a plain message and exit status 2, without a document. So does a failure to hand the build to the compiler version that `folio.toml` pins (SPEC §2.4).

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
    { "variant": null, "target": "pdf", "path": "out/main.pdf", "status": "skipped" }
  ]
}
```

### The document

| Field | Type | Meaning |
| --- | --- | --- |
| `version` | integer | The version of this shape: `1`. |
| `diagnostics` | array of diagnostics | Every diagnostic of the command, in the order the terminal would show them. A diagnostic that several builds report appears once. |
| `builds` | array of builds | The builds that were compiled, in the manifest's order, each once. It is empty when the command stopped before compiling, for example because the manifest has errors. |

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
| `target` | string | The target, as `folio.toml` names it: `"pdf"`, `"docx"`, `"pptx"` or `"live"`. |
| `path` | string or `null` | The output's path, relative to the project directory, when the build has one. |
| `status` | string | `"written"`: the output was written. `"skipped"`: it was not, because the build has errors. `"none"`: the target writes no file, or has no writer yet. `"failed"`: the build or the write failed, and a diagnostic says why. |

## Compatibility

- A program should ignore fields it does not know. New fields, and new values of `status`, may appear without a change of version.
- Removing or renaming a field, or changing the meaning or type of one, raises `version`.
