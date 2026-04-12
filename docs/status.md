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
- CLI commands:
  - `thebe new`
  - `thebe dev`
  - `thebe dev --watch`
  - `thebe check`
- `thebe-lsp` language server with diagnostics, hover, document symbols, definition, references, and initial completions.

## Implemented, But Still Narrow

- Template expressions are intentionally limited to simple identifiers and dotted field access.
- Completion support currently focuses on route templates, not the full language surface.
- Hover and definition for template bindings still operate at the binding-occurrence level rather than exact field definitions inside nested `Props` types.
- Editor TypeScript support relies on generated `.thebe/client/**` mirrors instead of a dedicated editor extension.

## Planned Or Missing

- General `src/components/**/*.trs` compilation is still planned. The shipped compiler path currently covers routes and `_layout.trs`, not standalone components.
- Named slot composition outside route layouts is still planned.
- Tree-sitter grammar for `.trs`.
- Syntax highlighting grammar for editors.
- Packaged editor extension and auto-launch story for `thebe-lsp`.
- Template attribute support for dynamic `:class` and generic `:attr` bindings.
- Richer completions for attributes, directives, component tags, and component props.
- Rename, code actions, formatting, and semantic tokens in the LSP.
- A dedicated `thebe build` production build flow.

## Reading The Docs Safely

Some pages in this doc set describe the intended Thebe model, not only the shipped implementation. When in doubt:

- routes, layouts, SSR, hydration, scoped CSS, `.thebe` artifacts, and the current LSP are real
- generic components, polished syntax highlighting, and a full editor extension are not shipped yet
