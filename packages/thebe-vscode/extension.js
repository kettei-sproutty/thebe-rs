const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");
const {
  createDocumentSelector,
  FILE_WATCH_GLOB,
  resolveServerCommand,
} = require("./client-config");
const {
  GENERATED_CLIENT_COMMAND_ID,
  GENERATED_TYPES_COMMAND_ID,
  resolveGeneratedClientMirrorPath,
  resolveGeneratedTypesMirrorPath,
  selectGeneratedClientLocation,
  selectGeneratedTypesLocation,
} = require("./generated-client");
const {
  INLINE_TYPESCRIPT_COMMAND_ID,
  resolveInlineTypeScriptView,
} = require("./inline-typescript");

let client;

async function activate(context) {
  const command = resolveServerCommand({
    configuredPath: vscode.workspace.getConfiguration("thebe").get("lsp.path"),
    extensionPath: context.extensionPath,
    workspaceFolders: (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath),
  });
  const clientOptions = {
    documentSelector: createDocumentSelector(),
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher(FILE_WATCH_GLOB),
    },
  };

  client = new LanguageClient(
    "thebe-lsp",
    "Thebe Language Server",
    {
      run: { command },
      debug: { command },
    },
    clientOptions,
  );

  context.subscriptions.push(
    client.start(),
    vscode.commands.registerCommand(GENERATED_CLIENT_COMMAND_ID, openGeneratedClientMirror),
    vscode.commands.registerCommand(GENERATED_TYPES_COMMAND_ID, openGeneratedTypesMirror),
    vscode.commands.registerCommand(INLINE_TYPESCRIPT_COMMAND_ID, openInlineTypeScriptView),
  );
}

async function openInlineTypeScriptView() {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== "thebe" || editor.document.uri.scheme !== "file") {
    void vscode.window.showErrorMessage("Open a saved Thebe route to view its inline TypeScript snapshot.");
    return;
  }

  const view = resolveInlineTypeScriptView({
    documentPath: editor.document.uri.fsPath,
    workspaceFolders: (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath),
    source: editor.document.getText(),
    selectionStartOffset: editor.document.offsetAt(editor.selection.start),
    selectionEndOffset: editor.document.offsetAt(editor.selection.end),
  });
  if (!view.ok) {
    const message = view.reason === "no-script"
      ? "No <script lang=\"ts\"> block was found in this route."
      : "Inline TypeScript snapshots are only available for route .trs files under src/routes.";
    void vscode.window.showErrorMessage(message);
    return;
  }

  try {
    const document = await vscode.workspace.openTextDocument({
      language: "typescript",
      content: view.content,
    });
    const targetEditor = await vscode.window.showTextDocument(document, {
      preview: false,
      viewColumn: vscode.ViewColumn.Beside,
    });
    const selection = new vscode.Selection(
      document.positionAt(view.selectionStartOffset),
      document.positionAt(view.selectionEndOffset),
    );
    targetEditor.selection = selection;
    targetEditor.revealRange(selection);
  } catch {
    void vscode.window.showErrorMessage("Unable to open the inline TypeScript snapshot for this route.");
  }
}

async function openGeneratedClientMirror() {
  await openGeneratedRouteArtifact({
    resolveMirrorPath: resolveGeneratedClientMirrorPath,
    selectLocation: selectGeneratedClientLocation,
    invalidRouteMessage: "Generated client mirrors are only available for route .trs files under src/routes.",
    notFoundMessage:
      "No generated client mirror was found for this route. Run thebe dev or thebe check after adding <script lang=\"ts\"> to refresh .thebe artifacts.",
  });
}

async function openGeneratedTypesMirror() {
  await openGeneratedRouteArtifact({
    resolveMirrorPath: resolveGeneratedTypesMirrorPath,
    selectLocation: selectGeneratedTypesLocation,
    invalidRouteMessage: "Generated props type mirrors are only available for route .trs files under src/routes.",
    notFoundMessage:
      "No generated props type mirror was found for this route. Run thebe dev or thebe check after adding <script lang=\"ts\"> to refresh .thebe artifacts.",
  });
}

async function openGeneratedRouteArtifact({
  resolveMirrorPath,
  selectLocation,
  invalidRouteMessage,
  notFoundMessage,
}) {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== "thebe" || editor.document.uri.scheme !== "file") {
    void vscode.window.showErrorMessage("Open a saved Thebe route to view its generated artifacts.");
    return;
  }

  const mirrorPath = resolveMirrorPath({
    documentPath: editor.document.uri.fsPath,
    workspaceFolders: (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath),
  });
  if (!mirrorPath) {
    void vscode.window.showErrorMessage(invalidRouteMessage);
    return;
  }

  let targetUri = vscode.Uri.file(mirrorPath);
  let targetRange = null;

  try {
    const locations = await vscode.commands.executeCommand(
      "vscode.executeDefinitionProvider",
      editor.document.uri,
      editor.selection.active,
    );
    const target = selectLocation({ locations, mirrorPath });
    if (target) {
      targetUri = target.uri;
      targetRange = target.range;
    }
  } catch {
    // Fall back to the deterministic generated mirror path when definition providers fail.
  }

  try {
    const document = await vscode.workspace.openTextDocument(targetUri);
    const targetEditor = await vscode.window.showTextDocument(document, {
      preview: false,
      viewColumn: vscode.ViewColumn.Beside,
    });
    if (targetRange) {
      targetEditor.selection = new vscode.Selection(targetRange.start, targetRange.end);
      targetEditor.revealRange(targetRange);
    }
  } catch {
    void vscode.window.showErrorMessage(notFoundMessage);
  }
}

async function deactivate() {
  if (client) {
    await client.stop();
  }
}

module.exports = {
  activate,
  deactivate,
};
