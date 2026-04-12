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
- `.thebe/manifest.json` is currently version 4 and records route/layout metadata, generated paths, handler signatures, template bindings, source spans, and route template symbols derived from `Props`.
- `.thebe/diagnostics.json` is currently version 1 and records structured project and file diagnostics with relative paths and source spans.

## Current LSP Features

`thebe-lsp` currently supports:

- Diagnostics sourced from `.thebe/diagnostics.json`.
- Hover for route handlers and template binding occurrences.
- Document symbols for route handlers and template bindings.
- Go-to-definition between `.trs` source files and generated Rust/TypeScript artifacts.
- References for route handlers and template bindings.
- Initial `.trs` completions:
  - top-level block snippets such as `<head>`, `<script setup>`, `<script lang="ts">`, and `<style>`
  - template symbol completions from route `Props` metadata plus current unsaved source
  - event-handler name completions from the current `<script lang="ts">` block

## Editor Behavior Already Implemented

The editor loop is not disk-only anymore.

- Unsaved source buffers are tracked through overlay-backed refreshes.
- `didOpen`, `didChange`, `didSave`, and `didClose` all refresh compiler state through `thebe-project`.
- `didChange` refreshes are debounced.
- The LSP keeps last-good artifacts so hover, definition, and references can keep working during transient invalid edits.
- Diagnostics publishing is coalesced so unchanged diagnostics are not republished on every refresh.

## Missing Tooling

The following editor pieces are still missing:

- A tree-sitter grammar for `.trs`.
- A TextMate grammar or other syntax highlighter for `.trs`.
- A packaged editor extension that wires up file associations, grammar registration, snippets, and `thebe-lsp` startup automatically.
- Semantic tokens.
- Rename support.
- Code actions.
- Formatting support.
- Component tag and component prop completions.
- Template attribute completions for directives and bound attributes such as `:if`, `:class`, and generic `:attr` bindings.
- Precise field-level template symbol metadata for exact hover and go-to-definition targets inside nested `Props` structures.

## Practical Scope Today

If you open a route-oriented Thebe project in an editor today, the expected tooling story is:

- compiler diagnostics work
- generated TypeScript mirrors work
- route/layout navigation works
- initial template and event completions work

If you want polished language ergonomics comparable to a mature framework, that is still future work.
