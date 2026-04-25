# Thebe VS Code Extension

This package wires Thebe `.trs` files into VS Code with:

- `.trs` language registration
- TextMate highlighting for Thebe block tags, template bindings, directives, events, component tags, and named-slot attributes
- snippet contributions for routes, components, named slots, and common blocks
- automatic `thebe-lsp` startup for saved and untitled `.trs` editors
- command palette, editor title, and editor context menu actions for opening a route's generated `.thebe/client/**` mirror and `.thebe/types/**` props mirror beside the source `.trs` file, reusing Thebe definition results to preserve matching positions when available
- an `Open Inline Rust View` command that opens the current route's `<script setup>` block as an untitled Rust snapshot with definition/reference jumps back into the source route
- an `Open Inline TypeScript View` command that opens the current route's `<script lang="ts">` block as a stable provider-backed virtual TypeScript document with definition/reference jumps back into the source route, `Props` type definition jumps into the generated props mirror, built-in TypeScript hover, live refresh from source `.trs` edits, and source-side TypeScript completions plus mapped errors/warnings inside the original `.trs` editor

The inline TypeScript snapshot now comes from `thebe-lsp` first through a `workspace/executeCommand` bridge, with the older extension-local resolver kept only as a compatibility fallback for older servers or startup timing gaps.

Source-side TypeScript completions plus mapped errors and warnings now also come from `thebe-lsp` after startup through an embedded TypeScript bridge that uses VS Code's bundled runtime. The extension keeps the older source-side completion and diagnostics provider only until `client.onReady()` so authoring still works during server bootstrap.

## Local Development

1. Install dependencies in this package with `npm install`.
2. Build the workspace `thebe-lsp` binary with `cargo build -p thebe-lsp`.
3. If needed, set `thebe.lsp.path` in VS Code to the absolute path of the binary.
4. Run `npm test` in this package to validate the extension's focused smoke checks for the Thebe document selector, project input watch glob, LSP server resolution precedence, generated artifact commands, and inline Rust/TypeScript bridge helpers.
5. Run `npm run test:e2e` in this package to launch the lightweight extension-host harness that verifies the generated artifact commands plus the inline Rust snapshot and provider-backed TypeScript virtual document's editor-opening, refresh, round-trip navigation behavior, source-side TypeScript completions, and mapped TypeScript diagnostics.

When the setting is empty in a development checkout, the extension first looks for a nearby `target/debug/thebe-lsp` above the extension folder, then tries `target/debug/thebe-lsp` in the current workspace, and finally falls back to `thebe-lsp` on `PATH`.

## Packaging

Run the packaging script from this package directory:

```sh
npm run package:vsix
```

That command:

- builds `thebe-lsp` from the workspace root in release mode
- copies the current platform binary into `bin/`
- produces a platform-specific VSIX such as `thebe-vscode-0.1.0-darwin-arm64.vsix`

You can override the server build if needed:

```sh
npm run package:vsix -- --profile debug
npm run package:vsix -- --server /absolute/path/to/thebe-lsp
```

The packaged extension prefers the bundled binary first, then falls back to the workspace `target/debug/thebe-lsp`, and finally `thebe-lsp` on `PATH`.
