# Folio development extension for VS Code

A small extension for trying `folio lsp` in VS Code while Folio has no
published editor extension. It is not part of the Rust workspace's build or
of `scripts/check.sh`, and needs Node.js only to install its one dependency.

What it does:

- runs `folio lsp`, which shows diagnostics while you type (SPEC §12.3);
- **Folio: Open Preview** (Command Palette) asks the language server to start
  the browser preview of the project and opens it, in your browser or, with
  `"folio.preview.openIn": "editor"`, in VS Code's Simple Browser;
- moving the cursor in a `.fol` file scrolls the preview to that place and
  marks it (turn off with `"folio.preview.followCursor": false`);
- clicking on a page of the preview selects the source it comes from.

The preview shows the files as saved: save, or turn on Auto Save, to see an
edit. In a file with unsaved changes the cursor and clicks may land a little
off.

## Running it

1. Build the binary: `cargo build --release --bin folio`. It is written to
   `target\release\folio.exe` (Windows) or `target/release/folio`.
2. Install the dependency, once: run `npm install` in this folder
   (`crates/folio-lsp/vscode`).
3. Close every VS Code window, so that the new one sees the environment
   variable, then start VS Code with this folder as a development extension
   and a project to open. Use your checkout's absolute paths.

   Windows (PowerShell):

   ```
   $env:FOLIO_LSP_PATH = "C:\path\to\folio\target\release\folio.exe"
   code --extensionDevelopmentPath=C:\path\to\folio\crates\folio-lsp\vscode C:\path\to\folio\tests\corpus\exam
   ```

   macOS and Linux:

   ```
   FOLIO_LSP_PATH=/path/to/folio/target/release/folio code --extensionDevelopmentPath=/path/to/folio/crates/folio-lsp/vscode /path/to/folio/tests/corpus/exam
   ```

   Instead of the variable, `"folio.path"` in the settings can name the
   binary. Copying the folder into `~/.vscode/extensions` does not work:
   current VS Code ignores extensions it has not registered. To install it
   for good, package it as a VSIX with `npx @vscode/vsce package` and install
   that.
4. In the window that opens ("[Extension Development Host]"), open
   `main.fol`. View → Output → "Folio" shows the server's log.
