# Routing and Handlers

Thebe uses a file-system router that maps directly to Axum underneath. There are no proprietary routing engines—if it works in Axum, it works in Thebe.

## File-System Rules
Files located in `src/routes/` determine your app's URLs.

| File path | Route |
|---|---|
| `src/routes/index.trs` | `GET /` |
| `src/routes/about.trs` | `GET /about` |
| `src/routes/blog/[slug].trs` | `GET /blog/:slug` |

*Precedence:* Static segments (like `/blog/new`) take precedence over dynamic segments (like `/blog/[slug]`). Duplicated methods on the same route result in a compile-time error.

## Handlers and Axum Extractors
In your route's `<script setup>`, any function annotated with an HTTP method macro (`#[thebe::get]`, `#[thebe::post]`, etc.) is automatically registered to that file's route.

Because Thebe generates standard Axum routes, you have full access to Axum extractors:

```rust
<script setup>
use axum::extract::{Path, State, Query};
use crate::AppState;

#[thebe::get]
pub async fn load_post(
    State(state): State<AppState>,
    Path(slug): Path<String>
) -> Props {
    let post = state.db.fetch(&slug).await;
    Props { post }
}
</script>
```

## Return Types
A Thebe handler is not strictly forced to return `Props`. It compiles into an Axum handler, meaning it can return anything that implements Axum's `IntoResponse`.

- **Returning `Props`**: Renders the `.trs` template and injects the JSON payload.
- **Returning a Redirect**: `axum::response::Redirect::to("/login")` safely halts rendering and issues an HTTP 302.
- **Returning Errors**: You can return `Result<Props, StatusCode>` naturally.
