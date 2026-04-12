# Thebe VS Code Extension

This package wires Thebe `.trs` files into VS Code with:

- `.trs` language registration
- TextMate highlighting for Thebe block tags, template bindings, directives, and component tags
- snippet contributions for routes, components, and common blocks
- automatic `thebe-lsp` startup

## Local Development

1. Install dependencies in this package with `npm install`.
2. Build the workspace `thebe-lsp` binary with `cargo build -p thebe-lsp`.
3. If needed, set `thebe.lsp.path` in VS Code to the absolute path of the binary.

When the setting is empty, the extension tries `target/debug/thebe-lsp` in the current workspace before falling back to `thebe-lsp` on `PATH`.
