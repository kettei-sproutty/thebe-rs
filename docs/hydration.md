# Hydration Protocol

Thebe does not use a Virtual DOM. Instead, it relies on a rigid, context-aware hydration protocol to bind reactive client state surgically to the DOM.

## Context-Aware Anchors

The template compiler analyzes your HTML structure during the build step. It knows that inserting loose `<-- comments -->` into certain HTML elements causes browsers to aggressively reorganize the DOM tree before JavaScript even runs.

To prevent this, Thebe generates a **Hydration Anchor Matrix** based on the specific DOM context:

1. **Safe Contexts (Phrasing Content, Divs, Spans):**
   The compiler uses paired comment markers bounding a specific text node.
   *Template:* `<span>{{ counter }}</span>`
   *SSR Emit:* `<span><!--thebe:counter-->0<!--/thebe:counter--></span>`

2. **Unsafe Contexts (Tables, Selects, Lists with strict whitespace):**
   Comment anchors inside elements like `<table>`, `<colgroup>`, or `<select>` are notoriously unreliable (browsers often hoist them out). In these contexts, the compiler automatically shifts to utilizing element-bound data attributes instead of loose text markers.
   *Example Approach:* The nearest valid parent element receives a `data-thebe-bind="key"` attribute, and the runtime hydrates the specific child node relative to that anchor.

3. **Attributes and Events:**
   Dynamic attributes (`:class="props.active"`) and events (`onclick="fn"`) are natively attached to the element they decorate via `data-thebe-*` attributes during SSR, which the `thebe-client` runtime consumes and cleans up during initialization.

## Predictability
By formalizing the hydration fallback rules based on DOM context, Thebe ensures that:
- SSR output is completely deterministic.
- Elements are updated precisely without full-subtree repaints.
- Users inspecting the DOM see exactly how the client is attached to the server's HTML.
