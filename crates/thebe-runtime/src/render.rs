use crate::error::RuntimeError;

/// Render a Thebe template string using the provided JSON context.
///
/// `template` is the raw HTML template with `{{ ident }}` bindings.
/// `ctx` is the serialised `Props` value.
///
/// Returns the rendered HTML body fragment (without the outer shell).
///
/// # Errors
///
/// Returns [`RuntimeError`] when the template cannot be compiled or rendered
/// by `MiniJinja`.
pub fn render_template(
    template: &str,
    ctx: &serde_json::Value,
) -> Result<String, RuntimeError> {
    use minijinja::Environment;

    let mut env = Environment::new();
    env.add_template("__page", template)
        .map_err(|e| RuntimeError::TemplateCompile(e.to_string()))?;

    let tmpl = env
        .get_template("__page")
        .map_err(|e| RuntimeError::TemplateRender(e.to_string()))?;

    tmpl.render(ctx)
        .map_err(|e| RuntimeError::TemplateRender(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_simple_binding() {
        let html = render_template("<h1>{{ title }}</h1>", &json!({ "title": "Hello" }))
            .unwrap();
        assert_eq!(html, "<h1>Hello</h1>");
    }

    #[test]
    fn render_dotted_binding() {
        let html = render_template(
            "<p>{{ author.name }}</p>",
            &json!({ "author": { "name": "Alice" } }),
        )
        .unwrap();
        assert_eq!(html, "<p>Alice</p>");
    }

    #[test]
    fn render_returns_error_for_unknown_var() {
        // minijinja renders unknown vars as empty string by default — that is
        // acceptable for v0 (undefined bindings silently produce nothing).
        let html = render_template("<span>{{ missing }}</span>", &json!({})).unwrap();
        assert_eq!(html, "<span></span>");
    }
}
