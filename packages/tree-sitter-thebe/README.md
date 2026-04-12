# tree-sitter-thebe

This package contains an initial tree-sitter grammar for Thebe `.trs` files.

The first cut focuses on:

- top-level Thebe block delimiters such as `<script setup>`, `<script lang="ts">`, `<script>`, `<style>`, and `<head>`
- template bindings in `{{ dotted.path }}` form
- generic HTML-like tags as fallback tokens

It is intentionally conservative and does not yet attempt full HTML-aware nesting or embedded Rust/TypeScript/CSS subgrammars. The VS Code extension in `packages/thebe-vscode/` still uses the TextMate grammar for editor highlighting today.
