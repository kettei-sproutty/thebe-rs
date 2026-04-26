const fs = require("node:fs");
const path = require("node:path");
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
  INLINE_TYPESCRIPT_SCHEME,
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
const inlineTypeScriptSnapshotSourcePaths = new Map();
const inlineTypeScriptSnapshotChanges = new vscode.EventEmitter();
const inlineTypeScriptDiagnostics = vscode.languages.createDiagnosticCollection("thebe-inline-typescript");
const inlineTypeScriptDiagnosticRefreshes = new Map();
const INLINE_RUST_SELECTOR = [{ language: "rust", scheme: "untitled" }];
const INLINE_TYPESCRIPT_SELECTOR = [{ scheme: INLINE_TYPESCRIPT_SCHEME }];
const INLINE_TYPESCRIPT_COMPLETION_TRIGGERS = [".", "\"", "'", "/", "@"];
const INLINE_TYPESCRIPT_DIAGNOSTIC_REFRESH_DELAY_MS = 75;
const INLINE_TYPESCRIPT_DIAGNOSTIC_RETRY_DELAY_MS = 150;
const INLINE_TYPESCRIPT_DIAGNOSTIC_MAX_ATTEMPTS = 8;
const INLINE_TYPESCRIPT_DIAGNOSTIC_COMMANDS = [
  "syntacticDiagnosticsSync",
  "semanticDiagnosticsSync",
  "suggestionDiagnosticsSync",
];
const INLINE_RUST_VIEW_LSP_COMMAND_ID = "thebe.inlineRustView";
const INLINE_TYPESCRIPT_VIEW_LSP_COMMAND_ID = "thebe.inlineTypeScriptView";
const INLINE_TYPESCRIPT_MAP_PROTOCOL_DIAGNOSTICS_LSP_COMMAND_ID = "thebe.mapInlineTypeScriptProtocolDiagnostics";
const TYPESCRIPT_TSSERVER_REQUEST_COMMAND_ID = "typescript.tsserverRequest";
const TYPESCRIPT_EXTENSION_ID = "vscode.typescript-language-features";

function resolveTypeScriptRuntimeConfig() {
  const extension = vscode.extensions.getExtension(TYPESCRIPT_EXTENSION_ID);
  if (!extension) {
    return null;
  }

  const libraryPathCandidates = [
    path.join(extension.extensionPath, "node_modules", "typescript", "lib", "typescript.js"),
    path.join(extension.extensionPath, "..", "node_modules", "typescript", "lib", "typescript.js"),
  ];
  const typescriptLibraryPath = libraryPathCandidates.find((filePath) => fs.existsSync(filePath));
  if (!typescriptLibraryPath) {
    return null;
  }

  return {
    nodePath: process.execPath,
    typescriptLibraryPath,
  };
}

async function activate(context) {
  const command = resolveServerCommand({
    configuredPath: vscode.workspace.getConfiguration("thebe").get("lsp.path"),
    extensionPath: context.extensionPath,
    workspaceFolders: (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath),
  });
  const typescriptRuntime = resolveTypeScriptRuntimeConfig();
  const inlineTypeScriptLspBridgeEnabled = Boolean(typescriptRuntime);
  let inlineTypeScriptLspBridgeReady = !inlineTypeScriptLspBridgeEnabled;
  const clientOptions = {
    documentSelector: createDocumentSelector(),
    initializationOptions: typescriptRuntime ? { typescriptRuntime } : undefined,
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

  const inlineTypeScriptFallbackDisposables = [
    inlineTypeScriptDiagnostics,
    {
      dispose: clearInlineTypeScriptDiagnosticRefreshes,
    },
    vscode.languages.registerCompletionItemProvider(
      createDocumentSelector(),
      {
        provideCompletionItems: provideThebeTypeScriptCompletionItems,
      },
      ...INLINE_TYPESCRIPT_COMPLETION_TRIGGERS,
    ),
    vscode.languages.onDidChangeDiagnostics((event) => {
      updateInlineTypeScriptDiagnostics(event.uris);
    }),
  ];

  const subscriptions = [
    client.start(),
    inlineTypeScriptSnapshotChanges,
    vscode.workspace.registerTextDocumentContentProvider(INLINE_TYPESCRIPT_SCHEME, {
      onDidChange: inlineTypeScriptSnapshotChanges.event,
      provideTextDocumentContent: provideInlineTypeScriptContent,
    }),
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
    vscode.workspace.onDidOpenTextDocument((document) => {
      void ensureInlineTypeScriptSnapshotDocument({ document });
      if (!inlineTypeScriptLspBridgeReady) {
        scheduleInlineTypeScriptDiagnosticsRefresh(document);
      }
    }),
    vscode.workspace.onDidChangeTextDocument((event) => {
      void refreshInlineTypeScriptSnapshot(event.document);
      void ensureInlineTypeScriptSnapshotDocument({ document: event.document });
      if (!inlineTypeScriptLspBridgeReady) {
        scheduleInlineTypeScriptDiagnosticsRefresh(event.document);
      }
    }),
    vscode.workspace.onDidCloseTextDocument((document) => {
      inlineRustSnapshots.delete(document.uri.toString());
      if (document.languageId === "thebe" && document.uri.scheme === "file") {
        clearInlineTypeScriptDiagnosticRefresh(document.uri.fsPath);
        clearInlineTypeScriptSnapshotForSourcePath(document.uri.fsPath);
      }
      deleteInlineTypeScriptSnapshot(document.uri);
    }),
    vscode.commands.registerCommand(GENERATED_CLIENT_COMMAND_ID, openGeneratedClientMirror),
    vscode.commands.registerCommand(GENERATED_TYPES_COMMAND_ID, openGeneratedTypesMirror),
    vscode.commands.registerCommand(INLINE_RUST_COMMAND_ID, openInlineRustView),
    vscode.commands.registerCommand(INLINE_TYPESCRIPT_COMMAND_ID, openInlineTypeScriptView),
    ...inlineTypeScriptFallbackDisposables,
  ];

  context.subscriptions.push(...subscriptions);

  if (inlineTypeScriptLspBridgeEnabled) {
    void client.onReady().then(async () => {
      await syncOpenThebeDocumentsToLsp();
      inlineTypeScriptLspBridgeReady = true;
      clearInlineTypeScriptDiagnosticRefreshes();
      inlineTypeScriptDiagnostics.clear();
      for (const disposable of inlineTypeScriptFallbackDisposables) {
        disposable.dispose();
      }
    }).catch(() => {});
  }

  for (const document of vscode.workspace.textDocuments) {
    void ensureInlineTypeScriptSnapshotDocument({ document });
    if (!inlineTypeScriptLspBridgeReady) {
      scheduleInlineTypeScriptDiagnosticsRefresh(document);
    }
  }
}

async function syncOpenThebeDocumentsToLsp() {
  if (!client) {
    return;
  }

  const documents = vscode.workspace.textDocuments.filter(
    (document) => document.languageId === "thebe" && document.uri.scheme === "file",
  );
  await Promise.all(documents.map((document) => client.sendNotification("textDocument/didChange", {
    textDocument: {
      uri: document.uri.toString(),
      version: document.version,
    },
    contentChanges: [{
      text: document.getText(),
    }],
  })));
}

async function openInlineRustView() {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== "thebe" || editor.document.uri.scheme !== "file") {
    void vscode.window.showErrorMessage("Open a saved Thebe route to view its inline Rust snapshot.");
    return;
  }

  const selectionStartOffset = editor.document.offsetAt(editor.selection.start);
  const selectionEndOffset = editor.document.offsetAt(editor.selection.end);
  const view = await resolveInlineRustViewForDocument({
    document: editor.document,
    selectionStartOffset,
    selectionEndOffset,
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

async function resolveInlineRustViewForDocument({
  document,
  selectionStartOffset = 0,
  selectionEndOffset = selectionStartOffset,
}) {
  if (!document || document.languageId !== "thebe" || document.uri.scheme !== "file") {
    return null;
  }

  const serverView = await requestInlineRustViewFromServer({
    document,
    selectionStartOffset,
    selectionEndOffset,
  });
  if (serverView) {
    return serverView;
  }

  return resolveInlineRustView({
    documentPath: document.uri.fsPath,
    workspaceFolders: (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath),
    source: document.getText(),
    selectionStartOffset,
    selectionEndOffset,
  });
}

async function requestInlineRustViewFromServer({
  document,
  selectionStartOffset,
  selectionEndOffset,
}) {
  if (!client) {
    return null;
  }

  try {
    await client.onReady();
    const response = await client.sendRequest("workspace/executeCommand", {
      command: INLINE_RUST_VIEW_LSP_COMMAND_ID,
      arguments: [{
        uri: document.uri.toString(),
        source: document.getText(),
        selectionStartOffset,
        selectionEndOffset,
      }],
    });
    return normalizeInlineRustView(response);
  } catch {
    return null;
  }
}

function normalizeInlineRustView(response) {
  if (!response || typeof response !== "object" || typeof response.ok !== "boolean") {
    return null;
  }

  if (!response.ok) {
    return typeof response.reason === "string" ? response : null;
  }

  if (
    typeof response.targetPath !== "string"
    || typeof response.sourcePath !== "string"
    || typeof response.sourceText !== "string"
    || typeof response.content !== "string"
    || typeof response.prefixLength !== "number"
    || typeof response.scriptStartOffset !== "number"
    || typeof response.scriptEndOffset !== "number"
    || typeof response.selectionStartOffset !== "number"
    || typeof response.selectionEndOffset !== "number"
  ) {
    return null;
  }

  return response;
}

async function openInlineTypeScriptView() {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== "thebe" || editor.document.uri.scheme !== "file") {
    void vscode.window.showErrorMessage("Open a saved Thebe route to view its inline TypeScript snapshot.");
    return;
  }

  const selectionStartOffset = editor.document.offsetAt(editor.selection.start);
  const selectionEndOffset = editor.document.offsetAt(editor.selection.end);

  const bridge = await ensureInlineTypeScriptSnapshotDocument({
    document: editor.document,
    selectionStartOffset,
    selectionEndOffset,
  });
  if (!bridge) {
    const view = await resolveInlineTypeScriptViewForDocument({
      document: editor.document,
      selectionStartOffset,
      selectionEndOffset,
    });
    const message = view.reason === "no-script"
      ? "No <script lang=\"ts\"> block was found in this route."
      : "Inline TypeScript snapshots are only available for route .trs files under src/routes.";
    void vscode.window.showErrorMessage(message);
    return;
  }

  try {
    let displayDocument = await vscode.workspace.openTextDocument(bridge.displayUri);
    if (displayDocument.languageId !== "typescript") {
      displayDocument = await vscode.languages.setTextDocumentLanguage(displayDocument, "typescript");
    }

    const targetEditor = await vscode.window.showTextDocument(displayDocument, {
      preview: false,
      viewColumn: vscode.ViewColumn.Beside,
    });
    const selection = new vscode.Selection(
      displayDocument.positionAt(bridge.view.selectionStartOffset),
      displayDocument.positionAt(bridge.view.selectionEndOffset),
    );
    targetEditor.selection = selection;
    targetEditor.revealRange(selection);
  } catch {
    void vscode.window.showErrorMessage("Unable to open the inline TypeScript snapshot for this route.");
  }
}

function provideInlineTypeScriptContent(uri) {
  return inlineTypeScriptSnapshots.get(uri.toString())?.content ?? "";
}

async function resolveInlineTypeScriptViewForDocument({
  document,
  selectionStartOffset = 0,
  selectionEndOffset = selectionStartOffset,
}) {
  if (!document || document.languageId !== "thebe" || document.uri.scheme !== "file") {
    return null;
  }

  const serverView = await requestInlineTypeScriptViewFromServer({
    document,
    selectionStartOffset,
    selectionEndOffset,
  });
  if (serverView) {
    return serverView;
  }

  return resolveInlineTypeScriptView({
    documentPath: document.uri.fsPath,
    workspaceFolders: (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath),
    source: document.getText(),
    selectionStartOffset,
    selectionEndOffset,
  });
}

async function requestInlineTypeScriptViewFromServer({
  document,
  selectionStartOffset,
  selectionEndOffset,
}) {
  if (!client) {
    return null;
  }

  try {
    await client.onReady();
    const response = await client.sendRequest("workspace/executeCommand", {
      command: INLINE_TYPESCRIPT_VIEW_LSP_COMMAND_ID,
      arguments: [{
        uri: document.uri.toString(),
        source: document.getText(),
        selectionStartOffset,
        selectionEndOffset,
      }],
    });
    return normalizeInlineTypeScriptView(response);
  } catch {
    return null;
  }
}

function normalizeInlineTypeScriptView(response) {
  if (!response || typeof response !== "object" || typeof response.ok !== "boolean") {
    return null;
  }

  if (!response.ok) {
    return typeof response.reason === "string" ? response : null;
  }

  if (
    typeof response.targetPath !== "string"
    || typeof response.sourcePath !== "string"
    || typeof response.sourceText !== "string"
    || typeof response.content !== "string"
    || typeof response.prefixLength !== "number"
    || typeof response.scriptStartOffset !== "number"
    || typeof response.scriptEndOffset !== "number"
    || typeof response.selectionStartOffset !== "number"
    || typeof response.selectionEndOffset !== "number"
  ) {
    return null;
  }

  if (
    response.generatedTypesPath !== null
    && typeof response.generatedTypesPath !== "string"
  ) {
    return null;
  }

  return response;
}

async function ensureInlineTypeScriptSnapshotDocument({
  document,
  selectionStartOffset = 0,
  selectionEndOffset = selectionStartOffset,
}) {
  if (!document || document.languageId !== "thebe" || document.uri.scheme !== "file") {
    return null;
  }

  const view = await resolveInlineTypeScriptViewForDocument({
    document,
    selectionStartOffset,
    selectionEndOffset,
  });
  if (!view.ok) {
    clearInlineTypeScriptSnapshotForSourcePath(document.uri.fsPath);
    return null;
  }

  const displayUri = rememberInlineTypeScriptSnapshot(view);
  const tsserverUri = rememberInlineTypeScriptSnapshot(
    view,
    createInlineTypeScriptTsserverUri({
      targetPath: view.targetPath,
      sourceVersion: document.version,
    }),
  );
  let virtualDocument = await vscode.workspace.openTextDocument(tsserverUri);
  if (virtualDocument.languageId !== "typescript") {
    virtualDocument = await vscode.languages.setTextDocumentLanguage(virtualDocument, "typescript");
  }

  return {
    displayUri,
    view,
    virtualDocument,
    virtualUri: tsserverUri,
  };
}

function rememberInlineTypeScriptSnapshot(view, documentUri = createInlineTypeScriptSnapshotUri(view.targetPath)) {
  const uriString = documentUri.toString();
  inlineTypeScriptSnapshots.set(uriString, view);
  if (documentUri.fragment.length === 0) {
    inlineTypeScriptSnapshotSourcePaths.set(view.sourcePath, uriString);
  }
  inlineTypeScriptSnapshotChanges.fire(documentUri);
  return documentUri;
}

function createInlineTypeScriptSnapshotUri(targetPath) {
  return vscode.Uri.file(targetPath).with({ scheme: INLINE_TYPESCRIPT_SCHEME });
}

function createInlineTypeScriptTsserverUri({ targetPath, sourceVersion }) {
  const parsed = path.parse(targetPath);
  const versionedPath = path.join(parsed.dir, `${parsed.name}.inline-${sourceVersion}${parsed.ext}`);
  return vscode.Uri.file(versionedPath).with({ scheme: INLINE_TYPESCRIPT_SCHEME });
}

async function refreshInlineTypeScriptSnapshot(document) {
  if (!document || document.languageId !== "thebe" || document.uri.scheme !== "file") {
    return;
  }

  const snapshotUriString = inlineTypeScriptSnapshotSourcePaths.get(document.uri.fsPath);
  if (!snapshotUriString) {
    return;
  }

  const sourceEditor = vscode.window.visibleTextEditors.find(
    (editor) => editor.document.uri.toString() === document.uri.toString(),
  );
  const selectionStartOffset = sourceEditor ? document.offsetAt(sourceEditor.selection.start) : 0;
  const selectionEndOffset = sourceEditor ? document.offsetAt(sourceEditor.selection.end) : selectionStartOffset;
  const view = await resolveInlineTypeScriptViewForDocument({
    document,
    selectionStartOffset,
    selectionEndOffset,
  });

  const snapshotUri = vscode.Uri.parse(snapshotUriString);
  if (!view.ok) {
    clearInlineTypeScriptSnapshotForSourcePath(document.uri.fsPath);
    return;
  }

  inlineTypeScriptSnapshots.set(snapshotUriString, view);
  inlineTypeScriptSnapshotChanges.fire(snapshotUri);
}

async function provideThebeTypeScriptCompletionItems(document, position, token, context) {
  const selectionOffset = document.offsetAt(position);
  const bridge = await ensureInlineTypeScriptSnapshotDocument({
    document,
    selectionStartOffset: selectionOffset,
    selectionEndOffset: selectionOffset,
  });
  if (!bridge || token.isCancellationRequested) {
    return undefined;
  }

  const virtualPosition = bridge.virtualDocument.positionAt(bridge.view.selectionStartOffset);
  const completions = await vscode.commands.executeCommand(
    "vscode.executeCompletionItemProvider",
    bridge.virtualUri,
    virtualPosition,
    context.triggerCharacter,
  );
  if (!completions || token.isCancellationRequested) {
    return undefined;
  }

  const fallbackRange = document.getWordRangeAtPosition(position) ?? new vscode.Range(position, position);
  return mapInlineTypeScriptCompletionList({
    completionList: completions,
    snapshot: bridge.view,
    sourceDocument: document,
    virtualDocument: bridge.virtualDocument,
    fallbackRange,
  });
}

function mapInlineTypeScriptCompletionList({
  completionList,
  snapshot,
  sourceDocument,
  virtualDocument,
  fallbackRange,
}) {
  const items = completionList.items.map((item) => mapInlineTypeScriptCompletionItem({
    item,
    snapshot,
    sourceDocument,
    virtualDocument,
    fallbackRange,
  }));
  return new vscode.CompletionList(items, completionList.isIncomplete);
}

function mapInlineTypeScriptCompletionItem({
  item,
  snapshot,
  sourceDocument,
  virtualDocument,
  fallbackRange,
}) {
  const mapped = new vscode.CompletionItem(item.label, item.kind);
  Object.assign(mapped, item);

  const mappedRange = mapInlineTypeScriptCompletionRange(item.range, snapshot, virtualDocument);
  const mappedTextEdit = mapInlineTypeScriptTextEdit(item.textEdit, snapshot, virtualDocument);
  const mappedAdditionalTextEdits = Array.isArray(item.additionalTextEdits)
    ? item.additionalTextEdits
      .map((edit) => mapInlineTypeScriptTextEdit(edit, snapshot, virtualDocument))
      .filter(Boolean)
    : undefined;

  if (mappedRange) {
    mapped.range = mappedRange;
  } else if (!mappedTextEdit) {
    mapped.range = fallbackRange;
  }

  if (mappedTextEdit) {
    mapped.textEdit = mappedTextEdit;
  } else {
    delete mapped.textEdit;
  }

  if (mappedAdditionalTextEdits) {
    mapped.additionalTextEdits = mappedAdditionalTextEdits;
  } else {
    delete mapped.additionalTextEdits;
  }

  return mapped;
}

function mapInlineTypeScriptCompletionRange(range, snapshot, virtualDocument) {
  if (!range) {
    return null;
  }

  if (range.start && range.end) {
    return mapInlineTypeScriptVirtualRange(range, snapshot, virtualDocument);
  }

  if (range.inserting && range.replacing) {
    const inserting = mapInlineTypeScriptVirtualRange(range.inserting, snapshot, virtualDocument);
    const replacing = mapInlineTypeScriptVirtualRange(range.replacing, snapshot, virtualDocument);
    if (!inserting || !replacing) {
      return null;
    }

    return { inserting, replacing };
  }

  return null;
}

function mapInlineTypeScriptTextEdit(edit, snapshot, virtualDocument) {
  if (!edit || !edit.range) {
    return null;
  }

  const range = mapInlineTypeScriptVirtualRange(edit.range, snapshot, virtualDocument);
  if (!range) {
    return null;
  }

  return new vscode.TextEdit(range, edit.newText);
}

function mapInlineTypeScriptVirtualRange(range, snapshot, virtualDocument) {
  const sourceRange = resolveInlineSourcePositionRange({
    view: snapshot,
    startOffset: virtualDocument.offsetAt(range.start),
    endOffset: virtualDocument.offsetAt(range.end),
  });
  if (!sourceRange) {
    return null;
  }

  return new vscode.Range(
    new vscode.Position(sourceRange.start.line, sourceRange.start.character),
    new vscode.Position(sourceRange.end.line, sourceRange.end.character),
  );
}

function scheduleInlineTypeScriptDiagnosticsRefresh(document) {
  if (!document || document.languageId !== "thebe" || document.uri.scheme !== "file") {
    return;
  }

  clearInlineTypeScriptDiagnosticRefresh(document.uri.fsPath);
  const expectedVersion = document.version;
  const timeout = setTimeout(() => {
    inlineTypeScriptDiagnosticRefreshes.delete(document.uri.fsPath);
    if (document.version !== expectedVersion) {
      return;
    }

    void refreshInlineTypeScriptDiagnosticsForDocument(document, expectedVersion);
  }, INLINE_TYPESCRIPT_DIAGNOSTIC_REFRESH_DELAY_MS);
  inlineTypeScriptDiagnosticRefreshes.set(document.uri.fsPath, timeout);
}

function clearInlineTypeScriptDiagnosticRefresh(sourcePath) {
  const timeout = inlineTypeScriptDiagnosticRefreshes.get(sourcePath);
  if (!timeout) {
    return;
  }

  clearTimeout(timeout);
  inlineTypeScriptDiagnosticRefreshes.delete(sourcePath);
}

function clearInlineTypeScriptDiagnosticRefreshes() {
  for (const timeout of inlineTypeScriptDiagnosticRefreshes.values()) {
    clearTimeout(timeout);
  }

  inlineTypeScriptDiagnosticRefreshes.clear();
}

async function refreshInlineTypeScriptDiagnosticsForDocument(document, expectedVersion = document.version) {
  const bridge = await ensureInlineTypeScriptSnapshotDocument({ document });
  if (!bridge) {
    inlineTypeScriptDiagnostics.delete(document.uri);
    return;
  }

  const mappedDiagnostics = await collectInlineTypeScriptDiagnostics({
    bridge,
    sourceDocument: document,
    expectedVersion,
  });
  if (!mappedDiagnostics) {
    return;
  }

  inlineTypeScriptDiagnostics.set(vscode.Uri.file(bridge.view.sourcePath), mappedDiagnostics);
}

async function collectInlineTypeScriptDiagnostics({ bridge, sourceDocument, expectedVersion }) {
  for (let attempt = 0; attempt < INLINE_TYPESCRIPT_DIAGNOSTIC_MAX_ATTEMPTS; attempt += 1) {
    if (!sourceDocument || sourceDocument.version !== expectedVersion) {
      return null;
    }

    const protocolDiagnostics = await requestInlineTypeScriptProtocolDiagnostics(bridge.virtualUri);
    let mappedDiagnostics;
    if (Array.isArray(protocolDiagnostics)) {
      mappedDiagnostics = await mapInlineTypeScriptProtocolDiagnosticsThroughServer({
        diagnostics: protocolDiagnostics,
        sourceDocument,
      });
      if (!Array.isArray(mappedDiagnostics)) {
        mappedDiagnostics = protocolDiagnostics
          .map((diagnostic) => mapInlineTypeScriptProtocolDiagnostic(diagnostic, bridge.view, bridge.virtualDocument))
          .filter(Boolean);
      }
    } else {
      mappedDiagnostics = vscode.languages.getDiagnostics(bridge.virtualUri)
        .map((diagnostic) => mapInlineTypeScriptDiagnostic(diagnostic, bridge.view, bridge.virtualDocument))
        .filter(Boolean);
    }

    const dedupedDiagnostics = dedupeInlineTypeScriptDiagnostics(mappedDiagnostics);
    if (dedupedDiagnostics.length > 0 || attempt === INLINE_TYPESCRIPT_DIAGNOSTIC_MAX_ATTEMPTS - 1) {
      return dedupedDiagnostics;
    }

    await waitForInlineTypeScriptDiagnosticsRetry();
  }

  return [];
}

async function requestInlineTypeScriptProtocolDiagnostics(virtualUri) {
  const extension = vscode.extensions.getExtension(TYPESCRIPT_EXTENSION_ID);
  if (extension && !extension.isActive) {
    try {
      await extension.activate();
    } catch {
      return null;
    }
  }

  try {
    const responses = await Promise.all(
      INLINE_TYPESCRIPT_DIAGNOSTIC_COMMANDS.map((command) => vscode.commands.executeCommand(
        TYPESCRIPT_TSSERVER_REQUEST_COMMAND_ID,
        command,
        { file: virtualUri },
      )),
    );

    return responses.flatMap((response) => (
      response && response.type === "response" && Array.isArray(response.body)
        ? response.body
        : []
    ));
  } catch {
    return null;
  }
}

async function mapInlineTypeScriptProtocolDiagnosticsThroughServer({ diagnostics, sourceDocument }) {
  if (!client || !sourceDocument || !Array.isArray(diagnostics)) {
    return null;
  }

  try {
    await client.onReady();
    const response = await client.sendRequest("workspace/executeCommand", {
      command: INLINE_TYPESCRIPT_MAP_PROTOCOL_DIAGNOSTICS_LSP_COMMAND_ID,
      arguments: [{
        uri: sourceDocument.uri.toString(),
        source: sourceDocument.getText(),
        diagnostics,
      }],
    });
    if (!Array.isArray(response)) {
      return null;
    }

    return response
      .map(normalizeMappedInlineTypeScriptProtocolDiagnostic)
      .filter(Boolean);
  } catch {
    return null;
  }
}

function normalizeMappedInlineTypeScriptProtocolDiagnostic(diagnostic) {
  if (!diagnostic || typeof diagnostic !== "object") {
    return null;
  }

  const start = diagnostic.range?.start;
  const end = diagnostic.range?.end;
  if (
    typeof start?.line !== "number"
    || typeof start?.character !== "number"
    || typeof end?.line !== "number"
    || typeof end?.character !== "number"
    || typeof diagnostic.message !== "string"
    || typeof diagnostic.severity !== "string"
  ) {
    return null;
  }

  const mapped = new vscode.Diagnostic(
    new vscode.Range(
      new vscode.Position(start.line, start.character),
      new vscode.Position(end.line, end.character),
    ),
    diagnostic.message,
    resolveMappedInlineTypeScriptDiagnosticSeverity(diagnostic.severity),
  );
  mapped.source = typeof diagnostic.source === "string" ? diagnostic.source : "ts";
  if (typeof diagnostic.code === "number" || typeof diagnostic.code === "string") {
    mapped.code = diagnostic.code;
  }

  const tags = resolveMappedInlineTypeScriptDiagnosticTags(diagnostic.tags);
  if (tags) {
    mapped.tags = tags;
  }

  return mapped;
}

function resolveMappedInlineTypeScriptDiagnosticSeverity(severity) {
  switch (severity) {
    case "error":
      return vscode.DiagnosticSeverity.Error;
    case "warning":
      return vscode.DiagnosticSeverity.Warning;
    case "hint":
      return vscode.DiagnosticSeverity.Hint;
    default:
      return vscode.DiagnosticSeverity.Information;
  }
}

function resolveMappedInlineTypeScriptDiagnosticTags(tags) {
  if (!Array.isArray(tags) || tags.length === 0) {
    return undefined;
  }

  const mappedTags = tags.flatMap((tag) => {
    switch (tag) {
      case "unnecessary":
        return vscode.DiagnosticTag.Unnecessary;
      case "deprecated":
        return vscode.DiagnosticTag.Deprecated;
      default:
        return [];
    }
  });

  return mappedTags.length > 0 ? mappedTags : undefined;
}

function mapInlineTypeScriptProtocolDiagnostic(diagnostic, snapshot, virtualDocument) {
  const virtualRange = resolveInlineTypeScriptProtocolRange(diagnostic, virtualDocument);
  if (!virtualRange) {
    return null;
  }

  const sourceRange = mapInlineTypeScriptVirtualRange(virtualRange, snapshot, virtualDocument);
  if (!sourceRange) {
    return null;
  }

  const mapped = new vscode.Diagnostic(
    sourceRange,
    readInlineTypeScriptProtocolDiagnosticMessage(diagnostic),
    resolveInlineTypeScriptProtocolDiagnosticSeverity(diagnostic.category),
  );
  mapped.source = diagnostic.source ?? "ts";
  if (typeof diagnostic.code === "number" || typeof diagnostic.code === "string") {
    mapped.code = diagnostic.code;
  }

  const tags = [];
  if (diagnostic.reportsUnnecessary) {
    tags.push(vscode.DiagnosticTag.Unnecessary);
  }
  if (diagnostic.reportsDeprecated) {
    tags.push(vscode.DiagnosticTag.Deprecated);
  }
  if (tags.length > 0) {
    mapped.tags = tags;
  }

  return mapped;
}

function resolveInlineTypeScriptProtocolRange(diagnostic, virtualDocument) {
  if (
    diagnostic
    && diagnostic.start
    && diagnostic.end
    && typeof diagnostic.start.line === "number"
    && typeof diagnostic.start.offset === "number"
    && typeof diagnostic.end.line === "number"
    && typeof diagnostic.end.offset === "number"
  ) {
    return new vscode.Range(
      new vscode.Position(Math.max(0, diagnostic.start.line - 1), Math.max(0, diagnostic.start.offset - 1)),
      new vscode.Position(Math.max(0, diagnostic.end.line - 1), Math.max(0, diagnostic.end.offset - 1)),
    );
  }

  if (typeof diagnostic?.start === "number") {
    const startOffset = Math.max(0, diagnostic.start);
    const endOffset = startOffset + Math.max(0, diagnostic.length ?? 0);
    return new vscode.Range(
      virtualDocument.positionAt(startOffset),
      virtualDocument.positionAt(endOffset),
    );
  }

  if (diagnostic?.textSpan && typeof diagnostic.textSpan.start === "number") {
    const startOffset = Math.max(0, diagnostic.textSpan.start);
    const endOffset = startOffset + Math.max(0, diagnostic.textSpan.length ?? 0);
    return new vscode.Range(
      virtualDocument.positionAt(startOffset),
      virtualDocument.positionAt(endOffset),
    );
  }

  return null;
}

function readInlineTypeScriptProtocolDiagnosticMessage(diagnostic) {
  if (typeof diagnostic?.text === "string" && diagnostic.text.length > 0) {
    return diagnostic.text;
  }

  const message = flattenInlineTypeScriptDiagnosticMessageText(diagnostic?.messageText);
  return message.length > 0 ? message : "TypeScript diagnostic";
}

function flattenInlineTypeScriptDiagnosticMessageText(messageText) {
  if (typeof messageText === "string") {
    return messageText;
  }

  if (!messageText || typeof messageText.messageText !== "string") {
    return "";
  }

  const next = Array.isArray(messageText.next)
    ? messageText.next
      .map((entry) => flattenInlineTypeScriptDiagnosticMessageText(entry))
      .filter((entry) => entry.length > 0)
    : [];
  return next.length > 0 ? `${messageText.messageText} ${next.join(" ")}` : messageText.messageText;
}

function resolveInlineTypeScriptProtocolDiagnosticSeverity(category) {
  switch (category) {
    case "error":
      return vscode.DiagnosticSeverity.Error;
    case "warning":
      return vscode.DiagnosticSeverity.Warning;
    case "suggestion":
      return vscode.DiagnosticSeverity.Hint;
    default:
      return vscode.DiagnosticSeverity.Information;
  }
}

function dedupeInlineTypeScriptDiagnostics(diagnostics) {
  const seen = new Set();
  return diagnostics.filter((diagnostic) => {
    const key = [
      diagnostic.code ?? "",
      diagnostic.message,
      diagnostic.range.start.line,
      diagnostic.range.start.character,
      diagnostic.range.end.line,
      diagnostic.range.end.character,
    ].join(":");
    if (seen.has(key)) {
      return false;
    }

    seen.add(key);
    return true;
  });
}

function waitForInlineTypeScriptDiagnosticsRetry() {
  return new Promise((resolve) => {
    setTimeout(resolve, INLINE_TYPESCRIPT_DIAGNOSTIC_RETRY_DELAY_MS);
  });
}

function updateInlineTypeScriptDiagnostics(uris) {
  const nextEntries = [];
  for (const uri of uris) {
    if (!uri || uri.scheme !== INLINE_TYPESCRIPT_SCHEME) {
      continue;
    }

    const snapshot = inlineTypeScriptSnapshots.get(uri.toString());
    const virtualDocument = vscode.workspace.textDocuments.find(
      (document) => document.uri.toString() === uri.toString(),
    );
    if (!snapshot || !virtualDocument) {
      continue;
    }

    const diagnostics = vscode.languages.getDiagnostics(uri)
      .map((diagnostic) => mapInlineTypeScriptDiagnostic(diagnostic, snapshot, virtualDocument))
      .filter(Boolean);
    nextEntries.push([vscode.Uri.file(snapshot.sourcePath), diagnostics]);
  }

  if (nextEntries.length > 0) {
    inlineTypeScriptDiagnostics.set(nextEntries);
  }
}

function mapInlineTypeScriptDiagnostic(diagnostic, snapshot, virtualDocument) {
  const range = mapInlineTypeScriptVirtualRange(diagnostic.range, snapshot, virtualDocument);
  if (!range) {
    return null;
  }

  const mapped = new vscode.Diagnostic(range, diagnostic.message, diagnostic.severity);
  mapped.source = diagnostic.source;
  mapped.code = diagnostic.code;
  mapped.tags = diagnostic.tags;
  return mapped;
}

function clearInlineTypeScriptSnapshotForSourcePath(sourcePath) {
  const snapshotUriString = inlineTypeScriptSnapshotSourcePaths.get(sourcePath);
  inlineTypeScriptDiagnostics.delete(vscode.Uri.file(sourcePath));
  for (const [uriString, snapshot] of inlineTypeScriptSnapshots.entries()) {
    if (snapshot.sourcePath === sourcePath) {
      inlineTypeScriptSnapshots.delete(uriString);
    }
  }
  if (!snapshotUriString) {
    return;
  }

  inlineTypeScriptSnapshotSourcePaths.delete(sourcePath);
}

function deleteInlineTypeScriptSnapshot(documentUri) {
  if (!documentUri || documentUri.scheme !== INLINE_TYPESCRIPT_SCHEME) {
    return;
  }

  const uriString = documentUri.toString();
  inlineTypeScriptSnapshots.delete(uriString);
  for (const [sourcePath, snapshotUriString] of inlineTypeScriptSnapshotSourcePaths.entries()) {
    if (snapshotUriString === uriString) {
      inlineTypeScriptSnapshotSourcePaths.delete(sourcePath);
      inlineTypeScriptDiagnostics.delete(vscode.Uri.file(sourcePath));
      break;
    }
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
