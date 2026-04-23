use crate::{error::CodegenError, template};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use quote::ToTokens;
use syn::{Fields, GenericArgument, Item, PathArguments, Type};
use thebe_parser::{SfcBlocks, SourceSpan};

/// The thebe-client runtime JS, compiled into the binary at build time.
///
/// Every generated route embeds this verbatim into the served HTML so no
/// external CDN or npm install is required during `thebe dev`.
const THEBE_CLIENT_RUNTIME: &str = include_str!("../scripts/runtime.js");

const APP_HTML_TITLE_PLACEHOLDER: &str = "%thebe.title%";
const APP_HTML_HEAD_PLACEHOLDER: &str = "%thebe.head%";
const APP_HTML_BODY_PLACEHOLDER: &str = "%thebe.body%";
const THEBE_ASSET_URL_PREFIX: &str = "/.thebe/assets";
const THEBE_DEV_ROUTE_ARTIFACTS_DIR: &str = ".thebe/dev/routes";
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
const THEBE_RUNTIME_SENTINEL: &str = "/* __thebe_runtime */";

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

#[derive(Debug, PartialEq, Eq)]
struct LocatedRouteHandler {
  handler: RouteHandler,
  span: SourceSpan,
}

/// Semantic handler metadata for a route discovered from `<script setup>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteHandlerInfo {
  /// HTTP method declared via `#[thebe::get]`, `#[thebe::post]`, and friends.
  pub method: &'static str,
  /// Rust function name of the route handler.
  pub name: String,
  /// Extractor and argument types preserved from the handler signature.
  pub param_types: Vec<String>,
  /// Whether the handler is declared `async`.
  pub is_async: bool,
  /// Concrete `State<T>` type required by the route handler, when present.
  pub state_type: Option<String>,
  /// Absolute source span of the handler declaration when the parser provided it.
  pub source_span: Option<SourceSpan>,
}

/// A `Props` field exposed to template navigation and completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSymbolDefinition {
  /// Full template-visible path, such as `profile.display_name`.
  pub path: String,
  /// Rust type that owns the field.
  pub owner_type: String,
  /// Rust field name at the end of `path`.
  pub field_name: String,
  /// Rust type rendered for the field.
  pub type_name: String,
  /// Span of the field identifier inside the source script block.
  pub span: SourceSpan,
}

#[derive(Clone)]
struct LocalNamedField {
  name: String,
  ty: Type,
  type_name: String,
  span: Option<SourceSpan>,
}

#[derive(Default, Clone)]
struct LocalTypeInfo {
  field_types: Vec<Type>,
  named_fields: Vec<LocalNamedField>,
}

#[derive(Clone, Copy)]
struct ModuleLiterals<'a> {
  app_html: &'a str,
  head_template: &'a str,
  template: &'a str,
  title_template: &'a str,
  dev_artifact_path: &'a str,
  runtime: &'a str,
  runtime_asset_url: &'a str,
  client_script: &'a str,
  client_script_asset_url: &'a str,
  dev_reload_script: &'a str,
  style: &'a str,
  style_asset_url: &'a str,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StaticAssetKind {
  Css,
  Js,
}

impl StaticAssetKind {
  fn content_type(self) -> &'static str {
    match self {
      Self::Css => "text/css; charset=utf-8",
      Self::Js => "text/javascript; charset=utf-8",
    }
  }

  fn extension(self) -> &'static str {
    match self {
      Self::Css => "css",
      Self::Js => "js",
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticAsset {
  pub contents: String,
  pub file_name: String,
  pub kind: StaticAssetKind,
  pub url_path: String,
}

impl StaticAsset {
  fn new(kind: StaticAssetKind, contents: String) -> Self {
    let hash = hash_asset_contents(kind, &contents);
    let file_name = format!("thebe-{hash}.{}", kind.extension());
    let url_path = format!("{THEBE_ASSET_URL_PREFIX}/{file_name}");

    Self {
      contents,
      file_name,
      kind,
      url_path,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedRoute {
  pub assets: Vec<StaticAsset>,
  pub dev_artifact: Option<DevRouteArtifact>,
  pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevRouteArtifact {
  pub head_template: String,
  pub layout_template: Option<String>,
  pub relative_path: String,
  pub style: String,
  pub template: String,
  pub title_template: String,
}

/// Metadata the CLI provides about a discovered route file.
pub struct RouteEntry {
  /// The Rust module name (e.g. `"index"`, `"about"`).
  pub mod_name: String,
  /// Path to the generated route module relative to the aggregate routes file.
  pub source_path: String,
  /// Concrete `State<T>` type required by the route handler, when present.
  pub state_type: Option<String>,
}

/// Return the built-in app shell used when a project does not provide `app.html`.
#[must_use]
pub fn default_app_html() -> &'static str {
  DEFAULT_APP_HTML
}

/// Return the concrete `State<T>` type required by a route, if any.
///
/// # Errors
///
/// Returns the same handler discovery errors as [`generate_route`].
pub fn route_state_type(blocks: &SfcBlocks) -> Result<Option<String>, CodegenError> {
  Ok(route_handler_info(blocks)?.state_type)
}

/// Return semantic handler metadata for a route.
///
/// # Errors
///
/// Returns the same handler discovery errors as [`generate_route`].
pub fn route_handler_info(blocks: &SfcBlocks) -> Result<RouteHandlerInfo, CodegenError> {
  let setup = blocks
    .script_setup
    .as_deref()
    .ok_or(CodegenError::MissingScriptSetup)?;
  let located = find_handler_with_span(setup)?;
  let handler = &located.handler;

  Ok(RouteHandlerInfo {
    method: handler.method.as_attr_name(),
    name: handler.name.clone(),
    param_types: handler.param_types.clone(),
    is_async: handler.is_async,
    state_type: handler_state_type(&handler).map(ToOwned::to_owned),
    source_span: blocks
      .script_setup_span
      .map(|script_setup_span| located.span.offset(script_setup_span.start)),
  })
}

/// Return completion-visible template symbols derived from route `Props`.
///
/// The returned paths are rooted at the template context itself, so a route
/// with `struct Props { title: String, profile: Profile }` produces
/// `title`, `profile`, and any nested dotted paths from local named structs
/// such as `profile.display_name`.
///
/// # Errors
///
/// Returns [`CodegenError::TypeBridge`] when `<script setup>` Rust cannot be
/// parsed for local type analysis.
pub fn route_template_symbol_definitions(
  blocks: &SfcBlocks,
) -> Result<Vec<TemplateSymbolDefinition>, CodegenError> {
  let Some(setup) = blocks.script_setup.as_deref() else {
    return Ok(Vec::new());
  };

  props_symbol_definitions(setup)
}

/// Return template-visible `Props` field definitions from a Rust script block.
///
/// Top-level `Props` fields are returned along with nested dotted paths that
/// resolve through locally defined named structs.
///
/// # Errors
///
/// Returns [`CodegenError::TypeBridge`] when the Rust source cannot be parsed
/// for local type analysis.
pub fn props_symbol_definitions(code: &str) -> Result<Vec<TemplateSymbolDefinition>, CodegenError> {
  let local_type_infos = collect_local_type_infos(code)?;
  let Some(props) = local_type_infos.get("Props") else {
    return Ok(Vec::new());
  };

  let mut definitions = Vec::new();
  for field in &props.named_fields {
    collect_template_symbol_definitions(
      "Props",
      field,
      &field.name,
      &local_type_infos,
      &mut definitions,
      &mut BTreeSet::new(),
    );
  }

  Ok(definitions)
}

pub fn route_template_symbols(blocks: &SfcBlocks) -> Result<Vec<String>, CodegenError> {
  Ok(
    route_template_symbol_definitions(blocks)?
      .into_iter()
      .map(|definition| definition.path)
      .collect(),
  )
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
/// 1. Injects `serde` and `ts-rs` derives on `Props` when needed.
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
  props_types_path: Option<&str>,
  components: &[ComponentMacro],
  minify: bool,
  dev_build_id: Option<&str>,
) -> Result<GeneratedRoute, CodegenError> {
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
  let client_script_ts = blocks
    .script_ts
    .as_deref()
    .filter(|ts| !ts.trim().is_empty());
  let props_types_path = match (client_script_ts, props_types_path) {
    (Some(_), Some(path)) => Some(path),
    (Some(_), None) => {
      return Err(CodegenError::TypeBridge(
        "client routes require a props type export path".to_owned(),
      ));
    }
    (None, _) => None,
  };
  let setup_with_serde = inject_props_derives(&setup_clean, props_types_path)?;

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

  // Expand component tags (<Card />) into Jinja {% call %} blocks, then
  // transform `:attr="key"` bindings, then inject hydration markers, then apply
  // CSS scoping — in that order so markers and scope attrs are not added to
  // Jinja control-flow tokens.
  let known_component_names: Vec<&str> = components.iter().map(|c| c.name.as_str()).collect();
  let template_expanded =
    template::expand_component_tags(&blocks.template, &known_component_names);
  let template_attr_bound = template::inject_attr_bindings(&template_expanded);
  let template_hydrated = template::inject_hydration_markers(&template_attr_bound);
  let template_scoped = thebe_css::add_scope_attrs(&template_hydrated, &scope);

  // Prepend compiled component macro sources (already scoped with component
  // scope IDs) before the route template body.
  let component_macros_source: String = components
    .iter()
    .map(|c| c.jinja_macro.as_str())
    .collect::<Vec<_>>()
    .join("\n");
  let template_with_macros = if component_macros_source.is_empty() {
    template_scoped
  } else {
    format!("{component_macros_source}\n{template_scoped}")
  };

  let app_html_literal = escape_rust_raw_str(app_html);

  // Merge component styles into the route style.
  let route_style = blocks
    .style
    .as_deref()
    .filter(|s| !s.trim().is_empty())
    .map(|s| thebe_css::process_style(s, &scope, minify))
    .transpose()
    .map_err(|e| CodegenError::CssError(e.to_string()))?
    .unwrap_or_default();
  let merged_component_styles: String = components
    .iter()
    .filter(|c| !c.style.is_empty())
    .map(|c| c.style.as_str())
    .collect::<Vec<_>>()
    .join("\n");
  let style = if merged_component_styles.is_empty() {
    route_style
  } else if route_style.is_empty() {
    merged_component_styles
  } else {
    format!("{merged_component_styles}\n{route_style}")
  };

  // Process the optional `<script lang="ts">` block.
  let client_js = client_script_ts
    .map(|ts| thebe_analyzer::analyze(ts, minify).map(|module| module.js))
    .transpose()?
    .unwrap_or_default();

  let runtime_str = if minify {
    format!(
      "{THEBE_RUNTIME_SENTINEL}\n{}",
      thebe_analyzer::minify_javascript(THEBE_CLIENT_RUNTIME)?
    )
  } else {
    THEBE_CLIENT_RUNTIME.to_string()
  };
  let client_script_str = client_js;

  // Process the optional layout.
  let layout_processed = layout
    .map(|(layout_blocks, layout_scope_path)| process_layout(layout_blocks, layout_scope_path, minify))
    .transpose()?;

  // Build the final style literal, optional layout template literal, and
  // merged head contribution. Route titles override layout titles.
  let (final_style, layout_template, merged_head) = match layout_processed {
    Some(processed_layout) => {
      let merged_style = if processed_layout.style.is_empty() {
        style.clone()
      } else if style.is_empty() {
        processed_layout.style.clone()
      } else {
        format!("{}\n{style}", processed_layout.style)
      };
      (
        merged_style,
        Some(processed_layout.template),
        merge_head_fragments(&processed_layout.head, &route_head),
      )
    }
    None => (style, None, route_head),
  };

  if merged_head.title_template.is_some() && !app_html.contains(APP_HTML_TITLE_PLACEHOLDER) {
    return Err(CodegenError::InvalidAppHtml(
      "route or layout `<head>` uses `<title>`, but app.html is missing `%thebe.title%`".to_owned(),
    ));
  }

  let route_path_literal = escape_rust_raw_str(route_path);
  let dev_artifact = dev_build_id.filter(|_| !minify).map(|_| DevRouteArtifact {
    head_template: merged_head.html_template.clone(),
    layout_template: layout_template.clone(),
    relative_path: dev_route_artifact_relative_path(route_path),
    style: final_style.clone(),
    template: template_with_macros.clone(),
    title_template: merged_head.title_template.clone().unwrap_or_default(),
  });

  let mut assets = Vec::new();
  let runtime_asset_url = if minify {
    let asset = StaticAsset::new(StaticAssetKind::Js, runtime_str.clone());
    let url_path = asset.url_path.clone();
    assets.push(asset);
    url_path
  } else {
    String::new()
  };
  let client_script_asset_url = if minify && !client_script_str.is_empty() {
    let asset = StaticAsset::new(StaticAssetKind::Js, client_script_str.clone());
    let url_path = asset.url_path.clone();
    assets.push(asset);
    url_path
  } else {
    String::new()
  };
  let style_asset_url = if minify && !final_style.is_empty() {
    let asset = StaticAsset::new(StaticAssetKind::Css, final_style.clone());
    let url_path = asset.url_path.clone();
    assets.push(asset);
    url_path
  } else {
    String::new()
  };

  let runtime_literal = escape_rust_raw_str(if minify { "" } else { &runtime_str });
  let client_script_literal =
    escape_rust_raw_str(if minify { "" } else { &client_script_str });
  let dev_artifact_path_literal = escape_rust_raw_str(
    dev_artifact
      .as_ref()
      .map_or("", |artifact| artifact.relative_path.as_str()),
  );
  let dev_reload_script_literal = escape_rust_raw_str(
    &dev_build_id
      .filter(|_| !minify)
      .map(|_| build_dev_reload_script())
      .unwrap_or_default(),
  );
  let final_style_literal = escape_rust_raw_str(if minify || dev_artifact.is_some() {
    ""
  } else {
    &final_style
  });
  let template_literal = escape_rust_raw_str(if dev_artifact.is_some() {
    ""
  } else {
    &template_with_macros
  });
  let head_literal = escape_rust_raw_str(if dev_artifact.is_some() {
    ""
  } else {
    &merged_head.html_template
  });
  let title_literal = escape_rust_raw_str(if dev_artifact.is_some() {
    ""
  } else {
    merged_head.title_template.as_deref().unwrap_or_default()
  });
  let runtime_asset_url_literal = escape_rust_raw_str(&runtime_asset_url);
  let client_script_asset_url_literal = escape_rust_raw_str(&client_script_asset_url);
  let style_asset_url_literal = escape_rust_raw_str(&style_asset_url);
  let layout_template_literal = layout_template.as_deref().map(|template| {
    escape_rust_raw_str(if dev_artifact.is_some() { "" } else { template })
  });

  let literals = ModuleLiterals {
    app_html: &app_html_literal,
    head_template: &head_literal,
    template: &template_literal,
    title_template: &title_literal,
    dev_artifact_path: &dev_artifact_path_literal,
    runtime: &runtime_literal,
    runtime_asset_url: &runtime_asset_url_literal,
    client_script: &client_script_literal,
    client_script_asset_url: &client_script_asset_url_literal,
    dev_reload_script: &dev_reload_script_literal,
    style: &final_style_literal,
    style_asset_url: &style_asset_url_literal,
    route_path: &route_path_literal,
    layout_template: layout_template_literal.as_deref(),
  };
  let wrapper = WrapperSource {
    params: &wrapper_params,
    call: &handler_call,
  };

  Ok(GeneratedRoute {
    assets,
    dev_artifact,
    source: build_route_module(
      &setup_with_serde,
      literals,
      wrapper,
      &handler,
      route_path,
      props_types_path.is_some(),
    ),
  })
}

fn build_route_module(
  setup_with_serde: &str,
  literals: ModuleLiterals<'_>,
  wrapper: WrapperSource<'_>,
  handler: &RouteHandler,
  route_path: &str,
  type_bridge_enabled: bool,
) -> String {
  let mut source = String::new();
  source.push_str("// AUTOGENERATED by thebe \u{2014} do not edit\n");
  source.push_str("#![allow(dead_code, private_interfaces)]\n");
  source.push_str(setup_with_serde);
  source.push_str("\n\n");
  write_module_constants(&mut source, literals);
  write_support_fns(&mut source, type_bridge_enabled);
  if literals.layout_template.is_some() {
    write_render_handler_with_layout(&mut source, wrapper);
  } else {
    write_render_handler(&mut source, wrapper);
  }
  write_router_fn(&mut source, handler, route_path, type_bridge_enabled);
  source
}

fn write_module_constants(source: &mut String, literals: ModuleLiterals<'_>) {
  writeln!(source, "const __APP_HTML: &str = {};", literals.app_html).expect("infallible");
  writeln!(
    source,
    "const __HEAD_TEMPLATE: &str = {};",
    literals.head_template
  )
  .expect("infallible");
  writeln!(source, "const __TEMPLATE: &str = {};", literals.template).expect("infallible");
  writeln!(
    source,
    "const __TITLE_TEMPLATE: &str = {};",
    literals.title_template
  )
  .expect("infallible");
  writeln!(
    source,
    "const __THEBE_DEV_ROUTE_ARTIFACT_PATH: &str = {};",
    literals.dev_artifact_path
  )
  .expect("infallible");
  writeln!(
    source,
    "const __CLIENT_RUNTIME: &str = {};",
    literals.runtime
  )
  .expect("infallible");
  writeln!(
    source,
    "const __RUNTIME_ASSET_URL: &str = {};",
    literals.runtime_asset_url
  )
  .expect("infallible");
  writeln!(source, "const __STYLE: &str = {};", literals.style).expect("infallible");
  writeln!(
    source,
    "const __STYLE_ASSET_URL: &str = {};",
    literals.style_asset_url
  )
  .expect("infallible");
  writeln!(
    source,
    "const __ROUTE_PATH: &str = {};",
    literals.route_path
  )
  .expect("infallible");
  writeln!(
    source,
    "const __CLIENT_SCRIPT: &str = {};",
    literals.client_script
  )
  .expect("infallible");
  writeln!(
    source,
    "const __CLIENT_SCRIPT_ASSET_URL: &str = {};",
    literals.client_script_asset_url
  )
  .expect("infallible");
  writeln!(
    source,
    "const __DEV_RELOAD_SCRIPT: &str = {};",
    literals.dev_reload_script
  )
  .expect("infallible");
  if let Some(layout_tmpl) = literals.layout_template {
    writeln!(source, "const __LAYOUT_TEMPLATE: &str = {layout_tmpl};").expect("infallible");
  }
  source.push('\n');
}

fn write_support_fns(source: &mut String, type_bridge_enabled: bool) {
  source.push_str(
    "type __ThebeResponse = Result<axum::response::Html<String>, axum::response::Response>;\n\n",
  );
  source.push_str("#[derive(serde::Deserialize)]\n");
  source.push_str("struct __ThebeDevRouteArtifact {\n");
  source.push_str("    head_template: String,\n");
  source.push_str("    layout_template: Option<String>,\n");
  source.push_str("    style: String,\n");
  source.push_str("    template: String,\n");
  source.push_str("    title_template: String,\n");
  source.push_str("}\n\n");
  source.push_str("fn __thebe_load_dev_route_artifact() -> Result<Option<__ThebeDevRouteArtifact>, axum::response::Response> {\n");
  source.push_str("    if __THEBE_DEV_ROUTE_ARTIFACT_PATH.is_empty() {\n");
  source.push_str("        return Ok(None);\n");
  source.push_str("    }\n\n");
  source.push_str("    let source = thebe_runtime::hotpatch::load_text_artifact(__THEBE_DEV_ROUTE_ARTIFACT_PATH)\n");
  source.push_str("        .map_err(|err| __thebe_internal_error(\"load dev route artifact\", err))?;\n");
  source.push_str("    serde_json::from_str(&source)\n");
  source.push_str("        .map(Some)\n");
  source.push_str("        .map_err(|err| __thebe_internal_error(\"parse dev route artifact\", err))\n");
  source.push_str("}\n\n");
  source.push_str("fn __thebe_render_app_html(title: &str, head: &str, body: &str) -> String {\n");
  source.push_str("    __APP_HTML\n");
  source.push_str("        .replace(\"%thebe.title%\", title)\n");
  source.push_str("        .replace(\"%thebe.head%\", head)\n");
  source.push_str("        .replace(\"%thebe.body%\", body)\n");
  source.push_str("}\n\n");
  source.push_str("fn __thebe_dev_reload_script() -> String {\n");
  source.push_str("    if __DEV_RELOAD_SCRIPT.is_empty() {\n");
  source.push_str("        return String::new();\n");
  source.push_str("    }\n\n");
  source.push_str("    let Some(events_url) = thebe_runtime::hotpatch::browser_events_url_from_env() else {\n");
  source.push_str("        return String::new();\n");
  source.push_str("    };\n\n");
  source.push_str("    __DEV_RELOAD_SCRIPT.replace(\"%thebe.events_url%\", &events_url)\n");
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

  if type_bridge_enabled {
    source.push_str("fn __thebe_export_types() {\n");
    source.push_str("    static __THEBE_EXPORT_TYPES: std::sync::Once = std::sync::Once::new();\n");
    source.push_str("    __THEBE_EXPORT_TYPES.call_once(|| {\n");
    source.push_str("        let cfg = ts_rs::Config::new()\n");
    source.push_str("            .with_out_dir(\".thebe/types\")\n");
    source.push_str("            .with_large_int(\"number\");\n");
    source.push_str("        if let Err(err) = <Props as ts_rs::TS>::export_all(&cfg) {\n");
    source.push_str("            eprintln!(\"thebe: failed to export TS bindings for {}: {err}\", __ROUTE_PATH);\n");
    source.push_str("        }\n");
    source.push_str("    });\n");
    source.push_str("}\n\n");
  }
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
  source.push_str("    let __dev_artifact = __thebe_load_dev_route_artifact()?;\n");
  source.push_str("    let __title_template = __dev_artifact.as_ref().map_or(__TITLE_TEMPLATE, |artifact| artifact.title_template.as_str());\n");
  source.push_str("    let __head_template = __dev_artifact.as_ref().map_or(__HEAD_TEMPLATE, |artifact| artifact.head_template.as_str());\n");
  source.push_str("    let __template = __dev_artifact.as_ref().map_or(__TEMPLATE, |artifact| artifact.template.as_str());\n");
  source.push_str("    let __style = __dev_artifact.as_ref().map_or(__STYLE, |artifact| artifact.style.as_str());\n");
  source.push_str(
    "    let __ctx = serde_json::to_value(&__props)\
         .map_err(|err| __thebe_internal_error(\"serialize props\", err))?;\n",
  );
  source.push_str(
    "    let __title = if __title_template.is_empty() {\n        String::new()\n    } else {\n        __thebe_render_fragment(\n            \"__title\",\n            __title_template,\n            &__ctx,\n            \"compile title template\",\n            \"load title template\",\n            \"render title template\",\n        )?\n    };\n",
  );
  source.push_str(
    "    let __head_html = if __head_template.is_empty() {\n        String::new()\n    } else {\n        __thebe_render_fragment(\n            \"__head\",\n            __head_template,\n            &__ctx,\n            \"compile head template\",\n            \"load head template\",\n            \"render head template\",\n        )?\n    };\n",
  );
  source.push_str(
    "    let __body = __thebe_render_fragment(\n        \"__page\",\n        __template,\n        &__ctx,\n        \"compile template\",\n        \"load template\",\n        \"render template\",\n    )?;\n",
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
    "    let __dev_reload_script = __thebe_dev_reload_script();\n    let __dev_reload = if __dev_reload_script.is_empty() {\n        String::new()\n    } else {\n        format!(r#\"\n<script data-thebe-dev-reload>{reload_script}</script>\"#, reload_script = __dev_reload_script)\n    };\n",
  );
  source.push_str(
    r##"    let __head = if __STYLE_ASSET_URL.is_empty() {
    if __style.is_empty() {
      __head_html
    } else if __head_html.is_empty() {
      format!(r#"<style data-thebe-head="style">{style}</style>"#, style = __style)
    } else {
      format!(r#"{head}
<style data-thebe-head="style">{style}</style>"#, head = __head_html, style = __style)
    }
  } else if __head_html.is_empty() {
    format!(r#"<link rel="stylesheet" href="{href}" data-thebe-head="style" />"#, href = __STYLE_ASSET_URL)
  } else {
    format!(r#"{head}
<link rel="stylesheet" href="{href}" data-thebe-head="style" />"#, head = __head_html, href = __STYLE_ASSET_URL)
  };
    let __body_with_scripts = if __RUNTIME_ASSET_URL.is_empty() {
      format!(r#"{body}
<script id="__thebe_props" type="application/json">{props_json}</script>
{dev_reload}
<script>{runtime}</script>
<script>{user_script}</script>"#,
"##,
  );
  source.push_str("        body = __body,\n");
  source.push_str("        props_json = __props_json,\n");
  source.push_str("        dev_reload = __dev_reload,\n");
  source.push_str("        runtime = __CLIENT_RUNTIME,\n");
  source.push_str("        user_script = __CLIENT_SCRIPT,\n");
  source.push_str(
    r##"    )
    } else if __CLIENT_SCRIPT_ASSET_URL.is_empty() {
      format!(r#"{body}
<script id="__thebe_props" type="application/json">{props_json}</script>
{dev_reload}
<script data-thebe-runtime src="{runtime_src}"></script>"#,
        body = __body,
        props_json = __props_json,
        dev_reload = __dev_reload,
        runtime_src = __RUNTIME_ASSET_URL,
      )
    } else {
      format!(r#"{body}
<script id="__thebe_props" type="application/json">{props_json}</script>
{dev_reload}
<script data-thebe-runtime src="{runtime_src}"></script>
<script src="{user_script_src}"></script>"#,
        body = __body,
        props_json = __props_json,
        dev_reload = __dev_reload,
        runtime_src = __RUNTIME_ASSET_URL,
        user_script_src = __CLIENT_SCRIPT_ASSET_URL,
      )
    };
"##,
  );
  source.push_str(
    "    let __html = __thebe_render_app_html(&__title, &__head, &__body_with_scripts);\n",
  );
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
  source.push_str("    let __dev_artifact = __thebe_load_dev_route_artifact()?;\n");
  source.push_str("    let __title_template = __dev_artifact.as_ref().map_or(__TITLE_TEMPLATE, |artifact| artifact.title_template.as_str());\n");
  source.push_str("    let __head_template = __dev_artifact.as_ref().map_or(__HEAD_TEMPLATE, |artifact| artifact.head_template.as_str());\n");
  source.push_str("    let __template = __dev_artifact.as_ref().map_or(__TEMPLATE, |artifact| artifact.template.as_str());\n");
  source.push_str("    let __style = __dev_artifact.as_ref().map_or(__STYLE, |artifact| artifact.style.as_str());\n");
  source.push_str("    let __layout_template = __dev_artifact.as_ref().and_then(|artifact| artifact.layout_template.as_deref()).unwrap_or(__LAYOUT_TEMPLATE);\n");
  source.push_str(
    "    let __ctx = serde_json::to_value(&__props)\
         .map_err(|err| __thebe_internal_error(\"serialize props\", err))?;\n",
  );
  source.push_str(
    "    let __title = if __title_template.is_empty() {\n        String::new()\n    } else {\n        __thebe_render_fragment(\n            \"__title\",\n            __title_template,\n            &__ctx,\n            \"compile title template\",\n            \"load title template\",\n            \"render title template\",\n        )?\n    };\n",
  );
  source.push_str(
    "    let __head_html = if __head_template.is_empty() {\n        String::new()\n    } else {\n        __thebe_render_fragment(\n            \"__head\",\n            __head_template,\n            &__ctx,\n            \"compile head template\",\n            \"load head template\",\n            \"render head template\",\n        )?\n    };\n",
  );
  source.push_str(
    "    let __route_body = __thebe_render_fragment(\n        \"__page\",\n        __template,\n        &__ctx,\n        \"compile template\",\n        \"load template\",\n        \"render template\",\n    )?;\n",
  );
  // Wrap the route body inside the layout template.
  source.push_str("        let __layout_ctx = serde_json::json!({ \"__slot\": __route_body });\n");
  source.push_str(
    "    let __body = __thebe_render_fragment(\n        \"__layout\",\n        __layout_template,\n        &__layout_ctx,\n        \"compile layout template\",\n        \"load layout template\",\n        \"render layout template\",\n    )?;\n",
  );
  write_html_assembly(source);
  source.push_str("}\n\n");
}

fn write_router_fn(
  source: &mut String,
  handler: &RouteHandler,
  route_path: &str,
  type_bridge_enabled: bool,
) {
  if let Some(state_type) = handler_state_type(handler) {
    writeln!(source, "pub fn router() -> axum::Router<{state_type}> {{").expect("infallible");
    if type_bridge_enabled {
      source.push_str("    __thebe_export_types();\n");
    }
    writeln!(source, "    axum::Router::<{state_type}>::new().route(").expect("infallible");
  } else {
    source.push_str("pub fn router<S>() -> axum::Router<S>\n");
    source.push_str("where\n");
    source.push_str("    S: Clone + Send + Sync + 'static,\n");
    source.push_str("{\n");
    if type_bridge_enabled {
      source.push_str("    __thebe_export_types();\n");
    }
    source.push_str("    axum::Router::<S>::new().route(\n");
  }
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

fn handler_state_type(handler: &RouteHandler) -> Option<&str> {
  handler
    .param_types
    .iter()
    .find_map(|param_type| extract_state_type(param_type))
}

fn extract_state_type(param_type: &str) -> Option<&str> {
  let trimmed = param_type.trim();
  let open_angle = trimmed.find('<')?;
  let prefix = trimmed[..open_angle].trim();
  if prefix.rsplit("::").next()? != "State" {
    return None;
  }

  let close_angle = trimmed.rfind('>')?;
  if close_angle <= open_angle + 1 {
    return None;
  }

  Some(trimmed[open_angle + 1..close_angle].trim())
}
/// Generate the aggregate routes module included by `src/main.rs`.
///
/// The generated file declares all route modules and exposes `thebe_routes()`.
/// Stateful routes must all agree on a single `State<T>` type so the helper can
/// return one concrete router type.
///
/// Returns [`CodegenError::MixedRouteStateTypes`] when routes require more than
/// one concrete state type.

/// A compiled component ready to be embedded in route template rendering.
///
/// The component's Jinja macro body is inlined into any route template that
/// uses the component so that all rendering stays within a single
/// `minijinja::Environment` template with no cross-template imports.
#[derive(Debug, Clone)]
pub struct ComponentMacro {
  /// PascalCase component name, e.g. `Card`.
  pub name: String,
  /// Jinja macro identifier used in `{% call %}` expressions, e.g. `__comp_card`.
  pub macro_name: String,
  /// Full `{% macro __comp_xxx(props) %}...{% endmacro %}` source.
  ///
  /// The component's HTML already has its own CSS scope attributes injected;
  /// `<slot />` has been replaced with `{{ caller() }}`.
  pub jinja_macro: String,
  /// Processed and scoped CSS for this component (empty if no `<style>` block).
  pub style: String,
}

/// Compile a component `.trs` file into a [`ComponentMacro`] ready for use in
/// route template rendering.
///
/// Unlike routes, components do not produce an Axum handler. The returned value
/// carries:
/// - A Jinja `{% macro %}` whose body is the scoped, slot-expanded template.
/// - Processed CSS scoped to the component via its own `scope_id`.
///
/// # Errors
///
/// Returns [`CodegenError`] when the template contains invalid bindings or the
/// `<style>` block cannot be processed.
pub fn generate_component(
  blocks: &SfcBlocks,
  component_name: &str,
  minify: bool,
) -> Result<ComponentMacro, CodegenError> {
  // Validate template bindings; components use `{{ props.field }}` (dotted paths).
  crate::template::parse_template(&blocks.template)?;

  let scope = thebe_css::scope_id(component_name);
  let macro_name = format!("__comp_{}", component_name.to_lowercase());

  // Replace <slot /> with {{ caller() }}, transform `:attr` bindings, then
  // apply CSS scope attributes.
  let template_slot_expanded = crate::template::expand_slot(&blocks.template);
  let template_attr_bound = crate::template::inject_attr_bindings(&template_slot_expanded);
  let template_scoped = thebe_css::add_scope_attrs(&template_attr_bound, &scope);

  let jinja_macro = format!(
    "{{% macro {macro_name}(props) %}}\n{template_scoped}\n{{% endmacro %}}",
  );

  let style = if let Some(css) = &blocks.style {
    thebe_css::process_style(css, &scope, minify).map_err(|e| CodegenError::CssError(e.to_string()))?
  } else {
    String::new()
  };

  Ok(ComponentMacro {
    name: component_name.to_owned(),
    macro_name,
    jinja_macro,
    style,
  })
}

pub fn generate_routes_file(
  routes: &[RouteEntry],
  assets: &[StaticAsset],
  _dev_build_id: Option<&str>,
) -> Result<String, CodegenError> {
  let mut source = String::new();
  source.push_str("// AUTOGENERATED by thebe \u{2014} do not edit\n");
  for route in routes {
    writeln!(source, "#[path = \"{}\"]", route.source_path).expect("infallible");
    writeln!(source, "mod {};", route.mod_name).expect("infallible");
  }
  source.push('\n');
  write_asset_support(&mut source, assets);
  match shared_route_state_type(routes)? {
    Some(state_type) => {
      writeln!(
        source,
        "pub(crate) fn thebe_routes() -> axum::Router<{state_type}> {{"
      )
      .expect("infallible");
      writeln!(source, "    axum::Router::<{state_type}>::new()").expect("infallible");
      write_asset_routes(&mut source, assets);
      for route in routes {
        if route.state_type.is_some() {
          writeln!(source, "        .merge({}::router())", route.mod_name).expect("infallible");
        } else {
          writeln!(
            source,
            "        .merge({}::router::<{state_type}>())",
            route.mod_name
          )
          .expect("infallible");
        }
      }
    }
    None => {
      source.push_str("pub(crate) fn thebe_routes<S>() -> axum::Router<S>\n");
      source.push_str("where\n");
      source.push_str("    S: Clone + Send + Sync + 'static,\n");
      source.push_str("{\n");
      source.push_str("    axum::Router::<S>::new()\n");
      write_asset_routes(&mut source, assets);
      for route in routes {
        writeln!(source, "        .merge({}::router::<S>())", route.mod_name).expect("infallible");
      }
    }
  }
  source.push_str("}\n");

  Ok(source)
}

fn write_asset_support(source: &mut String, assets: &[StaticAsset]) {
  let mut sorted_assets = assets.iter().collect::<Vec<_>>();
  sorted_assets.sort_by(|left, right| left.file_name.cmp(&right.file_name));

  for asset in &sorted_assets {
    let const_name = asset_const_name(asset);
    writeln!(
      source,
      "const {const_name}: &str = include_str!(\"../assets/{}\");",
      asset.file_name
    )
    .expect("infallible");
  }
  if !sorted_assets.is_empty() {
    source.push('\n');
  }

  for asset in &sorted_assets {
    let const_name = asset_const_name(asset);
    let fn_name = asset_fn_name(asset);
    writeln!(
      source,
      "async fn {fn_name}() -> impl axum::response::IntoResponse {{"
    )
    .expect("infallible");
    source.push_str("    (\n");
    writeln!(
      source,
      "        [(axum::http::header::CONTENT_TYPE, \"{}\"), (axum::http::header::CACHE_CONTROL, \"public, max-age=31536000, immutable\")],",
      asset.kind.content_type()
    )
    .expect("infallible");
    writeln!(source, "        {const_name},").expect("infallible");
    source.push_str("    )\n");
    source.push_str("}\n\n");
  }
}

fn write_asset_routes(source: &mut String, assets: &[StaticAsset]) {
  let mut sorted_assets = assets.iter().collect::<Vec<_>>();
  sorted_assets.sort_by(|left, right| left.file_name.cmp(&right.file_name));

  for asset in sorted_assets {
    let fn_name = asset_fn_name(asset);
    writeln!(
      source,
      "        .route(\"{}\", axum::routing::get({fn_name}))",
      asset.url_path
    )
    .expect("infallible");
  }
}

fn build_dev_reload_script() -> String {
  format!(
    r#"(() => {{
  if (typeof window === "undefined" || typeof window.EventSource !== "function") {{
    return;
  }}

  const reloadSource = new window.EventSource("%thebe.events_url%");

  const escapeRegex = (value) =>
    value.replace(/[.*+?^${{}}()|[\]\\]/g, "\\$&");

  const matchesRoutePattern = (pattern, pathname) => {{
    if (!pattern) {{
      return true;
    }}

    const source = escapeRegex(pattern).replace(/\\\{{[^/]+\\\}}/g, "[^/]+");
    return new window.RegExp("^" + source + "$", "u").test(pathname);
  }};

  const parsePayload = (event) => {{
    try {{
      return JSON.parse(event.data || "{{}}");
    }} catch (_) {{
      return {{}};
    }}
  }};

  reloadSource.addEventListener("style", (event) => {{
    const payload = parsePayload(event);
    if (!matchesRoutePattern(payload.routePattern || null, window.location.pathname)) {{
      return;
    }}

    if (payload.css && typeof window.__thebe_dev_apply_style === "function") {{
      window.__thebe_dev_apply_style(payload.css);
    }}
  }});

  reloadSource.addEventListener("template", (event) => {{
    const payload = parsePayload(event);
    if (!matchesRoutePattern(payload.routePattern || null, window.location.pathname)) {{
      return;
    }}

    if (typeof window.__thebe_dev_refresh === "function") {{
      window.__thebe_dev_refresh();
    }} else {{
      window.location.reload();
    }}
  }});

  reloadSource.addEventListener("reload", () => {{
    reloadSource.close();
    window.location.reload();
  }});

  window.addEventListener("beforeunload", () => reloadSource.close(), {{ once: true }});
}})();"#,
  )
}

fn asset_const_name(asset: &StaticAsset) -> String {
  format!(
    "__THEBE_ASSET_{}",
    asset_ident_fragment(&asset.file_name).to_ascii_uppercase()
  )
}

fn asset_fn_name(asset: &StaticAsset) -> String {
  format!("__thebe_asset_{}", asset_ident_fragment(&asset.file_name))
}

fn asset_ident_fragment(file_name: &str) -> String {
  file_name
    .chars()
    .map(|ch| {
      if ch.is_ascii_alphanumeric() {
        ch.to_ascii_lowercase()
      } else {
        '_'
      }
    })
    .collect()
}

fn hash_asset_contents(kind: StaticAssetKind, contents: &str) -> String {
  hash_text(kind.extension(), contents)
}

#[must_use]
pub fn dev_route_artifact_path(route_path: &str) -> String {
  dev_route_artifact_relative_path(route_path)
}

fn dev_route_artifact_relative_path(route_path: &str) -> String {
  format!(
    "{THEBE_DEV_ROUTE_ARTIFACTS_DIR}/thebe-route-{}.json",
    hash_text("route", route_path)
  )
}

fn hash_text(seed: &str, contents: &str) -> String {
  const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
  const FNV_PRIME: u64 = 1_099_511_628_211;

  let mut hash = FNV_OFFSET;
  for byte in seed.bytes().chain([0]).chain(contents.bytes()) {
    hash ^= u64::from(byte);
    hash = hash.wrapping_mul(FNV_PRIME);
  }
  format!("{hash:016x}")
}

fn shared_route_state_type(routes: &[RouteEntry]) -> Result<Option<&str>, CodegenError> {
  let mut state_types = routes
    .iter()
    .filter_map(|route| route.state_type.as_deref())
    .collect::<BTreeSet<_>>()
    .into_iter();

  let Some(first) = state_types.next() else {
    return Ok(None);
  };

  let rest = state_types.collect::<Vec<_>>();
  if rest.is_empty() {
    return Ok(Some(first));
  }

  let mut all = Vec::with_capacity(rest.len() + 1);
  all.push(first);
  all.extend(rest);
  Err(CodegenError::MixedRouteStateTypes(all.join(", ")))
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

  let title_start = lowercase
    .find("<title")
    .ok_or_else(|| CodegenError::InvalidHead("failed to find `<title>` tag start".to_owned()))?;
  let title_open_end_rel = head[title_start..].find('>').ok_or_else(|| {
    CodegenError::InvalidHead("`<title>` tag is missing a closing `>`".to_owned())
  })?;
  let title_open_end = title_start + title_open_end_rel;
  let title_close_start_rel = lowercase[title_open_end + 1..]
    .find("</title>")
    .ok_or_else(|| CodegenError::InvalidHead("`<title>` tag is missing `</title>`".to_owned()))?;
  let title_close_start = title_open_end + 1 + title_close_start_rel;
  let title_close_end = title_close_start + "</title>".len();

  let title_template = head[title_open_end + 1..title_close_start]
    .trim()
    .to_owned();
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
  minify: bool,
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
    .map(|s| thebe_css::process_style(s, &layout_scope, minify))
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ExistingTypeAttrs {
  has_serialize: bool,
  has_ts_derive: bool,
  has_ts_export_to: bool,
}

fn collect_type_bridge_targets(code: &str) -> Result<BTreeSet<String>, CodegenError> {
  let local_type_infos = collect_local_type_infos(code)?;

  if !local_type_infos.contains_key("Props") {
    return Err(CodegenError::TypeBridge(
      "client routes must define `Props` in `<script setup>`".to_owned(),
    ));
  }

  let mut targets = BTreeSet::from([String::from("Props")]);
  let mut pending = local_type_infos
    .get("Props")
    .map(|info| info.field_types.clone())
    .unwrap_or_default();

  while let Some(ty) = pending.pop() {
    let mut references = BTreeSet::new();
    collect_local_type_refs(&ty, &mut references);

    for reference in references {
      if targets.contains(&reference) {
        continue;
      }
      if let Some(field_types) = local_type_infos
        .get(&reference)
        .map(|info| &info.field_types)
      {
        targets.insert(reference.clone());
        pending.extend(field_types.iter().cloned());
      }
    }
  }

  Ok(targets)
}

fn collect_local_type_infos(code: &str) -> Result<BTreeMap<String, LocalTypeInfo>, CodegenError> {
  let file = syn::parse_file(code).map_err(|err| {
    CodegenError::TypeBridge(format!(
      "failed to parse `<script setup>` for Props analysis: {err}"
    ))
  })?;

  let mut local_type_infos = BTreeMap::new();
  for item in file.items {
    match item {
      Item::Struct(item_struct) => {
        local_type_infos.insert(
          item_struct.ident.to_string(),
          LocalTypeInfo {
            field_types: field_types(&item_struct.fields),
            named_fields: named_field_types(&item_struct.fields, code),
          },
        );
      }
      Item::Enum(item_enum) => {
        let field_types = item_enum
          .variants
          .iter()
          .flat_map(|variant| field_types(&variant.fields))
          .collect();
        local_type_infos.insert(
          item_enum.ident.to_string(),
          LocalTypeInfo {
            field_types,
            named_fields: Vec::new(),
          },
        );
      }
      _ => {}
    }
  }

  Ok(local_type_infos)
}

fn field_types(fields: &Fields) -> Vec<Type> {
  match fields {
    Fields::Named(fields) => fields.named.iter().map(|field| field.ty.clone()).collect(),
    Fields::Unnamed(fields) => fields
      .unnamed
      .iter()
      .map(|field| field.ty.clone())
      .collect(),
    Fields::Unit => Vec::new(),
  }
}

fn named_field_types(fields: &Fields, code: &str) -> Vec<LocalNamedField> {
  match fields {
    Fields::Named(fields) => fields
      .named
      .iter()
      .filter_map(|field| {
        let ident = field.ident.as_ref()?;
        Some(LocalNamedField {
          name: ident.to_string(),
          ty: field.ty.clone(),
          type_name: field.ty.to_token_stream().to_string(),
          span: source_span_from_proc_macro_span(code, ident.span()),
        })
      })
      .collect(),
    Fields::Unnamed(_) | Fields::Unit => Vec::new(),
  }
}

fn collect_template_symbol_definitions(
  owner_type: &str,
  field: &LocalNamedField,
  path: &str,
  local_type_infos: &BTreeMap<String, LocalTypeInfo>,
  definitions: &mut Vec<TemplateSymbolDefinition>,
  active_types: &mut BTreeSet<String>,
) {
  if let Some(span) = field.span
    && !definitions.iter().any(|definition| definition.path == path)
  {
    definitions.push(TemplateSymbolDefinition {
      path: path.to_owned(),
      owner_type: owner_type.to_owned(),
      field_name: field.name.clone(),
      type_name: field.type_name.clone(),
      span,
    });
  }

  let Some(local_type_name) = transparent_local_type_name(&field.ty) else {
    return;
  };
  let Some(type_info) = local_type_infos.get(&local_type_name) else {
    return;
  };
  if !active_types.insert(local_type_name.clone()) {
    return;
  }

  for field in &type_info.named_fields {
    collect_template_symbol_definitions(
      &local_type_name,
      field,
      &format!("{path}.{}", field.name),
      local_type_infos,
      definitions,
      active_types,
    );
  }

  active_types.remove(&local_type_name);
}

fn source_span_from_proc_macro_span(source: &str, span: proc_macro2::Span) -> Option<SourceSpan> {
  let start = span.start();
  let end = span.end();
  let start = byte_offset_from_line_column(source, start.line, start.column)?;
  let end = byte_offset_from_line_column(source, end.line, end.column)?;
  Some(SourceSpan { start, end })
}

fn byte_offset_from_line_column(source: &str, line: usize, column: usize) -> Option<usize> {
  if line == 0 {
    return None;
  }

  let mut current_line = 1usize;
  let mut current_column = 0usize;

  if line == 1 && column == 0 {
    return Some(0);
  }

  for (idx, ch) in source.char_indices() {
    if current_line == line && current_column == column {
      return Some(idx);
    }

    if ch == '\n' {
      current_line += 1;
      current_column = 0;
      if current_line == line && column == 0 {
        return Some(idx + 1);
      }
    } else {
      current_column += 1;
    }
  }

  (current_line == line && current_column == column).then_some(source.len())
}

fn transparent_local_type_name(ty: &Type) -> Option<String> {
  match ty {
    Type::Group(group) => transparent_local_type_name(&group.elem),
    Type::Paren(paren) => transparent_local_type_name(&paren.elem),
    Type::Path(type_path) => {
      if let Some(qself) = &type_path.qself {
        return transparent_local_type_name(&qself.ty);
      }

      let last = type_path.path.segments.last()?;
      match &last.arguments {
        PathArguments::None => Some(last.ident.to_string()),
        PathArguments::AngleBracketed(arguments)
          if matches!(last.ident.to_string().as_str(), "Box" | "Option") =>
        {
          let mut nested_types = arguments.args.iter().filter_map(|argument| match argument {
            GenericArgument::Type(inner) => Some(inner),
            _ => None,
          });
          let inner = nested_types.next()?;
          if nested_types.next().is_some() {
            return None;
          }
          transparent_local_type_name(inner)
        }
        _ => None,
      }
    }
    Type::Reference(reference) => transparent_local_type_name(&reference.elem),
    _ => None,
  }
}

fn collect_local_type_refs(ty: &Type, refs: &mut BTreeSet<String>) {
  match ty {
    Type::Array(array) => collect_local_type_refs(&array.elem, refs),
    Type::Group(group) => collect_local_type_refs(&group.elem, refs),
    Type::Paren(paren) => collect_local_type_refs(&paren.elem, refs),
    Type::Path(type_path) => {
      if let Some(qself) = &type_path.qself {
        collect_local_type_refs(&qself.ty, refs);
      }

      if let Some(last) = type_path.path.segments.last() {
        refs.insert(last.ident.to_string());
      }

      for segment in &type_path.path.segments {
        if let PathArguments::AngleBracketed(arguments) = &segment.arguments {
          for argument in &arguments.args {
            if let GenericArgument::Type(inner) = argument {
              collect_local_type_refs(inner, refs);
            }
          }
        }
      }
    }
    Type::Ptr(pointer) => collect_local_type_refs(&pointer.elem, refs),
    Type::Reference(reference) => collect_local_type_refs(&reference.elem, refs),
    Type::Slice(slice) => collect_local_type_refs(&slice.elem, refs),
    Type::Tuple(tuple) => {
      for element in &tuple.elems {
        collect_local_type_refs(element, refs);
      }
    }
    _ => {}
  }
}

/// Inject `serde` and `ts-rs` derives before route-local type definitions.
fn inject_props_derives(
  code: &str,
  props_types_path: Option<&str>,
) -> Result<String, CodegenError> {
  let type_bridge_targets = props_types_path
    .map(|_| collect_type_bridge_targets(code))
    .transpose()?
    .unwrap_or_default();

  let lines: Vec<&str> = code.lines().collect();
  let mut out = String::new();

  for (i, &line) in lines.iter().enumerate() {
    let trimmed = line.trim();
    if let Some(type_name) = declared_type_name(trimmed) {
      let attrs = scan_item_attributes(&lines, i);
      let is_props = type_name == "Props";
      let needs_type_bridge = type_bridge_targets.contains(type_name);

      if is_props && !attrs.has_serialize {
        out.push_str("#[derive(serde::Serialize)]\n");
      }

      if needs_type_bridge && !attrs.has_ts_derive {
        out.push_str("#[derive(ts_rs::TS)]\n");
      }

      if needs_type_bridge && is_props && !attrs.has_ts_export_to {
        let path = props_types_path.ok_or_else(|| {
          CodegenError::TypeBridge("missing props type export path for client route".to_owned())
        })?;
        writeln!(out, "#[ts(export_to = {path:?})]").expect("infallible");
      }
    }

    out.push_str(line);
    out.push('\n');
  }

  // Strip trailing newline added by the loop — callers add their own.
  Ok(out.trim_end_matches('\n').to_owned())
}

fn scan_item_attributes(lines: &[&str], index: usize) -> ExistingTypeAttrs {
  let mut attrs = ExistingTypeAttrs::default();
  let mut cursor = index;

  while cursor > 0 {
    cursor -= 1;
    let trimmed = lines[cursor].trim();

    if trimmed.is_empty() {
      break;
    }

    if trimmed.starts_with("///") || trimmed.starts_with("//!") {
      continue;
    }

    if !trimmed.starts_with("#[") {
      break;
    }

    if trimmed.contains("serde::Serialize") || trimmed.contains("Serialize") {
      attrs.has_serialize = true;
    }
    if trimmed.contains("derive") && trimmed.contains("TS") {
      attrs.has_ts_derive = true;
    }
    if trimmed.starts_with("#[ts(") && trimmed.contains("export_to") {
      attrs.has_ts_export_to = true;
    }
  }

  attrs
}

fn declared_type_name(line: &str) -> Option<&str> {
  let mut words = line.split_whitespace();
  while let Some(word) = words.next() {
    if word == "struct" || word == "enum" {
      return words.next().map(clean_decl_name);
    }
  }
  None
}

fn clean_decl_name(raw_name: &str) -> &str {
  raw_name
    .trim_end_matches('{')
    .trim_end_matches('(')
    .trim_end_matches(';')
    .split('<')
    .next()
    .unwrap_or(raw_name)
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
  Ok(find_handler_with_span(setup)?.handler)
}

fn find_handler_with_span(setup: &str) -> Result<LocatedRouteHandler, CodegenError> {
  let lines: Vec<&str> = setup.lines().collect();
  let line_starts = line_start_offsets(setup);

  for (idx, line) in lines.iter().enumerate() {
    let trimmed = line.trim();
    let Some(method) = parse_thebe_method(trimmed)? else {
      continue;
    };

    let mut signature = String::new();
    let mut signature_start = None;
    let raw_remainder = line.split_once(']').map_or("", |(_, rest)| rest);
    let remainder = raw_remainder.trim_start();
    if !remainder.is_empty() {
      let remainder_offset = line_starts[idx]
        + (line.len() - raw_remainder.len())
        + (raw_remainder.len() - remainder.len());
      signature_start = Some(remainder_offset);
      signature.push_str(remainder);
      if let Some(boundary) = signature_boundary_offset(&signature) {
        return parse_handler_signature(&signature, method).map(|handler| LocatedRouteHandler {
          handler,
          span: SourceSpan {
            start: remainder_offset,
            end: remainder_offset + boundary,
          },
        });
      }
    }

    for (next_idx, next) in lines.iter().enumerate().skip(idx + 1) {
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

      if signature.is_empty() {
        signature_start = Some(line_starts[next_idx] + (next.len() - next.trim_start().len()));
      }

      if !signature.is_empty() {
        signature.push('\n');
      }
      signature.push_str(next);

      if let Some(boundary) = signature_boundary_offset(&signature) {
        return parse_handler_signature(&signature, method).map(|handler| LocatedRouteHandler {
          handler,
          span: SourceSpan {
            start: signature_start.unwrap_or(line_starts[next_idx]),
            end: signature_start.unwrap_or(line_starts[next_idx]) + boundary,
          },
        });
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

fn signature_boundary_offset(signature: &str) -> Option<usize> {
  let mut paren_depth = 0u32;
  let mut bracket_depth = 0u32;
  let mut angle_depth = 0u32;
  let mut in_string: Option<char> = None;
  let mut chars = signature.char_indices().peekable();

  while let Some((idx, ch)) = chars.next() {
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
        return Some(idx + ch.len_utf8());
      }
      _ => {}
    }
  }

  None
}

fn line_start_offsets(source: &str) -> Vec<usize> {
  let mut starts = vec![0];
  starts.extend(source.match_indices('\n').map(|(idx, _)| idx + 1));
  starts
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
  fn extract_state_type_detects_plain_and_qualified_extractors() {
    assert_eq!(extract_state_type("State<AppState>"), Some("AppState"));
    assert_eq!(
      extract_state_type("axum::extract::State<crate::AppState>"),
      Some("crate::AppState")
    );
    assert_eq!(extract_state_type("Path<String>"), None);
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
  fn find_handler_with_span_tracks_declaration_extent() {
    let setup = "#[thebe::post]\npub async fn submit(\n    Json(body): Json<FormData>,\n) -> Props {\n    todo!()\n}";
    let located = find_handler_with_span(setup).unwrap();

    assert_eq!(
      &setup[located.span.start..located.span.end],
      "pub async fn submit(\n    Json(body): Json<FormData>,\n) -> Props {"
    );
  }

  #[test]
  fn route_handler_info_reports_handler_metadata() {
    let blocks = SfcBlocks {
      script_setup: Some(
        "#[thebe::patch]\npub async fn update(State(state): State<crate::AppState>, Json(body): Json<UpdateBody>) -> Props { todo!() }"
          .to_owned(),
      ),
      ..SfcBlocks::default()
    };

    let info = route_handler_info(&blocks).unwrap();

    assert_eq!(info.method, "patch");
    assert_eq!(info.name, "update");
    assert!(info.is_async);
    assert_eq!(
      info.param_types,
      vec!["State<crate::AppState>", "Json<UpdateBody>"]
    );
    assert_eq!(info.state_type.as_deref(), Some("crate::AppState"));
    assert!(info.source_span.is_none());
  }

  #[test]
  fn route_template_symbols_expand_nested_local_props_fields() {
    let blocks = SfcBlocks {
      script_setup: Some(
        "struct Props {\n    title: String,\n    profile: Profile,\n    maybe_profile: Option<Profile>,\n    posts: Vec<Post>,\n}\n\nstruct Profile {\n    display_name: String,\n}\n\nstruct Post {\n    title: String,\n}"
          .to_owned(),
      ),
      ..SfcBlocks::default()
    };

    let symbols = route_template_symbols(&blocks).unwrap();

    assert_eq!(
      symbols,
      vec![
        "title",
        "profile",
        "profile.display_name",
        "maybe_profile",
        "maybe_profile.display_name",
        "posts",
      ]
    );
  }

  #[test]
  fn route_template_symbol_definitions_preserve_field_spans() {
    let script_setup = "struct Props {\n    profile: Profile,\n}\n\nstruct Profile {\n    display_name: String,\n}";
    let blocks = SfcBlocks {
      script_setup: Some(script_setup.to_owned()),
      ..SfcBlocks::default()
    };

    let definitions = route_template_symbol_definitions(&blocks).unwrap();
    let profile = definitions
      .iter()
      .find(|definition| definition.path == "profile")
      .expect("profile definition");
    let display_name = definitions
      .iter()
      .find(|definition| definition.path == "profile.display_name")
      .expect("nested definition");

    assert_eq!(&script_setup[profile.span.start..profile.span.end], "profile");
    assert_eq!(profile.owner_type, "Props");
    assert_eq!(profile.type_name, "Profile");
    assert_eq!(
      &script_setup[display_name.span.start..display_name.span.end],
      "display_name"
    );
    assert_eq!(display_name.owner_type, "Profile");
    assert_eq!(display_name.type_name, "String");
  }

  #[test]
  fn inject_serde_derive_adds_attribute() {
    let code = "struct Props {\n    title: String,\n}";
    let result = inject_props_derives(code, None).unwrap();
    assert!(result.contains("#[derive(serde::Serialize)]"));
  }

  #[test]
  fn inject_serde_derive_skips_when_already_present() {
    let code = "#[derive(serde::Serialize)]\nstruct Props {\n    title: String,\n}";
    let result = inject_props_derives(code, None).unwrap();
    // Should appear exactly once, not twice.
    assert_eq!(result.matches("serde::Serialize").count(), 1);
  }

  #[test]
  fn inject_serde_derive_handles_pub_crate_struct_props() {
    let code = "pub(crate) struct Props {\n    title: String,\n}";
    let result = inject_props_derives(code, None).unwrap();
    assert!(result.contains("#[derive(serde::Serialize)]"));
  }

  #[test]
  fn inject_props_derives_adds_ts_bridge_to_props_dependencies() {
    let code =
      "struct Props {\n    state: CounterState,\n}\n\nstruct CounterState {\n    count: i64,\n}";
    let result = inject_props_derives(code, Some("routes/index.ts")).unwrap();

    assert_eq!(result.matches("derive(ts_rs::TS)").count(), 2);
    assert!(result.contains("#[ts(export_to = \"routes/index.ts\")]"));
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
        "struct Props { counter: i32 }\n\n#[thebe::get]\npub fn handler() -> Props { Props { counter: 0 } }"
          .to_owned(),
      ),
      script_ts: Some(
        "let props = getProps<Props>();\nfunction inc() { props.counter += 1; }".to_owned(),
      ),
      template: "<span>{{ counter }}</span>".to_owned(),
      ..SfcBlocks::default()
    };

    let generated = generate_route(
      &blocks,
      "/",
      None,
      default_app_html(),
      Some("routes/index.ts"),
      &[],
      false,
      Some("build-1"),
    )
    .unwrap();
    let src = generated.source;

    assert!(generated.assets.is_empty());
    assert!(src.contains("const __APP_HTML"), "app shell const missing");
    // The generated source must contain both client consts.
    assert!(src.contains("__CLIENT_RUNTIME"), "runtime const missing");
    assert!(src.contains("__CLIENT_SCRIPT"), "user script const missing");
    assert!(src.contains("data-thebe-head=\"style\""));
    assert!(src.contains("fn __thebe_internal_error"));
    assert!(src.contains("fn __thebe_render_app_html"));
    assert!(src.contains("fn __thebe_export_types"));
    assert!(src.contains("with_large_int(\"number\")"));
    assert!(src.contains("-> __ThebeResponse"));
    assert!(src.contains("data-thebe-dev-reload"));
    assert!(src.contains("browser_events_url_from_env"));
    assert!(src.contains("%thebe.events_url%"));
    assert!(src.contains("EventSource"));
    assert!(!src.contains("setInterval(async ()"));
    assert!(!src.contains("AbortController"));

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
    assert!(src.contains("__thebe_register(\"inc\""));
  }

  #[test]
  fn generate_route_extracts_prod_assets() {
    use thebe_parser::SfcBlocks;

    let blocks = SfcBlocks {
      script_setup: Some(
        "struct Props { counter: i32 }\n\n#[thebe::get]\npub fn handler() -> Props { Props { counter: 0 } }"
          .to_owned(),
      ),
      script_ts: Some(
        "let props = getProps<Props>();\nfunction inc() { props.counter += 1; }".to_owned(),
      ),
      style: Some("span { color: red; }".to_owned()),
      template: "<span>{{ counter }}</span>".to_owned(),
      ..SfcBlocks::default()
    };

    let generated = generate_route(
      &blocks,
      "/",
      None,
      default_app_html(),
      Some("routes/index.ts"),
      &[],
      true,
      None,
    )
    .unwrap();

    assert_eq!(generated.assets.len(), 3);
    assert!(generated.source.contains("const __RUNTIME_ASSET_URL"));
    assert!(generated.source.contains("const __CLIENT_SCRIPT_ASSET_URL"));
    assert!(generated.source.contains("const __STYLE_ASSET_URL"));
    assert!(generated.source.contains("data-thebe-runtime src=\"{runtime_src}\""));
    assert!(generated.source.contains("<link rel=\"stylesheet\" href=\"{href}\" data-thebe-head=\"style\" />"));
    assert!(generated
      .assets
      .iter()
      .any(|asset| asset.kind == StaticAssetKind::Css && asset.contents.contains("{color:red}")));
    assert!(generated
      .assets
      .iter()
      .any(|asset| asset.kind == StaticAssetKind::Js && asset.contents.contains("props.counter+=1")));
  }

  #[test]
  fn generate_route_keeps_runtime_sentinel_when_minifying() {
    use thebe_parser::SfcBlocks;

    let blocks = SfcBlocks {
      script_setup: Some(
        "struct Props { count: i32 }\n\n#[thebe::get]\npub fn handler() -> Props { Props { count: 0 } }"
          .to_owned(),
      ),
      template: "<span>{{ count }}</span>".to_owned(),
      ..SfcBlocks::default()
    };

    let generated = generate_route(&blocks, "/", None, default_app_html(), None, &[], true, None).unwrap();
    let runtime_asset = generated
      .assets
      .iter()
      .find(|asset| asset.kind == StaticAssetKind::Js)
      .unwrap();

    assert!(runtime_asset.contents.contains("/* __thebe_runtime */"));
    assert!(!runtime_asset.contents.contains("Responsibilities:"));
  }

  #[test]
  fn generate_route_returns_analyzer_error_for_invalid_client_script() {
    use thebe_parser::SfcBlocks;

    let blocks = SfcBlocks {
      script_setup: Some(
        "struct Props { counter: i32 }\n\n#[thebe::get]\npub fn handler() -> Props { Props { counter: 0 } }"
          .to_owned(),
      ),
      script_ts: Some(
        "let props = getProps<Props>();\nfunction inc(step: number { props.counter += step; }"
          .to_owned(),
      ),
      template: "<span>{{ counter }}</span>".to_owned(),
      ..SfcBlocks::default()
    };

    let err = generate_route(
      &blocks,
      "/",
      None,
      default_app_html(),
      Some("routes/index.ts"),
      &[],
      true,
      None,
    )
    .unwrap_err();

    assert!(matches!(err, CodegenError::Analyzer(_)));
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

    let app_html =
      "<html><head><title>%thebe.title%</title>%thebe.head%</head><body>%thebe.body%</body></html>";
    let src = generate_route(&blocks, "/", None, app_html, None, &[], true, None)
      .unwrap()
      .source;

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

    let src = generate_route(&blocks, "/blog/:slug", None, default_app_html(), None, &[], true, None)
      .unwrap()
      .source;

    assert!(src.contains(
      "async fn __thebe_render_handler(__thebe_arg0: Path<String>, __thebe_arg1: State<AppState>)"
    ));
    assert!(src.contains("let __props = create(__thebe_arg0, __thebe_arg1).await;"));
    assert!(src.contains("axum::routing::post(__thebe_render_handler)"));
    assert!(src.contains("pub fn router() -> axum::Router<AppState>"));
  }

  #[test]
  fn generate_route_keeps_stateless_router_generic() {
    use thebe_parser::SfcBlocks;

    let blocks = SfcBlocks {
      script_setup: Some(
        "struct Props { title: String }\n\n#[thebe::get]\npub fn handler() -> Props { Props { title: \"Counter\".to_owned() } }"
          .to_owned(),
      ),
      template: "<h1>{{ title }}</h1>".to_owned(),
      ..SfcBlocks::default()
    };

    let src = generate_route(&blocks, "/", None, default_app_html(), None, &[], true, None)
      .unwrap()
      .source;

    assert!(src.contains("pub fn router<S>() -> axum::Router<S>"));
    assert!(src.contains("axum::Router::<S>::new().route("));
  }

  #[test]
  fn generate_routes_file_uses_path_attributes_for_nested_routes() {
    let source = generate_routes_file(&[
      RouteEntry {
        mod_name: "route__index".to_owned(),
        source_path: "routes/index.rs".to_owned(),
        state_type: None,
      },
      RouteEntry {
        mod_name: "route__blog__dyn_slug".to_owned(),
        source_path: "routes/blog/[slug].rs".to_owned(),
        state_type: None,
      },
    ], &[], None)
    .unwrap();

    assert!(source.contains("#[path = \"routes/index.rs\"]"));
    assert!(source.contains("mod route__index;"));
    assert!(source.contains("#[path = \"routes/blog/[slug].rs\"]"));
    assert!(source.contains("mod route__blog__dyn_slug;"));
    assert!(source.contains(".merge(route__blog__dyn_slug::router::<S>())"));
    assert!(source.contains("pub(crate) fn thebe_routes<S>() -> axum::Router<S>"));
    assert!(!source.contains("async fn main()"));
  }

  #[test]
  fn generate_routes_file_specializes_stateless_routes_for_shared_state() {
    let source = generate_routes_file(&[
      RouteEntry {
        mod_name: "route__index".to_owned(),
        source_path: "routes/index.rs".to_owned(),
        state_type: None,
      },
      RouteEntry {
        mod_name: "route__profile".to_owned(),
        source_path: "routes/profile.rs".to_owned(),
        state_type: Some("AppState".to_owned()),
      },
    ], &[], None)
    .unwrap();

    assert!(source.contains("pub(crate) fn thebe_routes() -> axum::Router<AppState>"));
    assert!(source.contains(".merge(route__index::router::<AppState>())"));
    assert!(source.contains(".merge(route__profile::router())"));
  }

  #[test]
  fn generate_routes_file_rejects_mixed_route_state_types() {
    let err = generate_routes_file(&[
      RouteEntry {
        mod_name: "route__index".to_owned(),
        source_path: "routes/index.rs".to_owned(),
        state_type: Some("AppState".to_owned()),
      },
      RouteEntry {
        mod_name: "route__admin".to_owned(),
        source_path: "routes/admin.rs".to_owned(),
        state_type: Some("AdminState".to_owned()),
      },
    ], &[], None)
    .unwrap_err();

    assert!(matches!(err, CodegenError::MixedRouteStateTypes(_)));
  }

  #[test]
  fn generate_routes_file_serves_hashed_assets() {
    let source = generate_routes_file(
      &[RouteEntry {
        mod_name: "route__index".to_owned(),
        source_path: "routes/index.rs".to_owned(),
        state_type: None,
      }],
      &[
        StaticAsset {
          contents: "body{color:red}".to_owned(),
          file_name: "thebe-deadbeef.css".to_owned(),
          kind: StaticAssetKind::Css,
          url_path: "/.thebe/assets/thebe-deadbeef.css".to_owned(),
        },
        StaticAsset {
          contents: "console.log(1);".to_owned(),
          file_name: "thebe-feedface.js".to_owned(),
          kind: StaticAssetKind::Js,
          url_path: "/.thebe/assets/thebe-feedface.js".to_owned(),
        },
      ],
      None,
    )
    .unwrap();

    assert!(source.contains("include_str!(\"../assets/thebe-deadbeef.css\")"));
    assert!(source.contains("include_str!(\"../assets/thebe-feedface.js\")"));
    assert!(source.contains("public, max-age=31536000, immutable"));
    assert!(source.contains(".route(\"/.thebe/assets/thebe-deadbeef.css\", axum::routing::get(__thebe_asset_thebe_deadbeef_css))"));
    assert!(source.contains(".route(\"/.thebe/assets/thebe-feedface.js\", axum::routing::get(__thebe_asset_thebe_feedface_js))"));
  }

  #[test]
  fn generate_routes_file_keeps_routes_free_of_app_owned_dev_reload_endpoints() {
    let source = generate_routes_file(
      &[RouteEntry {
        mod_name: "route__index".to_owned(),
        source_path: "routes/index.rs".to_owned(),
        state_type: None,
      }],
      &[],
      Some("build-1"),
    )
    .unwrap();

    assert!(!source.contains(".thebe/dev/events"));
    assert!(!source.contains("axum::response::sse::Sse"));
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

    let err = generate_route(&blocks, "/", None, "<html>%thebe.head%</html>", None, &[], true, None).unwrap_err();

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

    let err = generate_route(&blocks, "/", None, default_app_html(), None, &[], true, None).unwrap_err();

    assert!(matches!(err, CodegenError::InvalidAppHtml(_)));
  }
}
