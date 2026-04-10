# State and Reactivity

Thebe bridges the server and client using a strictly defined unidirectional flow. It is important to remember that Thebe is **not** a magical full-stack synchronization framework; it is an SSR-first framework with surgical client-side interactivity.

## The `Props` Contract
`Props` is the initial server-rendered payload for a route or component.
It represents the state of the system at the exact moment the HTTP request was fulfilled.

1. The server serializes `Props` into a secure JSON block within the HTML response.
2. On the client, `<script lang="ts">` calls `getProps<Props>()`.
3. `getProps()` immediately parses the JSON and exposes a reactive, local view of that initial payload.

## Client Mutations are Local
When you mutate state on the client, you are **only updating the local UI**.

```ts
<script lang="ts">
  let props = getProps<Props>();

  function increment() {
    // This updates the DOM immediately via Thebe's hydration markers.
    // IT DOES NOT send a network request to the server.
    props.counter += 1;
  }
</script>
```

Client-side mutations do not automatically sync back to the server. To update the server's truth, you must use standard web mechanisms like forms and route handlers (see [Forms and Mutations](./forms-and-mutations.md)).

## Derived State
Because inline template logic (e.g., `{{ props.count * 2 }}`) is intentionally restricted, complex reactive computations must be modeled using `derived()` inside your TypeScript block.

```ts
<script lang="ts">
  let props = getProps<Props>();

  // Re-evaluates only when `props.counter` changes
  const displayLabel = derived(() => `Current Count: ${props.counter}`);
</script>

<span>{{ displayLabel }}</span>
```

For v0, `getProps()` uses a deeply reactive Proxy to intercept local state changes. This is a convenience for local interaction, but the core mental model remains: **server renders initial state, client plays with a local copy, forms flush reality back to the server.**
