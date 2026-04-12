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
- Hover for route handlers, precise nested `Props` fields inside template bindings, and component props.
- Document symbols for route handlers, template bindings, and component props.
- Go-to-definition between `.trs` source files and generated Rust/TypeScript artifacts, plus exact nested `Props` field targets and component tag/prop targets.
- References for route handlers, template bindings, and precise nested `Props` field paths.
- Semantic tokens for block tags, template bindings, component tags, directives, and event attributes.
- Rename support for route handlers and client event handlers.
- Code actions for inserting missing top-level blocks and adding `ts-rs` when typed client routes require it.
- Formatting support for normalizing `.trs` block layout.
- `.trs` completions for:
  - top-level block snippets such as `<head>`, `<script setup>`, `<script lang="ts">`, and `<style>`
  - template symbol completions from route `Props` metadata plus current unsaved source
  - event-handler name completions from the current `<script lang="ts">` block
  - component tag completions
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
- Component completion does not yet insert or rewrite Rust `use crate::components::...` imports automatically.
- Rename support is currently scoped to route handlers and client event handlers rather than arbitrary Rust or TypeScript symbols.
- Formatting currently normalizes top-level `.trs` structure, but it does not invoke dedicated Rust, TypeScript, or CSS formatters inside embedded blocks.

## Practical Scope Today

If you open a route-oriented Thebe project in an editor today, the expected tooling story is:

- compiler diagnostics work
- generated TypeScript mirrors work
- route/layout navigation works
- semantic highlighting, formatting, rename, and code actions work for the currently supported Thebe surface
- template, attribute, event, and component completions work

If you want polished language ergonomics comparable to a mature framework, that is still future work.
