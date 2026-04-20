use crate::error::CodegenError;
use std::fmt::Write as _;
use thebe_parser::SourceSpan;

/// A segment of a parsed Thebe template.
///
/// Used during validation; the field contents are intentionally not read by
/// the codegen (which passes the raw template string to minijinja at runtime).
#[derive(Debug)]
pub enum TemplatePart {
  /// A run of static HTML text.
  Literal(#[allow(dead_code)] String),
  /// A `{{ ident }}` or `{{ ident.field }}` binding.
  Binding(#[allow(dead_code)] String),
}

/// A validated binding occurrence within a template segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateBindingOccurrence {
  /// Binding path, e.g. `title` or `post.author.name`.
  pub name: String,
  /// Byte range of the full `{{ ... }}` token within the template segment.
  pub span: SourceSpan,
}

/// Parse a Thebe template string into a flat list of [`TemplatePart`]s.
///
/// Only simple identifier and dotted-field bindings are supported (v0 grammar).
/// Anything else (arithmetic, function calls, ternaries) is rejected.
pub fn parse_template(template: &str) -> Result<Vec<TemplatePart>, CodegenError> {
  let mut parts: Vec<TemplatePart> = Vec::new();
  let mut literal = String::new();
  let mut chars = template.chars().peekable();

  while let Some(ch) = chars.next() {
    if ch == '{' && chars.peek().copied() == Some('{') {
      chars.next(); // consume second '{'

      // Collect the binding content until `}}`.
      let mut binding = String::new();
      loop {
        match chars.next() {
          None => return Err(CodegenError::UnclosedBinding),
          Some('}') if chars.peek().copied() == Some('}') => {
            chars.next(); // consume second '}'
            break;
          }
          Some(c) => binding.push(c),
        }
      }

      let ident = binding.trim().to_owned();
      validate_binding(&ident)?;

      if !literal.is_empty() {
        parts.push(TemplatePart::Literal(std::mem::take(&mut literal)));
      }
      parts.push(TemplatePart::Binding(ident));
    } else {
      literal.push(ch);
    }
  }

  if !literal.is_empty() {
    parts.push(TemplatePart::Literal(literal));
  }

  Ok(parts)
}

/// Return the distinct template bindings in order of first appearance.
///
/// # Errors
///
/// Returns the same validation errors as [`parse_template`].
pub fn list_template_bindings(template: &str) -> Result<Vec<String>, CodegenError> {
  let mut bindings = Vec::new();

  for binding in list_template_binding_occurrences(template)? {
    if !bindings.contains(&binding.name) {
      bindings.push(binding.name);
    }
  }

  Ok(bindings)
}

/// Return all validated binding occurrences in source order.
///
/// # Errors
///
/// Returns the same validation errors as [`parse_template`].
pub fn list_template_binding_occurrences(
  template: &str,
) -> Result<Vec<TemplateBindingOccurrence>, CodegenError> {
  let mut bindings = Vec::new();
  let bytes = template.as_bytes();
  let mut idx = 0usize;

  while idx < bytes.len() {
    if bytes[idx] == b'{' && bytes.get(idx + 1).is_some_and(|byte| *byte == b'{') {
      let start = idx;
      idx += 2;
      let content_start = idx;

      loop {
        match (bytes.get(idx), bytes.get(idx + 1)) {
          (None, _) => return Err(CodegenError::UnclosedBinding),
          (Some(b'}'), Some(b'}')) => {
            let binding = template[content_start..idx].trim().to_owned();
            validate_binding(&binding)?;
            bindings.push(TemplateBindingOccurrence {
              name: binding,
              span: SourceSpan {
                start,
                end: idx + 2,
              },
            });
            idx += 2;
            break;
          }
          _ => idx += 1,
        }
      }
    } else {
      idx += 1;
    }
  }

  Ok(bindings)
}

/// Validate that a binding is a simple identifier or dotted field path.
///
/// Valid: `title`, `post`, `post.author`, `post.author.name`
/// Invalid: `a + b`, `fn()`, `0bad`, `.leading_dot`
fn validate_binding(ident: &str) -> Result<(), CodegenError> {
  if ident.is_empty() {
    return Err(CodegenError::InvalidBinding(ident.to_owned()));
  }
  for part in ident.split('.') {
    if part.is_empty() {
      return Err(CodegenError::InvalidBinding(ident.to_owned()));
    }
    let mut chars = part.chars();
    let first = chars.next().expect("part is non-empty");
    if !first.is_alphabetic() && first != '_' {
      return Err(CodegenError::InvalidBinding(ident.to_owned()));
    }
    for c in chars {
      if !c.is_alphanumeric() && c != '_' {
        return Err(CodegenError::InvalidBinding(ident.to_owned()));
      }
    }
  }
  Ok(())
}

/// HTML tags that cause the browser parser to hoist or reject loose comment
/// nodes placed inside them. Reactive bindings in these contexts fall back to
/// `<span data-thebe-bind="…">` anchors instead of comment markers.
const UNSAFE_CTX_TAGS: &[&str] = &[
  "table", "thead", "tbody", "tfoot", "tr", "td", "th", "caption", "col", "colgroup", "select",
  "option", "optgroup",
];

/// HTML void elements — never have children, so never pushed onto the tag
/// context stack.
fn is_void_element(name: &str) -> bool {
  matches!(
    name,
    "area"
      | "base"
      | "br"
      | "col"
      | "embed"
      | "hr"
      | "img"
      | "input"
      | "link"
      | "meta"
      | "param"
      | "source"
      | "track"
      | "wbr"
  )
}

/// Transform a Thebe template so that each `{{ ident }}` binding is wrapped in
/// an SSR hydration anchor appropriate for its DOM context.
///
/// **Safe contexts** (phrasing content, divs, spans, …):
/// ```text
/// {{ counter }}  →  <!--thebe:counter-->{{ counter }}<!--/thebe:counter-->
/// ```
///
/// **Unsafe contexts** (table cells, `<select>`, …) where browsers hoist
/// comment nodes out of the structure:
/// ```text
/// {{ counter }}  →  <span data-thebe-bind="counter">{{ counter }}</span>
/// ```
///
/// Dotted paths (`{{ user.name }}`) use the full path as the anchor key.
///
/// Call [`parse_template`] first to validate the template; this function does
/// not re-validate.
pub fn inject_hydration_markers(template: &str) -> String {
  let mut out = String::with_capacity(template.len() + 128);
  let mut tag_stack: Vec<String> = Vec::new();
  let mut chars = template.chars().peekable();

  while let Some(ch) = chars.next() {
    if ch == '<' {
      // Accumulate the full tag token up to and including `>`.
      let mut tag_buf = String::from('<');
      for c in chars.by_ref() {
        tag_buf.push(c);
        if c == '>' {
          break;
        }
      }

      // Extract tag name and decide whether to push / pop the stack.
      let inner = tag_buf.trim_start_matches('<').trim_end_matches('>').trim();
      let is_closing = inner.starts_with('/');
      let name_part = if is_closing { &inner[1..] } else { inner };
      let name: String = name_part
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or("")
        .to_lowercase();

      if !name.is_empty() {
        if is_closing {
          if let Some(pos) = tag_stack.iter().rposition(|t| t == &name) {
            tag_stack.truncate(pos);
          }
        } else if !is_void_element(&name) && !inner.ends_with('/') {
          tag_stack.push(name);
        }
      }

      out.push_str(&tag_buf);
    } else if ch == '{' && chars.peek().copied() == Some('{') {
      chars.next(); // consume second `{`

      let mut binding = String::new();
      let mut closed = false;
      loop {
        match chars.next() {
          None => break,
          Some('}') if chars.peek().copied() == Some('}') => {
            chars.next(); // consume second `}`
            closed = true;
            break;
          }
          Some(c) => binding.push(c),
        }
      }

      if !closed {
        // Malformed — pass through; validation already caught this.
        out.push_str("{{");
        out.push_str(&binding);
        continue;
      }

      let ident = binding.trim();
      let in_unsafe_ctx = tag_stack
        .iter()
        .any(|t| UNSAFE_CTX_TAGS.contains(&t.as_str()));

      if in_unsafe_ctx {
        write!(
          out,
          r#"<span data-thebe-bind="{ident}">{{{{ {ident} }}}}</span>"#
        )
        .expect("infallible");
      } else {
        write!(
          out,
          "<!--thebe:{ident}-->{{{{ {ident} }}}}<!--/thebe:{ident}-->"
        )
        .expect("infallible");
      }
    } else {
      out.push(ch);
    }
  }

  out
}

/// Escape a string for use inside a Rust double-quoted string literal.
#[allow(dead_code)] // retained for potential future use
pub fn escape_rust_str(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for c in s.chars() {
    match c {
      '\\' => out.push_str("\\\\"),
      '"' => out.push_str("\\\""),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      c => out.push(c),
    }
  }
  out
}

// ── Component expansion ──────────────────────────────────────────────────────

/// Replace `<slot>` / `<slot/>` / `<slot />` in a component template body with
/// the Minijinja `{{ caller() }}` expression.
///
/// Used when building a component's `{% macro %}` body so that child content
/// passed via `{% call %}` blocks is rendered in the right position.
pub fn expand_slot(template: &str) -> String {
  use thebe_parser::{TemplateToken, tokenize_template};

  let tokens = tokenize_template(template);
  let mut out = String::with_capacity(template.len());
  for token in tokens {
    match token {
      TemplateToken::Text(s) => out.push_str(s),
      TemplateToken::Slot => out.push_str("{{ caller() }}"),
      // Nested components inside a component — pass through as raw HTML for now.
      TemplateToken::ComponentOpen {
        name,
        attrs,
        self_closing,
      } => {
        out.push('<');
        out.push_str(name);
        for attr in &attrs {
          out.push(' ');
          out.push_str(attr.name);
          if let Some(val) = attr.value {
            write!(out, "=\"{val}\"").expect("infallible");
          }
        }
        if self_closing {
          out.push_str(" />");
        } else {
          out.push('>');
        }
      }
      TemplateToken::ComponentClose { name } => {
        write!(out, "</{name}>").expect("infallible");
      }
    }
  }
  out
}

/// Build a Minijinja dict-literal string from component tag attributes.
///
/// * `:name="expr"` → dynamic: `"name": expr` (value emitted as Jinja expression)
/// * `name="literal"` → static: `"name": "literal"`
/// * `name` (boolean, no value) → `"name": true`
fn build_jinja_props(attrs: &[thebe_parser::TemplateAttr<'_>]) -> String {
  let mut out = String::from("{");
  for (i, attr) in attrs.iter().enumerate() {
    if i > 0 {
      out.push_str(", ");
    }
    let key = attr.name.trim_start_matches(':');
    write!(out, "\"{key}\": ").expect("infallible");
    match (attr.name.starts_with(':'), attr.value) {
      (true, Some(expr)) => out.push_str(expr),
      (false, Some(literal)) => write!(out, "\"{literal}\"").expect("infallible"),
      (_, None) => out.push_str("true"),
    }
  }
  out.push('}');
  out
}

/// Expand PascalCase component tags in a route template into Minijinja
/// `{% call %}` / `{% endcall %}` blocks.
///
/// `known_names` is the set of component PascalCase names that are registered
/// (e.g. `["Card", "Button"]`).  Only matching tags are transformed; unrecognised
/// uppercase tags are passed through verbatim.
///
/// The returned string is the expanded template body **without** the macro
/// definitions prepended — the caller is responsible for prepending them.
pub fn expand_component_tags(template: &str, known_names: &[&str]) -> String {
  use thebe_parser::{TemplateToken, tokenize_template};

  if known_names.is_empty() {
    return template.to_owned();
  }

  let tokens = tokenize_template(template);
  let mut out = String::with_capacity(template.len() + 256);

  for token in tokens {
    match token {
      TemplateToken::Text(s) => out.push_str(s),
      TemplateToken::Slot => out.push_str("<slot />"),
      TemplateToken::ComponentOpen {
        name,
        attrs,
        self_closing,
      } => {
        if known_names.contains(&name) {
          let macro_name = format!("__comp_{}", name.to_lowercase());
          let props_dict = build_jinja_props(&attrs);
          if self_closing {
            write!(out, "{{% call {macro_name}({props_dict}) %}}{{% endcall %}}",)
              .expect("infallible");
          } else {
            write!(out, "{{% call {macro_name}({props_dict}) %}}").expect("infallible");
          }
        } else {
          // Unknown / unregistered PascalCase tag — pass through as raw HTML.
          out.push('<');
          out.push_str(name);
          for attr in &attrs {
            out.push(' ');
            out.push_str(attr.name);
            if let Some(val) = attr.value {
              write!(out, "=\"{val}\"").expect("infallible");
            }
          }
          if self_closing {
            out.push_str(" />");
          } else {
            out.push('>');
          }
        }
      }
      TemplateToken::ComponentClose { name } => {
        if known_names.contains(&name) {
          out.push_str("{% endcall %}");
        } else {
          write!(out, "</{name}>").expect("infallible");
        }
      }
    }
  }

  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_single_binding() {
    let parts = parse_template("Hello {{ name }}!").unwrap();
    assert!(matches!(parts[0], TemplatePart::Literal(ref s) if s == "Hello "));
    assert!(matches!(parts[1], TemplatePart::Binding(ref s) if s == "name"));
    assert!(matches!(parts[2], TemplatePart::Literal(ref s) if s == "!"));
  }

  #[test]
  fn parse_dotted_binding() {
    let parts = parse_template("{{ post.author.name }}").unwrap();
    assert!(matches!(parts[0], TemplatePart::Binding(ref s) if s == "post.author.name"));
  }

  #[test]
  fn parse_unclosed_binding_returns_error() {
    assert!(parse_template("{{ oops").is_err());
  }

  #[test]
  fn validate_rejects_expressions() {
    assert!(parse_template("{{ a + b }}").is_err());
    assert!(parse_template("{{ fn() }}").is_err());
    assert!(parse_template("{{ }}").is_err());
  }

  #[test]
  fn list_template_bindings_deduplicates_in_appearance_order() {
    let bindings =
      list_template_bindings("<h1>{{ title }}</h1><p>{{ user.name }}</p><span>{{ title }}</span>")
        .unwrap();

    assert_eq!(bindings, vec!["title", "user.name"]);
  }

  #[test]
  fn list_template_binding_occurrences_preserves_spans() {
    let template = "<h1>{{ title }}</h1><p>{{ user.name }}</p>";
    let occurrences = list_template_binding_occurrences(template).unwrap();

    assert_eq!(occurrences[0].name, "title");
    assert_eq!(
      &template[occurrences[0].span.start..occurrences[0].span.end],
      "{{ title }}"
    );
    assert_eq!(occurrences[1].name, "user.name");
    assert_eq!(
      &template[occurrences[1].span.start..occurrences[1].span.end],
      "{{ user.name }}"
    );
  }

  // ── inject_hydration_markers ────────────────────────────────────────────

  #[test]
  fn hydration_safe_context_emits_comment_markers() {
    let out = inject_hydration_markers("<span>{{ counter }}</span>");
    assert_eq!(
      out,
      "<span><!--thebe:counter-->{{ counter }}<!--/thebe:counter--></span>"
    );
  }

  #[test]
  fn hydration_dotted_path_uses_full_key() {
    let out = inject_hydration_markers("<p>{{ user.name }}</p>");
    assert_eq!(
      out,
      "<p><!--thebe:user.name-->{{ user.name }}<!--/thebe:user.name--></p>"
    );
  }

  #[test]
  fn hydration_inside_table_cell_emits_span() {
    let out = inject_hydration_markers("<table><tr><td>{{ count }}</td></tr></table>");
    assert!(
      out.contains(r#"<span data-thebe-bind="count">{{ count }}</span>"#),
      "expected data-thebe-bind span, got: {out}"
    );
  }

  #[test]
  fn hydration_after_table_reverts_to_comment_markers() {
    let out = inject_hydration_markers("<table><tr><td>{{ a }}</td></tr></table><p>{{ b }}</p>");
    assert!(out.contains(r#"data-thebe-bind="a""#), "a should use span");
    assert!(
      out.contains("<!--thebe:b-->{{ b }}<!--/thebe:b-->"),
      "b should use comment markers"
    );
  }

  #[test]
  fn hydration_inside_select_emits_span() {
    let out = inject_hydration_markers("<select><option>{{ label }}</option></select>");
    assert!(
      out.contains(r#"data-thebe-bind="label""#),
      "expected data-thebe-bind inside select"
    );
  }

  #[test]
  fn hydration_no_bindings_is_passthrough() {
    let tmpl = "<h1>Hello world</h1>";
    assert_eq!(inject_hydration_markers(tmpl), tmpl);
  }
}
