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
const {
  INLINE_TYPESCRIPT_COMMAND_ID,
  INLINE_TYPESCRIPT_SCHEME,
  resolveInlineSourcePositionRange,
  resolveInlineSourceRange,
  resolveInlineTargetPositionRange,
  resolveInlineTypeScriptView,
} = require("../inline-typescript");
const {
  INLINE_RUST_COMMAND_ID,
  resolveGeneratedServerMirrorPath,
  resolveInlineRustSourcePositionRange,
  resolveInlineRustSourceRange,
  resolveInlineRustView,
} = require("../inline-rust");

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

test("server resolver prefers workspace debug build for development extensions", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "thebe-vscode-test-"));
  const repoPath = path.join(root, "repo");
  const workspacePath = path.join(root, "fixture-workspace");
  const extensionPath = path.join(repoPath, "packages", "thebe-vscode");
  const bundled = path.join(extensionPath, "bin", "thebe-lsp");
  const workspaceBinary = path.join(repoPath, "target", "debug", "thebe-lsp");

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

  assert.strictEqual(command, workspaceBinary);
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

test("inline rust command id stays stable", () => {
  assert.strictEqual(INLINE_RUST_COMMAND_ID, "thebe.openInlineRustView");
});

test("inline typescript command id stays stable", () => {
  assert.strictEqual(INLINE_TYPESCRIPT_COMMAND_ID, "thebe.openInlineTypeScriptView");
});

test("inline typescript scheme stays stable", () => {
  assert.strictEqual(INLINE_TYPESCRIPT_SCHEME, "thebe-inline-ts");
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

test("generated server resolver maps route files into .thebe/server mirrors", () => {
  const workspacePath = path.join("/tmp", "thebe-app");

  const mirrorPath = resolveGeneratedServerMirrorPath({
    documentPath: path.join(workspacePath, "src", "routes", "blog", "[slug].trs"),
    workspaceFolders: [workspacePath],
  });

  assert.strictEqual(
    mirrorPath,
    path.join(workspacePath, ".thebe", "server", "routes", "blog", "[slug].rs"),
  );
});

test("generated server resolver ignores layouts and non-route files", () => {
  const workspacePath = path.join("/tmp", "thebe-app");

  const layoutPath = resolveGeneratedServerMirrorPath({
    documentPath: path.join(workspacePath, "src", "routes", "_layout.trs"),
    workspaceFolders: [workspacePath],
  });
  const componentPath = resolveGeneratedServerMirrorPath({
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

test("inline typescript view builds route snapshot with type import", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<script setup>\nstruct Props {}\n</script>\n\n<script lang="ts">\nconst props = getProps<Props>();\nfunction increment() {\n  return props.count;\n}\n</script>\n`;
  const selectionOffset = source.indexOf("increment") + 2;

  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: selectionOffset,
    selectionEndOffset: selectionOffset,
    fileExists: (filePath) => filePath.endsWith(path.join(".thebe", "types", "routes", "counter.ts")),
    readFile: () => "type Props = {\n  count: number;\n};\n\nexport default Props;\n",
  });

  assert.ok(view.ok);
  assert.strictEqual(
    view.targetPath,
    path.join(workspacePath, ".thebe", "client", "routes", "counter.ts"),
  );
  assert.match(view.content, /declare function getProps<T = unknown>\(\): T;/);
  assert.match(view.content, /type Props = \{/);
  assert.match(view.content, /export default Props;/);
  assert.match(view.content, /function increment\(\)/);
  assert.ok(view.selectionStartOffset > view.content.indexOf("function increment"));
});

test("inline rust view builds route snapshot from script setup", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<script setup>\n#[thebe::get]\nfn handler() -> Props {\n  Props {}\n}\n</script>\n`;
  const selectionOffset = source.indexOf("handler") + 2;

  const view = resolveInlineRustView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: selectionOffset,
    selectionEndOffset: selectionOffset,
  });

  assert.ok(view.ok);
  assert.strictEqual(
    view.targetPath,
    path.join(workspacePath, ".thebe", "server", "routes", "counter.rs"),
  );
  assert.match(view.content, /inline Rust view/);
  assert.match(view.content, /const __ROUTE_PATH: &str = "\/counter";/);
  assert.match(view.content, /fn handler\(\) -> Props/);
  assert.match(view.content, /type __ThebeResponse = Result<axum::response::Html<String>, axum::response::Response>;/);
  assert.match(view.content, /pub fn router<S>\(\) -> axum::Router<S>/);
  assert.ok(view.selectionStartOffset > view.content.indexOf("fn handler"));
});

test("inline rust view normalizes dynamic route paths inside the wrapper", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<script setup>\nfn handler() {}\n</script>\n`;

  const view = resolveInlineRustView({
    documentPath: path.join(workspacePath, "src", "routes", "blog", "[slug].trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
  });

  assert.ok(view.ok);
  assert.match(view.content, /const __ROUTE_PATH: &str = "\/blog\/\{slug\}";/);
});

test("inline typescript view falls back to unknown props without generated types", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<script lang="ts">\nconst props = getProps<Props>();\n</script>\n`;

  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
    fileExists: () => false,
  });

  assert.ok(view.ok);
  assert.match(view.content, /type Props = unknown;/);
});

test("inline typescript source range maps snapshot offsets back into the route script", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<script lang="ts">\nconst props = getProps<Props>();\nfunction increment() {\n  return props.count;\n}\n</script>\n`;
  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: source.indexOf("increment") + 2,
    selectionEndOffset: source.indexOf("increment") + 8,
    fileExists: () => false,
  });
  const inlineStart = view.content.indexOf("increment") + 2;
  const inlineEnd = view.content.indexOf("increment") + 8;

  const sourceRange = resolveInlineSourceRange({
    view,
    startOffset: inlineStart,
    endOffset: inlineEnd,
  });

  assert.deepStrictEqual(sourceRange, {
    startOffset: source.indexOf("increment") + 2,
    endOffset: source.indexOf("increment") + 8,
  });
});

test("inline typescript source range ignores snapshot prefix offsets", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<script lang="ts">\nconst props = getProps<Props>();\n</script>\n`;
  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
    fileExists: () => false,
  });

  const sourceRange = resolveInlineSourceRange({
    view,
    startOffset: 0,
    endOffset: 4,
  });

  assert.strictEqual(sourceRange, null);
});

test("inline typescript source positions preserve the route line and column", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<div />\n<script lang="ts">\nfunction increment() {}\n</script>\n`;
  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
    fileExists: () => false,
  });
  const inlineStart = view.content.indexOf("increment");
  const inlineEnd = inlineStart + "increment".length;

  const sourceRange = resolveInlineSourcePositionRange({
    view,
    startOffset: inlineStart,
    endOffset: inlineEnd,
  });

  assert.deepStrictEqual(sourceRange, {
    start: { line: 2, character: 9 },
    end: { line: 2, character: 18 },
  });
});

test("inline typescript target positions preserve the virtual document line and column", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<div />\n<script lang="ts">\nfunction increment() {}\n</script>\n`;
  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
    fileExists: () => false,
  });
  const sourceStart = source.indexOf("increment");
  const sourceEnd = sourceStart + "increment".length;

  const targetRange = resolveInlineTargetPositionRange({
    view,
    startOffset: sourceStart,
    endOffset: sourceEnd,
  });

  assert.deepStrictEqual(targetRange, {
    start: { line: 5, character: 9 },
    end: { line: 5, character: 18 },
  });
});

test("inline typescript target positions ignore offsets outside the script block", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<div />\n<script lang="ts">\nfunction increment() {}\n</script>\n`;
  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
    fileExists: () => false,
  });

  const targetRange = resolveInlineTargetPositionRange({
    view,
    startOffset: 0,
    endOffset: 4,
  });

  assert.strictEqual(targetRange, null);
});

test("inline rust source range maps snapshot offsets back into the route script", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<script setup>\n#[thebe::get]\nfn handler() -> Props {\n  Props {}\n}\n</script>\n`;
  const view = resolveInlineRustView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: source.indexOf("handler") + 2,
    selectionEndOffset: source.indexOf("handler") + 7,
  });
  const inlineStart = view.content.indexOf("handler") + 2;
  const inlineEnd = view.content.indexOf("handler") + 7;

  const sourceRange = resolveInlineRustSourceRange({
    view,
    startOffset: inlineStart,
    endOffset: inlineEnd,
  });

  assert.deepStrictEqual(sourceRange, {
    startOffset: source.indexOf("handler") + 2,
    endOffset: source.indexOf("handler") + 7,
  });
});

test("inline rust source positions preserve the route line and column", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const source = `<div />\n<script setup>\nfn handler() {}\n</script>\n`;
  const view = resolveInlineRustView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
  });
  const inlineStart = view.content.indexOf("handler");
  const inlineEnd = inlineStart + "handler".length;

  const sourceRange = resolveInlineRustSourcePositionRange({
    view,
    startOffset: inlineStart,
    endOffset: inlineEnd,
  });

  assert.deepStrictEqual(sourceRange, {
    start: { line: 2, character: 3 },
    end: { line: 2, character: 10 },
  });
});

test("inline typescript view rejects non-route files", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "components", "Card.trs"),
    workspaceFolders: [workspacePath],
    source: `<script lang="ts">\nconst count = 1;\n</script>`,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
  });

  assert.deepStrictEqual(view, { ok: false, reason: "not-route" });
});

test("inline rust view rejects non-route files", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const view = resolveInlineRustView({
    documentPath: path.join(workspacePath, "src", "components", "Card.trs"),
    workspaceFolders: [workspacePath],
    source: `<script setup>\nfn handler() {}\n</script>`,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
  });

  assert.deepStrictEqual(view, { ok: false, reason: "not-route" });
});

test("inline typescript view rejects routes without client script", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const view = resolveInlineTypeScriptView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source: `<script setup>\nstruct Props {}\n</script>`,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
  });

  assert.deepStrictEqual(view, { ok: false, reason: "no-script" });
});

test("inline rust view rejects routes without script setup", () => {
  const workspacePath = path.join("/tmp", "thebe-app");
  const view = resolveInlineRustView({
    documentPath: path.join(workspacePath, "src", "routes", "counter.trs"),
    workspaceFolders: [workspacePath],
    source: `<script lang="ts">\nfunction increment() {}\n</script>`,
    selectionStartOffset: 0,
    selectionEndOffset: 0,
  });

  assert.deepStrictEqual(view, { ok: false, reason: "no-script" });
});

test("package manifest contributes generated artifact commands", () => {
  assert.ok(
    packageJson.contributes.commands.some(
      (command) => command.command === INLINE_RUST_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus.commandPalette.some(
      (item) => item.command === INLINE_RUST_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.commands.some(
      (command) => command.command === INLINE_TYPESCRIPT_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus.commandPalette.some(
      (item) => item.command === INLINE_TYPESCRIPT_COMMAND_ID,
    ),
  );
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
      (item) => item.command === INLINE_RUST_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus["editor/title"].some(
      (item) => item.command === INLINE_TYPESCRIPT_COMMAND_ID,
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
      (item) => item.command === INLINE_RUST_COMMAND_ID,
    ),
  );
  assert.ok(
    packageJson.contributes.menus["editor/context"].some(
      (item) => item.command === INLINE_TYPESCRIPT_COMMAND_ID,
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
