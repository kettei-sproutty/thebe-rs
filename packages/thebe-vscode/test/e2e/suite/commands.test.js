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

  test("inline rust snapshot hover returns source-backed route hover", async () => {
    await openFixtureRouteAt("handler");

    await vscode.commands.executeCommand("thebe.openInlineRustView");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    const offset = editor.document.getText().indexOf("handler") + 2;
    const position = editor.document.positionAt(offset);
    const hovers = await vscode.commands.executeCommand(
      "vscode.executeHoverProvider",
      editor.document.uri,
      position,
    );

    const hoverText = hovers.flatMap((hover) => hover.contents).map(stringifyHoverContent).join("\n");
    assert.match(hoverText, /GET \//i);
    assert.match(hoverText, /Handler `handler`/i);
  });

  test("thebe source receives mapped inline Rust diagnostics", async () => {
    await openFixtureRouteAt("handler");

    await vscode.commands.executeCommand("thebe.openInlineRustView");

    const rustEditor = vscode.window.activeTextEditor;
    assert.ok(rustEditor);
    const diagnosticSource = "thebe-inline-rust-test";
    const message = "Injected inline Rust diagnostic";
    const sourceOffset = routeUri.fsPath && (await vscode.workspace.openTextDocument(routeUri)).getText().indexOf("Props { count: 0 }");
    const expectedSourceDocument = await vscode.workspace.openTextDocument(routeUri);
    const expectedStart = expectedSourceDocument.positionAt(sourceOffset + 2);
    const expectedEnd = expectedSourceDocument.positionAt(sourceOffset + 7);
    const rustOffset = rustEditor.document.getText().indexOf("Props { count: 0 }");
    const rustStart = rustEditor.document.positionAt(rustOffset + 2);
    const rustEnd = rustEditor.document.positionAt(rustOffset + 7);
    const collection = vscode.languages.createDiagnosticCollection(diagnosticSource);

    try {
      collection.set(rustEditor.document.uri, [
        new vscode.Diagnostic(
          new vscode.Range(rustStart, rustEnd),
          message,
          vscode.DiagnosticSeverity.Error,
        ),
      ]);

      await waitFor(() => {
        const diagnostics = vscode.languages.getDiagnostics(routeUri);
        return diagnostics.some((diagnostic) => diagnostic.message === message);
      });

      const diagnostics = vscode.languages.getDiagnostics(routeUri);
      const diagnostic = diagnostics.find((entry) => entry.message === message);
      assert.ok(diagnostic);
      assert.strictEqual(diagnostic.range.start.line, expectedStart.line);
      assert.strictEqual(diagnostic.range.start.character, expectedStart.character);
      assert.strictEqual(diagnostic.range.end.line, expectedEnd.line);
      assert.strictEqual(diagnostic.range.end.character, expectedEnd.character);
    } finally {
      collection.clear();
      collection.dispose();
      await waitFor(() => {
        const diagnostics = vscode.languages.getDiagnostics(routeUri);
        return diagnostics.every((diagnostic) => diagnostic.message !== message);
      });
    }
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

  test("inline typescript virtual document receives TypeScript hover", async () => {
    await openFixtureRouteAt("count + 1");

    await vscode.commands.executeCommand("thebe.openInlineTypeScriptView");

    const editor = vscode.window.activeTextEditor;
    assert.ok(editor);
    const offset = editor.document.getText().indexOf("props.count") + "props.".length + 2;
    const position = editor.document.positionAt(offset);
    const hovers = await vscode.commands.executeCommand(
      "vscode.executeHoverProvider",
      editor.document.uri,
      position,
    );

    const hoverText = hovers.flatMap((hover) => hover.contents).map(stringifyHoverContent).join("\n");
    assert.match(hoverText, /count/i);
    assert.match(hoverText, /bigint/i);
  });

  test("thebe source receives TypeScript completions inside script lang ts", async () => {
    const editor = await openFixtureRouteAt("props.count");
    const offset = editor.document.getText().indexOf("props.count") + "props.".length;
    const position = editor.document.positionAt(offset);
    const completions = await vscode.commands.executeCommand(
      "vscode.executeCompletionItemProvider",
      editor.document.uri,
      position,
      ".",
    );

    const labels = completions.items.map((item) => typeof item.label === "string" ? item.label : item.label.label);
    assert.ok(labels.includes("count"));
  });

  test("thebe source receives mapped TypeScript diagnostics inside script lang ts", async () => {
    const editor = await openFixtureRouteAt("props.count + 1");
    const operatorDiagnostic = /Operator '\+' cannot be applied to types 'bigint' and '1'/i;
    const invalidExpression = "props.count + 1";
    const validExpression = "props.count + 1n";

    await waitFor(() => {
      const diagnostics = vscode.languages.getDiagnostics(editor.document.uri);
      return diagnostics.some((diagnostic) => operatorDiagnostic.test(diagnostic.message));
    });

    let diagnostics = vscode.languages.getDiagnostics(editor.document.uri);
    assert.ok(diagnostics.some((diagnostic) => operatorDiagnostic.test(diagnostic.message)));

    await editor.edit((editBuilder) => {
      const startOffset = editor.document.getText().indexOf(invalidExpression);
      const start = editor.document.positionAt(startOffset);
      const end = editor.document.positionAt(startOffset + invalidExpression.length);
      editBuilder.replace(new vscode.Range(start, end), validExpression);
    });

    await waitFor(() => {
      diagnostics = vscode.languages.getDiagnostics(editor.document.uri);
      return diagnostics.every((diagnostic) => !operatorDiagnostic.test(diagnostic.message));
    });

    await editor.edit((editBuilder) => {
      const startOffset = editor.document.getText().indexOf(validExpression);
      const start = editor.document.positionAt(startOffset);
      const end = editor.document.positionAt(startOffset + validExpression.length);
      editBuilder.replace(new vscode.Range(start, end), invalidExpression);
    });

    await waitFor(() => {
      diagnostics = vscode.languages.getDiagnostics(editor.document.uri);
      return diagnostics.some((diagnostic) => operatorDiagnostic.test(diagnostic.message));
    });
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

  function stringifyHoverContent(content) {
    if (typeof content === "string") {
      return content;
    }

    if (content && typeof content.value === "string") {
      return content.value;
    }

    return "";
  }
});
