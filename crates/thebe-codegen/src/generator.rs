use crate::{error::CodegenError, template};
use std::fmt::Write as _;
use thebe_parser::SfcBlocks;

/// The thebe-client runtime JS, compiled into the binary at build time.
///
/// Every generated route embeds this verbatim into the served HTML so no
/// external CDN or npm install is required during `thebe dev`.
const THEBE_CLIENT_RUNTIME: &str = include_str!("../../../packages/thebe-client/runtime.js");

const APP_HTML_TITLE_PLACEHOLDER: &str = "%thebe.title%";
const APP_HTML_HEAD_PLACEHOLDER: &str = "%thebe.head%";
const APP_HTML_BODY_PLACEHOLDER: &str = "%thebe.body%";
const DEFAULT_APP_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  %thebe.head%
</head>
<body>
  %thebe.body%
</body>
</html>"#;

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
  app_html: &'a str,
  head_template: &'a str,
  template: &'a str,
  title_template: &'a str,
  runtime: &'a str,
  client_script: &'a str,
  style: &'a str,
  route_path: &'a str,
  /// Pre-processed layout template, or `None` when no layout wraps this route.
  layout_template: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct WrapperSource<'a> {
  params: &'a str,
  call: &'a str,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct HeadFragments {
  title_template: Option<String>,
  html_template: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessedLayout {
  template: String,
  style: String,
  head: HeadFragments,
}

/// Metadata the CLI provides about a discovered route file.
pub struct RouteEntry {
  /// The Rust module name (e.g. `"index"`, `"about"`).
  pub mod_name: String,
  /// Path to the generated route module relative to `src/main.rs`.
  pub source_path: String,
}

/// Return the built-in app shell used when a project does not provide `app.html`.
#[must_use]
pub fn default_app_html() -> &'static str {
  DEFAULT_APP_HTML
}

/// Validate that `app.html` contains the required Thebe placeholders.
///
/// A valid shell must contain exactly one `%thebe.head%` placeholder and
/// exactly one `%thebe.body%` placeholder.
///
/// # Errors
///
/// Returns [`CodegenError::InvalidAppHtml`] when a placeholder is missing or
/// duplicated.
pub fn validate_app_html(app_html: &str) -> Result<(), CodegenError> {
  validate_app_html_placeholder(app_html, APP_HTML_HEAD_PLACEHOLDER)?;
  validate_app_html_placeholder(app_html, APP_HTML_BODY_PLACEHOLDER)?;
  validate_optional_app_html_placeholder(app_html, APP_HTML_TITLE_PLACEHOLDER)
}

fn validate_app_html_placeholder(app_html: &str, placeholder: &str) -> Result<(), CodegenError> {
  let count = app_html.match_indices(placeholder).count();
  match count {
    1 => Ok(()),
    0 => Err(CodegenError::InvalidAppHtml(format!(
      "missing `{placeholder}` placeholder"
    ))),
    _ => Err(CodegenError::InvalidAppHtml(format!(
      "`{placeholder}` must appear exactly once"
    ))),
  }
}

fn validate_optional_app_html_placeholder(
  app_html: &str,
  placeholder: &str,
) -> Result<(), CodegenError> {
  let count = app_html.match_indices(placeholder).count();
  if count <= 1 {
    Ok(())
  } else {
    Err(CodegenError::InvalidAppHtml(format!(
      "`{placeholder}` must appear at most once"
    )))
  }
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
  app_html: &str,
) -> Result<String, CodegenError> {
  let setup = blocks
    .script_setup
    .as_deref()
    .ok_or(CodegenError::MissingScriptSetup)?;

  validate_app_html(app_html)?;

  // Validate the template before committing to codegen.
  template::parse_template(&blocks.template)?;

  let route_head = process_head_block(blocks.head.as_deref())?;

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

  let app_html_literal = escape_rust_raw_str(app_html);
  let style_literal = escape_rust_raw_str(&style);
  let template_literal = escape_rust_raw_str(&template_scoped);

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

  // Build the final style literal, optional layout template literal, and
  // merged head contribution. Route titles override layout titles.
  let (final_style_literal, layout_template_opt, merged_head) = match layout_processed {
    Some(processed_layout) => {
      let merged_style = if processed_layout.style.is_empty() {
        style.clone()
      } else if style.is_empty() {
        processed_layout.style.clone()
      } else {
        format!("{}\n{style}", processed_layout.style)
      };
      (
        escape_rust_raw_str(&merged_style),
        Some(escape_rust_raw_str(&processed_layout.template)),
        merge_head_fragments(&processed_layout.head, &route_head),
      )
    }
    None => (style_literal.clone(), None, route_head),
  };

  if merged_head.title_template.is_some() && !app_html.contains(APP_HTML_TITLE_PLACEHOLDER) {
    return Err(CodegenError::InvalidAppHtml(
      "route or layout `<head>` uses `<title>`, but app.html is missing `%thebe.title%`"
        .to_owned(),
    ));
  }

  let head_literal = escape_rust_raw_str(&merged_head.html_template);
  let title_literal = escape_rust_raw_str(
    merged_head
      .title_template
      .as_deref()
      .unwrap_or_default(),
  );
  let route_path_literal = escape_rust_raw_str(route_path);

  let literals = ModuleLiterals {
    app_html: &app_html_literal,
    head_template: &head_literal,
    template: &template_literal,
    title_template: &title_literal,
    runtime: &runtime_literal,
    client_script: &client_script_literal,
    style: &final_style_literal,
    route_path: &route_path_literal,
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
  write_support_fns(&mut source);
  if literals.layout_template.is_some() {
    write_render_handler_with_layout(&mut source, wrapper);
  } else {
    write_render_handler(&mut source, wrapper);
  }
  write_router_fn(&mut source, handler, route_path);
  source
}

fn write_module_constants(source: &mut String, literals: ModuleLiterals<'_>) {
  writeln!(source, "const __APP_HTML: &str = {};", literals.app_html).expect("infallible");
  writeln!(source, "const __HEAD_TEMPLATE: &str = {};", literals.head_template)
    .expect("infallible");
  writeln!(source, "const __TEMPLATE: &str = {};", literals.template).expect("infallible");
  writeln!(source, "const __TITLE_TEMPLATE: &str = {};", literals.title_template)
    .expect("infallible");
  writeln!(
    source,
    "const __CLIENT_RUNTIME: &str = {};",
    literals.runtime
  )
  .expect("infallible");
  writeln!(source, "const __STYLE: &str = {};", literals.style).expect("infallible");
  writeln!(source, "const __ROUTE_PATH: &str = {};", literals.route_path).expect("infallible");
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

fn write_support_fns(source: &mut String) {
  source.push_str(
    "type __ThebeResponse = Result<axum::response::Html<String>, axum::response::Response>;\n\n",
  );
  source.push_str("fn __thebe_render_app_html(title: &str, head: &str, body: &str) -> String {\n");
  source.push_str("    __APP_HTML\n");
  source.push_str("        .replace(\"%thebe.title%\", title)\n");
  source.push_str("        .replace(\"%thebe.head%\", head)\n");
  source.push_str("        .replace(\"%thebe.body%\", body)\n");
  source.push_str("}\n\n");
  source.push_str(
    "fn __thebe_render_fragment(\n        template_name: &str,\n        template_source: &str,\n        ctx: &serde_json::Value,\n        compile_stage: &str,\n        load_stage: &str,\n        render_stage: &str,\n    ) -> Result<String, axum::response::Response> {\n",
  );
  source.push_str("    use minijinja::Environment;\n\n");
  source.push_str("    let mut env = Environment::new();\n");
  source.push_str(
    "    env.add_template(template_name, template_source)\n        .map_err(|err| __thebe_internal_error(compile_stage, err))?;\n",
  );
  source.push_str(
    "    env.get_template(template_name)\n        .map_err(|err| __thebe_internal_error(load_stage, err))?\n        .render(ctx)\n        .map_err(|err| __thebe_internal_error(render_stage, err))\n",
  );
  source.push_str("}\n\n");
  source.push_str("fn __thebe_escape_html(input: &str) -> String {\n");
  source.push_str("    let mut escaped = String::with_capacity(input.len());\n");
  source.push_str("    for ch in input.chars() {\n");
  source.push_str("        match ch {\n");
  source.push_str("            '&' => escaped.push_str(\"&amp;\"),\n");
  source.push_str("            '<' => escaped.push_str(\"&lt;\"),\n");
  source.push_str("            '>' => escaped.push_str(\"&gt;\"),\n");
  source.push_str("            _ => escaped.push(ch),\n");
  source.push_str("        }\n");
  source.push_str("    }\n");
  source.push_str("    escaped\n");
  source.push_str("}\n\n");
  source.push_str("fn __thebe_internal_error(stage: &str, err: impl std::fmt::Display) -> axum::response::Response {\n");
  source.push_str("    use axum::response::IntoResponse;\n\n");
  source.push_str("    let route = __thebe_escape_html(__ROUTE_PATH);\n");
  source.push_str("    let stage = __thebe_escape_html(stage);\n");
  source.push_str("    let message = __thebe_escape_html(&err.to_string());\n\n");
  source.push_str("    (\n");
  source.push_str("        axum::http::StatusCode::INTERNAL_SERVER_ERROR,\n");
  source.push_str("        axum::response::Html(format!(\n");
  source.push_str(
    "            \"<!DOCTYPE html>\\n\\\n             <html>\\n\\\n             <head><title>Thebe Error</title></head>\\n\\\n             <body>\\n\\\n             <h1>500 - Thebe render error</h1>\\n\\\n             <p><strong>Route:</strong> {route}</p>\\n\\\n             <p><strong>Stage:</strong> {stage}</p>\\n\\\n             <pre>{message}</pre>\\n\\\n             </body>\\n\\\n             </html>\",\n",
  );
  source.push_str("            route = route,\n");
  source.push_str("            stage = stage,\n");
  source.push_str("            message = message,\n");
  source.push_str("        )),\n");
  source.push_str("    )\n");
  source.push_str("        .into_response()\n");
  source.push_str("}\n\n");
}

fn write_render_handler(source: &mut String, wrapper: WrapperSource<'_>) {
  if wrapper.params.is_empty() {
    source.push_str("async fn __thebe_render_handler() -> __ThebeResponse {\n");
  } else {
    writeln!(
      source,
      "async fn __thebe_render_handler({}) -> __ThebeResponse {{",
      wrapper.params
    )
    .expect("infallible");
  }
  source.push_str(wrapper.call);
  source.push_str(
    "    let __ctx = serde_json::to_value(&__props)\
         .map_err(|err| __thebe_internal_error(\"serialize props\", err))?;\n",
  );
  source.push_str(
    "    let __title = if __TITLE_TEMPLATE.is_empty() {\n        String::new()\n    } else {\n        __thebe_render_fragment(\n            \"__title\",\n            __TITLE_TEMPLATE,\n            &__ctx,\n            \"compile title template\",\n            \"load title template\",\n            \"render title template\",\n        )?\n    };\n",
  );
  source.push_str(
    "    let __head_html = if __HEAD_TEMPLATE.is_empty() {\n        String::new()\n    } else {\n        __thebe_render_fragment(\n            \"__head\",\n            __HEAD_TEMPLATE,\n            &__ctx,\n            \"compile head template\",\n            \"load head template\",\n            \"render head template\",\n        )?\n    };\n",
  );
  source.push_str(
    "    let __body = __thebe_render_fragment(\n        \"__page\",\n        __TEMPLATE,\n        &__ctx,\n        \"compile template\",\n        \"load template\",\n        \"render template\",\n    )?;\n",
  );
  write_html_assembly(source);
  source.push_str("}\n\n");
}

/// Emit the `let __props_json`, `let __html = format!(…)`, and `Html(__html)`
/// tail that is identical in both the plain and layout render handlers.
///
/// Preconditions (variables that must already be in scope in the generated code):
/// - `__ctx: serde_json::Value`
/// - `__head_html: String`
/// - `__title: String`
/// - `__body: String`
fn write_html_assembly(source: &mut String) {
  source.push_str("    let __props_json = __ctx.to_string();\n");
  source.push_str(
  r##"    let __head = if __STYLE.is_empty() {
    __head_html
  } else if __head_html.is_empty() {
    format!(r#"<style data-thebe-head="style">{style}</style>"#, style = __STYLE)
  } else {
    format!(r#"{head}
<style data-thebe-head="style">{style}</style>"#, head = __head_html, style = __STYLE)
  };
    let __body_with_scripts = format!(r#"{body}
<script id="__thebe_props" type="application/json">{props_json}</script>
<script>{runtime}</script>
<script>{user_script}</script>"#,
"##,
  );
  source.push_str("        body = __body,\n");
  source.push_str("        props_json = __props_json,\n");
  source.push_str("        runtime = __CLIENT_RUNTIME,\n");
  source.push_str("        user_script = __CLIENT_SCRIPT,\n");
  source.push_str("    );\n");
  source.push_str("    let __html = __thebe_render_app_html(&__title, &__head, &__body_with_scripts);\n");
  source.push_str("    Ok(axum::response::Html(__html))\n");
}

/// Like [`write_render_handler`] but renders the route body first, then wraps
/// it inside the layout template before assembling the HTML shell.
fn write_render_handler_with_layout(source: &mut String, wrapper: WrapperSource<'_>) {
  if wrapper.params.is_empty() {
    source.push_str("async fn __thebe_render_handler() -> __ThebeResponse {\n");
  } else {
    writeln!(
      source,
      "async fn __thebe_render_handler({}) -> __ThebeResponse {{",
      wrapper.params
    )
    .expect("infallible");
  }
  source.push_str(wrapper.call);
  source.push_str(
    "    let __ctx = serde_json::to_value(&__props)\
         .map_err(|err| __thebe_internal_error(\"serialize props\", err))?;\n",
  );
  source.push_str(
    "    let __title = if __TITLE_TEMPLATE.is_empty() {\n        String::new()\n    } else {\n        __thebe_render_fragment(\n            \"__title\",\n            __TITLE_TEMPLATE,\n            &__ctx,\n            \"compile title template\",\n            \"load title template\",\n            \"render title template\",\n        )?\n    };\n",
  );
  source.push_str(
    "    let __head_html = if __HEAD_TEMPLATE.is_empty() {\n        String::new()\n    } else {\n        __thebe_render_fragment(\n            \"__head\",\n            __HEAD_TEMPLATE,\n            &__ctx,\n            \"compile head template\",\n            \"load head template\",\n            \"render head template\",\n        )?\n    };\n",
  );
  source.push_str(
    "    let __route_body = __thebe_render_fragment(\n        \"__page\",\n        __TEMPLATE,\n        &__ctx,\n        \"compile template\",\n        \"load template\",\n        \"render template\",\n    )?;\n",
  );
  // Wrap the route body inside the layout template.
  source.push_str(
    "        let __layout_ctx = serde_json::json!({ \"__slot\": __route_body });\n",
  );
  source.push_str(
    "    let __body = __thebe_render_fragment(\n        \"__layout\",\n        __LAYOUT_TEMPLATE,\n        &__layout_ctx,\n        \"compile layout template\",\n        \"load layout template\",\n        \"render layout template\",\n    )?;\n",
  );
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

fn process_head_block(head: Option<&str>) -> Result<HeadFragments, CodegenError> {
  let Some(head) = head.map(str::trim).filter(|head| !head.is_empty()) else {
    return Ok(HeadFragments::default());
  };

  template::parse_template(head)?;

  let (title_template, html_without_title) = extract_title_template(head)?;
  let html_template = if html_without_title.trim().is_empty() {
    String::new()
  } else {
    thebe_css::add_html_attr(html_without_title.trim(), "data-thebe-head", "")
  };

  Ok(HeadFragments {
    title_template,
    html_template,
  })
}

fn merge_head_fragments(layout_head: &HeadFragments, route_head: &HeadFragments) -> HeadFragments {
  let title_template = route_head
    .title_template
    .clone()
    .or_else(|| layout_head.title_template.clone());

  let html_template = match (
    layout_head.html_template.trim(),
    route_head.html_template.trim(),
  ) {
    ("", "") => String::new(),
    ("", route_html) => route_html.to_owned(),
    (layout_html, "") => layout_html.to_owned(),
    (layout_html, route_html) => format!("{layout_html}\n{route_html}"),
  };

  HeadFragments {
    title_template,
    html_template,
  }
}

fn extract_title_template(head: &str) -> Result<(Option<String>, String), CodegenError> {
  let lowercase = head.to_ascii_lowercase();
  let title_count = lowercase.match_indices("<title").count();

  if title_count == 0 {
    return Ok((None, head.to_owned()));
  }
  if title_count > 1 {
    return Err(CodegenError::InvalidHead(
      "only one `<title>` tag is allowed per `<head>` block".to_owned(),
    ));
  }

  let title_start = lowercase.find("<title").ok_or_else(|| {
    CodegenError::InvalidHead("failed to find `<title>` tag start".to_owned())
  })?;
  let title_open_end_rel = head[title_start..].find('>').ok_or_else(|| {
    CodegenError::InvalidHead("`<title>` tag is missing a closing `>`".to_owned())
  })?;
  let title_open_end = title_start + title_open_end_rel;
  let title_close_start_rel = lowercase[title_open_end + 1..]
    .find("</title>")
    .ok_or_else(|| CodegenError::InvalidHead("`<title>` tag is missing `</title>`".to_owned()))?;
  let title_close_start = title_open_end + 1 + title_close_start_rel;
  let title_close_end = title_close_start + "</title>".len();

  let title_template = head[title_open_end + 1..title_close_start].trim().to_owned();
  let html_without_title = format!("{}{}", &head[..title_start], &head[title_close_end..]);

  Ok((Some(title_template), html_without_title))
}

/// Process a `_layout.trs` SFC into a render-ready structure for embedding in
/// a generated route module.
///
/// The layout template has `<slot />` replaced with `{{ __slot }}` and CSS
/// scoping applied.  Hydration markers are intentionally **not** injected
/// since layout templates are rendered server-side only.
fn process_layout(
  layout_blocks: &SfcBlocks,
  layout_scope_path: &str,
) -> Result<ProcessedLayout, CodegenError> {
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

  let head = process_head_block(layout_blocks.head.as_deref())?;

  Ok(ProcessedLayout {
    template: scoped_template,
    style: layout_style,
    head,
  })
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

    let src = generate_route(&blocks, "/", None, default_app_html()).unwrap();

    assert!(src.contains("const __APP_HTML"), "app shell const missing");
    // The generated source must contain both client consts.
    assert!(src.contains("__CLIENT_RUNTIME"), "runtime const missing");
    assert!(src.contains("__CLIENT_SCRIPT"), "user script const missing");
    assert!(src.contains("data-thebe-head=\"style\""));
    assert!(src.contains("fn __thebe_internal_error"));
    assert!(src.contains("fn __thebe_render_app_html"));
    assert!(src.contains("-> __ThebeResponse"));

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
  fn generate_route_supports_head_templates() {
    use thebe_parser::SfcBlocks;

    let blocks = SfcBlocks {
      script_setup: Some(
        "struct Props { title: String }\n\n#[thebe::get]\npub fn handler() -> Props { Props { title: \"Counter\".to_owned() } }".to_owned(),
      ),
      head: Some(
        r#"<title>{{ title }}</title>
<meta name="description" content="Counter page" />"#
          .to_owned(),
      ),
      template: "<h1>{{ title }}</h1>".to_owned(),
      ..SfcBlocks::default()
    };

    let app_html = "<html><head><title>%thebe.title%</title>%thebe.head%</head><body>%thebe.body%</body></html>";
    let src = generate_route(&blocks, "/", None, app_html).unwrap();

    assert!(src.contains("const __HEAD_TEMPLATE"));
    assert!(src.contains("const __TITLE_TEMPLATE"));
    assert!(src.contains("data-thebe-head=\"\""));
    assert!(src.contains("render title template"));
    assert!(src.contains("render head template"));
    assert!(src.contains("replace(\"%thebe.title%\", title)"));
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

    let src = generate_route(&blocks, "/blog/:slug", None, default_app_html()).unwrap();

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

  #[test]
  fn default_app_html_contains_required_placeholders() {
    let app_html = default_app_html();

    assert!(app_html.contains(APP_HTML_HEAD_PLACEHOLDER));
    assert!(app_html.contains(APP_HTML_BODY_PLACEHOLDER));
    assert!(validate_app_html(app_html).is_ok());
  }

  #[test]
  fn generate_route_rejects_invalid_app_html() {
    let blocks = SfcBlocks {
      script_setup: Some(
        "#[thebe::get]\npub fn handler() -> Props { Props { counter: 0 } }".to_owned(),
      ),
      template: "<span>{{ counter }}</span>".to_owned(),
      ..SfcBlocks::default()
    };

    let err = generate_route(&blocks, "/", None, "<html>%thebe.head%</html>").unwrap_err();

    assert!(matches!(err, CodegenError::InvalidAppHtml(_)));
  }

  #[test]
  fn generate_route_requires_title_placeholder_when_head_defines_title() {
    let blocks = SfcBlocks {
      script_setup: Some(
        "#[thebe::get]\npub fn handler() -> Props { Props { counter: 0 } }".to_owned(),
      ),
      head: Some("<title>Counter</title>".to_owned()),
      template: "<span>{{ counter }}</span>".to_owned(),
      ..SfcBlocks::default()
    };

    let err = generate_route(&blocks, "/", None, default_app_html()).unwrap_err();

    assert!(matches!(err, CodegenError::InvalidAppHtml(_)));
  }
}
