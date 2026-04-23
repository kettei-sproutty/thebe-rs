# Forms and Mutations

Thebe embraces the web platform. Because client state is strictly local, **HTML forms are the canonical way to mutate server state**.

This aligns inherently with Progressive Enhancement, ensuring that your app works reliably even before JavaScript has loaded or hydration has completed.

## The POST Flow
In v0, the idiomatic mutation path is a standard HTML `<form>` posting to a `#[thebe::post]` handler residing in the same route file.

```html
<script setup>
use axum::{Form, response::Redirect};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreatePost {
    title: String,
}

#[thebe::get]
pub async fn view() -> Props {
    Props { error: None }
}

#[thebe::post]
pub async fn create(Form(data): Form<CreatePost>) -> Result<Redirect, Props> {
    if data.title.is_empty() {
        // Re-render the same page with an error
        return Err(Props { error: Some("Title required".into()) });
    }

    // Save to DB...

    // Redirect to the new resource
    Ok(Redirect::to("/success"))
}
</script>

<!-- Notice: no preventDefault, no custom JS client fetch -->
<form method="post">
  <input type="text" name="title" />
  <button type="submit">Create</button>
    <p class="err">{{ error }}</p>
</form>
```

## Progressive Enhancement by Default
1. **Server-First:** When the form is submitted, the browser executes a standard encoded POST request.
2. **Server Response:** The `#[thebe::post]` handler executes. It can either return a fresh `Props` struct (which fully server-renders the `.trs` file anew, useful for validation errors) or return an axum `Redirect`.
3. **Future Extensibility:** While v0 relies on full-page POSTs, this architectural baseline means future Thebe versions can easily introduce client-side form interception (`<form :enhance>`) that performs behind-the-scenes fetch requests and surgical DOM swaps—without requiring you to rewrite your handlers.
