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
  resolveInlineSourcePositionRange,
  resolveInlineTypeScriptView,
} = require("./inline-typescript");
const {
  INLINE_RUST_COMMAND_ID,
  resolveInlineRustSourcePositionRange,
  resolveInlineRustView,
} = require("./inline-rust");

let client;
const inlineRustSnapshots = new Map();
const inlineTypeScriptSnapshots = new Map();
const INLINE_RUST_SELECTOR = [{ language: "rust", scheme: "untitled" }];
const INLINE_TYPESCRIPT_SELECTOR = [{ language: "typescript", scheme: "untitled" }];

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
    vscode.languages.registerDefinitionProvider(INLINE_RUST_SELECTOR, {
      provideDefinition: provideInlineRustDefinition,
    }),
    vscode.languages.registerReferenceProvider(INLINE_RUST_SELECTOR, {
      provideReferences: provideInlineRustReferences,
    }),
    vscode.languages.registerDefinitionProvider(INLINE_TYPESCRIPT_SELECTOR, {
      provideDefinition: provideInlineTypeScriptDefinition,
    }),
    vscode.languages.registerTypeDefinitionProvider(INLINE_TYPESCRIPT_SELECTOR, {
      provideTypeDefinition: provideInlineTypeScriptTypeDefinition,
    }),
    vscode.languages.registerReferenceProvider(INLINE_TYPESCRIPT_SELECTOR, {
      provideReferences: provideInlineTypeScriptReferences,
    }),
    vscode.workspace.onDidCloseTextDocument((document) => {
      inlineRustSnapshots.delete(document.uri.toString());
      inlineTypeScriptSnapshots.delete(document.uri.toString());
    }),
    vscode.commands.registerCommand(GENERATED_CLIENT_COMMAND_ID, openGeneratedClientMirror),
    vscode.commands.registerCommand(GENERATED_TYPES_COMMAND_ID, openGeneratedTypesMirror),
    vscode.commands.registerCommand(INLINE_RUST_COMMAND_ID, openInlineRustView),
    vscode.commands.registerCommand(INLINE_TYPESCRIPT_COMMAND_ID, openInlineTypeScriptView),
  );
}

async function openInlineRustView() {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== "thebe" || editor.document.uri.scheme !== "file") {
    void vscode.window.showErrorMessage("Open a saved Thebe route to view its inline Rust snapshot.");
    return;
  }

  const view = resolveInlineRustView({
    documentPath: editor.document.uri.fsPath,
    workspaceFolders: (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath),
    source: editor.document.getText(),
    selectionStartOffset: editor.document.offsetAt(editor.selection.start),
    selectionEndOffset: editor.document.offsetAt(editor.selection.end),
  });
  if (!view.ok) {
    const message = view.reason === "no-script"
      ? "No <script setup> block was found in this route."
      : "Inline Rust snapshots are only available for route .trs files under src/routes.";
    void vscode.window.showErrorMessage(message);
    return;
  }

  try {
    const document = await vscode.workspace.openTextDocument({
      language: "rust",
      content: view.content,
    });
    inlineRustSnapshots.set(document.uri.toString(), view);
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
    void vscode.window.showErrorMessage("Unable to open the inline Rust snapshot for this route.");
  }
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
    inlineTypeScriptSnapshots.set(document.uri.toString(), view);
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

function provideInlineRustDefinition(document, position) {
  const snapshot = inlineRustSnapshots.get(document.uri.toString());
  if (!snapshot || document.getText() !== snapshot.content) {
    return undefined;
  }

  const sourceLocation = resolveInlineRustSourceLocation(document, position, snapshot);
  return sourceLocation ?? undefined;
}

function provideInlineRustReferences(document, position) {
  const snapshot = inlineRustSnapshots.get(document.uri.toString());
  if (!snapshot || document.getText() !== snapshot.content) {
    return undefined;
  }

  const sourceLocation = resolveInlineRustSourceLocation(document, position, snapshot);
  return sourceLocation ? [sourceLocation] : undefined;
}

function provideInlineTypeScriptDefinition(document, position) {
  const snapshot = inlineTypeScriptSnapshots.get(document.uri.toString());
  if (!snapshot || document.getText() !== snapshot.content) {
    return undefined;
  }

  const sourceLocation = resolveInlineSourceLocation(document, position, snapshot);
  return sourceLocation ?? undefined;
}

function provideInlineTypeScriptTypeDefinition(document, position) {
  const snapshot = inlineTypeScriptSnapshots.get(document.uri.toString());
  if (!snapshot || document.getText() !== snapshot.content || !snapshot.generatedTypesPath) {
    return undefined;
  }

  const range = document.getWordRangeAtPosition(position);
  if (!range || document.getText(range) !== "Props") {
    return undefined;
  }

  return new vscode.Location(vscode.Uri.file(snapshot.generatedTypesPath), new vscode.Position(0, 0));
}

function provideInlineTypeScriptReferences(document, position) {
  const snapshot = inlineTypeScriptSnapshots.get(document.uri.toString());
  if (!snapshot || document.getText() !== snapshot.content) {
    return undefined;
  }

  const sourceLocation = resolveInlineSourceLocation(document, position, snapshot);
  return sourceLocation ? [sourceLocation] : undefined;
}

function resolveInlineRustSourceLocation(document, position, snapshot) {
  const range = document.getWordRangeAtPosition(position) ?? new vscode.Range(position, position);
  const sourceRange = resolveInlineRustSourcePositionRange({
    view: snapshot,
    startOffset: document.offsetAt(range.start),
    endOffset: document.offsetAt(range.end),
  });
  if (!sourceRange) {
    return null;
  }

  const sourceUri = vscode.Uri.file(snapshot.sourcePath);
  const start = new vscode.Position(sourceRange.start.line, sourceRange.start.character);
  const end = new vscode.Position(sourceRange.end.line, sourceRange.end.character);
  return new vscode.Location(sourceUri, new vscode.Range(start, end));
}

function resolveInlineSourceLocation(document, position, snapshot) {
  const range = document.getWordRangeAtPosition(position) ?? new vscode.Range(position, position);
  const sourceRange = resolveInlineSourcePositionRange({
    view: snapshot,
    startOffset: document.offsetAt(range.start),
    endOffset: document.offsetAt(range.end),
  });
  if (!sourceRange) {
    return null;
  }

  const sourceUri = vscode.Uri.file(snapshot.sourcePath);
  const start = new vscode.Position(sourceRange.start.line, sourceRange.start.character);
  const end = new vscode.Position(sourceRange.end.line, sourceRange.end.character);
  return new vscode.Location(sourceUri, new vscode.Range(start, end));
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
