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
- `.thebe/manifest.json` is currently version 6 and records route/layout/component metadata, generated paths, handler signatures, direct component dependencies, template bindings, exact field-level template symbol definitions, source spans, and route template symbols derived from `Props`.
- `.thebe/diagnostics.json` is currently version 1 and records structured project and file diagnostics with relative paths and source spans.
- `packages/thebe-vscode/` ships a packaged VS Code extension with `.trs` language registration, snippets, TextMate highlighting, automatic `thebe-lsp` startup, command palette plus editor title/context actions for opening a route's generated `.thebe/client/**` and `.thebe/types/**` mirrors beside the source `.trs` file while preserving matching locations when available, an `Open Inline TypeScript View` command that opens the current `<script lang="ts">` block as a stable provider-backed virtual TypeScript document with local definition/reference jumps back into the source route, `Props` type definition jumps into the generated `.thebe/types/**` mirror, and live refresh from source `.trs` edits, and an `Open Inline Rust View` command that opens the current `<script setup>` block as an untitled Rust snapshot with local definition/reference jumps back into the source route.
- `packages/tree-sitter-thebe/` ships an initial tree-sitter grammar for `.trs` block tags, nested generic component/html template elements, HTML comments, attributes, template bindings, and raw script/style injection points.

## Current LSP Features

`thebe-lsp` currently supports:

- Diagnostics sourced from `.thebe/diagnostics.json`.
- Hover for route handlers, precise nested `Props` fields inside template bindings, component tags/import aliases, component props, and named-slot attributes on `<template slot="...">` / `<slot name="..." />`.
- Document highlights for Thebe-owned symbols in the current `.trs` file.
- Document symbols for route handlers, template bindings, and component props.
- Go-to-definition between `.trs` source files and generated Rust/TypeScript artifacts, plus exact nested `Props` field targets, component tag/prop targets, and component import aliases.
- Go-to-definition from a route `<script lang="ts">` block into its generated `.thebe/client/**` mirror, preserving the corresponding script position so it can act as a concrete bridge into external TypeScript tooling before full inline `tsserver` integration exists.
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
  - template attribute completions for bound attributes such as `:class`, generic `:attr`, common `on*` handlers, and named-slot attributes on `<template>` / `<slot>`

## Editor Behavior Already Implemented

The editor loop is not disk-only anymore.

- Unsaved source buffers are tracked through overlay-backed refreshes.
- `didOpen`, `didChange`, `didSave`, and `didClose` all refresh compiler state through `thebe-project`.
- `didChange` refreshes are debounced.
- The LSP keeps last-good artifacts so hover, definition, and references can keep working during transient invalid edits.
- Diagnostics publishing is coalesced so unchanged diagnostics are not republished on every refresh.
- The extension package now has a lightweight extension-host harness that validates the generated client/types commands plus the inline TypeScript virtual document and inline Rust snapshot commands, including TypeScript live refresh and the source round-trips for both bridges, against a real VS Code instance.

## Remaining Gaps

The editor story is broader now, but a few edges are still intentionally narrow:

- The tree-sitter grammar is still initial and does not yet model full HTML-aware tag matching or full embedded Rust/TypeScript/CSS subgrammars, even though it now exposes nested generic template elements plus script/style raw-content injection queries.
- Rename support is currently scoped to route handlers, route template symbols, component prop definitions/usages, component tag/import relationships across known `.trs` sources, and client event handlers rather than arbitrary Rust or TypeScript symbols.
- Formatting now normalizes top-level `.trs` structure and uses best-effort block formatters for embedded Rust, TypeScript, and CSS, but it still does not provide full language-service formatting semantics inside those blocks.

## Planned Rust Analyzer Bridge

The first Rust-side virtual-document step now exists in the VS Code extension as an explicit `Open Inline Rust View` command, but deeper `rust-analyzer` integration still needs one extra layer of generated server context.

- Source of truth should stay the current `.trs` buffer plus the overlay-backed `thebe-project` refresh path, not stale disk content.
- The current inline Rust snapshot already mirrors the route's deterministic `.thebe/server/routes/**` target path and records a source map back into the original `<script setup>` span.
- The next step is to replace the bare extracted block with the deterministic wrapper/import/module scaffolding that `thebe-codegen` emits around route handlers so the virtual document matches generated server context more closely.
- After that wrapper is stable, diagnostics, definition, hover, and rename should round-trip through the virtual Rust source map instead of inferring relationships from generated file text after the fact.
- The initial refresh model can stay explicit-command or on-demand open first, then graduate to debounced live regeneration once the offset map is stable.
- Once that fuller virtual Rust layer is stable, `rust-analyzer` integration can ride on top of it the same way the inline TypeScript phase now rides on an untitled TypeScript snapshot.

## Practical Scope Today

If you open a route-oriented Thebe project in an editor today, the expected tooling story is:

- compiler diagnostics work
- generated TypeScript mirrors work
- inline Rust snapshots and provider-backed inline TypeScript virtual documents work as explicit opt-in editor bridges
- route/layout navigation works
- semantic highlighting, formatting, rename, hover, and code actions work for the currently supported Thebe surface
- template, attribute, event, component, and named-slot completions work
- the VS Code extension ships snippets for route/component boilerplate plus named-slot declaration and fill patterns

If you want polished language ergonomics comparable to a mature framework, that is still future work.
