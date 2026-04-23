# Components

> Status: standalone `src/components/**/*.trs` compilation is shipped. Components support typed `Props`, explicit Rust imports, scoped CSS, PascalCase tag expansion, and a default `<slot />`. Named slots are still planned.

Components are reusable `.trs` files that live in the `src/components/` directory. They possess similar capabilities to route files but operate with stricter constraints to maintain application hygiene.

## The `<script>` Block
Unlike routes (which use `<script setup>`), components use a `<script>` block. This block defines the component's server-side constraints.
- **Allowed:** Imports, helper functions, Struct derivations, and the component's `Props`.
- **Forbidden:** HTTP Handlers (`#[thebe::get]`, etc.). A component cannot expose an endpoint.

```html
<!-- src/components/Card.trs -->
<script>
// Compiled into the server-side module for `crate::components::Card`
pub struct Props {
    pub title: String,
    pub active_class: String,
}
</script>

<script lang="ts">
  let props = getProps<Props>();
</script>

<div class="card" :class="props.active_class">
  <h2>{{ props.title }}</h2>
  <slot /> <!-- Renders children passed from the parent -->
</div>
```

## Explicit Imports
When using a component in another file, you must explicitly import it via its generated Rust module path. This prevents namespace collisions and keeps your codebase easy for IDEs to analyze.

```html
<!-- src/routes/index.trs -->
<script setup>
use crate::components::Card;

#[thebe::get]
pub async fn handler() -> Props { /* ... */ }
</script>

<Card :title="item_title" :active_class="card_kind">
  <p>Inside the default slot!</p>
</Card>
```

## Slots
Slots allow parents to pass HTML fragments into children.
- **Scope Ownership:** Slot content fundamentally belongs to the **parent's** reactivity scope. Any bindings within the passed slot resolve against the parent's `getProps()`, not the child's.
- **Named Slots:** Still planned. Today the shipped component slot surface is the default `<slot />` only.
