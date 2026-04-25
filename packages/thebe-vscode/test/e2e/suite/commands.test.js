const assert = require("node:assert/strict");
const path = require("node:path");
const vscode = require("vscode");

suite("Thebe extension commands", () => {
  const fixtureRoot = path.resolve(__dirname, "..", "fixture-workspace");
  const routeUri = vscode.Uri.file(path.join(fixtureRoot, "src", "routes", "index.trs"));

  teardown(async () => {
    await vscode.commands.executeCommand("workbench.action.closeAllEditors");
  });

  test("generated client mirror command opens generated client file", async () => {
    await openFixtureRouteAt("increment");

    await vscode.commands.executeCommand("thebe.openGeneratedClientMirror");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    assert.ok(editor.document.uri.fsPath.endsWith(path.join(".thebe", "client", "routes", "index.ts")));
  });

  test("generated props types command opens generated types file", async () => {
    await openFixtureRouteAt("increment");

    await vscode.commands.executeCommand("thebe.openGeneratedTypesMirror");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    assert.ok(editor.document.uri.fsPath.endsWith(path.join(".thebe", "types", "routes", "index.ts")));
  });

  test("inline typescript view command opens provider-backed typescript snapshot", async () => {
    const sourceEditor = await openFixtureRouteAt("increment");

    await vscode.commands.executeCommand("thebe.openInlineTypeScriptView");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    assert.strictEqual(editor.document.uri.scheme, "thebe-inline-ts");
    assert.ok(editor.document.uri.path.endsWith(path.join(".thebe", "client", "routes", "index.ts")));
    assert.strictEqual(editor.document.languageId, "typescript");
    assert.match(editor.document.getText(), /declare function getProps<T = unknown>\(\): T;/);
    assert.match(editor.document.getText(), /function increment\(\)/);

    await sourceEditor.edit((editBuilder) => {
      const start = sourceEditor.document.positionAt(sourceEditor.document.getText().indexOf("increment"));
      const end = sourceEditor.document.positionAt(sourceEditor.document.getText().indexOf("increment") + "increment".length);
      editBuilder.replace(new vscode.Range(start, end), "incrementLater");
    });

    await waitFor(() => editor.document.getText().includes("incrementLater"));

    await sourceEditor.edit((editBuilder) => {
      const start = sourceEditor.document.positionAt(sourceEditor.document.getText().indexOf("incrementLater"));
      const end = sourceEditor.document.positionAt(sourceEditor.document.getText().indexOf("incrementLater") + "incrementLater".length);
      editBuilder.replace(new vscode.Range(start, end), "increment");
    });

    await waitFor(() => editor.document.getText().includes("function increment()"));
  });

  test("inline rust view command opens untitled rust snapshot", async () => {
    await openFixtureRouteAt("handler");

    await vscode.commands.executeCommand("thebe.openInlineRustView");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    assert.strictEqual(editor.document.uri.scheme, "untitled");
    assert.strictEqual(editor.document.languageId, "rust");
    assert.match(editor.document.getText(), /inline Rust view/);
    assert.match(editor.document.getText(), /fn handler\(\) -> Props/);
  });

  test("inline rust snapshot definition returns the source route", async () => {
    await openFixtureRouteAt("handler");

    await vscode.commands.executeCommand("thebe.openInlineRustView");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    const offset = editor.document.getText().indexOf("handler") + 2;
    const position = editor.document.positionAt(offset);
    const locations = await vscode.commands.executeCommand(
      "vscode.executeDefinitionProvider",
      editor.document.uri,
      position,
    );

    const location = locations.find((candidate) => candidate.uri.fsPath.endsWith(path.join("src", "routes", "index.trs")));
    assert.ok(location);
    assert.strictEqual(location.range.start.line, 6);
    assert.strictEqual(location.range.start.character, 3);
  });

  test("inline typescript snapshot definition returns the source route", async () => {
    await openFixtureRouteAt("increment");

    await vscode.commands.executeCommand("thebe.openInlineTypeScriptView");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    const offset = editor.document.getText().indexOf("increment") + 2;
    const position = editor.document.positionAt(offset);
    const locations = await vscode.commands.executeCommand(
      "vscode.executeDefinitionProvider",
      editor.document.uri,
      position,
    );

    const location = locations.find((candidate) => candidate.uri.fsPath.endsWith(path.join("src", "routes", "index.trs")));
    assert.ok(location);
    assert.strictEqual(location.range.start.line, 14);
    assert.strictEqual(location.range.start.character, 9);
  });

  test("inline typescript snapshot type definition returns generated props types", async () => {
    await openFixtureRouteAt("Props");

    await vscode.commands.executeCommand("thebe.openInlineTypeScriptView");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    const offset = editor.document.getText().indexOf("getProps<Props>") + "getProps<".length + 2;
    const position = editor.document.positionAt(offset);
    const locations = await vscode.commands.executeCommand(
      "vscode.executeTypeDefinitionProvider",
      editor.document.uri,
      position,
    );

    assert.ok(
      locations.some((location) => location.uri.fsPath.endsWith(path.join(".thebe", "types", "routes", "index.ts"))),
    );
  });

  async function openFixtureRouteAt(symbol) {
    const document = await vscode.workspace.openTextDocument(routeUri);
    const editor = await vscode.window.showTextDocument(document);
    const offset = document.getText().indexOf(symbol) + 2;
    const position = document.positionAt(offset);
    editor.selection = new vscode.Selection(position, position);
    return editor;
  }

  async function waitFor(predicate, timeoutMs = 5000) {
    const startedAt = Date.now();
    while (Date.now() - startedAt < timeoutMs) {
      if (predicate()) {
        return;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }

    assert.fail("timed out waiting for inline snapshot update");
  }
});
