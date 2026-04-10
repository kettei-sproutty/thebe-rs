# AGENTS.md

High-signal guidance for OpenCode agents working on Thebe.

## Project Status

**Early design phase.** Moving towards Milestone 1 (server-only SSR). No crates exist yet. See README.md:462-480 for the MVP implementation plan.

## What Thebe Is

A compiler-driven full-stack Rust web framework using Single File Components (`.trs`). Each `.trs` file combines:
- `<script setup>`: Rust server handlers (Axum)
- `<script lang="ts">`: TypeScript client reactivity
- Template: HTML with `{{ }}` bindings
- `<style>`: Scoped CSS via LightningCSS

Compiles to `axum::Router` with SSR + fine-grained client hydration. Zero virtual DOM.

## Workspace Structure

Cargo workspace with resolver="3". Future crates (not yet created):
- `crates/thebe-parser/` — `.trs` → SFC AST (HTML-aware tokenizer, not regex)
- `crates/thebe-analyzer/` — Reactive variable analysis from TS
- `crates/thebe-template/` — Template → SSR string + hydration markers
- `crates/thebe-codegen/` — Rust handler + Axum route generation
- `crates/thebe-css/` — LightningCSS transform + style scoping
- `crates/thebe-macros/` — `#[thebe::get]`, `#[thebe::post]`, etc.
- `crates/thebe-runtime/` — SSR render, Props injection, Axum re-exports
- `crates/thebe-cli/` — `thebe dev`, `thebe build`, FS scanner
- `packages/thebe-client/` — npm package: signals, hydration runtime

See README.md:391-410 for full structure.

## Code Style

- **Rustfmt:** 2-space indentation, no tabs (see `rustfmt.toml`)
- **Clippy:** Pedantic warnings enabled workspace-wide
- **Imports:** Explicit imports everywhere (e.g., `use crate::components::Button`). No glob imports.

## Critical Architecture Constraints

### Parsing Rules
- **Context-aware parsing required.** Cannot use regex splitting for `.trs` files. Must use HTML-aware tokenizer (e.g., `html5ever` style) to safely extract `<script>`, `<style>`, template blocks. See README.md:433-443.
- `<script lang="ts">` passed to `swc` for TypeScript parsing.
- `<style>` extracted for LightningCSS processing.

### Reactivity Model (v0)
- **Deep reactive Proxy** for `getProps<Props>()` in client code. Zero static analysis for v0.
- Props never mutated in template expressions. Only mutations in `<script lang="ts">` trigger UI updates.
- Complex template expressions (ternaries, arithmetic, function calls) forbidden in `{{ }}`. Must use `derived()` in TS. See README.md:486 (Non-Goals).

### Hydration Protocol
- SSR output wraps reactive bindings in comment markers: `<!--thebe:counter-->0<!--/thebe:counter-->`
- Client runtime uses `TreeWalker` to find markers and bind signals to text nodes. No VDOM diffs.
- **Special handling for table/select:** Comment markers can be hoisted by browser parser. Must detect these contexts and use data attributes instead. See README.md:452-456.

### Type Bridge
- Use `ts-rs` crate to auto-generate TypeScript `.d.ts` from Rust `Props` structs. Zero double-typing. See README.md:445-447.

## File System Router

Routes live in `src/routes/`, components in `src/components/`.
- Routes define handlers via `#[thebe::get]`, `#[thebe::post]`, etc. in `<script setup>`
- Components have no `<script setup>` and no handlers. Use `<script>` (no "setup") to define `Props`.
- Dynamic segments: `[slug].trs` → `/blog/:slug` → `Path<String>` extractor

See README.md:112-149.

## Axum Integration

Thebe compiles to standard `axum::Router`. All Axum extractors work (State, Path, Query, Json, etc.). Tower middleware, WebSockets, streaming supported. See README.md:362-388.

## Milestone 1 Goals

Server-only SSR. Build:
1. `thebe-parser`: Basic block extraction from `.trs`
2. `thebe-codegen`: Wrap `<script setup>` into async Axum handler
3. **Goal:** Run `thebe dev` and see static Rust-generated HTML in browser

See README.md:462-464 for full milestone breakdown.

## Non-Goals for v0

Explicitly **out of scope** to keep architecture simple:
- Complex template expressions (inline ternaries, arithmetic in `{{ }}`)
- Scoped slots (passing reactive vars down into slot blocks)
- Event modifiers (`.prevent`, `.stop`, inline arrows in `onclick`)
- Server-side suspense/streaming
- Custom hydration markers (protocol is rigid)

See README.md:483-491.

## Documentation

Read `docs/*.md` for deep dives:
- `architecture.md` — Compiler pipeline, parsing strategy
- `syntax-and-semantics.md` — `.trs` file structure
- `routing-and-handlers.md` — File-system router, Axum handlers
- `state-and-reactivity.md` — Props bridge, client reactivity model
- `components.md` — Component Props, slots
- `hydration.md` — Marker protocol, TreeWalker strategy

## Example .trs File

See `stupid.trs` in root for a minimal working example (design reference, not functional yet).

## Common Pitfalls

1. **Do not parse .trs with regex.** Use HTML-aware tokenizer to respect nested tags, strings with HTML chars, etc.
2. **Do not add glob imports.** Explicit `use` statements required everywhere.
3. **Client TS cannot mutate Props directly in template.** Mutations only in `<script lang="ts">` functions.
4. **Handlers must return `Props`, not HTML.** SSR rendering is template-driven, not manual string building.
5. **Components have no handlers.** Only routes (files in `src/routes/`) define `#[thebe::get]`, etc.
