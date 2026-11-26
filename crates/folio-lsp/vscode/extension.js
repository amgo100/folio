// A development extension for Folio: it runs `folio lsp` (SPEC §12.3) and
// opens the browser preview that the language server runs (SPEC §12.2),
// keeping the two in step: the cursor scrolls the preview (`folio/reveal`),
// and a click on a page selects its source (`window/showDocument`, which
// the language client handles).
const vscode = require('vscode');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');

let client;
let previewRunning = false;
let timer;

exports.activate = (context) => {
  const config = vscode.workspace.getConfiguration('folio');
  const command = config.get('path') || process.env.FOLIO_LSP_PATH || 'folio';
  client = new LanguageClient('folio', 'Folio',
    { command, args: ['lsp'], transport: TransportKind.stdio },
    {
      // Sources, and the project files, whose saves the server reads.
      documentSelector: [
        { scheme: 'file', language: 'folio' },
        { scheme: 'file', pattern: '**/folio.{toml,lock}' },
      ],
      // Changes made outside the editor too.
      synchronize: { fileEvents: vscode.workspace.createFileSystemWatcher('**/*') },
    });

  context.subscriptions.push(vscode.commands.registerCommand('folio.openPreview', async () => {
    try {
      const result = await client.sendRequest('workspace/executeCommand', {
        command: 'folio.preview',
        arguments: [],
      });
      previewRunning = true;
      const uri = vscode.Uri.parse(result.url);
      if (vscode.workspace.getConfiguration('folio').get('preview.openIn') === 'editor') {
        await vscode.commands.executeCommand('simpleBrowser.show', result.url);
      } else {
        await vscode.env.openExternal(uri);
      }
      const editor = vscode.window.activeTextEditor;
      if (editor) reveal(editor);
    } catch (e) {
      vscode.window.showErrorMessage(`Folio: cannot open the preview: ${e.message || e}`);
    }
  }));

  context.subscriptions.push(vscode.window.onDidChangeTextEditorSelection((event) => {
    if (!previewRunning || event.textEditor.document.languageId !== 'folio') return;
    if (!vscode.workspace.getConfiguration('folio').get('preview.followCursor')) return;
    clearTimeout(timer);
    timer = setTimeout(() => reveal(event.textEditor), 100);
  }));

  return client.start();
};

function reveal(editor) {
  if (!client || editor.document.languageId !== 'folio') return;
  const position = editor.selection.active;
  client.sendNotification('folio/reveal', {
    textDocument: { uri: client.code2ProtocolConverter.asUri(editor.document.uri) },
    position: { line: position.line, character: position.character },
  });
}

exports.deactivate = () => client && client.stop();
