# Status

This page tracks what is actually implemented in the repository today versus what is still planned.

## Implemented

- HTML-aware `.trs` parsing in `crates/thebe-parser`.
- Route compilation in `crates/thebe-codegen`.
- SSR rendering through `crates/thebe-runtime`.
- Scoped CSS through `crates/thebe-css`.
- File-system routing for `src/routes/**/*.trs`.
- `_layout.trs` route layouts, including `<slot />` replacement for layout composition.
- `app.html` shell support with `%thebe.head%`, `%thebe.body%`, and optional `%thebe.title%`.
- `<head>` support in routes and layouts.
- Fine-grained hydration markers for reactive template bindings.
- Client `getProps<Props>()` bridge with `ts-rs`-generated types for client routes.
- Event-handler discovery and client runtime wiring for `on*` attributes.
- Generated `.thebe/` workspace artifacts through `crates/thebe-project`.
- Production route JS/CSS extraction into hashed `/.thebe/assets/*` assets served by generated route helpers.
- CLI commands:
  - `thebe new`
  - `thebe dev`
  - `thebe dev --watch`
  - `thebe build`
  - `thebe check`
- `thebe-lsp` language server with diagnostics, semantic tokens, document highlights, linked editing, hover, document symbols, definition, references, rename, code actions, formatting, and richer completions.
- Packaged editor assets under `packages/thebe-vscode/` and `packages/tree-sitter-thebe/`.

## Implemented, But Still Narrow

- Template expressions are intentionally limited to simple identifiers and dotted field access.
- The shipped formatter now does best-effort formatting for embedded Rust, TypeScript, and CSS blocks, but it still does not provide full language-service formatting semantics inside those blocks.
- Rename support is currently limited to route handlers, route template symbols, component prop definitions/usages, local component tag/import relationships, and client event handlers.
- The tree-sitter grammar is still an initial grammar rather than a full HTML-aware parser.
- Production assets are emitted under `.thebe/assets` and served by generated routes rather than a standalone public dist pipeline.

## Planned Or Missing

- General `src/components/**/*.trs` compilation is still planned. The shipped compiler path currently covers routes and `_layout.trs`, not standalone components.
- Named slot composition outside route layouts is still planned.
- Template attribute support for dynamic `:class` and generic `:attr` bindings.

## Reading The Docs Safely

Some pages in this doc set describe the intended Thebe model, not only the shipped implementation. When in doubt:

- routes, layouts, SSR, hydration, scoped CSS, `.thebe` artifacts, the current LSP, and the initial editor extension assets are real
- generic standalone component compilation and runtime dynamic attribute support are not shipped yet
