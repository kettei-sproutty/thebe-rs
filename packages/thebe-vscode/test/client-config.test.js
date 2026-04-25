const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const packageJson = require("../package.json");

const {
  createDocumentSelector,
  FILE_WATCH_GLOB,
  resolveServerCommand,
} = require("../client-config");
const {
  GENERATED_CLIENT_COMMAND_ID,
  GENERATED_TYPES_COMMAND_ID,
  resolveGeneratedClientMirrorPath,
  resolveGeneratedTypesMirrorPath,
  selectGeneratedClientLocation,
  selectGeneratedTypesLocation,
} = require("../generated-client");

test("thebe document selector includes untitled editors", () => {
  const selector = createDocumentSelector();

  assert.deepStrictEqual(selector, [{ language: "thebe" }]);
  assert.ok(!Object.hasOwn(selector[0], "scheme"));
});

test("thebe file watcher tracks generated project inputs", () => {
  assert.strictEqual(FILE_WATCH_GLOB, "**/*.{trs,toml,html}");
});

test("server resolver prefers configured path over bundled and workspace binaries", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "thebe-vscode-test-"));
  const extensionPath = path.join(root, "extension");
  const workspacePath = path.join(root, "workspace");

  fs.mkdirSync(path.join(extensionPath, "bin"), { recursive: true });
  fs.mkdirSync(path.join(workspacePath, "target", "debug"), { recursive: true });
  fs.writeFileSync(path.join(extensionPath, "bin", "thebe-lsp"), "");
  fs.writeFileSync(path.join(workspacePath, "target", "debug", "thebe-lsp"), "");

  const command = resolveServerCommand({
    configuredPath: "/custom/thebe-lsp",
    extensionPath,
    workspaceFolders: [workspacePath],
    platform: "darwin",
  });

  assert.strictEqual(command, "/custom/thebe-lsp");
});

test("server resolver prefers bundled binary before workspace debug build", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "thebe-vscode-test-"));
  const extensionPath = path.join(root, "extension");
  const workspacePath = path.join(root, "workspace");
  const bundled = path.join(extensionPath, "bin", "thebe-lsp");
  const workspaceBinary = path.join(workspacePath, "target", "debug", "thebe-lsp");

  fs.mkdirSync(path.dirname(bundled), { recursive: true });
  fs.mkdirSync(path.dirname(workspaceBinary), { recursive: true });
  fs.writeFileSync(bundled, "");
  fs.writeFileSync(workspaceBinary, "");

  const command = resolveServerCommand({
    configuredPath: "",
    extensionPath,
    workspaceFolders: [workspacePath],
    platform: "darwin",
  });

  assert.strictEqual(command, bundled);
});

test("server resolver falls back to workspace debug build before PATH", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "thebe-vscode-test-"));
  const workspacePath = path.join(root, "workspace");
  const workspaceBinary = path.join(workspacePath, "target", "debug", "thebe-lsp");

  fs.mkdirSync(path.dirname(workspaceBinary), { recursive: true });
  fs.writeFileSync(workspaceBinary, "");

  const command = resolveServerCommand({
    configuredPath: undefined,
    extensionPath: path.join(root, "missing-extension"),
    workspaceFolders: [workspacePath],
    platform: "darwin",
  });

  assert.strictEqual(command, workspaceBinary);
});

test("server resolver falls back to platform executable name when nothing exists", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "thebe-vscode-test-"));

  const unixCommand = resolveServerCommand({
    configuredPath: null,
    extensionPath: path.join(root, "missing-extension"),
    workspaceFolders: [path.join(root, "missing-workspace")],
    platform: "linux",
  });
  const windowsCommand = resolveServerCommand({
    configuredPath: null,
    extensionPath: path.join(root, "missing-extension"),
    workspaceFolders: [path.join(root, "missing-workspace")],
    platform: "win32",
  });

  assert.strictEqual(unixCommand, "thebe-lsp");
  assert.strictEqual(windowsCommand, "thebe-lsp.exe");
});

test("generated client command id stays stable", () => {
  assert.strictEqual(GENERATED_CLIENT_COMMAND_ID, "thebe.openGeneratedClientMirror");
});

test("generated types command id stays stable", () => {
  assert.strictEqual(GENERATED_TYPES_COMMAND_ID, "thebe.openGeneratedTypesMirror");
});

test("generated client resolver maps route files into .thebe/client mirrors", () => {
  const workspacePath = path.join("/tmp", "thebe-app");

  const mirrorPath = resolveGeneratedClientMirrorPath({
    documentPath: path.join(workspacePath, "src", "routes", "blog", "[slug].trs"),
    workspaceFolders: [workspacePath],
  });

  assert.strictEqual(
    mirrorPath,
    path.join(workspacePath, ".thebe", "client", "routes", "blog", "[slug].ts"),
  );
});

test("generated client resolver ignores layouts and non-route files", () => {
  const workspacePath = path.join("/tmp", "thebe-app");

  const layoutPath = resolveGeneratedClientMirrorPath({
    documentPath: path.join(workspacePath, "src", "routes", "_layout.trs"),
    workspaceFolders: [workspacePath],
  });
  const componentPath = resolveGeneratedClientMirrorPath({
    documentPath: path.join(workspacePath, "src", "components", "Card.trs"),
    workspaceFolders: [workspacePath],
  });

  assert.strictEqual(layoutPath, null);
  assert.strictEqual(componentPath, null);
});

test("generated types resolver maps route files into .thebe/types mirrors", () => {
  const workspacePath = path.join("/tmp", "thebe-app");

  const mirrorPath = resolveGeneratedTypesMirrorPath({
    documentPath: path.join(workspacePath, "src", "routes", "blog", "[slug].trs"),
    workspaceFolders: [workspacePath],
  });

  assert.strictEqual(
    mirrorPath,
    path.join(workspacePath, ".thebe", "types", "routes", "blog", "[slug].ts"),
  );
});

test("generated types resolver ignores layouts and non-route files", () => {
  const workspacePath = path.join("/tmp", "thebe-app");

  const layoutPath = resolveGeneratedTypesMirrorPath({
    documentPath: path.join(workspacePath, "src", "routes", "_layout.trs"),
    workspaceFolders: [workspacePath],
  });
  const componentPath = resolveGeneratedTypesMirrorPath({
    documentPath: path.join(workspacePath, "src", "components", "Card.trs"),
    workspaceFolders: [workspacePath],
  });

  assert.strictEqual(layoutPath, null);
  assert.strictEqual(componentPath, null);
});

test("generated client selector keeps the matching location range", () => {
  const mirrorPath = path.join("/tmp", "thebe-app", ".thebe", "client", "routes", "about.ts");
  const expectedRange = {
    start: { line: 4, character: 2 },
    end: { line: 4, character: 11 },
  };

  const location = selectGeneratedClientLocation({
    mirrorPath,
    locations: [
      {
        uri: { fsPath: mirrorPath },
        range: expectedRange,
      },
    ],
  });

  assert.deepStrictEqual(location, {
    uri: { fsPath: mirrorPath },
    range: expectedRange,
  });
});

test("generated client selector supports location links", () => {
  const mirrorPath = path.join("/tmp", "thebe-app", ".thebe", "client", "routes", "about.ts");
  const expectedRange = {
    start: { line: 7, character: 1 },
    end: { line: 7, character: 9 },
  };

  const location = selectGeneratedClientLocation({
    mirrorPath,
    locations: [
      {
        targetUri: { fsPath: mirrorPath },
        targetSelectionRange: expectedRange,
      },
    ],
  });

  assert.deepStrictEqual(location, {
    uri: { fsPath: mirrorPath },
    range: expectedRange,
  });
});

test("generated types selector keeps the matching location range", () => {
  const mirrorPath = path.join("/tmp", "thebe-app", ".thebe", "types", "routes", "about.ts");
  const expectedRange = {
    start: { line: 2, character: 6 },
    end: { line: 2, character: 11 },
  };

  const location = selectGeneratedTypesLocation({
    mirrorPath,
    locations: [
      {
        uri: { fsPath: mirrorPath },
        range: expectedRange,
      },
    ],
  });

  assert.deepStrictEqual(location, {
    uri: { fsPath: mirrorPath },
    range: expectedRange,
  });
});

test("package manifest contributes generated artifact commands", () => {
  assert.ok(
    packageJson.contributes.commands.some(
      (command) => command.command === GENERATED_CLIENT_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus.commandPalette.some(
      (item) => item.command === GENERATED_CLIENT_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.commands.some(
      (command) => command.command === GENERATED_TYPES_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus.commandPalette.some(
      (item) => item.command === GENERATED_TYPES_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus["editor/title"].some(
      (item) => item.command === GENERATED_CLIENT_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus["editor/title"].some(
      (item) => item.command === GENERATED_TYPES_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus["editor/context"].some(
      (item) => item.command === GENERATED_CLIENT_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus["editor/context"].some(
      (item) => item.command === GENERATED_TYPES_COMMAND_ID,
    ),
  );
});
