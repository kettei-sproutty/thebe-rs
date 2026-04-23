# Architecture

Thebe is a compiler-driven framework that bridges Rust (server) and TypeScript (client) through a unified `.trs` file format. It leverages existing, battle-tested tools like Axum, SWC, and LightningCSS rather than reinventing the wheel.

## Core Philosophy
1. **Server-First, Axum Native:** Thebe compiles down to a standard `axum::Router`. It is built *on top* of Axum, not alongside it.
2. **Context-Aware Parsing:** The compiler uses an HTML-aware tokenizer to accurately respect web semantics. It does *not* rely on brittle regex splitting.
3. **Explicit Contracts:** Server and client boundaries are strict. `Props` are the initial bridge; mutations are handled via standard HTTP flows.
4. **Rigid Hydration:** The template compiler emits highly specific, DOM-context-aware hydration anchors rather than shipping a full Virtual DOM.

## Compiler Pipeline

```text
.trs file
  │
  ├── thebe-parser (HTML-aware tokenizer)
  │     ├── <script setup> / <script> → Rust code
  │     ├── <style> → LightningCSS
  │     ├── <script lang="ts"> → swc
  │     └── Template → tokenized template structure
  │
  ├── thebe-analyzer
  │     └── Analyzes client script handlers and emits typed mirror JS
  │
  ├── thebe-codegen
  │     ├── Expands components and default slots
  │     ├── Injects hydration markers and dynamic attribute bindings
  │     └── Generates Rust route modules, asset handlers, and dev artifacts
  │
  ├── thebe-css
  │     └── Scopes CSS and injects scope attributes
  │
  ├── thebe-runtime
  │     └── Renders templates, assembles the HTML shell, and serves hotpatch runtime glue
  │
  ├── thebe-project
  │     └── Writes `.thebe/manifest.json`, `.thebe/diagnostics.json`, `.thebe/client/**`, and `.thebe/types/**`
  │
  ├── thebe-cli
  │     └── Orchestrates build, watch, hotpatch, and project refresh
  │
  └── thebe-lsp
        └── Consumes `.thebe` artifacts plus unsaved overlays for tooling
```

## Parsing Strategy
- **Outer SFC & Template:** Parsed using an HTML-aware parser/tokenizer (e.g., `html5ever` style). This ensures that embedded closing tags in strings, nested components, and malformed (but valid) HTML are handled exactly as a browser would.
- **TypeScript Blocks:** Passed directly to SWC for parsing and analysis.
- **Rust Blocks:** Handled as raw tokens until the codegen/macro phase, ensuring they compile cleanly into standard Rust modules.

## Generated Output
When you build a Thebe project, components and routes are compiled into standard Rust modules.
For example, `src/components/Card.trs` generates a Rust module accessible via `crate::components::Card`, allowing for explicit and stable imports across your project.
