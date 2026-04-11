mod error;

pub use error::CssError;

use std::convert::Infallible;

use lightningcss::visit_types;
use lightningcss::{
  selector::{Combinator, Component, Selector, SelectorList},
  stylesheet::{MinifyOptions, ParserOptions, PrinterOptions, StyleSheet},
  values::{ident::Ident, string::CowArcStr},
  visitor::{Visit, VisitTypes, Visitor},
};

/// Compute a deterministic 8-hex-char scope ID from a route file path using
/// FNV-1a (32-bit) so that the same file always produces the same string.
#[must_use]
pub fn scope_id(file_path: &str) -> String {
  const FNV_OFFSET: u32 = 2_166_136_261;
  const FNV_PRIME: u32 = 16_777_619;

  let mut hash = FNV_OFFSET;
  for byte in file_path.bytes() {
    hash ^= u32::from(byte);
    hash = hash.wrapping_mul(FNV_PRIME);
  }
  format!("{hash:08x}")
}

/// Process a CSS block by:
/// 1. Appending `[data-thebe-c-{scope_id}]` to every selector, restricting
///    rules to elements rendered by this component.
/// 2. Minifying the output.
///
/// # Errors
///
/// Returns [`CssError`] on CSS parse, transform, or print failure.
pub fn process_style(css: &str, scope_id: &str) -> Result<String, CssError> {
  let attr_name = format!("data-thebe-c-{scope_id}");
  let mut stylesheet =
    StyleSheet::parse(css, ParserOptions::default()).map_err(|e| CssError::Parse(e.to_string()))?;

  let mut visitor = ScopeVisitor {
    attr_name: &attr_name,
  };
  // The error type is `Infallible`, so `unwrap` is safe here.
  Visit::visit(&mut stylesheet, &mut visitor).unwrap();

  stylesheet
    .minify(MinifyOptions::default())
    .map_err(|e| CssError::Minify(e.to_string()))?;

  let result = stylesheet
    .to_css(PrinterOptions {
      minify: true,
      ..Default::default()
    })
    .map_err(|e| CssError::Print(e.to_string()))?;

  Ok(result.code)
}

/// Inject `data-thebe-c-{scope_id}=""` onto every HTML opening tag in
/// `template`.  Closing tags, comments, and doctypes are passed through
/// unchanged.
///
/// This pairs with [`process_style`]: the scoped CSS selectors target
/// `[data-thebe-c-{scope_id}]`, and every element in the template carries
/// that attribute, so styles are confined to this component.
#[must_use]
pub fn add_scope_attrs(template: &str, scope_id: &str) -> String {
  add_html_attr(template, &format!("data-thebe-c-{scope_id}"), "")
}

/// Inject a static attribute onto every HTML opening tag in `template`.
///
/// Closing tags, comments, and doctypes are passed through unchanged.
#[must_use]
pub fn add_html_attr(template: &str, attr_name: &str, attr_value: &str) -> String {
  let attr = format!(" {attr_name}=\"{attr_value}\"");
  add_opening_tag_attr(template, &attr)
}

fn add_opening_tag_attr(template: &str, attr: &str) -> String {
  let mut out = String::with_capacity(template.len() + attr.len() * 8);
  let mut chars = template.chars().peekable();

  while let Some(ch) = chars.next() {
    if ch != '<' {
      out.push(ch);
      continue;
    }

    // Decide what kind of token follows `<`.
    match chars.peek().copied() {
      Some(c) if c.is_ascii_alphabetic() => {
        // Opening tag — consume tag name.
        out.push('<');
        while let Some(&c) = chars.peek() {
          if c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.') {
            out.push(c);
            chars.next();
          } else {
            break;
          }
        }

        // Consume attribute list and `>`, respecting quoted values.
        let mut tail = String::new();
        let mut quote: Option<char> = None;
        loop {
          let Some(c) = chars.next() else {
            // Truncated tag — flush whatever we have.
            out.push_str(&tail);
            break;
          };
          if let Some(q) = quote {
            tail.push(c);
            if c == q {
              quote = None;
            }
          } else if c == '"' || c == '\'' {
            quote = Some(c);
            tail.push(c);
          } else if c == '>' {
            // Insert the scope attribute just before `>`.
            if tail.ends_with('/') {
              tail.pop();
              out.push_str(tail.trim_end());
              out.push_str(&attr);
              out.push_str("/>");
            } else {
              out.push_str(&tail);
              out.push_str(&attr);
              out.push('>');
            }
            break;
          } else {
            tail.push(c);
          }
        }
      }

      _ => {
        // Closing tag `</…>`, comment `<!--…-->`, or doctype `<!…>`:
        // pass through unchanged.
        out.push('<');
        for c in chars.by_ref() {
          out.push(c);
          if c == '>' {
            break;
          }
        }
      }
    }
  }

  out
}

// ── Selector-scoping visitor ──────────────────────────────────────────────────

struct ScopeVisitor<'a> {
  attr_name: &'a str,
}

impl<'i> Visitor<'i> for ScopeVisitor<'_> {
  type Error = Infallible;

  fn visit_types(&self) -> VisitTypes {
    visit_types!(SELECTORS)
  }

  fn visit_selector_list(&mut self, selectors: &mut SelectorList<'i>) -> Result<(), Self::Error> {
    for selector in &mut selectors.0 {
      scope_selector(selector, self.attr_name);
    }
    Ok(())
  }
}

/// Append `[{attr_name}]` to the last compound selector in source order.
///
/// `iter_raw_match_order()` yields components in **match order** (rightmost
/// compound first, e.g. `nav a` → `[a, Desc, nav]`). `Selector::from(Vec)`
/// rebuilds via `SelectorBuilder` and expects **source order** (left-to-right).
///
/// Algorithm:
/// 1. Split the match-order vec into compound slices + inter-compound
///    combinators (ignoring `Combinator::PseudoElement`).
/// 2. Reverse both slices to obtain source order.
/// 3. Append the scope attribute to the last compound (the direct match
///    target), placing it before any pseudo-classes.
/// 4. Reassemble in source order and pass to `Selector::from`.
fn scope_selector<'i>(selector: &mut Selector<'i>, attr_name: &str) {
  let components: Vec<Component<'i>> = selector.iter_raw_match_order().cloned().collect();

  // Split into compound slices and the CSS combinators between them.
  // We deliberately skip `Combinator::PseudoElement` / `SlotAssignment` since
  // those are internal markers, not real inter-compound combinators.
  let mut compounds: Vec<Vec<Component<'i>>> = Vec::new();
  let mut combinators: Vec<Component<'i>> = Vec::new();
  let mut current: Vec<Component<'i>> = Vec::new();

  for component in components {
    if let Component::Combinator(ref comb) = component {
      if !matches!(comb, Combinator::PseudoElement | Combinator::SlotAssignment) {
        compounds.push(std::mem::take(&mut current));
        combinators.push(component);
        continue;
      }
    }
    current.push(component);
  }
  compounds.push(current);

  // Reverse: match order has rightmost compound first; source order has it last.
  compounds.reverse();
  combinators.reverse();

  // Append scope to the last compound in source order (the element being styled),
  // before any pseudo-class components so the attribute precedes them.
  let last = compounds
    .last_mut()
    .expect("selector has at least one compound");
  let insert_pos = last
    .iter()
    .position(|c| {
      matches!(
        c,
        Component::NonTSPseudoClass(_) | Component::PseudoElement(_)
      )
    })
    .unwrap_or(last.len());

  // `CowArcStr` accepts `String`, which it wraps in an Arc internally.
  let local_name = Ident(CowArcStr::from(attr_name.to_owned()));
  let local_name_lower = Ident(CowArcStr::from(attr_name.to_owned()));
  last.insert(
    insert_pos,
    Component::AttributeInNoNamespaceExists {
      local_name,
      local_name_lower,
    },
  );

  // Reassemble in source order for Selector::from.
  let mut new_components: Vec<Component<'i>> = Vec::new();
  for (i, compound) in compounds.into_iter().enumerate() {
    new_components.extend(compound);
    if i < combinators.len() {
      new_components.push(combinators[i].clone());
    }
  }

  *selector = Selector::from(new_components);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::{add_html_attr, add_scope_attrs, process_style, scope_id};

  #[test]
  fn scope_id_is_deterministic() {
    let id1 = scope_id("src/routes/index.trs");
    let id2 = scope_id("src/routes/index.trs");
    assert_eq!(id1, id2);
    assert_eq!(id1.len(), 8);
    // Different paths produce different IDs.
    let id3 = scope_id("src/routes/about.trs");
    assert_ne!(id1, id3);
  }

  #[test]
  fn process_style_scopes_simple_selector() {
    let css = "button { color: red; }";
    let id = "abc123";
    let out = process_style(css, id).unwrap();
    // scope attr must be appended to the type selector, not prepended
    let expected = format!("button[data-thebe-c-{id}]");
    assert!(out.contains(&expected), "output: {out}");
  }

  #[test]
  fn process_style_descendant_selector() {
    let css = "nav a { color: red; }";
    let id = "abc123";
    let out = process_style(css, id).unwrap();
    // scope goes on the last compound in source order (`a`), not on `nav`
    let expected = format!("nav a[data-thebe-c-{id}]");
    assert!(out.contains(&expected), "output: {out}");
  }

  #[test]
  fn process_style_pseudo_class_selector() {
    let css = "button:hover { color: red; }";
    let id = "abc123";
    let out = process_style(css, id).unwrap();
    // scope goes between base selector and pseudo-class
    let expected = format!("button[data-thebe-c-{id}]:hover");
    assert!(out.contains(&expected), "output: {out}");
  }

  #[test]
  fn process_style_scopes_class_selector() {
    let css = ".controls { display: flex; }";
    let id = "abc123";
    let out = process_style(css, id).unwrap();
    assert!(
      out.contains(&format!("[data-thebe-c-{id}]")),
      "output: {out}"
    );
  }

  #[test]
  fn add_scope_attrs_simple() {
    let tmpl = "<div><span>hello</span></div>";
    let out = add_scope_attrs(tmpl, "abc123");
    assert!(out.contains(r#"data-thebe-c-abc123=""#), "output: {out}");
  }

  #[test]
  fn add_scope_attrs_preserves_existing_attrs() {
    let tmpl = r#"<button class="btn">click</button>"#;
    let out = add_scope_attrs(tmpl, "abc123");
    assert!(out.contains(r#"class="btn""#), "output: {out}");
    assert!(out.contains(r#"data-thebe-c-abc123=""#), "output: {out}");
  }

  #[test]
  fn add_scope_attrs_skips_closing_tags() {
    let tmpl = "<div></div>";
    let out = add_scope_attrs(tmpl, "abc123");
    // Only the opening tag should carry the attribute.
    assert_eq!(
      out.matches("data-thebe-c-abc123").count(),
      1,
      "output: {out}"
    );
  }

  #[test]
  fn add_html_attr_marks_head_elements() {
    let html = r#"<title>Page</title><meta name="description" content="Hi">"#;
    let out = add_html_attr(html, "data-thebe-head", "");

    assert!(out.contains(r#"<title data-thebe-head="">Page</title>"#));
    assert!(
      out.contains(r#"<meta name="description" content="Hi" data-thebe-head="">"#),
      "output: {out}"
    );
  }
}
