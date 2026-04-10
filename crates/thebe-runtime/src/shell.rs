/// Wrap a rendered body fragment in a minimal HTML5 shell.
///
/// `body` — the inner HTML produced by [`render_template`].
/// `props_json` — the serialised `Props` JSON string injected as
///   `<script id="__thebe_props" type="application/json">` for the client
///   runtime to consume during hydration.
pub fn html_shell(body: &str, props_json: &str) -> String {
    format!(
        "<!DOCTYPE html>\n\
         <html>\n\
         <body>\n\
         {body}\n\
         <script id=\"__thebe_props\" type=\"application/json\">{props_json}</script>\n\
         </body>\n\
         </html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_shell_embeds_props_json() {
        let out = html_shell("<h1>Hi</h1>", r#"{"title":"Hi"}"#);
        assert!(out.contains(r#"id="__thebe_props""#));
        assert!(out.contains(r#"{"title":"Hi"}"#));
        assert!(out.contains("<!DOCTYPE html>"));
    }
}
