# Thebe

A compiler-driven, server-first web framework for Rust, built on [Axum](https://github.com/tokio-rs/axum) and centered on Single File Components (`.trs`).

Thebe compiles `.trs` files into standard Axum routes with server-side rendering, scoped CSS, and narrowly scoped client hydration.

The goal is not to replace Axum with a separate platform. The goal is to keep Rust in charge of the server while giving the UI a single file format with explicit boundaries between server logic, client interactivity, templates, and styles.

---

## Status

Thebe is past the server-only proof-of-concept stage. The repository already ships the core route pipeline: `.trs` parsing, SSR, scoped CSS, client hydration, generated `.thebe/` tooling artifacts, a compiler-backed LSP, and initial editor packages for VS Code and tree-sitter.

What is still missing is the rest of the runtime and product surface: dynamic attribute bindings, general component compilation, and deeper polish on the initial editor packages.

For a repo-accurate view of shipped versus planned work, see [docs/status.md](docs/status.md) and [docs/editor-tooling.md](docs/editor-tooling.md).

## Design Priorities

- **Axum-native output:** Thebe compiles to plain `axum::Router` handlers and should always preserve an escape hatch to raw Axum.
- **Context-aware parsing:** `.trs` files must be parsed with HTML-aware tooling, not regex splitting.
- **Explicit boundaries:** Rust handles server work, TypeScript handles local client reactivity, and standard HTTP flows handle server mutation.
- **Minimal hydration:** The runtime should attach to precise DOM nodes instead of diffing full subtrees.
- **Tight v0 scope:** Template syntax and client behavior stay intentionally limited until the server-only slice is stable.

## File Format

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

The Thebe compiler transforms each section independently and wires them together at build time. In the main path, a route handler returns `Props`, the server renders the template, and the client hydrates only the bindings that actually need client-side updates.

---

## Example

```html
<!-- src/routes/index.trs -->

<script setup>
use anyhow::Result;
use reqwest::Client;

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

<h1>{{ title }}</h1>
<button onclick="increment">Increment</button>
<span>Counter: {{ counter }}</span>

<form method="post" action="/submit">
  <!-- Standard forms are the canonical way to mutate server state -->
  <input type="text" name="update" />
  <button type="submit">Save</button>
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
- [Current Status](docs/status.md)
- [Configuration & Tailwind](docs/configuration.md)
- [Editor Tooling & LSP](docs/editor-tooling.md)
- [Hotpatch Engine (planned)](docs/hotpatch-engine.md)
- [Syntax & File Semantics](docs/syntax-and-semantics.md)
- [Routing & Axum Handlers](docs/routing-and-handlers.md)
- [State & Reactivity](docs/state-and-reactivity.md)
- [Forms & Server Mutations](docs/forms-and-mutations.md)
- [Components & Slots (planned)](docs/components.md)
- [Context-Aware Hydration](docs/hydration.md)

---

## Project Structure

```
my-app/
├── Cargo.toml
├── thebe.toml          # Optional compiler hooks and Tailwind CSS config
├── app.html            # outer document shell (`%thebe.head%`, `%thebe.body%`)
├── src/
│   ├── main.rs
│   ├── routes/          # file-system router
│   │   ├── index.trs            →  GET /
│   │   ├── about.trs            →  GET /about
│   │   └── blog/
│   │       ├── index.trs        →  GET /blog
│   │       └── [slug].trs       →  GET /blog/:slug
│   └── components/      # planned reusable components (not implemented yet)
│       ├── Button.trs
│       ├── Card.trs
│       └── layout/
│           └── Header.trs
└── public/              # static assets (served via tower-http)
```

`app.html` defines the document shell for every route. Thebe injects route CSS
and route/layout head contributions into `%thebe.head%`, and the rendered page
body plus hydration scripts into `%thebe.body%`.

If you want per-route titles, add `%thebe.title%` inside a `<title>` tag in
`app.html`. A route or `_layout.trs` file can then declare a top-level
`<head>` block, and its `<title>` content will be rendered into that
placeholder while the rest of the head tags are merged into `%thebe.head%`.

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
The default model is handlers returning `Props` and letting the template own rendering. Redirects and other Axum responses are useful escape hatches, but they should stay explicit.

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

Routes that include `<script lang="ts">` should keep `ts-rs = "12"` in the app's `Cargo.toml`. During `thebe dev`, Thebe writes all generated artifacts into `.thebe/`: `.thebe/server/routes.rs` exposes `thebe_routes()`, `.thebe/server/routes/**` contains the generated Rust modules, `.thebe/manifest.json` describes route/layout/generated-path metadata plus semantic facts like handler signatures, template bindings, template symbols, and source spans, `.thebe/types/**` contains exported `Props` bindings, and `.thebe/client/**` mirrors each client script with a local `Props` import so editors have a concrete TypeScript project to read. `thebe check` complements that output with `.thebe/diagnostics.json`, a versioned diagnostics file that records project-level and file-level validation errors with relative paths and source spans.

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

*Possible later direction, not v0:* If the proxy-based model proves itself, a future compiler pass could narrow runtime reactivity through static analysis. That is intentionally deferred until the basic end-to-end model works.

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

- **Static** (`{{ title }}`): rendered server-side only, emitted as plain text
- **Reactive** (`{{ counter }}`): rendered server-side with comment markers for fine-grained client hydration

*Note on Expression Boundaries:* For v0, `{{ expr }}` only supports **simple identifiers** and **property access** (e.g. `{{ user.name }}`). Complex logic (arithmetic, ternaries, function calls) must be pushed into `<script lang="ts">` using `derived()`.

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

## Components (Planned)

General `src/components/**/*.trs` compilation is part of the intended Thebe model, but it is not implemented in the current compiler yet. Today the shipped composition primitive is route layouts via `_layout.trs`.

The syntax below describes the target component model rather than a feature you can rely on today.

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
│   ├── thebe-analyzer/     # SWC-based client script analysis and event discovery
│   ├── thebe-codegen/      # Rust handler generation, template metadata, typed props export glue
│   ├── thebe-css/          # LightningCSS transform + style scoping
│   ├── thebe-project/      # shared manifest/diagnostics generation + .thebe workspace refresh
│   ├── thebe-runtime/      # SSR render and app shell assembly
│   ├── thebe-cli/          # `thebe new`, `thebe dev`, `thebe check`
│   └── thebe-lsp/          # `tower-lsp` server over `.thebe` manifest + diagnostics artifacts
└── packages/
  └── thebe-client/       # browser runtime bundle embedded by codegen
    └── runtime.js
```

---

## Build Pipeline

```
.trs route/layout
  │
  ├── thebe-parser    →  SfcBlocks { head, script_setup, script_ts, style, template }
  ├── thebe-codegen   →  handler metadata, Rust route modules, template metadata
  ├── thebe-analyzer  →  client script analysis, TS stripping, event discovery
  ├── thebe-css       →  scoped CSS and HTML scope attributes
  ├── thebe-runtime   →  SSR template rendering and app shell assembly at request time
  ├── thebe-project   →  `.thebe/manifest.json`, `.thebe/diagnostics.json`, `.thebe/client/**`, `.thebe/types/**`
  └── thebe-lsp       →  diagnostics, navigation, and completions over those artifacts plus unsaved overlays
```

Current compilation support is route- and layout-focused. General component compilation and editor grammar tooling are still pending.

  ## Key Compiler Constraints

  ### Context-Aware Parsing

  A `.trs` file cannot be split safely with regex. The outer document must be parsed with HTML-aware tooling so embedded closing tags, quoted HTML-like strings, and malformed-but-browser-valid markup are handled the way a browser would handle them.

  - `<script setup>` and `<script>` are extracted as Rust source for server-side modules.
  - `<script lang="ts">` is extracted and parsed with `swc`.
  - `<style>` is extracted for LightningCSS.
  - The remaining template is parsed into an HTML AST that classifies static DOM and reactive bindings.

  ### Rust to TypeScript Type Bridge

  To avoid double-typing, Thebe uses [`ts-rs`](https://crates.io/crates/ts-rs) to generate TypeScript definitions from Rust `Props` structs. `getProps<Props>()` should reflect the server type, not a second hand-maintained declaration.

  Today all generated artifacts are emitted into a generated `.thebe/` workspace:
  - `.thebe/server/routes.rs` is included by `src/main.rs` and exposes `thebe_routes()` for app composition.
  - `.thebe/server/routes/**` contains the generated Rust route modules.
  - `.thebe/manifest.json` records route and layout metadata for tooling, including source files, generated artifact paths, handler signatures, template bindings, template symbols, and source spans for direct editor navigation.
  - `.thebe/diagnostics.json` is written by `thebe check` and captures structured project/file diagnostics with relative source paths and source spans.
  - `.thebe/types/**` contains the exported `ts-rs` bindings for each client route's `Props` type.
  - `.thebe/client/**` contains a typed mirror of each `<script lang="ts">` block that imports its matching `Props` definition.
  - `.thebe/tsconfig.json` gives the editor a dedicated TypeScript project without forcing a root `tsconfig.json` on the app.

  ### Hydration Protocol

  The template compiler emits two artifacts for reactive bindings:

  1. **SSR HTML:** Stable hydration anchors in the rendered document.
  2. **Client metadata:** Enough information for the runtime to reconnect those anchors to local reactive state.

  In safe DOM contexts, Thebe can use paired comment markers around a text node:

  ```html
  <span><!--thebe:counter-->0<!--/thebe:counter--></span>
  ```

  In unsafe contexts such as tables or selects, the compiler must fall back to element-bound anchors instead of loose comments, because the browser may hoist or reorder comment nodes before hydration runs.

---

## Current Status Snapshot

Shipped today:

- `.trs` parsing, SSR rendering, scoped CSS, layouts, app shells, and hydration markers.
- Client `Props` bridging and generated TypeScript mirrors under `.thebe/`.
- CLI flows for `thebe new`, `thebe dev`, `thebe dev --watch`, and `thebe check`.
- `thebe-lsp` diagnostics, semantic tokens, hover, document symbols, definition, references, rename, code actions, formatting, and richer completions.
- Packaged editor assets under `packages/thebe-vscode/` and `packages/tree-sitter-thebe/`.

Still missing:

- Dynamic `:class` and generic `:attr` bindings.
- General standalone component compilation in `src/components/**`.
- Deeper polish on the initial formatter, rename surface, and tree-sitter grammar.

The detailed breakdown lives in [docs/status.md](docs/status.md) and [docs/editor-tooling.md](docs/editor-tooling.md).

---

## Non-Goals for v0

To keep the initial release laser-focused and the compiler architecture simple, the following are explicitly **out of scope** for version 0:
- **Complex Template Expressions:** No inline arithmetic, ternaries, or function calls in `{{ }}`. Use `derived()` in `<script lang="ts">`.
- **Scoped Slots:** Passing reactive variables *down* from a component into a slot block.
- **Complex Event Modifiers:** No `.prevent`, `.stop`, or inline arrow functions for `onclick`.
- **Server-Side Suspense / Streaming:** Full document generation happens in one block.
- **Custom Hydration Markers:** We will use static comments or ID-based markers. The protocol will be rigid and documented.

---

## License

MIT
