# tree-sitter-thebe

This package contains an initial tree-sitter grammar for Thebe `.trs` files.

The first cut focuses on:

- explicit tag nodes for top-level Thebe blocks such as `<script setup>`, `<script lang="ts">`, `<script>`, `<style>`, and `<head>`
- structured open, close, and self-closing tags for normal HTML tags and PascalCase component tags
- attribute names and quoted/unquoted attribute values, including Thebe surfaces like `:attr`, `on*`, `slot`, and `name`
- raw-content grouping for Thebe block bodies, so script/style contents stay isolated from template tags
- injection queries for Rust, TypeScript, and CSS inside Thebe block bodies
- template bindings in `{{ dotted.path }}` form

It is intentionally conservative and does not yet attempt full HTML-aware nesting or full embedded Rust/TypeScript/CSS subgrammars. The grammar now exposes injection points for those raw block bodies, but the VS Code extension in `packages/thebe-vscode/` still uses the TextMate grammar for editor highlighting today.
