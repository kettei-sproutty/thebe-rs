use crate::{error::CodegenError, template};
use std::fmt::Write as _;
use thebe_parser::SfcBlocks;

/// The thebe-client runtime JS, compiled into the binary at build time.
///
/// Every generated route embeds this verbatim into the served HTML so no
/// external CDN or npm install is required during `thebe dev`.
const THEBE_CLIENT_RUNTIME: &str = include_str!("../../../packages/thebe-client/runtime.js");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpMethod {
  Delete,
  Get,
  Head,
  Options,
  Patch,
  Post,
  Put,
}

impl HttpMethod {
  fn as_attr_name(self) -> &'static str {
    match self {
      Self::Delete => "delete",
      Self::Get => "get",
      Self::Head => "head",
      Self::Options => "options",
      Self::Patch => "patch",
      Self::Post => "post",
      Self::Put => "put",
    }
  }

  fn routing_fn(self) -> &'static str {
    self.as_attr_name()
  }
}

#[derive(Debug, PartialEq, Eq)]
struct RouteHandler {
  method: HttpMethod,
  name: String,
  param_types: Vec<String>,
  is_async: bool,
}

#[derive(Clone, Copy)]
struct ModuleLiterals<'a> {
  template: &'a str,
  runtime: &'a str,
  client_script: &'a str,
  style: &'a str,
  /// Pre-processed layout template, or `None` when no layout wraps this route.
  layout_template: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct WrapperSource<'a> {
  params: &'a str,
  call: &'a str,
}

/// Metadata the CLI provides about a discovered route file.
pub struct RouteEntry {
  /// The Rust module name (e.g. `"index"`, `"about"`).
  pub mod_name: String,
  /// Path to the generated route module relative to `src/main.rs`.
  pub source_path: String,
}

/// Generate the Rust source code for a single route module.
///
/// The output is a complete, self-contained `.rs` file that:
/// 1. Injects `#[derive(serde::Serialize)]` on the `Props` struct.
/// 2. Pastes the `<script setup>` content (stripping `#[thebe::]` attrs).
/// 3. Stores the raw template as a `const` string.
/// 4. Embeds the thebe-client runtime JS and the processed `<script lang="ts">`
///    as `const` strings.
/// 5. Emits `__thebe_render_handler` that calls the handler, renders the
///    template at runtime via `minijinja`, injects the serialised Props JSON,
///    and returns an `axum::response::Html<String>` with client scripts.
/// 6. Emits `router()` that wires up the Axum route.
///
/// When `layout` is `Some((layout_blocks, layout_scope_path))` the route body
/// is wrapped inside the layout template before the full HTML shell is emitted.
///
/// # Errors
///
/// Returns [`CodegenError`] when `<script setup>` is absent, no HTTP handler
/// function is found, or the template contains invalid syntax.
pub fn generate_route(
  blocks: &SfcBlocks,
  route_path: &str,
  layout: Option<(&SfcBlocks, &str)>,
) -> Result<String, CodegenError> {
  let setup = blocks
    .script_setup
    .as_deref()
    .ok_or(CodegenError::MissingScriptSetup)?;

  // Validate the template before committing to codegen.
  template::parse_template(&blocks.template)?;

  let handler = find_handler(setup)?;
  let setup_clean = strip_thebe_attrs(setup);
  let setup_with_serde = inject_serde_derive(&setup_clean);

  let wrapper_params = handler
    .param_types
    .iter()
    .enumerate()
    .map(|(idx, ty)| format!("__thebe_arg{idx}: {ty}"))
    .collect::<Vec<_>>()
    .join(", ");
  let call_args = (0..handler.param_types.len())
    .map(|idx| format!("__thebe_arg{idx}"))
    .collect::<Vec<_>>()
    .join(", ");

  let handler_call = if handler.is_async {
    format!("    let __props = {}({call_args}).await;\n", handler.name)
  } else {
    format!("    let __props = {}({call_args});\n", handler.name)
  };

  // Compute a deterministic scope ID and apply CSS scoping.
  let scope = thebe_css::scope_id(route_path);
  let template_scoped =
    thebe_css::add_scope_attrs(&template::inject_hydration_markers(&blocks.template), &scope);
  let style = blocks
    .style
    .as_deref()
    .filter(|s| !s.trim().is_empty())
    .map(|s| thebe_css::process_style(s, &scope))
    .transpose()
    .map_err(|e| CodegenError::CssError(e.to_string()))?
    .unwrap_or_default();

  let template_literal = escape_rust_raw_str(&template_scoped);
  let style_literal = escape_rust_raw_str(&style);

  // Process the optional `<script lang="ts">` block.
  let client_js = blocks
    .script_ts
    .as_deref()
    .map(|ts| {
      thebe_analyzer::analyze(ts)
        .map(|m| m.js)
        .unwrap_or_default()
    })
    .unwrap_or_default();
  let runtime_literal = escape_rust_raw_str(THEBE_CLIENT_RUNTIME);
  let client_script_literal = escape_rust_raw_str(&client_js);

  // Process the optional layout.
  let layout_processed = layout
    .map(|(layout_blocks, layout_scope_path)| {
      process_layout(layout_blocks, layout_scope_path)
    })
    .transpose()?;

  // Build the final style literal and optional layout template literal.
  // When a layout is present, layout style is prepended to the route style.
  let (final_style_literal, layout_template_opt) = match layout_processed {
    Some((scoped_layout_tmpl, layout_style)) => {
      let merged_style = if layout_style.is_empty() {
        style.clone()
      } else if style.is_empty() {
        layout_style
      } else {
        format!("{layout_style}\n{style}")
      };
      (
        escape_rust_raw_str(&merged_style),
        Some(escape_rust_raw_str(&scoped_layout_tmpl)),
      )
    }
    None => (style_literal.clone(), None),
  };

  let literals = ModuleLiterals {
    template: &template_literal,
    runtime: &runtime_literal,
    client_script: &client_script_literal,
    style: &final_style_literal,
    layout_template: layout_template_opt.as_deref(),
  };
  let wrapper = WrapperSource {
    params: &wrapper_params,
    call: &handler_call,
  };

  Ok(build_route_module(
    &setup_with_serde,
    literals,
    wrapper,
    &handler,
    route_path,
  ))
}

fn build_route_module(
  setup_with_serde: &str,
  literals: ModuleLiterals<'_>,
  wrapper: WrapperSource<'_>,
  handler: &RouteHandler,
  route_path: &str,
) -> String {
  let mut source = String::new();
  source.push_str("// AUTOGENERATED by thebe \u{2014} do not edit\n");
  source.push_str("#![allow(dead_code, private_interfaces)]\n");
  source.push_str(setup_with_serde);
  source.push_str("\n\n");
  write_module_constants(&mut source, literals);
  if literals.layout_template.is_some() {
    write_render_handler_with_layout(&mut source, wrapper);
  } else {
    write_render_handler(&mut source, wrapper);
  }
  write_router_fn(&mut source, handler, route_path);
  source
}

fn write_module_constants(source: &mut String, literals: ModuleLiterals<'_>) {
  writeln!(source, "const __TEMPLATE: &str = {};", literals.template).expect("infallible");
  writeln!(
    source,
    "const __CLIENT_RUNTIME: &str = {};",
    literals.runtime
  )
  .expect("infallible");
  writeln!(source, "const __STYLE: &str = {};", literals.style).expect("infallible");
  writeln!(
    source,
    "const __CLIENT_SCRIPT: &str = {};",
    literals.client_script
  )
  .expect("infallible");
  if let Some(layout_tmpl) = literals.layout_template {
    writeln!(source, "const __LAYOUT_TEMPLATE: &str = {layout_tmpl};").expect("infallible");
  }
  source.push('\n');
}

fn write_render_handler(source: &mut String, wrapper: WrapperSource<'_>) {
  if wrapper.params.is_empty() {
    source.push_str("async fn __thebe_render_handler() -> axum::response::Html<String> {\n");
  } else {
    writeln!(
      source,
      "async fn __thebe_render_handler({}) -> axum::response::Html<String> {{",
      wrapper.params
    )
    .expect("infallible");
  }
  source.push_str(wrapper.call);
  source.push_str(
    "    let __ctx = serde_json::to_value(&__props)\
         .expect(\"Props serialisation error\");\n",
  );
  source.push_str("    let __body = {\n");
  source.push_str("        use minijinja::Environment;\n");
  source.push_str("        let mut env = Environment::new();\n");
  source.push_str(
    "        env.add_template(\"__page\", __TEMPLATE)\
         .expect(\"template compile error\");\n",
  );
  source.push_str(
    "        env.get_template(\"__page\")\
         .expect(\"template not found\")\n",
  );
  source.push_str(
    "            .render(&__ctx)\
         .expect(\"template render error\")\n",
  );
  source.push_str("    };\n");
  write_html_assembly(source);
  source.push_str("}\n\n");
}

/// Emit the `let __props_json`, `let __html = format!(…)`, and `Html(__html)`
/// tail that is identical in both the plain and layout render handlers.
///
/// Preconditions (variables that must already be in scope in the generated code):
/// - `__ctx: serde_json::Value`
/// - `__body: String`
fn write_html_assembly(source: &mut String) {
  source.push_str("    let __props_json = __ctx.to_string();\n");
  source.push_str("    let __html = format!(\n");
  source.push_str(
    "        \"<!DOCTYPE html>\\n\\
         <html>\\n\\
         <head><style>{style}</style></head>\\n\\
         <body>\\n\\
         {body}\\n\\
         <script id=\\\"__thebe_props\\\" type=\\\"application/json\\\">{props_json}</script>\\n\\
         <script>{runtime}</script>\\n\\
         <script>{user_script}</script>\\n\\
         </body>\\n\\
         </html>\",\n",
  );
  source.push_str("        style = __STYLE,\n");
  source.push_str("        body = __body,\n");
  source.push_str("        props_json = __props_json,\n");
  source.push_str("        runtime = __CLIENT_RUNTIME,\n");
  source.push_str("        user_script = __CLIENT_SCRIPT,\n");
  source.push_str("    );\n");
  source.push_str("    axum::response::Html(__html)\n");
}

/// Like [`write_render_handler`] but renders the route body first, then wraps
/// it inside the layout template before assembling the HTML shell.
fn write_render_handler_with_layout(source: &mut String, wrapper: WrapperSource<'_>) {
  if wrapper.params.is_empty() {
    source.push_str("async fn __thebe_render_handler() -> axum::response::Html<String> {\n");
  } else {
    writeln!(
      source,
      "async fn __thebe_render_handler({}) -> axum::response::Html<String> {{",
      wrapper.params
    )
    .expect("infallible");
  }
  source.push_str(wrapper.call);
  source.push_str(
    "    let __ctx = serde_json::to_value(&__props)\
         .expect(\"Props serialisation error\");\n",
  );
  // Render the route template into a string first.
  source.push_str("    let __route_body = {\n");
  source.push_str("        use minijinja::Environment;\n");
  source.push_str("        let mut env = Environment::new();\n");
  source.push_str(
    "        env.add_template(\"__page\", __TEMPLATE)\
         .expect(\"template compile error\");\n",
  );
  source.push_str(
    "        env.get_template(\"__page\")\
         .expect(\"template not found\")\n",
  );
  source.push_str(
    "            .render(&__ctx)\
         .expect(\"template render error\")\n",
  );
  source.push_str("    };\n");
  // Wrap the route body inside the layout template.
  source.push_str("    let __body = {\n");
  source.push_str("        use minijinja::Environment;\n");
  source.push_str("        let mut env = Environment::new();\n");
  source.push_str(
    "        env.add_template(\"__layout\", __LAYOUT_TEMPLATE)\
         .expect(\"layout template compile error\");\n",
  );
  source.push_str(
    "        let __layout_ctx = serde_json::json!({ \"__slot\": __route_body });\n",
  );
  source.push_str(
    "        env.get_template(\"__layout\")\
         .expect(\"layout template not found\")\n",
  );
  source.push_str(
    "            .render(&__layout_ctx)\
         .expect(\"layout render error\")\n",
  );
  source.push_str("    };\n");
  write_html_assembly(source);
  source.push_str("}\n\n");
}

fn write_router_fn(source: &mut String, handler: &RouteHandler, route_path: &str) {
  source.push_str("pub fn router() -> axum::Router {\n");
  source.push_str("    axum::Router::new().route(\n");
  writeln!(source, "        \"{route_path}\",").expect("infallible");
  writeln!(
    source,
    "        axum::routing::{}(__thebe_render_handler),",
    handler.method.routing_fn()
  )
  .expect("infallible");
  source.push_str("    )\n");
  source.push_str("}\n");
}

/// Generate `src/__thebe_routes.rs` that declares all route modules and
/// exposes a `__thebe_router()` helper.  The static `src/main.rs`
/// (scaffolded by `thebe new`, never regenerated) does
/// `include!("__thebe_routes.rs")` and owns the `main()` entry-point.
#[must_use]
pub fn generate_routes_file(routes: &[RouteEntry]) -> String {
  let mut source = String::new();
  source.push_str("// AUTOGENERATED by thebe \u{2014} do not edit\n");
  for route in routes {
    writeln!(source, "#[path = \"{}\"]", route.source_path).expect("infallible");
    writeln!(source, "mod {};", route.mod_name).expect("infallible");
  }
  source.push('\n');
  source.push_str("fn __thebe_router() -> axum::Router {\n");
  source.push_str("    axum::Router::new()\n");
  for route in routes {
    writeln!(source, "        .merge({}::router())", route.mod_name).expect("infallible");
  }
  source.push_str(
    "        .fallback_service(tower_http::services::ServeDir::new(\"public\"))\n",
  );
  source.push_str("}\n");

  source
}

/// Replace all `<slot />`, `<slot/>`, and `<slot></slot>` occurrences in a
/// layout template with the minijinja binding `{{ __slot }}`.
fn replace_slot(template: &str) -> String {
  template
    .replace("<slot></slot>", "{{ __slot }}")
    .replace("<slot />", "{{ __slot }}")
    .replace("<slot/>", "{{ __slot }}")
}

/// Process a `_layout.trs` SFC into a (scoped_template, style_string) pair
/// ready for embedding in a generated route module.
///
/// The layout template has `<slot />` replaced with `{{ __slot }}` and CSS
/// scoping applied.  Hydration markers are intentionally **not** injected
/// since layout templates are rendered server-side only.
///
/// Returns `(scoped_template_text, processed_style_css)` — **not** escaped
/// as Rust raw-string literals; the caller applies [`escape_rust_raw_str`].
fn process_layout(
  layout_blocks: &SfcBlocks,
  layout_scope_path: &str,
) -> Result<(String, String), CodegenError> {
  // Validate the layout template (slot placeholder is a valid identifier).
  let with_slot = replace_slot(&layout_blocks.template);
  template::parse_template(&with_slot)?;

  // Apply CSS scoping to the layout template elements.
  let layout_scope = thebe_css::scope_id(layout_scope_path);
  let scoped_template = thebe_css::add_scope_attrs(&with_slot, &layout_scope);

  // Process the layout's optional `<style>` block.
  let layout_style = layout_blocks
    .style
    .as_deref()
    .filter(|s| !s.trim().is_empty())
    .map(|s| thebe_css::process_style(s, &layout_scope))
    .transpose()
    .map_err(|e| CodegenError::CssError(e.to_string()))?
    .unwrap_or_default();

  Ok((scoped_template, layout_style))
}

/// Inject `#[derive(serde::Serialize)]` immediately before every `struct Props`
/// definition that does not already carry the attribute.
fn inject_serde_derive(code: &str) -> String {
  let lines: Vec<&str> = code.lines().collect();
  let mut out = String::new();

  for (i, &line) in lines.iter().enumerate() {
    let trimmed = line.trim();
    // Detect `struct Props` declarations.
    if declares_props_struct(trimmed) {
      // Check the preceding lines for an existing serde derive.
      let already_derived =
        lines[..i].iter().rev().take(5).any(|prev| {
          prev.trim().contains("serde::Serialize") || prev.trim().contains("Serialize")
        });
      if !already_derived {
        out.push_str("#[derive(serde::Serialize)]\n");
      }
    }
    out.push_str(line);
    out.push('\n');
  }

  // Strip trailing newline added by the loop — callers add their own.
  out.trim_end_matches('\n').to_owned()
}

fn declares_props_struct(line: &str) -> bool {
  let mut words = line.split_whitespace();
  while let Some(word) = words.next() {
    if word == "struct" {
      return words.next().is_some_and(|name| name == "Props");
    }
  }
  false
}

/// Remove lines that consist solely of `#[thebe::...]` attributes.
fn strip_thebe_attrs(code: &str) -> String {
  code
    .lines()
    .filter(|line| {
      let t = line.trim();
      !(t.starts_with("#[thebe::") && t.ends_with(']'))
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Escape `template` so it can be embedded as a Rust raw string literal.
///
/// We use `r#"..."#` with enough hashes to survive any `"#` sequences in the
/// template.  A maximum of 16 hashes should be enough for any real HTML.
fn escape_rust_raw_str(template: &str) -> String {
  let mut max_run = 0usize;
  let bytes = template.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'"' {
      // Count consecutive '#' after the quote.
      let mut run = 0usize;
      i += 1;
      while i < bytes.len() && bytes[i] == b'#' {
        run += 1;
        i += 1;
      }
      if run > max_run {
        max_run = run;
      }
    } else {
      i += 1;
    }
  }
  let hashes = "#".repeat(max_run + 1);
  format!("r{hashes}\"{template}\"{hashes}")
}

fn find_handler(setup: &str) -> Result<RouteHandler, CodegenError> {
  let lines: Vec<&str> = setup.lines().collect();

  for (idx, line) in lines.iter().enumerate() {
    let trimmed = line.trim();
    let Some(method) = parse_thebe_method(trimmed)? else {
      continue;
    };

    let mut signature = String::new();
    let remainder = line.split_once(']').map_or("", |(_, rest)| rest).trim();
    if !remainder.is_empty() {
      signature.push_str(remainder);
      if signature_complete(&signature) {
        return parse_handler_signature(&signature, method);
      }
    }

    for next in &lines[idx + 1..] {
      let trimmed = next.trim();
      if signature.is_empty()
        && (trimmed.is_empty()
          || trimmed.starts_with("#[")
          || trimmed.starts_with("///")
          || trimmed.starts_with("//!")
          || trimmed.starts_with("//"))
      {
        continue;
      }

      if !signature.is_empty() {
        signature.push('\n');
      }
      signature.push_str(next);

      if signature_complete(&signature) {
        return parse_handler_signature(&signature, method);
      }
    }

    return Err(CodegenError::InvalidHandlerSignature(
      method.as_attr_name().to_owned(),
    ));
  }

  Err(CodegenError::MissingHandler)
}

fn parse_thebe_method(line: &str) -> Result<Option<HttpMethod>, CodegenError> {
  let Some(raw_method) = line
    .strip_prefix("#[thebe::")
    .and_then(|rest| rest.split(']').next())
  else {
    return Ok(None);
  };

  let method = match raw_method.trim() {
    "delete" => HttpMethod::Delete,
    "get" => HttpMethod::Get,
    "head" => HttpMethod::Head,
    "options" => HttpMethod::Options,
    "patch" => HttpMethod::Patch,
    "post" => HttpMethod::Post,
    "put" => HttpMethod::Put,
    other => return Err(CodegenError::UnsupportedMethod(other.to_owned())),
  };

  Ok(Some(method))
}

fn signature_complete(signature: &str) -> bool {
  let mut paren_depth = 0u32;
  let mut bracket_depth = 0u32;
  let mut angle_depth = 0u32;
  let mut in_string: Option<char> = None;
  let mut chars = signature.chars();

  while let Some(ch) = chars.next() {
    if let Some(delim) = in_string {
      if ch == '\\' {
        chars.next();
      } else if ch == delim {
        in_string = None;
      }
      continue;
    }

    match ch {
      '"' | '\'' | '`' => in_string = Some(ch),
      '(' => paren_depth += 1,
      ')' if paren_depth > 0 => paren_depth -= 1,
      '[' => bracket_depth += 1,
      ']' if bracket_depth > 0 => bracket_depth -= 1,
      '<' => angle_depth += 1,
      '>' if angle_depth > 0 => angle_depth -= 1,
      '{' | ';' if paren_depth == 0 && bracket_depth == 0 && angle_depth == 0 => {
        return true;
      }
      _ => {}
    }
  }

  false
}

fn parse_handler_signature(
  signature: &str,
  method: HttpMethod,
) -> Result<RouteHandler, CodegenError> {
  let fn_pos = signature
    .find("fn ")
    .ok_or_else(|| CodegenError::InvalidHandlerSignature(method.as_attr_name().to_owned()))?;
  let before_fn = &signature[..fn_pos];
  let is_async = before_fn.split_whitespace().any(|token| token == "async");

  let after_fn = &signature[fn_pos + 3..];
  let name: String = after_fn
    .trim_start()
    .chars()
    .take_while(|c| c.is_alphanumeric() || *c == '_')
    .collect();
  if name.is_empty() {
    return Err(CodegenError::InvalidHandlerSignature(
      method.as_attr_name().to_owned(),
    ));
  }

  let open_paren = after_fn
    .find('(')
    .ok_or_else(|| CodegenError::InvalidHandlerSignature(method.as_attr_name().to_owned()))?;
  let params_start = fn_pos + 3 + open_paren;
  let params_end = find_matching_paren(signature, params_start)
    .ok_or_else(|| CodegenError::InvalidHandlerSignature(method.as_attr_name().to_owned()))?;
  let params = &signature[params_start + 1..params_end];
  let param_types = split_top_level(params, ',')
    .into_iter()
    .map(str::trim)
    .filter(|param| !param.is_empty())
    .map(|param| {
      split_param_type(param)
        .ok_or_else(|| CodegenError::InvalidHandlerSignature(method.as_attr_name().to_owned()))
    })
    .collect::<Result<Vec<_>, _>>()?;

  Ok(RouteHandler {
    method,
    name,
    param_types,
    is_async,
  })
}

fn find_matching_paren(signature: &str, open_paren: usize) -> Option<usize> {
  let bytes = signature.as_bytes();
  if bytes.get(open_paren).copied()? != b'(' {
    return None;
  }

  let mut depth = 0u32;
  for (idx, byte) in bytes.iter().enumerate().skip(open_paren) {
    match byte {
      b'(' => depth += 1,
      b')' => {
        depth -= 1;
        if depth == 0 {
          return Some(idx);
        }
      }
      _ => {}
    }
  }

  None
}

fn split_top_level(input: &str, separator: char) -> Vec<&str> {
  let mut parts = Vec::new();
  let mut start = 0usize;
  let mut paren_depth = 0u32;
  let mut bracket_depth = 0u32;
  let mut brace_depth = 0u32;
  let mut angle_depth = 0u32;
  let mut in_string: Option<char> = None;
  let mut escaped = false;

  for (idx, ch) in input.char_indices() {
    if let Some(delim) = in_string {
      if escaped {
        escaped = false;
        continue;
      }
      if ch == '\\' {
        escaped = true;
      } else if ch == delim {
        in_string = None;
      }
      continue;
    }

    match ch {
      '"' | '\'' | '`' => in_string = Some(ch),
      '(' => paren_depth += 1,
      ')' if paren_depth > 0 => paren_depth -= 1,
      '[' => bracket_depth += 1,
      ']' if bracket_depth > 0 => bracket_depth -= 1,
      '{' => brace_depth += 1,
      '}' if brace_depth > 0 => brace_depth -= 1,
      '<' => angle_depth += 1,
      '>' if angle_depth > 0 => angle_depth -= 1,
      _ if ch == separator
        && paren_depth == 0
        && bracket_depth == 0
        && brace_depth == 0
        && angle_depth == 0 =>
      {
        parts.push(&input[start..idx]);
        start = idx + ch.len_utf8();
      }
      _ => {}
    }
  }

  parts.push(&input[start..]);
  parts
}

fn split_param_type(param: &str) -> Option<String> {
  let mut paren_depth = 0u32;
  let mut bracket_depth = 0u32;
  let mut brace_depth = 0u32;
  let mut angle_depth = 0u32;
  let mut in_string: Option<char> = None;
  let mut escaped = false;

  for (idx, ch) in param.char_indices() {
    if let Some(delim) = in_string {
      if escaped {
        escaped = false;
        continue;
      }
      if ch == '\\' {
        escaped = true;
      } else if ch == delim {
        in_string = None;
      }
      continue;
    }

    match ch {
      '"' | '\'' | '`' => in_string = Some(ch),
      '(' => paren_depth += 1,
      ')' if paren_depth > 0 => paren_depth -= 1,
      '[' => bracket_depth += 1,
      ']' if bracket_depth > 0 => bracket_depth -= 1,
      '{' => brace_depth += 1,
      '}' if brace_depth > 0 => brace_depth -= 1,
      '<' => angle_depth += 1,
      '>' if angle_depth > 0 => angle_depth -= 1,
      ':' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 && angle_depth == 0 => {
        return Some(param[idx + 1..].trim().to_owned());
      }
      _ => {}
    }
  }

  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn strip_thebe_attrs_removes_attr_lines() {
    let input = "#[thebe::get]\npub fn handler() -> Props { todo!() }";
    let result = strip_thebe_attrs(input);
    assert!(!result.contains("#[thebe::get]"));
    assert!(result.contains("pub fn handler()"));
  }

  #[test]
  fn find_handler_detects_sync_fn() {
    let setup = "#[thebe::get]\npub fn my_handler() -> Props { todo!() }";
    let handler = find_handler(setup).unwrap();
    assert_eq!(handler.name, "my_handler");
    assert_eq!(handler.method, HttpMethod::Get);
    assert!(handler.param_types.is_empty());
    assert!(!handler.is_async);
  }

  #[test]
  fn find_handler_detects_async_fn() {
    let setup = "#[thebe::post]\npub async fn submit() -> Props { todo!() }";
    let handler = find_handler(setup).unwrap();
    assert_eq!(handler.name, "submit");
    assert_eq!(handler.method, HttpMethod::Post);
    assert!(handler.is_async);
  }

  #[test]
  fn find_handler_preserves_extractor_types() {
    let setup = "#[thebe::patch]\npub async fn update(\n    Path(slug): Path<String>,\n    State(state): State<AppState>,\n    Json(body): Json<UpdateBody>,\n) -> Props {\n    todo!()\n}";
    let handler = find_handler(setup).unwrap();
    assert_eq!(
      handler.param_types,
      vec![
        "Path<String>".to_owned(),
        "State<AppState>".to_owned(),
        "Json<UpdateBody>".to_owned(),
      ]
    );
  }

  #[test]
  fn find_handler_returns_error_when_missing() {
    let setup = "pub fn helper() {}";
    assert!(matches!(
      find_handler(setup),
      Err(CodegenError::MissingHandler)
    ));
  }

  #[test]
  fn inject_serde_derive_adds_attribute() {
    let code = "struct Props {\n    title: String,\n}";
    let result = inject_serde_derive(code);
    assert!(result.contains("#[derive(serde::Serialize)]"));
  }

  #[test]
  fn inject_serde_derive_skips_when_already_present() {
    let code = "#[derive(serde::Serialize)]\nstruct Props {\n    title: String,\n}";
    let result = inject_serde_derive(code);
    // Should appear exactly once, not twice.
    assert_eq!(result.matches("serde::Serialize").count(), 1);
  }

  #[test]
  fn inject_serde_derive_handles_pub_crate_struct_props() {
    let code = "pub(crate) struct Props {\n    title: String,\n}";
    let result = inject_serde_derive(code);
    assert!(result.contains("#[derive(serde::Serialize)]"));
  }

  #[test]
  fn escape_rust_raw_str_handles_plain_template() {
    let s = escape_rust_raw_str("<h1>{{ title }}</h1>");
    assert!(s.starts_with("r#\""));
    assert!(s.ends_with("\"#"));
    assert!(s.contains("{{ title }}"));
  }

  #[test]
  fn generate_route_embeds_client_runtime() {
    use thebe_parser::SfcBlocks;

    let blocks = SfcBlocks {
      script_setup: Some(
        "#[thebe::get]\npub fn handler() -> Props { Props { counter: 0 } }".to_owned(),
      ),
      script_ts: Some(
        "let props = getProps<Props>();\nfunction inc() { props.counter += 1; }".to_owned(),
      ),
      template: "<span>{{ counter }}</span>".to_owned(),
      ..SfcBlocks::default()
    };

    let src = generate_route(&blocks, "/").unwrap();

    // The generated source must contain both client consts.
    assert!(src.contains("__CLIENT_RUNTIME"), "runtime const missing");
    assert!(src.contains("__CLIENT_SCRIPT"), "user script const missing");

    // The format! call must reference both as named args.
    assert!(src.contains("runtime = __CLIENT_RUNTIME"));
    assert!(src.contains("user_script = __CLIENT_SCRIPT"));

    // The analyzer must have stripped the generic type parameter.
    assert!(
      !src.contains("getProps<Props>()"),
      "TS generics not stripped"
    );
    assert!(src.contains("getProps()"), "stripped call missing");

    // Registration call must be appended.
    assert!(src.contains("__thebe_register(\"inc\", inc)"));
  }

  #[test]
  fn generate_route_uses_declared_http_method_and_forwarded_extractors() {
    use thebe_parser::SfcBlocks;

    let blocks = SfcBlocks {
            script_setup: Some(
                "struct Props { slug: String }\n\n#[thebe::post]\npub async fn create(\n    Path(slug): Path<String>,\n    State(state): State<AppState>,\n) -> Props {\n    let _ = state;\n    Props { slug }\n}"
                    .to_owned(),
            ),
            template: "<p>{{ slug }}</p>".to_owned(),
            ..SfcBlocks::default()
        };

    let src = generate_route(&blocks, "/blog/:slug").unwrap();

    assert!(src.contains(
      "async fn __thebe_render_handler(__thebe_arg0: Path<String>, __thebe_arg1: State<AppState>)"
    ));
    assert!(src.contains("let __props = create(__thebe_arg0, __thebe_arg1).await;"));
    assert!(src.contains("axum::routing::post(__thebe_render_handler)"));
  }

  #[test]
  fn generate_routes_file_uses_path_attributes_for_nested_routes() {
    let source = generate_routes_file(&[
      RouteEntry {
        mod_name: "route__index".to_owned(),
        source_path: "routes/index.rs".to_owned(),
      },
      RouteEntry {
        mod_name: "route__blog__dyn_slug".to_owned(),
        source_path: "routes/blog/[slug].rs".to_owned(),
      },
    ]);

    assert!(source.contains("#[path = \"routes/index.rs\"]"));
    assert!(source.contains("mod route__index;"));
    assert!(source.contains("#[path = \"routes/blog/[slug].rs\"]"));
    assert!(source.contains("mod route__blog__dyn_slug;"));
    assert!(source.contains(".merge(route__blog__dyn_slug::router())"));
    assert!(source.contains("fn __thebe_router()"));
    assert!(!source.contains("async fn main()"));
  }
}
