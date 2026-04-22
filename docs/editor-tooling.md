# Editor Tooling

Thebe already ships a compiler-backed editor integration layer, but the full language tooling story is not complete yet. This page separates what exists today from the remaining work so the docs stay honest.

## Shipped Today

- `crates/thebe-lsp` provides a `tower-lsp` language server for `.trs` files and generated Thebe artifacts.
- `crates/thebe-project` owns the shared tooling model and emits `.thebe/manifest.json` and `.thebe/diagnostics.json` for both the CLI and the LSP.
- Routes with client code emit a generated editor workspace under `.thebe/`:
  - `.thebe/types/**` contains `ts-rs` exports for route `Props`.
  - `.thebe/client/**` mirrors each `<script lang="ts">` block with a concrete `Props` import.
  - `.thebe/thebe-env.d.ts` declares `getProps()` for those mirrors.
  - `.thebe/tsconfig.json` gives editors a self-contained TypeScript project without forcing a root `tsconfig.json`.
- `.thebe/manifest.json` is currently version 5 and records route/layout/component metadata, generated paths, handler signatures, template bindings, exact field-level template symbol definitions, source spans, and route template symbols derived from `Props`.
- `.thebe/diagnostics.json` is currently version 1 and records structured project and file diagnostics with relative paths and source spans.
- `packages/thebe-vscode/` ships a packaged VS Code extension with `.trs` language registration, snippets, TextMate highlighting, and automatic `thebe-lsp` startup.
- `packages/tree-sitter-thebe/` ships an initial tree-sitter grammar for `.trs` block structure and template bindings.

## Current LSP Features

`thebe-lsp` currently supports:

- Diagnostics sourced from `.thebe/diagnostics.json`.
- Hover for route handlers, precise nested `Props` fields inside template bindings, component tags/import aliases, and component props.
- Document highlights for Thebe-owned symbols in the current `.trs` file.
- Document symbols for route handlers, template bindings, and component props.
- Go-to-definition between `.trs` source files and generated Rust/TypeScript artifacts, plus exact nested `Props` field targets, component tag/prop targets, and component import aliases.
- References for route handlers, template bindings, precise nested `Props` field paths, and component tag/prop usages across known `.trs` sources, including component import aliases as starting points.
- Workspace symbol search across loaded Thebe project manifests for routes, handlers, template symbols, layouts, components, and component props.
- Semantic tokens for block tags, template bindings, component tags, directives, and event attributes.
- Linked editing for matched template tag pairs.
- Rename support for route handlers, route template symbols, component prop definitions/usages, component tag/import relationships across known `.trs` sources, and client event handlers.
- Code actions for inserting missing top-level blocks and adding `ts-rs` when typed client routes require it.
- Formatting support for normalizing `.trs` block layout, plus best-effort formatting for embedded Rust, TypeScript, and CSS blocks.
- `.trs` completions for:
  - top-level block snippets such as `<head>`, `<script setup>`, `<script lang="ts">`, and `<style>`
  - template symbol completions from route `Props` metadata plus current unsaved source
  - event-handler name completions from the current `<script lang="ts">` block
  - component tag completions, including missing component import insertion when a matching Rust block is present
  - component prop completions
  - template attribute completions for directives and bound attributes such as `:if`, `:class`, generic `:attr`, and common `on*` handlers

## Editor Behavior Already Implemented

The editor loop is not disk-only anymore.

- Unsaved source buffers are tracked through overlay-backed refreshes.
- `didOpen`, `didChange`, `didSave`, and `didClose` all refresh compiler state through `thebe-project`.
- `didChange` refreshes are debounced.
- The LSP keeps last-good artifacts so hover, definition, and references can keep working during transient invalid edits.
- Diagnostics publishing is coalesced so unchanged diagnostics are not republished on every refresh.

## Remaining Gaps

The editor story is broader now, but a few edges are still intentionally narrow:

- The tree-sitter grammar is still initial and does not yet model full HTML-aware nesting or embedded Rust/TypeScript/CSS subgrammars.
- Rename support is currently scoped to route handlers, route template symbols, component prop definitions/usages, component tag/import relationships across known `.trs` sources, and client event handlers rather than arbitrary Rust or TypeScript symbols.
- Formatting now normalizes top-level `.trs` structure and uses best-effort block formatters for embedded Rust, TypeScript, and CSS, but it still does not provide full language-service formatting semantics inside those blocks.

## Practical Scope Today

If you open a route-oriented Thebe project in an editor today, the expected tooling story is:

- compiler diagnostics work
- generated TypeScript mirrors work
- route/layout navigation works
- semantic highlighting, formatting, rename, and code actions work for the currently supported Thebe surface
- template, attribute, event, and component completions work

If you want polished language ergonomics comparable to a mature framework, that is still future work.
