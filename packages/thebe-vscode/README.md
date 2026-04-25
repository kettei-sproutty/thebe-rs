# Thebe VS Code Extension

This package wires Thebe `.trs` files into VS Code with:

- `.trs` language registration
- TextMate highlighting for Thebe block tags, template bindings, directives, events, component tags, and named-slot attributes
- snippet contributions for routes, components, named slots, and common blocks
- automatic `thebe-lsp` startup for saved and untitled `.trs` editors
- command palette, editor title, and editor context menu actions for opening a route's generated `.thebe/client/**` mirror and `.thebe/types/**` props mirror beside the source `.trs` file, reusing Thebe definition results to preserve matching positions when available
- an `Open Inline Rust View` command that opens the current route's `<script setup>` block as an untitled Rust snapshot with definition/reference jumps back into the source route
- an `Open Inline TypeScript View` command that opens the current route's `<script lang="ts">` block as an untitled TypeScript snapshot with definition/reference jumps back into the source route and `Props` type definition jumps into the generated props mirror

## Local Development

1. Install dependencies in this package with `npm install`.
2. Build the workspace `thebe-lsp` binary with `cargo build -p thebe-lsp`.
3. If needed, set `thebe.lsp.path` in VS Code to the absolute path of the binary.
4. Run `npm test` in this package to validate the extension's focused smoke checks for the Thebe document selector, project input watch glob, LSP server resolution precedence, generated artifact commands, and inline Rust/TypeScript snapshot helpers.
5. Run `npm run test:e2e` in this package to launch the lightweight extension-host harness that verifies the generated artifact commands plus the inline Rust and TypeScript snapshots' editor-opening and round-trip navigation behavior.

When the setting is empty, the extension tries `target/debug/thebe-lsp` in the current workspace before falling back to `thebe-lsp` on `PATH`.

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
