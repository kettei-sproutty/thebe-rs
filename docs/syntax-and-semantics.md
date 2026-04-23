# Syntax and Semantics

Thebe uses the `.trs` extension to define routes and components. To ensure predictable behavior, the framework enforces strict semantic boundaries between server code, client code, and templates.

## Block Semantics

### Route Files (`src/routes/**/*.trs`)
Route files define the entry points for your application.

- `<head>`: Optional route or layout head contribution. Non-title tags are merged into `app.html`'s `%thebe.head%`, and `<title>` content renders into `%thebe.title%` when present.
- `<script setup>`: Server route module scope. This block is **compiled into the server-side route module**. It may define standard Rust imports, helper functions, the `Props` struct, and HTTP handlers (annotated with `#[thebe::get]`, `#[thebe::post]`, etc.).
- `<script lang="ts">`: Browser-only client code. Runs after hydration to provide local interactivity.
- **Template**: HTML layout combining static SSR output and fine-grained hydration targets.
- `<style>`: CSS scoped strictly to the current file.

### Component Files (`src/components/**/*.trs`)
Component files define reusable UI elements.

- `<script>`: Server component module scope. This block is **compiled into the server-side component module**. It may define imports, helper functions, and the component's `Props`. **It cannot define HTTP handlers.**
- `<script lang="ts">`: Browser-only client code.
- **Template**: HTML layout with hydration targets and `<slot />` definitions.
- `<style>`: Scoped CSS.

## Explicit Imports
Thebe does **not** use auto-discovery magic for components. Imports must be explicit and use standard Rust module paths. Components compile into deterministic Rust modules.

```rust
<script setup>
use crate::components::layout::Header;
use crate::components::Card;

// ...
</script>

<Header />
<Card :title="post.title" />
```

## Supported Template Grammar (v0)
To keep parsing and hydration reliable, Thebe restricts template expressions to clear, analyzable constructs. Complex logic must be pushed into `<script lang="ts">` using `derived()`.

**Supported:**
- HTML elements and Text nodes
- Component tags (`<Card>`)
- Simple bindings: `{{ ident }}` or `{{ ident.prop.deep }}`
- Dynamic attributes: `:attr="ident.prop"`
- Event bindings: `on*="fnName"` or simple calls like `oninput="fnName(this.value)"`
- Slots: `<slot />`

**Not Supported (Use `derived()` instead):**
- Inline arithmetic (`{{ a + b }}`)
- Ternaries (`{{ ok ? 'yes' : 'no' }}`)
- Function calls (`{{ formatDate(date) }}`)
- Inline object literals or spread attributes
