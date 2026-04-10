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
  │     └── Template → HTML AST
  │
  ├── thebe-analyzer
  │     └── Identifies reactive bindings vs static DOM
  │
  ├── thebe-template
  │     └── Emits SSR render functions & Hydration Anchor Matrix
  │
  ├── thebe-codegen
  │     ├── Server: Generates Rust modules, handlers, and Axum routing code
  │     └── Client: Generates TS signals, proxies, and hydration logic
  │
  └── thebe-cli
        └── Orchestrates the build, watches files, and serves local dev
```

## Parsing Strategy
- **Outer SFC & Template:** Parsed using an HTML-aware parser/tokenizer (e.g., `html5ever` style). This ensures that embedded closing tags in strings, nested components, and malformed (but valid) HTML are handled exactly as a browser would.
- **TypeScript Blocks:** Passed directly to SWC for parsing and analysis.
- **Rust Blocks:** Handled as raw tokens until the codegen/macro phase, ensuring they compile cleanly into standard Rust modules.

## Generated Output
When you build a Thebe project, components and routes are compiled into standard Rust modules.
For example, `src/components/Card.trs` generates a Rust module accessible via `crate::components::Card`, allowing for explicit and stable imports across your project.
