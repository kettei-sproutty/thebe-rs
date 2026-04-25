# Thebe VS Code Extension

This package wires Thebe `.trs` files into VS Code with:

- `.trs` language registration
- TextMate highlighting for Thebe block tags, template bindings, directives, events, component tags, and named-slot attributes
- snippet contributions for routes, components, named slots, and common blocks
- automatic `thebe-lsp` startup for saved and untitled `.trs` editors
- command palette, editor title, and editor context menu actions for opening a route's generated `.thebe/client/**` mirror and `.thebe/types/**` props mirror beside the source `.trs` file, reusing Thebe definition results to preserve matching positions when available

## Local Development

1. Install dependencies in this package with `npm install`.
2. Build the workspace `thebe-lsp` binary with `cargo build -p thebe-lsp`.
3. If needed, set `thebe.lsp.path` in VS Code to the absolute path of the binary.
4. Run `npm test` in this package to validate the extension's focused smoke checks for the Thebe document selector, project input watch glob, LSP server resolution precedence, and generated artifact commands.

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
