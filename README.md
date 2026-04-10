# Thebe

A full-stack web framework for Rust, built on [Axum](https://github.com/tokio-rs/axum), using Single File Components (`.trs`).

Write your server logic in Rust, your reactive client code in TypeScript, your styles in CSS — all in one file.

---

## Overview

A `.trs` file has four sections:

```html
<script setup>
  <!-- Rust: server handlers, data fetching, Props definition -->
</script>

<script lang="ts">
  <!-- TypeScript: client reactivity -->
</script>

<!-- Template: HTML with {{ }} bindings -->

<style>
  /* CSS: scoped via LightningCSS */
</style>
```

The Thebe compiler transforms each section independently and wires them together at build time. The result is an Axum router with SSR, fine-grained client hydration, and scoped CSS — with zero boilerplate.

---

## Example

```html
<!-- src/routes/index.trs -->

<script setup>
use anyhow::Result;
use reqwest::Client;
// Explicit imports keep scope clear and IDEs happy
use crate::components::Button;

async fn fetch_title() -> Result<String> {
    let client = Client::new();
    let response = client.get("https://api.example.com/data").send().await?;
    Ok(response.text().await?)
}

struct Props {
    title: String,
    counter: i32,
}

#[thebe::get]
pub async fn handler() -> Props {
    let title = fetch_title().await.unwrap_or_default();
    Props { title, counter: 0 }
}
</script>

<script lang="ts">
  let props = getProps<Props>();

  function increment() {
    props.counter += 1;
  }
</script>

<h1>{{ props.title }}</h1>
<Button :onclick="increment">Increment</Button>
<span>Counter: {{ props.counter }}</span>

<form method="post" action="/submit">
  <!-- Standard forms are the canonical way to mutate server state -->
  <input type="text" name="update" />
  <Button type="submit">Save</Button>
</form>

<style>
  h1 {
    color: blue;
  }
</style>
```

`main.rs`:

```rust
#[tokio::main]
async fn main() {
    thebe::run().await;
}
```

---

## Documentation

Thebe's design strictly bounds complexity by keeping server code, client updates, and routing explicitly separated. Dive into the core concepts:

- [Architecture & Parsing Strategy](docs/architecture.md)
- [Syntax & File Semantics](docs/syntax-and-semantics.md)
- [Routing & Axum Handlers](docs/routing-and-handlers.md)
- [State & Reactivity](docs/state-and-reactivity.md)
- [Forms & Server Mutations](docs/forms-and-mutations.md)
- [Components & Slots](docs/components.md)
- [Context-Aware Hydration](docs/hydration.md)

---

## Project Structure

```
my-app/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── routes/          # file-system router
│   │   ├── index.trs            →  GET /
│   │   ├── about.trs            →  GET /about
│   │   └── blog/
│   │       ├── index.trs        →  GET /blog
│   │       └── [slug].trs       →  GET /blog/:slug
│   └── components/      # reusable components (no handlers)
│       ├── Button.trs
│       ├── Card.trs
│       └── layout/
│           └── Header.trs
└── public/              # static assets (served via tower-http)
```

### FS Router Rules

| File path | Route |
|---|---|
| `src/routes/index.trs` | `/` |
| `src/routes/about.trs` | `/about` |
| `src/routes/blog/index.trs` | `/blog` |
| `src/routes/blog/[slug].trs` | `/blog/:slug` |

Dynamic segments (`[slug]`) map directly to Axum's `Path` extractor:

```rust
#[thebe::get]
pub async fn handler(Path(slug): Path<String>) -> Props {
    /* ... */
}
```

---

## Handlers

Annotate functions in `<script setup>` with the appropriate HTTP method attribute. The route is derived from the file's location in `src/routes/` — you never write the path manually.

```rust
#[thebe::get]
pub async fn handler() -> Props { /* ... */ }

#[thebe::post]
pub async fn create(Json(body): Json<CreateBody>) -> Props { /* ... */ }

#[thebe::delete]
pub async fn remove(Path(id): Path<u32>) -> Props { /* ... */ }
```

Multiple handlers in one file map to multiple HTTP methods on the same route.

### Axum Extractors

All Axum extractors work directly in handler function parameters:

```rust
#[thebe::get]
pub async fn handler(
    State(db): State<Db>,
    Path(id): Path<u32>,
    Query(q): Query<SearchParams>,
) -> Props {
    /* ... */
}
```

---

## Props

`Props` is the bridge between server and client. Define it as a plain Rust struct in `<script setup>`. The compiler automatically derives `Serialize` and inlines the serialized value into the HTML response as:

```html
<!-- The JSON serializer strictly escapes HTML characters (e.g. `<` -> `\u003c`, `&` -> `\u0026`) to prevent XSS attacks -->
<script id="__thebe_props" type="application/json">{"title":"...","counter":0}</script>
```

---

## Client Reactivity

`<script lang="ts">` runs in the browser. `getProps<Props>()` reads the server-inlined JSON.

For **v0**, `getProps<Props>()` returns a deeply reactive Proxy object (like Vue 3's `reactive`). This gives deep mutation tracking for free, so you can write normal JavaScript without worrying about assignment rewriting or forced destructuring.

```ts
let props = getProps<Props>();  // Returns a reactive Proxy

// props.title is never mutated → remains static
// props.counter is mutated below → triggers UI updates

function increment() {
    props.counter += 1;  // Proxy intercepts the set() and triggers UI updates
}

function addUser() {
    props.users.push("Alice"); // Deep reactivity works out of the box
}
```

*Future Path (v1 "Smart Compiler"):* In later versions, the compiler will evolve to use static usage analysis. Instead of shipping a runtime Proxy, the SWC pass will determine exactly which fields are mutated or read reactively, and automatically upgrade only those specific fields to signals at compile time, reverting to "zero JS cost" for static variables.

### `derived()`

Expressions mixing static and reactive values, or complex reactive computations, must be wrapped in `derived()`.

```ts
let props = getProps<Props>();

const label = derived(() => `Page ${props.counter} of 10`);
```

```html
<span>{{ label }}</span>
```

`derived` maps to a computed signal — it re-evaluates only when its reactive dependencies change.

---

## Template

The template section is plain HTML with `{{ expr }}` bindings. The compiler classifies each binding:

- **Static** (`{{ props.title }}`): rendered server-side only, emitted as plain text
- **Reactive** (`{{ props.counter }}`): rendered server-side with comment markers for fine-grained client hydration

*Note on Expression Boundaries:* For v0, `{{ expr }}` only supports **simple identifiers** and **property access** (e.g. `{{ props.user.name }}`). Complex logic (arithmetic, ternaries, function calls) must be pushed into `<script lang="ts">` using `derived()`.

```html
<!-- SSR output for reactive binding -->
<span>Counter: <!--thebe:counter-->0<!--/thebe:counter--></span>
```

The client runtime locates these markers and creates a text node driven by the signal. Only that text node ever touches the DOM when the signal changes — no virtual DOM, no full re-render.

### Attributes and Event Handlers

**Dynamic Attributes:**
Use a colon `:` prefix to bind an attribute to a JavaScript expression. Do not use `{{ }}` inside attribute strings.
```html
<!-- Good -->
<Card :title="post.title" />

<!-- Bad (Compile Error) -->
<Card title="{{ post.title }}" />
```

**Event Handlers:**
```html
<button onclick="increment">+1</button>
```

`onclick="fnName"` compiles to `data-thebe-on="click:increment"`. The client runtime attaches real event listeners after hydration. V0 only supports passing a function identifier (no inline arguments or modifiers like `.prevent`).

---

## Components

Files in `src/components/` have no `<script setup>` and no handlers. Instead, they use a strict `<script>` block to define server-side helpers and `Props`.

### Component Props (The `Props` trait)
To define what a component accepts, declare a generic `Props` struct in a standard `<script>` block. This code is compiled into the server-side module for the component.

```html
<!-- src/components/Card.trs -->
<script>
pub struct Props {
    pub title: String,
    pub excerpt: String,
}
</script>

<script lang="ts">
  // Client gets perfect autocomplete
  let props = getProps<Props>();
</script>

<div class="card">
  <h2>{{ props.title }}</h2>
  <p>{{ props.excerpt }}</p>
  <slot />
</div>
```

Usage in a route requires an **explicit import**:

```html
<script setup>
use crate::components::Card;
</script>

<Card :title="props.post.title" :excerpt="props.post.excerpt">
  <p>Read more...</p>
</Card>
```

### Slots

**Default slot:**

```html
<Card>
  <p>Content goes here</p>
</Card>
```

**Named slots:**

```html
<Layout>
  <thebe:slot name="header">
    <h1>{{ props.title }}</h1>
  </thebe:slot>

  <p>Main content</p>

  <thebe:slot name="footer">
    <small>© 2026</small>
  </thebe:slot>
</Layout>
```

`<thebe:slot>` is a compile-time construct — it emits no DOM element. Slot content belongs to the **parent's reactivity scope**: reactive variables from the parent are available inside slot content.

---

## Styles

`<style>` is processed by [LightningCSS](https://lightningcss.dev/) — minification, vendor prefixes, nesting, and modern CSS transforms are applied automatically.

Styles are **scoped to the component**. The compiler generates a unique attribute (e.g., `data-thebe-c-abc123`) and injects it into both the rendered HTML elements and the CSS selectors:

```css
/* you write: */
h1 { color: blue; }

/* compiled to: */
h1[data-thebe-c-abc123] { color: #00f; }
```

---

## Axum Integration

Thebe is built on top of Axum, not alongside it. `thebe::run()` is just Axum under the hood. You can drop down to raw Axum at any point, ensuring you can inject custom App State (like database pools) cleanly into the file-system router:

```rust
use axum::{routing::get, Router};

#[tokio::main]
async fn main() {
    let db = Db::new();

    // thebe::router() accepts your generic state type S to wire up your extractors
    let app = thebe::router::<Db>()
        .with_state(db)
        .route("/api/health", get(health))  // raw axum route
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower_http::compression::CompressionLayer::new());

    axum::serve(
        tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap(),
        app,
    ).await.unwrap();
}
```

Because the router is a plain `axum::Router`, Tower middleware, WebSockets, streaming, multipart, and any other Axum feature work without any Thebe-specific adapter.

---

## Workspace Layout (Thebe itself)

```
thebe/
├── crates/
│   ├── thebe-parser/       # .trs → SFC AST (block splitter)
│   ├── thebe-analyzer/     # reactive var analysis from <script lang="ts">
│   ├── thebe-template/     # template compiler → SSR string + hydration map
│   ├── thebe-codegen/      # Rust handler + Axum route generation
│   ├── thebe-css/          # LightningCSS transform + style scoping
│   ├── thebe-macros/       # proc-macro: #[thebe::get], #[thebe::post], etc.
│   ├── thebe-runtime/      # SSR render, Props injection, Axum re-export
│   └── thebe-cli/          # `thebe dev`, `thebe build`, FS scanner
└── packages/
    └── thebe-client/       # npm package: signals, hydration runtime, getProps
        └── src/
            ├── signals.ts  # signal / effect / computed (~50 lines, zero deps)
            ├── hydrate.ts  # marker traversal + event wiring
            └── runtime.ts  # getProps<T>(), derived()
```

---

## Build Pipeline

```
.trs file
  │
  ├── thebe-parser      →  SFC { script_setup, script_client, style, template }
  │
  ├── thebe-analyzer    →  ReactiveVars { reactive: Set, static: Set }
  │       │
  │  ┌────┴──────────────────────┐
  │  ▼                           ▼
  │  thebe-template          thebe-codegen (client)
  │  SSR template string      signals + assignment rewrites → .js
  │  + hydration map
  │
  ├── thebe-codegen (server)  →  async axum handler + register_routes()
  │
  └── thebe-css               →  scoped CSS output
```

---Context-Aware Parsing
A `.trs` file cannot be parsed safely with a basic regex. Thebe relies on an HTML-aware tokenizer (compatible with HTML5 parsing rules) to safely map the outer document structure, while deferring TypeScript parsing strictly to `swc`. This guarantees that nested tags, strings containing HTML characters, and strange namespace boundaries are resolved truthfully.
- `<script setup>` / `<script>`: Compiled into standard server-side Rust modules.
- `<script lang="ts">`: Passed to `swc`.
- `<style>`: Extracted for LightningCSS.
- **Template**: The remaining HTML. Parsed into an AST that maps static DOM vs `{{ expr }}` bindings based on their DOM contextgs, escaped tags, or nested HTML in TS). Thebe relies on a more robust block extraction strategy (e.g. leveraging an HTML-compliant tokenizer or Tree-sitter) to safely split the file into:
- `<script setup>`: Extracted as raw Rust code.
- `<script lang="ts">`: Extracted as TypeScript code, parsed via `swc`.
- `<style>`: Extracted as CSS.
- **Template**: The remaining HTML. Parsed into an AST that classifies static DOM vs `{{ expr }}` bindings.

### 2. Rust ↔ TypeScript Type Generation
To eliminate double-typing, Thebe uses the [`ts-rs`](https://crates.io/crates/ts-rs) crate.
The transpiler automatically injects `#[derive(serde::Serialize, ts_rs::TS)]` onto the `Props` struct. During `thebe build`, it emits TypeScript `.d.ts` interfaces directly into the project's hidden cache. Your `<script lang="ts">` gets 100% accurate autocomplete for `getProps<Props>()` directly from the Rust struct definitions.

### 3. Template AST & Hydration Protocol
The template compiler converts `{{ expr }}` into two paired artifacts:
1. **SSR Output**: Reactive bindings are enclosed in special, invisible DOM comment markers.
   *Source:* `<span>{{ counter }}</span>`
   *SSR HTML:* `<span><!--thebe:counter-->0<!--/thebe:counter--></span>`
2. **Client Hydration**: Fast marker traversal.
   On load, the slim `thebe-client` runtime uses a `TreeWalker` to find all `<!--thebe:*-->` comments. It captures a reference to the `TextNode` between the comments and bounds it exclusively to that signal's `effect()`. Virtual DOM diffs are bypassed completely.
   *(Note: For elements with strict parsing rules like `<table>` or `<select>`, comment markers risk being hoisted out of the nested structures by the browser. Thebe's template compiler detects these domains and strategically targets the nearest safe node or injects data attributes instead of loose comment nodes).*

---

## MVP Implementation Plan

**Milestone 1: The Basic Slice (Server Only)**
- `thebe-parser`: Basic block extraction.
- `thebe-codegen`: Wrap `<script setup>` into an async Axum handler.
- *Goal*: Run `thebe dev` and see a static, Rust-generated HTML string in the browser.

**Milestone 2: The Props Bridge**
- Wire up minijinja for SSR templates.
- Compile rust structs into `<script id="__thebe_props">` JSON in the response.
- Inject `ts-rs` definition files for LSP autocomplete.

**Milestone 3: JS Reactivity & Event Wiring**
- `thebe-analyzer` pass to inject proxy behaviors.
- Bundle `thebe-client` (alien-signals + recursive Proxy wrapping + basic event attacher).

**Milestone 4: Fine-grained Hydration**
- Teach the template compiler to emit `<!--thebe:id-->` markers dynamically (accounting for table hoist behaviors).
- `hydrate.ts` wires the `TextNode` UI references to the parsed proxies.
- *Goal*: `onclick` events hydrate correctly and update `props.counter` without DOM repaints.

---

## Non-Goals for v0

To keep the initial release laser-focused and the compiler architecture simple, the following are explicitly **out of scope** for version 0:
- **Complex Template Expressions:** No inline arithmetic, ternaries, or function calls in `{{ }}`. Use `derived()` in `<script lang="ts">`.
- **Scoped Slots:** Passing reactive variables *down* from a component into a slot block.
- **Complex Event Modifiers:** No `.prevent`, `.stop`, or inline arrow functions for `onclick`.
- **Server-Side Suspense / Streaming:** Full document generation happens in one block.
- **Custom Hydration Markers:** We will use static comments or ID-based markers. The protocol will be rigid and documented.

---

## Status

Early design phase. Moving towards Milestone 1.

---

## License

MIT
