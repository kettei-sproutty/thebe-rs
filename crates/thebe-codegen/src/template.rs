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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentInstanceAttr {
  pub name: String,
  pub value: Option<String>,
  pub dynamic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentInstance {
  pub component_name: String,
  pub macro_name: String,
  pub instance_id: String,
  pub attrs: Vec<ComponentInstanceAttr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentExpansion {
  pub template: String,
  pub instances: Vec<ComponentInstance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownComponent<'a> {
  pub name: &'a str,
  pub named_slots: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotExpansion {
  pub template: String,
  pub named_slots: Vec<String>,
}

// ── Dynamic attribute bindings (:attr="key") ────────────────────────────────

/// Transform a Thebe template so that each `:attr="key"` binding on a plain
/// HTML element tag is rewritten to a concrete attribute plus a client-side
/// hydration marker.
///
/// ```text
/// <div :class="theme">
///   → <div class="{{ theme }}" data-thebe-attr="class:theme">
/// ```
///
/// Multiple dynamic attributes on the same element are coalesced into a single
/// `data-thebe-attr` value:
///
/// ```text
/// <div :class="theme" :id="itemId">
///   → <div class="{{ theme }}" id="{{ itemId }}" data-thebe-attr="class:theme,id:itemId">
/// ```
///
/// Static attrs that share a name with a dynamic attr (e.g. `class` alongside
/// `:class`) are dropped — the dynamic binding takes precedence.
///
/// PascalCase (component) tags are skipped — they are handled by
/// `expand_component_tags`.
///
/// Call this **before** `inject_hydration_markers`. The injected `{{ expr }}`
/// inside attribute position is consumed as part of the tag buffer by the
/// later pass and will not be wrapped in text-content comment markers.
pub fn inject_attr_bindings(template: &str) -> String {
  let mut out = String::with_capacity(template.len() + 64);
  let mut chars = template.chars().peekable();

  while let Some(ch) = chars.next() {
    if ch != '<' {
      out.push(ch);
      continue;
    }

    // Accumulate the full tag, quote-aware, up to and including `>`.
    let mut tag_buf = String::from('<');
    let mut in_quote: Option<char> = None;
    loop {
      let Some(c) = chars.next() else { break };
      tag_buf.push(c);
      match in_quote {
        Some(q) if c == q => in_quote = None,
        None if c == '"' || c == '\'' => in_quote = Some(c),
        None if c == '>' => break,
        _ => {}
      }
    }

    out.push_str(&transform_tag_attr_bindings(&tag_buf));
  }

  out
}

/// Rewrite `:attr="key"` dynamic bindings in a single **complete** tag string.
///
/// Returns the tag unchanged when:
/// * it is a closing, comment, or doctype token
/// * the tag name starts with an uppercase letter (component tag)
/// * the tag contains no `:name="…"` attributes
fn transform_tag_attr_bindings(tag: &str) -> String {
  let inner = tag.trim_start_matches('<');

  // Closing tags, comments (`<!--`), and doctypes (`<!`): pass through.
  let first_meaningful = inner.trim_start();
  if first_meaningful.starts_with('/') || first_meaningful.starts_with('!') {
    return tag.to_owned();
  }

  // Extract tag name.
  let name_end = first_meaningful
    .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
    .unwrap_or(first_meaningful.len());
  let tag_name = &first_meaningful[..name_end];

  // Skip PascalCase (component) and void/empty.
  if tag_name.is_empty() || tag_name.chars().next().map_or(false, char::is_uppercase) {
    return tag.to_owned();
  }

  // Fast path: no `:` in the rest of the tag → nothing to transform.
  let after_name = &first_meaningful[name_end..];
  if !after_name.contains(':') {
    return tag.to_owned();
  }

  // Determine self-closing before stripping the trailing `>`.
  let self_closing = tag.ends_with("/>") || tag.trim_end().ends_with("/>");

  // Strip the trailing `>` (and `/` for self-closing) so the attr parser
  // receives only the attribute list.
  let attrs_raw = if self_closing {
    after_name
      .strip_suffix('>')
      .and_then(|s| s.strip_suffix('/'))
      .unwrap_or(after_name)
  } else {
    after_name.strip_suffix('>').unwrap_or(after_name)
  };

  let attrs = parse_tag_attrs(attrs_raw);

  // Collect the bare names that have a corresponding dynamic binding.
  let dynamic_bare_names: Vec<&str> = attrs
    .iter()
    .filter(|(name, _)| name.starts_with(':'))
    .map(|(name, _)| name[1..].as_ref())
    .collect();

  if dynamic_bare_names.is_empty() {
    return tag.to_owned();
  }

  // Rebuild the tag.
  let mut rebuilt = format!("<{tag_name}");
  let mut data_attr_spec = String::new();

  for (name, value) in &attrs {
    if let Some(bare_name) = name.strip_prefix(':') {
      // Dynamic binding: `:foo="bar"` → `foo="{{ bar }}"`.
      let key = value.as_deref().unwrap_or("").trim();
      if is_valid_binding_key(key) {
        if !data_attr_spec.is_empty() {
          data_attr_spec.push(',');
        }
        write!(data_attr_spec, "{bare_name}:{key}").expect("infallible");
        write!(rebuilt, r#" {bare_name}="{{{{ {key} }}}}""#).expect("infallible");
      } else {
        // Invalid key — pass through verbatim to avoid silently dropping attrs.
        match value {
          Some(v) => write!(rebuilt, r#" {name}="{v}""#).expect("infallible"),
          None => write!(rebuilt, " {name}").expect("infallible"),
        }
      }
    } else if dynamic_bare_names.contains(&name.as_str()) {
      // Static attr shadowed by a dynamic binding with the same name: drop it.
    } else {
      // Static attr: emit unchanged.
      match value {
        Some(v) => write!(rebuilt, r#" {name}="{v}""#).expect("infallible"),
        None => write!(rebuilt, " {name}").expect("infallible"),
      }
    }
  }

  if !data_attr_spec.is_empty() {
    write!(rebuilt, r#" data-thebe-attr="{data_attr_spec}""#).expect("infallible");
  }

  if self_closing {
    rebuilt.push_str(" />");
  } else {
    rebuilt.push('>');
  }

  rebuilt
}

/// Parse the attribute list portion of a tag (the text between the tag name
/// and the closing `>` / `/>`).
///
/// Returns a `Vec` of `(name, Option<value>)` pairs in source order.  Attribute
/// names may begin with `:` for dynamic bindings.  Quoted values are unquoted.
fn parse_tag_attrs(s: &str) -> Vec<(String, Option<String>)> {
  let mut attrs = Vec::new();
  let mut chars = s.chars().peekable();

  loop {
    // Skip whitespace.
    while chars.peek().map_or(false, |c| c.is_ascii_whitespace()) {
      chars.next();
    }

    // Stop at end, `>`, or the `/` of `/>`.
    match chars.peek() {
      None | Some('>') | Some('/') => break,
      _ => {}
    }

    // Read attr name; may start with `:`.
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
      if c.is_ascii_whitespace() || c == '=' || c == '>' || c == '/' {
        break;
      }
      name.push(c);
      chars.next();
    }

    if name.is_empty() {
      break;
    }

    // Skip whitespace.
    while chars.peek().map_or(false, |c| c.is_ascii_whitespace()) {
      chars.next();
    }

    if chars.peek() != Some(&'=') {
      // Boolean attribute — no value.
      attrs.push((name, None));
      continue;
    }

    chars.next(); // consume `=`

    // Skip whitespace.
    while chars.peek().map_or(false, |c| c.is_ascii_whitespace()) {
      chars.next();
    }

    // Read value — quoted or bare.
    let value = match chars.peek().copied() {
      Some(q @ '"') | Some(q @ '\'') => {
        chars.next(); // consume open quote
        let mut v = String::new();
        for c in chars.by_ref() {
          if c == q {
            break;
          }
          v.push(c);
        }
        v
      }
      _ => {
        let mut v = String::new();
        while let Some(&c) = chars.peek() {
          if c.is_ascii_whitespace() || c == '>' {
            break;
          }
          v.push(c);
          chars.next();
        }
        v
      }
    };

    attrs.push((name, Some(value)));
  }

  attrs
}

/// Return `true` if `key` is a valid binding key (identifier or dotted path).
fn is_valid_binding_key(key: &str) -> bool {
  validate_binding(key).is_ok()
}

pub fn prefix_event_handler_attrs(template: &str, event_fns: &[String], prefix: &str) -> String {
  if event_fns.is_empty() {
    return template.to_owned();
  }

  let mut out = String::with_capacity(template.len() + 64);
  let mut chars = template.chars().peekable();

  while let Some(ch) = chars.next() {
    if ch != '<' {
      out.push(ch);
      continue;
    }

    let mut tag_buf = String::from('<');
    let mut in_quote: Option<char> = None;
    loop {
      let Some(c) = chars.next() else { break };
      tag_buf.push(c);
      match in_quote {
        Some(q) if c == q => in_quote = None,
        None if c == '"' || c == '\'' => in_quote = Some(c),
        None if c == '>' => break,
        _ => {}
      }
    }

    out.push_str(&transform_tag_event_handlers(&tag_buf, event_fns, prefix));
  }

  out
}

fn transform_tag_event_handlers(tag: &str, event_fns: &[String], prefix: &str) -> String {
  let inner = tag.trim_start_matches('<');
  let first_meaningful = inner.trim_start();
  if first_meaningful.starts_with('/') || first_meaningful.starts_with('!') {
    return tag.to_owned();
  }

  let name_end = first_meaningful
    .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
    .unwrap_or(first_meaningful.len());
  let tag_name = &first_meaningful[..name_end];
  if tag_name.is_empty() || tag_name.chars().next().is_some_and(char::is_uppercase) {
    return tag.to_owned();
  }

  let self_closing = tag.ends_with("/>") || tag.trim_end().ends_with("/>");
  let after_name = &first_meaningful[name_end..];
  let attrs_raw = if self_closing {
    after_name
      .strip_suffix('>')
      .and_then(|s| s.strip_suffix('/'))
      .unwrap_or(after_name)
  } else {
    after_name.strip_suffix('>').unwrap_or(after_name)
  };
  let attrs = parse_tag_attrs(attrs_raw);

  let mut rebuilt = format!("<{tag_name}");
  for (name, value) in attrs {
    let rewritten_value = if name.starts_with("on") {
      value
        .as_deref()
        .map(|expr| prefix_event_handler_expr(expr, event_fns, prefix))
    } else {
      value
    };

    match rewritten_value {
      Some(v) => write!(rebuilt, r#" {name}="{v}""#).expect("infallible"),
      None => write!(rebuilt, " {name}").expect("infallible"),
    }
  }

  if self_closing {
    rebuilt.push_str(" />");
  } else {
    rebuilt.push('>');
  }

  rebuilt
}

fn prefix_event_handler_expr(expr: &str, event_fns: &[String], prefix: &str) -> String {
  let trimmed = expr.trim();

  for event_fn in event_fns {
    if trimmed == event_fn {
      return format!("{prefix}{event_fn}");
    }

    if let Some(rest) = trimmed.strip_prefix(event_fn)
      && rest.starts_with('(')
    {
      return format!("{prefix}{event_fn}{rest}");
    }
  }

  expr.to_owned()
}

// ── Component expansion ──────────────────────────────────────────────────────

/// Replace `<slot>` / `<slot/>` / `<slot />` in a component template body with
/// the Minijinja `caller()` expression, defaulting to empty output when the
/// component was invoked without a `{% call %}` block.
///
/// Used when building a component's `{% macro %}` body so that child content
/// passed via `{% call %}` blocks is rendered in the right position.
#[cfg_attr(not(test), expect(dead_code, reason = "compat wrapper for callers that only need expanded slot text"))]
pub fn expand_slot(template: &str) -> String {
  expand_slot_with_metadata(template).template
}

pub fn expand_slot_with_metadata(template: &str) -> SlotExpansion {
  use thebe_parser::{TemplateToken, tokenize_template};

  let tokens = tokenize_template(template);
  let mut out = String::with_capacity(template.len());
  let mut named_slots = Vec::new();
  for token in tokens {
    match token {
      TemplateToken::Text(s) => out.push_str(s),
      TemplateToken::Slot { name } => {
        if let Some(name) = name {
          if !named_slots.iter().any(|existing| existing == name) {
            named_slots.push(name.to_owned());
          }
          let binding = slot_binding_name(name);
          write!(
            out,
            "{{{{ {binding}() if {binding} is not none else '' }}}}"
          )
          .expect("infallible");
        } else {
          out.push_str("{{ caller() if caller is defined else '' }}");
        }
      }
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

  SlotExpansion {
    template: out,
    named_slots,
  }
}

pub fn slot_binding_name(slot_name: &str) -> String {
  let mut binding = String::from("__slot_");
  for ch in slot_name.chars() {
    if ch.is_ascii_alphanumeric() || ch == '_' {
      binding.push(ch);
    } else {
      binding.push('_');
    }
  }
  binding
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

#[must_use]
pub fn list_used_component_names(template: &str, known_names: &[&str]) -> Vec<String> {
  use thebe_parser::{TemplateToken, tokenize_template};

  if known_names.is_empty() {
    return Vec::new();
  }

  let mut used = Vec::new();

  for token in tokenize_template(template) {
    if let TemplateToken::ComponentOpen { name, .. } = token
      && known_names.contains(&name)
      && !used.iter().any(|used_name| used_name == name)
    {
      used.push(name.to_owned());
    }
  }

  used
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
#[expect(dead_code, reason = "compat wrapper for callers that only need expanded template text")]
pub fn expand_component_tags(template: &str, known_names: &[&str]) -> String {
  let known_components = known_names
    .iter()
    .copied()
    .map(|name| KnownComponent {
      name,
      named_slots: &[],
    })
    .collect::<Vec<_>>();
  expand_component_tags_with_instances(template, &known_components).template
}

pub fn expand_component_tags_with_instances(
  template: &str,
  known_components: &[KnownComponent<'_>],
) -> ComponentExpansion {
  if known_components.is_empty() {
    return ComponentExpansion {
      template: template.to_owned(),
      instances: Vec::new(),
    };
  }

  let mut instances = Vec::new();
  let mut instance_idx = 0usize;
  let template = expand_component_tags_with_instances_impl(
    template,
    known_components,
    &mut instances,
    &mut instance_idx,
  );

  ComponentExpansion { template, instances }
}

fn expand_component_tags_with_instances_impl(
  template: &str,
  known_components: &[KnownComponent<'_>],
  instances: &mut Vec<ComponentInstance>,
  instance_idx: &mut usize,
) -> String {
  use thebe_parser::{TemplateToken, tokenize_template};

  let tokens = tokenize_template(template);
  let mut out = String::with_capacity(template.len() + 256);
  let mut token_idx = 0usize;

  while token_idx < tokens.len() {
    match &tokens[token_idx] {
      TemplateToken::Text(text) => out.push_str(text),
      TemplateToken::Slot { name } => {
        if let Some(name) = name {
          write!(out, r#"<slot name="{name}" />"#).expect("infallible");
        } else {
          out.push_str("<slot />");
        }
      }
      TemplateToken::ComponentOpen {
        name,
        attrs,
        self_closing,
      } => {
        if let Some(component) = known_components
          .iter()
          .copied()
          .find(|component| component.name == *name)
        {
          let current_instance_idx = *instance_idx;
          let instance_id = format!("c{current_instance_idx}");
          let macro_name = format!("__comp_{}_{}", name.to_lowercase(), current_instance_idx);
          let props_dict = build_jinja_props(attrs);
          instances.push(ComponentInstance {
            component_name: (*name).to_owned(),
            macro_name: macro_name.clone(),
            instance_id,
            attrs: attrs
              .iter()
              .map(|attr| ComponentInstanceAttr {
                name: attr.name.trim_start_matches(':').to_owned(),
                value: attr.value.map(str::to_owned),
                dynamic: attr.name.starts_with(':'),
              })
              .collect(),
          });
          *instance_idx += 1;

          if *self_closing {
            write!(out, "{{{{ {macro_name}({props_dict}) }}}}").expect("infallible");
          } else {
            let Some(close_idx) = find_matching_component_close(&tokens, token_idx, name) else {
              out.push_str(&render_component_open_tag(name, attrs, *self_closing));
              token_idx += 1;
              continue;
            };

            let inner = tokens[token_idx + 1..close_idx]
              .iter()
              .map(token_to_source)
              .collect::<String>();
            let extracted_slots = extract_component_slots(&inner, component.named_slots);
            let default_content = expand_component_tags_with_instances_impl(
              &extracted_slots.default_content,
              known_components,
              instances,
              instance_idx,
            );
            let mut named_slot_args = String::new();

            for named_slot in extracted_slots.named_slots {
              let binding = slot_binding_name(&named_slot.name);
              let slot_macro_name = format!("{macro_name}{binding}");
              let slot_content = expand_component_tags_with_instances_impl(
                &named_slot.content,
                known_components,
                instances,
                instance_idx,
              );
              write!(
                out,
                "{{% macro {slot_macro_name}() %}}{slot_content}{{% endmacro %}}"
              )
              .expect("infallible");
              write!(named_slot_args, ", {binding}={slot_macro_name}").expect("infallible");
            }

            if default_content.trim().is_empty() {
              write!(out, "{{{{ {macro_name}({props_dict}{named_slot_args}) }}}}")
                .expect("infallible");
            } else {
              write!(
                out,
                "{{% call {macro_name}({props_dict}{named_slot_args}) %}}{default_content}{{% endcall %}}"
              )
              .expect("infallible");
            }
            token_idx = close_idx;
          }
        } else {
          out.push_str(&render_component_open_tag(name, attrs, *self_closing));
        }
      }
      TemplateToken::ComponentClose { name } => {
        write!(out, "</{name}>").expect("infallible");
      }
    }

    token_idx += 1;
  }

  out
}

fn render_component_open_tag(
  name: &str,
  attrs: &[thebe_parser::TemplateAttr<'_>],
  self_closing: bool,
) -> String {
  let mut out = String::new();
  out.push('<');
  out.push_str(name);
  for attr in attrs {
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
  out
}

fn token_to_source(token: &thebe_parser::TemplateToken<'_>) -> String {
  match token {
    thebe_parser::TemplateToken::Text(text) => (*text).to_owned(),
    thebe_parser::TemplateToken::Slot { name } => name.map_or_else(
      || String::from("<slot />"),
      |name| format!(r#"<slot name="{name}" />"#),
    ),
    thebe_parser::TemplateToken::ComponentOpen {
      name,
      attrs,
      self_closing,
    } => render_component_open_tag(name, attrs, *self_closing),
    thebe_parser::TemplateToken::ComponentClose { name } => format!("</{name}>"),
  }
}

fn find_matching_component_close(
  tokens: &[thebe_parser::TemplateToken<'_>],
  open_idx: usize,
  component_name: &str,
) -> Option<usize> {
  let mut depth = 0usize;

  for (idx, token) in tokens.iter().enumerate().skip(open_idx + 1) {
    match token {
      thebe_parser::TemplateToken::ComponentOpen {
        name,
        self_closing,
        ..
      } if *name == component_name && !self_closing => depth += 1,
      thebe_parser::TemplateToken::ComponentClose { name } if *name == component_name => {
        if depth == 0 {
          return Some(idx);
        }
        depth -= 1;
      }
      _ => {}
    }
  }

  None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractedNamedSlot {
  name: String,
  content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractedComponentSlots {
  default_content: String,
  named_slots: Vec<ExtractedNamedSlot>,
}

#[derive(Debug, Clone)]
struct ParsedHtmlTag {
  start: usize,
  end: usize,
  name: String,
  attrs: Vec<(String, Option<String>)>,
  is_closing: bool,
  self_closing: bool,
}

fn extract_component_slots(content: &str, allowed_named_slots: &[String]) -> ExtractedComponentSlots {
  if allowed_named_slots.is_empty() {
    return ExtractedComponentSlots {
      default_content: content.to_owned(),
      named_slots: Vec::new(),
    };
  }

  let mut default_content = String::new();
  let mut named_slots = Vec::new();
  let mut idx = 0usize;
  let mut segment_start = 0usize;
  let mut depth = 0usize;

  while idx < content.len() {
    let Some(tag) = parse_html_tag_at(content, idx) else {
      let ch = content[idx..]
        .chars()
        .next()
        .expect("char boundary should yield a character");
      idx += ch.len_utf8();
      continue;
    };

    if depth == 0
      && !tag.is_closing
      && tag.name.eq_ignore_ascii_case("template")
      && let Some(slot_name) = tag.attrs.iter().find_map(|(name, value)| {
        (name == "slot").then_some(value.as_deref()).flatten()
      })
      && allowed_named_slots.iter().any(|allowed| allowed == slot_name)
      && let Some((inner_start, wrapper_end, inner_end)) = find_template_wrapper_bounds(content, &tag)
    {
      default_content.push_str(&content[segment_start..tag.start]);
      push_named_slot(&mut named_slots, slot_name, content[inner_start..inner_end].trim());
      idx = wrapper_end;
      segment_start = wrapper_end;
      continue;
    }

    if !tag.is_closing && !tag.self_closing && !is_void_html_element(&tag.name) {
      depth += 1;
    } else if tag.is_closing && depth > 0 {
      depth -= 1;
    }
    idx = tag.end;
  }

  default_content.push_str(&content[segment_start..]);

  ExtractedComponentSlots {
    default_content,
    named_slots,
  }
}

fn push_named_slot(named_slots: &mut Vec<ExtractedNamedSlot>, name: &str, content: &str) {
  if let Some(existing) = named_slots.iter_mut().find(|slot| slot.name == name) {
    existing.content.push_str(content);
    return;
  }

  named_slots.push(ExtractedNamedSlot {
    name: name.to_owned(),
    content: content.to_owned(),
  });
}

fn find_template_wrapper_bounds(content: &str, open_tag: &ParsedHtmlTag) -> Option<(usize, usize, usize)> {
  let mut idx = open_tag.end;
  let mut depth = 1usize;

  while idx < content.len() {
    let Some(tag) = parse_html_tag_at(content, idx) else {
      let ch = content[idx..].chars().next()?;
      idx += ch.len_utf8();
      continue;
    };

    if tag.name.eq_ignore_ascii_case("template") {
      if tag.is_closing {
        depth -= 1;
        if depth == 0 {
          return Some((open_tag.end, tag.end, tag.start));
        }
      } else if !tag.self_closing {
        depth += 1;
      }
    }

    idx = tag.end;
  }

  None
}

fn parse_html_tag_at(content: &str, start: usize) -> Option<ParsedHtmlTag> {
  if !content[start..].starts_with('<') {
    return None;
  }

  let bytes = content.as_bytes();
  let mut idx = start + 1;
  if idx >= bytes.len() || matches!(bytes[idx], b'!' | b'?') {
    return None;
  }

  while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
    idx += 1;
  }

  let is_closing = bytes.get(idx) == Some(&b'/');
  if is_closing {
    idx += 1;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
      idx += 1;
    }
  }

  let name_start = idx;
  while idx < bytes.len()
    && (bytes[idx].is_ascii_alphanumeric() || matches!(bytes[idx], b'-' | b'_'))
  {
    idx += 1;
  }
  if idx == name_start {
    return None;
  }

  let name = content[name_start..idx].to_owned();
  let attrs_start = idx;
  let mut in_quote = None;
  while idx < bytes.len() {
    match in_quote {
      Some(quote) if bytes[idx] == quote => in_quote = None,
      None if matches!(bytes[idx], b'"' | b'\'') => in_quote = Some(bytes[idx]),
      None if bytes[idx] == b'>' => break,
      _ => {}
    }
    idx += 1;
  }
  if idx >= bytes.len() {
    return None;
  }

  let end = idx + 1;
  let self_closing = !is_closing && content[start..end].trim_end().ends_with("/>");
  let attrs = if is_closing {
    Vec::new()
  } else {
    let attrs_raw = if self_closing {
      &content[attrs_start..end - 2]
    } else {
      &content[attrs_start..end - 1]
    };
    parse_tag_attrs(attrs_raw)
  };

  Some(ParsedHtmlTag {
    start,
    end,
    name,
    attrs,
    is_closing,
    self_closing,
  })
}

fn is_void_html_element(name: &str) -> bool {
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn list_used_component_names_returns_known_pascal_case_tags_once() {
    let used = list_used_component_names(
      "<Card /><div></div><Badge></Badge><Card><span /></Card><Unknown />",
      &["Card", "Badge"],
    );

    assert_eq!(used, vec![String::from("Card"), String::from("Badge")]);
  }

  #[test]
  fn expand_component_tags_with_instances_tracks_component_use_sites() {
    let known_components = [
      KnownComponent {
        name: "Card",
        named_slots: &[],
      },
      KnownComponent {
        name: "Badge",
        named_slots: &[],
      },
    ];
    let expansion = expand_component_tags_with_instances(
      r#"<Card :title="name" count><Badge label="x" /></Card>"#,
      &known_components,
    );

    assert!(expansion.template.contains("__comp_card_0({\"title\": name, \"count\": true})"));
    assert!(expansion.template.contains("__comp_badge_1({\"label\": \"x\"})"));
    assert_eq!(expansion.instances.len(), 2);
    assert_eq!(expansion.instances[0].component_name, "Card");
    assert_eq!(expansion.instances[0].instance_id, "c0");
    assert_eq!(expansion.instances[0].attrs[0].name, "title");
    assert!(expansion.instances[0].attrs[0].dynamic);
    assert_eq!(expansion.instances[0].attrs[0].value.as_deref(), Some("name"));
    assert_eq!(expansion.instances[0].attrs[1].name, "count");
    assert!(!expansion.instances[0].attrs[1].dynamic);
    assert_eq!(expansion.instances[0].attrs[1].value, None);
    assert_eq!(expansion.instances[1].component_name, "Badge");
    assert_eq!(expansion.instances[1].instance_id, "c1");
  }

  #[test]
  fn prefix_event_handler_attrs_scopes_component_handlers() {
    let out = prefix_event_handler_attrs(
      r#"<button onclick="increment" oninput="adjust(this.value)" data-x="keep"></button>"#,
      &[String::from("increment"), String::from("adjust")],
      "__thebe_component_c0__",
    );

    assert!(out.contains(r#"onclick="__thebe_component_c0__increment""#), "{out}");
    assert!(out.contains(r#"oninput="__thebe_component_c0__adjust(this.value)""#), "{out}");
    assert!(out.contains(r#"data-x="keep""#), "{out}");
  }

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

  #[test]
  fn expand_slot_defaults_to_empty_when_no_caller_exists() {
    let out = expand_slot("<div><slot /></div>");

    assert_eq!(out, "<div>{{ caller() if caller is defined else '' }}</div>");
  }

  #[test]
  fn expand_slot_collects_named_slot_placeholders() {
    let expansion = expand_slot_with_metadata(
      r#"<article><slot name="header" /><slot /><slot name="actions" /></article>"#,
    );

    assert_eq!(
      expansion.template,
      "<article>{{ __slot_header() if __slot_header is not none else '' }}{{ caller() if caller is defined else '' }}{{ __slot_actions() if __slot_actions is not none else '' }}</article>"
    );
    assert_eq!(expansion.named_slots, vec![String::from("header"), String::from("actions")]);
  }

  #[test]
  fn expand_component_tags_with_instances_routes_named_slot_templates() {
    let named_slots = vec![String::from("header")];
    let expansion = expand_component_tags_with_instances(
      r#"<Card><template slot="header"><h1>{{ title }}</h1></template><p>{{ body }}</p></Card>"#,
      &[KnownComponent {
        name: "Card",
        named_slots: &named_slots,
      }],
    );

    assert!(
      expansion
        .template
        .contains("{% macro __comp_card_0__slot_header() %}<h1>{{ title }}</h1>{% endmacro %}"),
      "{}",
      expansion.template
    );
    assert!(
      expansion
        .template
        .contains("{% call __comp_card_0({}, __slot_header=__comp_card_0__slot_header) %}"),
      "{}",
      expansion.template
    );
    assert!(expansion.template.contains("<p>{{ body }}</p>{% endcall %}"), "{}", expansion.template);
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

  // ── inject_attr_bindings ─────────────────────────────────────────────────

  #[test]
  fn attr_binding_rewrites_dynamic_class() {
    let out = inject_attr_bindings(r#"<div :class="theme">text</div>"#);
    assert_eq!(
      out,
      r#"<div class="{{ theme }}" data-thebe-attr="class:theme">text</div>"#
    );
  }

  #[test]
  fn attr_binding_multiple_attrs_are_coalesced() {
    let out = inject_attr_bindings(r#"<a :href="url" :title="label">link</a>"#);
    assert!(out.contains(r#"href="{{ url }}""#), "{out}");
    assert!(out.contains(r#"title="{{ label }}""#), "{out}");
    assert!(out.contains(r#"data-thebe-attr="href:url,title:label""#), "{out}");
  }

  #[test]
  fn attr_binding_static_attr_is_preserved() {
    let out = inject_attr_bindings(r#"<input type="text" :value="query" />"#);
    assert!(out.contains(r#"type="text""#), "{out}");
    assert!(out.contains(r#"value="{{ query }}""#), "{out}");
    assert!(out.contains(r#"data-thebe-attr="value:query""#), "{out}");
  }

  #[test]
  fn attr_binding_shadows_static_class() {
    let out = inject_attr_bindings(r#"<div class="card" :class="extra">x</div>"#);
    let class_count = out.matches("class=").count();
    assert_eq!(class_count, 1, "only one class attr expected: {out}");
    assert!(out.contains(r#"class="{{ extra }}""#), "{out}");
  }

  #[test]
  fn attr_binding_self_closing_stays_self_closing() {
    let out = inject_attr_bindings(r#"<img :src="url" alt="photo" />"#);
    assert!(out.ends_with("/>"), "should remain self-closing: {out}");
    assert!(out.contains(r#"src="{{ url }}""#), "{out}");
    assert!(out.contains(r#"data-thebe-attr="src:url""#), "{out}");
  }

  #[test]
  fn attr_binding_dotted_key() {
    let out = inject_attr_bindings(r#"<img :src="user.avatar" />"#);
    assert!(out.contains(r#"src="{{ user.avatar }}""#), "{out}");
    assert!(out.contains(r#"data-thebe-attr="src:user.avatar""#), "{out}");
  }

  #[test]
  fn attr_binding_skips_component_tags() {
    let input = r#"<Card :title="name">x</Card>"#;
    assert_eq!(inject_attr_bindings(input), input);
  }

  #[test]
  fn attr_binding_no_dynamic_is_passthrough() {
    let tmpl = r#"<button class="btn" disabled>ok</button>"#;
    assert_eq!(inject_attr_bindings(tmpl), tmpl);
  }

  #[test]
  fn attr_binding_skips_closing_tags() {
    let tmpl = r#"</div>"#;
    assert_eq!(inject_attr_bindings(tmpl), tmpl);
  }
}
