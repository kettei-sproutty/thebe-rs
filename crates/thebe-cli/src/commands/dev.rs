use anyhow::Context;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;
use thebe_parser::SourceSpan;
use walkdir::WalkDir;

const THEBE_DIR: &str = ".thebe";
const THEBE_DIAGNOSTICS_FILE: &str = ".thebe/diagnostics.json";
const THEBE_MANIFEST_FILE: &str = ".thebe/manifest.json";
const THEBE_SERVER_ROUTES_DIR: &str = ".thebe/server/routes";
const THEBE_SERVER_COMPONENTS_DIR: &str = ".thebe/server/components";
const THEBE_SERVER_ROUTES_FILE: &str = ".thebe/server/routes.rs";
const TYPECHECK_CLIENT_DIR: &str = ".thebe/client";
const TYPECHECK_TYPES_DIR: &str = ".thebe/types";
const TYPECHECK_ENV_FILE: &str = ".thebe/thebe-env.d.ts";
const TYPECHECK_TSCONFIG_FILE: &str = ".thebe/tsconfig.json";
const TYPECHECK_TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "lib": ["ES2022", "DOM"],
    "noEmit": true,
    "strict": false,
    "skipLibCheck": true
  },
  "include": ["thebe-env.d.ts", "client/**/*.ts", "types/**/*.ts"]
}
"#;
const TYPECHECK_ENV_DECLS: &str = "declare function getProps<T = unknown>(): T;\n";

struct ParsedRoute {
  source: String,
  trs_path: PathBuf,
  blocks: thebe_parser::SfcBlocks,
  route_path: String,
  mod_name: String,
  handler_info: thebe_codegen::RouteHandlerInfo,
  state_type: Option<String>,
  template_bindings: Vec<String>,
  template_binding_spans: Vec<thebe_codegen::TemplateBindingOccurrence>,
  types_export_path: Option<String>,
}

struct ParsedLayout {
  source: String,
  blocks: thebe_parser::SfcBlocks,
  scope_path: String,
  trs_path: PathBuf,
  template_bindings: Vec<String>,
  template_binding_spans: Vec<thebe_codegen::TemplateBindingOccurrence>,
}

struct LoadedAppHtml {
  contents: String,
  source_path: Option<PathBuf>,
}

struct ParsedComponent {
  trs_path: PathBuf,
  component_macro: thebe_codegen::ComponentMacro,
}

/// Run `thebe dev`: parse all `.trs` route files, emit generated Rust sources,
/// and hand off to `cargo run`. Pass `watch = true` to auto-restart on changes.
pub fn run(watch: bool) -> anyhow::Result<()> {
  let project_root = find_project_root()?;
  println!("thebe: project root at {}", project_root.display());

  let src_dir = project_root.join("src");
  let routes_dir = src_dir.join("routes");
  anyhow::ensure!(
    routes_dir.exists(),
    "no `src/routes/` directory found — create your route `.trs` files there"
  );

  run_codegen(&project_root, &src_dir, &routes_dir)?;

  if watch {
    run_watch(&project_root, &src_dir, &routes_dir)
  } else {
    println!("thebe: running `cargo run`\u{2026}");
    let status = Command::new("cargo")
      .arg("run")
      .current_dir(&project_root)
      .status()
      .context("failed to invoke `cargo run`")?;
    std::process::exit(status.code().unwrap_or(1));
  }
}

/// Run `thebe check`: validate project files and emit `.thebe/diagnostics.json`.
pub fn check() -> anyhow::Result<()> {
  let project_root = find_project_root()?;
  println!("thebe: project root at {}", project_root.display());

  let diagnostics = collect_project_diagnostics(&project_root)?;
  write_diagnostics_file(&project_root, &diagnostics)?;

  if diagnostics.is_empty() {
    println!("thebe: no diagnostics");
    return Ok(());
  }

  anyhow::bail!("found {} diagnostic(s)", diagnostics.len())
}

/// Re-run codegen for every `.trs` file and refresh generated `.thebe` artifacts.
fn run_codegen(project_root: &Path, src_dir: &Path, routes_dir: &Path) -> anyhow::Result<()> {
  // Parse all `_layout.trs` files first so each route can find its wrapper.
  let layouts = collect_layouts(routes_dir)?;
  let app_html = load_app_html(project_root)?;

  // Collect components from src/components/ (optional directory).
  let components_dir = src_dir.join("components");
  let component_macros: Vec<thebe_codegen::ComponentMacro> = if components_dir.exists() {
    collect_components(&components_dir)?
      .into_iter()
      .map(|c| c.component_macro)
      .collect()
  } else {
    Vec::new()
  };

  let trs_files = collect_trs_files(routes_dir)?;
  anyhow::ensure!(
    !trs_files.is_empty(),
    "no `.trs` files found in `src/routes/`"
  );

  let parsed_routes = collect_parsed_routes(&trs_files, routes_dir)?;
  let needs_type_bridge = parsed_routes
    .iter()
    .any(|route| route.types_export_path.is_some());

  if needs_type_bridge {
    ensure_ts_rs_dependency(project_root)?;
  }
  prepare_generated_workspace(project_root, needs_type_bridge)?;

  let mut route_entries: Vec<thebe_codegen::RouteEntry> = Vec::new();

  for route in &parsed_routes {
    // Find the nearest `_layout.trs` ancestor for this route.
    let layout_arg = find_layout(&layouts, &route.trs_path, routes_dir)
      .map(|layout| (&layout.blocks, layout.scope_path.as_str()));

    let generated = thebe_codegen::generate_route(
      &route.blocks,
      &route.route_path,
      layout_arg,
      &app_html.contents,
      route.types_export_path.as_deref(),
      &component_macros,
        false,
    )
    .with_context(|| format!("codegen error for {}", route.trs_path.display()))?;

    let relative_rs_path = route
      .trs_path
      .strip_prefix(routes_dir)
      .with_context(|| format!("route {} is outside src/routes/", route.trs_path.display()))?
      .with_extension("rs");
    let rs_path = project_root
      .join(THEBE_SERVER_ROUTES_DIR)
      .join(&relative_rs_path);
    if let Some(parent) = rs_path.parent() {
      std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&rs_path, &generated.source)
      .with_context(|| format!("failed to write {}", rs_path.display()))?;

    if let Some(types_export_path) = &route.types_export_path {
      let client_ts = route
        .blocks
        .script_ts
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("client route is missing `<script lang=\"ts\">`"))?;
      write_typecheck_mirror(project_root, types_export_path, client_ts).with_context(|| {
        format!(
          "failed to write type mirror for {}",
          route.trs_path.display()
        )
      })?;
    }

    println!(
      "thebe: {} \u{2192} {}",
      route.trs_path.display(),
      rs_path.display()
    );

    let source_path = Path::new("routes")
      .join(&relative_rs_path)
      .to_string_lossy()
      .replace('\\', "/");

    route_entries.push(thebe_codegen::RouteEntry {
      mod_name: route.mod_name.clone(),
      source_path,
      state_type: route.state_type.clone(),
    });
  }

    let routes_file = thebe_codegen::generate_routes_file(&route_entries, &[])
    .context("failed to generate .thebe/server/routes.rs")?;
  let routes_path = project_root.join(THEBE_SERVER_ROUTES_FILE);
  std::fs::write(&routes_path, &routes_file)
    .with_context(|| format!("failed to write {}", routes_path.display()))?;
  println!("thebe: generated {}", routes_path.display());

  let manifest = build_thebe_manifest(
    project_root,
    routes_dir,
    &parsed_routes,
    &layouts,
    app_html.source_path.as_deref(),
  )?;
  let manifest_path = project_root.join(THEBE_MANIFEST_FILE);
  std::fs::write(&manifest_path, manifest)
    .with_context(|| format!("failed to write {}", manifest_path.display()))?;
  println!("thebe: generated {}", manifest_path.display());

  write_diagnostics_file(project_root, &[])?;

  remove_legacy_generated_sources(src_dir)?;

  Ok(())
}

fn collect_parsed_routes(
  trs_files: &[PathBuf],
  routes_dir: &Path,
) -> anyhow::Result<Vec<ParsedRoute>> {
  let mut routes = Vec::with_capacity(trs_files.len());

  for trs_path in trs_files {
    let source = std::fs::read_to_string(trs_path)
      .with_context(|| format!("failed to read {}", trs_path.display()))?;

    let blocks = thebe_parser::parse_sfc(&source)
      .with_context(|| format!("parse error in {}", trs_path.display()))?;
    let handler_info = thebe_codegen::route_handler_info(&blocks)
      .with_context(|| format!("failed to inspect handler in {}", trs_path.display()))?;
    let template_bindings =
      thebe_codegen::list_template_bindings(&blocks.template).with_context(|| {
        format!(
          "failed to inspect template bindings in {}",
          trs_path.display()
        )
      })?;
    let template_binding_spans =
      collect_template_binding_occurrences(&source, &blocks.template_spans).with_context(|| {
        format!(
          "failed to inspect template binding spans in {}",
          trs_path.display()
        )
      })?;

    let types_export_path =
      route_has_client_script(&blocks).then(|| type_bridge_export_path(trs_path, routes_dir));

    routes.push(ParsedRoute {
      source,
      trs_path: trs_path.clone(),
      route_path: file_to_route_path(trs_path, routes_dir),
      mod_name: file_to_mod_name(trs_path, routes_dir),
      state_type: handler_info.state_type.clone(),
      handler_info,
      template_bindings,
      template_binding_spans,
      blocks,
      types_export_path,
    });
  }

  Ok(routes)
}

fn route_has_client_script(blocks: &thebe_parser::SfcBlocks) -> bool {
  blocks
    .script_ts
    .as_deref()
    .is_some_and(|script| !script.trim().is_empty())
}

fn collect_project_diagnostics(project_root: &Path) -> anyhow::Result<Vec<serde_json::Value>> {
  let mut diagnostics = Vec::new();
  let src_dir = project_root.join("src");
  let routes_dir = src_dir.join("routes");

  if !routes_dir.exists() {
    diagnostics.push(project_diagnostic(
      "project",
      "missing-routes-dir",
      format!(
        "no `{}` directory found — create your route `.trs` files there",
        routes_dir
          .strip_prefix(project_root)
          .unwrap_or(&routes_dir)
          .display()
      ),
    ));
    return Ok(diagnostics);
  }

  let route_files = collect_trs_files(&routes_dir)?;
  let layout_files = collect_layout_files(&routes_dir)?;
  let cargo_has_ts_rs = has_ts_rs_dependency(project_root)?;

  if route_files.is_empty() {
    diagnostics.push(project_diagnostic(
      "project",
      "missing-routes",
      "no `.trs` files found in `src/routes/`".to_owned(),
    ));
  }

  let app_html = read_app_html_for_diagnostics(project_root)?;
  if let Some(diagnostic) = validate_app_html_for_diagnostics(project_root, &app_html)? {
    diagnostics.push(diagnostic);
  }
  let app_html_source = app_html.as_ref().map_or_else(
    || thebe_codegen::default_app_html().to_owned(),
    |loaded| loaded.contents.clone(),
  );
  let app_html_valid = app_html
    .as_ref()
    .is_none_or(|loaded| thebe_codegen::validate_app_html(&loaded.contents).is_ok());

  let mut layouts = HashMap::new();
  for layout_path in layout_files {
    match parse_layout_for_diagnostics(project_root, &layout_path, &routes_dir)? {
      Ok(layout) => {
        let dir = layout_path.parent().unwrap_or(&routes_dir).to_path_buf();
        layouts.insert(dir, layout);
      }
      Err(diagnostic) => diagnostics.push(diagnostic),
    }
  }

  let mut route_entries = Vec::new();
  let mut any_client_routes = false;

  for route_path in route_files {
    let analysis = analyze_route_for_diagnostics(
      project_root,
      &routes_dir,
      &layouts,
      &app_html_source,
      app_html_valid,
      &route_path,
      &mut diagnostics,
    )?;

    if let Some(route) = analysis {
      any_client_routes |= route.types_export_path.is_some();
      let relative_rs_path = route
        .trs_path
        .strip_prefix(&routes_dir)
        .with_context(|| format!("route {} is outside src/routes/", route.trs_path.display()))?
        .with_extension("rs");
      let source_path = Path::new("routes")
        .join(relative_rs_path)
        .to_string_lossy()
        .replace('\\', "/");
      route_entries.push(thebe_codegen::RouteEntry {
        mod_name: route.mod_name.clone(),
        source_path,
        state_type: route.state_type.clone(),
      });
    }
  }

  if any_client_routes && !cargo_has_ts_rs {
    let cargo_toml_path = project_root.join("Cargo.toml");
    let cargo_toml = std::fs::read_to_string(&cargo_toml_path)
      .with_context(|| format!("failed to read {}", cargo_toml_path.display()))?;
    diagnostics.push(file_diagnostic(
      project_root,
      &cargo_toml_path,
      &cargo_toml,
      None,
      "project",
      "missing-ts-rs",
      "typed `<script lang=\"ts\">` routes require `ts-rs = \"12\"` under `[dependencies]`"
        .to_owned(),
    )?);
  }

  if let Err(err) = thebe_codegen::generate_routes_file(&route_entries, &[]) {
    diagnostics.push(project_diagnostic(
      "project",
      "mixed-route-state-types",
      err.to_string(),
    ));
  }

  Ok(diagnostics)
}

fn collect_layout_files(routes_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
  let mut files = Vec::new();

  for entry in WalkDir::new(routes_dir).min_depth(1) {
    let entry = entry.context("failed to read directory entry")?;
    if entry.file_type().is_file()
      && entry.path().extension().is_some_and(|ext| ext == "trs")
      && entry
        .path()
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        == "_layout"
    {
      files.push(entry.into_path());
    }
  }

  files.sort();
  Ok(files)
}

fn analyze_route_for_diagnostics(
  project_root: &Path,
  routes_dir: &Path,
  layouts: &HashMap<PathBuf, ParsedLayout>,
  app_html: &str,
  app_html_valid: bool,
  route_path: &Path,
  diagnostics: &mut Vec<serde_json::Value>,
) -> anyhow::Result<Option<ParsedRoute>> {
  let source = std::fs::read_to_string(route_path)
    .with_context(|| format!("failed to read {}", route_path.display()))?;

  let blocks = match thebe_parser::parse_sfc(&source) {
    Ok(blocks) => blocks,
    Err(err) => {
      diagnostics.push(file_diagnostic(
        project_root,
        route_path,
        &source,
        None,
        "parser",
        "parse-error",
        err.to_string(),
      )?);
      return Ok(None);
    }
  };

  let handler_info = match thebe_codegen::route_handler_info(&blocks) {
    Ok(info) => info,
    Err(err) => {
      diagnostics.push(codegen_error_diagnostic(
        project_root,
        route_path,
        &source,
        &blocks,
        &err,
      )?);
      return Ok(None);
    }
  };

  let template_bindings = match thebe_codegen::list_template_bindings(&blocks.template) {
    Ok(bindings) => bindings,
    Err(err) => {
      diagnostics.push(codegen_error_diagnostic(
        project_root,
        route_path,
        &source,
        &blocks,
        &err,
      )?);
      return Ok(None);
    }
  };

  let template_binding_spans =
    match collect_template_binding_occurrences(&source, &blocks.template_spans) {
      Ok(bindings) => bindings,
      Err(err) => {
        diagnostics.push(file_diagnostic(
          project_root,
          route_path,
          &source,
          Some(template_area_span(&blocks, &source)),
          "template",
          "template-analysis-error",
          err.to_string(),
        )?);
        return Ok(None);
      }
    };

  let types_export_path =
    route_has_client_script(&blocks).then(|| type_bridge_export_path(route_path, routes_dir));

  let route = ParsedRoute {
    source,
    trs_path: route_path.to_path_buf(),
    route_path: file_to_route_path(route_path, routes_dir),
    mod_name: file_to_mod_name(route_path, routes_dir),
    state_type: handler_info.state_type.clone(),
    handler_info,
    template_bindings,
    template_binding_spans,
    blocks,
    types_export_path,
  };

  if app_html_valid {
    let layout_arg = find_layout(layouts, &route.trs_path, routes_dir)
      .map(|layout| (&layout.blocks, layout.scope_path.as_str()));
    if let Err(err) = thebe_codegen::generate_route(
      &route.blocks,
      &route.route_path,
      layout_arg,
      app_html,
      route.types_export_path.as_deref(),
      &[],
      false,
    ) {
      diagnostics.push(codegen_error_diagnostic(
        project_root,
        &route.trs_path,
        &route.source,
        &route.blocks,
        &err,
      )?);
      return Ok(None);
    }
  }

  Ok(Some(route))
}

fn parse_layout_for_diagnostics(
  project_root: &Path,
  layout_path: &Path,
  routes_dir: &Path,
) -> anyhow::Result<Result<ParsedLayout, serde_json::Value>> {
  let source = std::fs::read_to_string(layout_path)
    .with_context(|| format!("failed to read {}", layout_path.display()))?;

  let blocks = match thebe_parser::parse_sfc(&source) {
    Ok(blocks) => blocks,
    Err(err) => {
      return Ok(Err(file_diagnostic(
        project_root,
        layout_path,
        &source,
        None,
        "parser",
        "parse-error",
        err.to_string(),
      )?));
    }
  };

  let template_bindings = match thebe_codegen::list_template_bindings(&blocks.template) {
    Ok(bindings) => bindings,
    Err(err) => {
      return Ok(Err(file_diagnostic(
        project_root,
        layout_path,
        &source,
        Some(template_area_span(&blocks, &source)),
        "template",
        "template-analysis-error",
        err.to_string(),
      )?));
    }
  };

  let template_binding_spans =
    match collect_template_binding_occurrences(&source, &blocks.template_spans) {
      Ok(bindings) => bindings,
      Err(err) => {
        return Ok(Err(file_diagnostic(
          project_root,
          layout_path,
          &source,
          Some(template_area_span(&blocks, &source)),
          "template",
          "template-analysis-error",
          err.to_string(),
        )?));
      }
    };

  let rel = layout_path.strip_prefix(routes_dir).unwrap_or(layout_path);
  let scope_path = rel.with_extension("").to_string_lossy().replace('\\', "/");

  Ok(Ok(ParsedLayout {
    source,
    blocks,
    scope_path,
    trs_path: layout_path.to_path_buf(),
    template_bindings,
    template_binding_spans,
  }))
}

fn write_diagnostics_file(
  project_root: &Path,
  diagnostics: &[serde_json::Value],
) -> anyhow::Result<()> {
  let diagnostics_path = project_root.join(THEBE_DIAGNOSTICS_FILE);
  if let Some(parent) = diagnostics_path.parent() {
    std::fs::create_dir_all(parent)
      .with_context(|| format!("failed to create {}", parent.display()))?;
  }

  let diagnostics_contents = build_diagnostics_file(diagnostics)?;
  std::fs::write(&diagnostics_path, diagnostics_contents)
    .with_context(|| format!("failed to write {}", diagnostics_path.display()))
}

fn build_diagnostics_file(diagnostics: &[serde_json::Value]) -> anyhow::Result<String> {
  let diagnostics_file = serde_json::json!({
    "version": 1,
    "diagnostics": diagnostics,
  });

  serde_json::to_string_pretty(&diagnostics_file)
    .context("failed to serialize .thebe/diagnostics.json")
}

fn project_diagnostic(category: &str, code: &str, message: String) -> serde_json::Value {
  serde_json::json!({
    "kind": "project",
    "severity": "error",
    "category": category,
    "code": code,
    "message": message,
    "filePath": serde_json::Value::Null,
    "sourceSpan": serde_json::Value::Null,
  })
}

fn file_diagnostic(
  project_root: &Path,
  file_path: &Path,
  source: &str,
  span: Option<SourceSpan>,
  category: &str,
  code: &str,
  message: String,
) -> anyhow::Result<serde_json::Value> {
  Ok(serde_json::json!({
    "kind": "file",
    "severity": "error",
    "category": category,
    "code": code,
    "message": message,
    "filePath": to_project_relative_path(project_root, file_path)?,
    "sourceSpan": span
      .or_else(|| whole_source_span(source))
      .map(|span| source_span_json(source, span)),
  }))
}

fn codegen_error_diagnostic(
  project_root: &Path,
  file_path: &Path,
  source: &str,
  blocks: &thebe_parser::SfcBlocks,
  err: &thebe_codegen::CodegenError,
) -> anyhow::Result<serde_json::Value> {
  let (category, code, span) = match err {
    thebe_codegen::CodegenError::Analyzer(_) => {
      ("client-script", "analyzer-error", blocks.script_ts_span)
    }
    thebe_codegen::CodegenError::Parse(_) => ("parser", "parse-error", None),
    thebe_codegen::CodegenError::UnsupportedMethod(_) => {
      ("handler", "unsupported-method", blocks.script_setup_span)
    }
    thebe_codegen::CodegenError::InvalidHandlerSignature(_) => (
      "handler",
      "invalid-handler-signature",
      blocks.script_setup_span,
    ),
    thebe_codegen::CodegenError::UnclosedBinding => (
      "template",
      "unclosed-binding",
      Some(template_area_span(blocks, source)),
    ),
    thebe_codegen::CodegenError::InvalidBinding(_) => (
      "template",
      "invalid-binding",
      Some(template_area_span(blocks, source)),
    ),
    thebe_codegen::CodegenError::MissingHandler => {
      ("handler", "missing-handler", blocks.script_setup_span)
    }
    thebe_codegen::CodegenError::MissingScriptSetup => ("handler", "missing-script-setup", None),
    thebe_codegen::CodegenError::InvalidAppHtml(_) => ("app-html", "invalid-app-html", None),
    thebe_codegen::CodegenError::InvalidHead(_) => ("head", "invalid-head", blocks.head_span),
    thebe_codegen::CodegenError::TypeBridge(_) => (
      "typecheck",
      "type-bridge-error",
      blocks.script_setup_span.or(blocks.script_ts_span),
    ),
    thebe_codegen::CodegenError::MixedRouteStateTypes(_) => {
      return Ok(project_diagnostic(
        "project",
        "mixed-route-state-types",
        err.to_string(),
      ));
    }
    thebe_codegen::CodegenError::CssError(_) => ("style", "css-error", blocks.style_span),
  };

  file_diagnostic(
    project_root,
    file_path,
    source,
    span,
    category,
    code,
    err.to_string(),
  )
}

fn template_area_span(blocks: &thebe_parser::SfcBlocks, source: &str) -> SourceSpan {
  match (
    blocks.template_spans.first().copied(),
    blocks.template_spans.last().copied(),
  ) {
    (Some(first), Some(last)) => SourceSpan {
      start: first.start,
      end: last.end,
    },
    _ => whole_source_span(source).unwrap_or_default(),
  }
}

fn whole_source_span(source: &str) -> Option<SourceSpan> {
  (!source.is_empty()).then_some(SourceSpan {
    start: 0,
    end: source.len(),
  })
}

fn read_app_html_for_diagnostics(project_root: &Path) -> anyhow::Result<Option<LoadedAppHtml>> {
  let app_html_path = project_root.join("app.html");
  if !app_html_path.exists() {
    return Ok(None);
  }

  let contents = std::fs::read_to_string(&app_html_path)
    .with_context(|| format!("failed to read {}", app_html_path.display()))?;

  Ok(Some(LoadedAppHtml {
    contents,
    source_path: Some(app_html_path),
  }))
}

fn validate_app_html_for_diagnostics(
  project_root: &Path,
  app_html: &Option<LoadedAppHtml>,
) -> anyhow::Result<Option<serde_json::Value>> {
  let Some(app_html) = app_html else {
    return Ok(None);
  };

  if let Err(err) = thebe_codegen::validate_app_html(&app_html.contents) {
    if let Some(source_path) = app_html.source_path.as_deref() {
      return Ok(Some(file_diagnostic(
        project_root,
        source_path,
        &app_html.contents,
        None,
        "app-html",
        "invalid-app-html",
        err.to_string(),
      )?));
    }

    return Ok(Some(project_diagnostic(
      "app-html",
      "invalid-app-html",
      err.to_string(),
    )));
  }

  Ok(None)
}

fn ensure_ts_rs_dependency(project_root: &Path) -> anyhow::Result<()> {
  anyhow::ensure!(
    has_ts_rs_dependency(project_root)?,
    "typed `<script lang=\"ts\">` routes require `ts-rs` in {} — add `ts-rs = \"12\"` under `[dependencies]`",
    project_root.join("Cargo.toml").display()
  );

  Ok(())
}

fn has_ts_rs_dependency(project_root: &Path) -> anyhow::Result<bool> {
  let cargo_toml_path = project_root.join("Cargo.toml");
  let cargo_toml = std::fs::read_to_string(&cargo_toml_path)
    .with_context(|| format!("failed to read {}", cargo_toml_path.display()))?;

  Ok(cargo_toml.contains("ts-rs"))
}

fn prepare_generated_workspace(project_root: &Path, typecheck_enabled: bool) -> anyhow::Result<()> {
  let thebe_dir = project_root.join(THEBE_DIR);

  if thebe_dir.exists() {
    std::fs::remove_dir_all(&thebe_dir)
      .with_context(|| format!("failed to remove {}", thebe_dir.display()))?;
  }

  std::fs::create_dir_all(project_root.join(THEBE_SERVER_ROUTES_DIR)).with_context(|| {
    format!(
      "failed to create {}",
      project_root.join(THEBE_SERVER_ROUTES_DIR).display()
    )
  })?;

  std::fs::create_dir_all(project_root.join(THEBE_SERVER_COMPONENTS_DIR)).with_context(|| {
    format!(
      "failed to create {}",
      project_root.join(THEBE_SERVER_COMPONENTS_DIR).display()
    )
  })?;

  if !typecheck_enabled {
    return Ok(());
  }

  std::fs::create_dir_all(project_root.join(TYPECHECK_CLIENT_DIR)).with_context(|| {
    format!(
      "failed to create {}",
      project_root.join(TYPECHECK_CLIENT_DIR).display()
    )
  })?;
  std::fs::create_dir_all(project_root.join(TYPECHECK_TYPES_DIR)).with_context(|| {
    format!(
      "failed to create {}",
      project_root.join(TYPECHECK_TYPES_DIR).display()
    )
  })?;
  std::fs::write(
    project_root.join(TYPECHECK_TSCONFIG_FILE),
    TYPECHECK_TSCONFIG,
  )
  .with_context(|| {
    format!(
      "failed to write {}",
      project_root.join(TYPECHECK_TSCONFIG_FILE).display()
    )
  })?;
  std::fs::write(project_root.join(TYPECHECK_ENV_FILE), TYPECHECK_ENV_DECLS).with_context(
    || {
      format!(
        "failed to write {}",
        project_root.join(TYPECHECK_ENV_FILE).display()
      )
    },
  )?;

  Ok(())
}

fn remove_legacy_generated_sources(src_dir: &Path) -> anyhow::Result<()> {
  let legacy_routes_file = src_dir.join("__thebe_routes.rs");
  remove_autogenerated_file(&legacy_routes_file)?;

  let legacy_routes_dir = src_dir.join("routes");
  if !legacy_routes_dir.exists() {
    return Ok(());
  }

  for entry in WalkDir::new(&legacy_routes_dir).min_depth(1) {
    let entry = entry.context("failed to read directory entry")?;
    if entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "rs") {
      remove_autogenerated_file(entry.path())?;
    }
  }

  Ok(())
}

fn remove_autogenerated_file(path: &Path) -> anyhow::Result<()> {
  if !path.exists() {
    return Ok(());
  }

  let contents =
    std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
  if contents.starts_with("// AUTOGENERATED by thebe") {
    std::fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
  }

  Ok(())
}

fn type_bridge_export_path(trs_path: &Path, routes_dir: &Path) -> String {
  let rel = trs_path.strip_prefix(routes_dir).unwrap_or(trs_path);
  Path::new("routes")
    .join(rel)
    .with_extension("ts")
    .to_string_lossy()
    .replace('\\', "/")
}

fn write_typecheck_mirror(
  project_root: &Path,
  types_export_path: &str,
  client_ts: &str,
) -> anyhow::Result<()> {
  let rel_path = Path::new(types_export_path);
  let mirror_path = project_root.join(TYPECHECK_CLIENT_DIR).join(rel_path);
  let props_import_path = mirror_props_import_path(rel_path);
  let contents = build_typecheck_mirror(client_ts, &props_import_path);

  if let Some(parent) = mirror_path.parent() {
    std::fs::create_dir_all(parent)
      .with_context(|| format!("failed to create {}", parent.display()))?;
  }

  std::fs::write(&mirror_path, contents)
    .with_context(|| format!("failed to write {}", mirror_path.display()))?;

  Ok(())
}

fn mirror_props_import_path(types_export_path: &Path) -> String {
  let depth = types_export_path
    .parent()
    .map_or(0, |parent| parent.components().count());
  let prefix = "../".repeat(depth + 1);
  let target = types_export_path.with_extension("");
  format!(
    "{prefix}types/{}",
    target.to_string_lossy().replace('\\', "/")
  )
}

fn build_typecheck_mirror(client_ts: &str, props_import_path: &str) -> String {
  let mut output = String::new();
  output.push_str("// AUTOGENERATED by thebe — do not edit\n");
  writeln!(
    output,
    "import type {{ Props }} from \"{props_import_path}\";"
  )
  .expect("infallible");
  output.push('\n');
  output.push_str(client_ts.trim());
  output.push('\n');
  output
}

fn build_thebe_manifest(
  project_root: &Path,
  routes_dir: &Path,
  routes: &[ParsedRoute],
  layouts: &HashMap<PathBuf, ParsedLayout>,
  app_html_path: Option<&Path>,
) -> anyhow::Result<String> {
  let layout_entries = build_thebe_manifest_layouts(project_root, layouts)?;
  let routes = routes
    .iter()
    .map(|route| build_thebe_manifest_route(project_root, routes_dir, route, layouts))
    .collect::<anyhow::Result<Vec<_>>>()?;

  let manifest = serde_json::json!({
    "version": 3,
    "serverRouterPath": THEBE_SERVER_ROUTES_FILE,
    "appHtml": {
      "sourcePath": app_html_path
        .map(|path| to_project_relative_path(project_root, path))
        .transpose()?,
      "usesDefault": app_html_path.is_none(),
    },
    "layouts": layout_entries,
    "routes": routes,
  });

  serde_json::to_string_pretty(&manifest).context("failed to serialize .thebe/manifest.json")
}

fn build_thebe_manifest_route(
  project_root: &Path,
  routes_dir: &Path,
  route: &ParsedRoute,
  layouts: &HashMap<PathBuf, ParsedLayout>,
) -> anyhow::Result<serde_json::Value> {
  let layout = find_layout(layouts, &route.trs_path, routes_dir);
  let relative_rs_path = route
    .trs_path
    .strip_prefix(routes_dir)
    .with_context(|| format!("route {} is outside src/routes/", route.trs_path.display()))?
    .with_extension("rs");

  Ok(serde_json::json!({
    "routePath": route.route_path,
    "sourcePath": to_project_relative_path(project_root, &route.trs_path)?,
    "moduleName": route.mod_name,
    "generatedServerPath": format!(
      "{THEBE_SERVER_ROUTES_DIR}/{}",
      relative_rs_path.to_string_lossy().replace('\\', "/")
    ),
    "handler": {
      "method": route.handler_info.method,
      "name": &route.handler_info.name,
      "isAsync": route.handler_info.is_async,
      "paramTypes": &route.handler_info.param_types,
      "sourceSpan": route
        .handler_info
        .source_span
        .map(|span| source_span_json(&route.source, span)),
    },
    "stateType": &route.state_type,
    "templateBindings": &route.template_bindings,
    "templateBindingSpans": route
      .template_binding_spans
      .iter()
      .map(|binding| template_binding_span_json(&route.source, binding))
      .collect::<Vec<_>>(),
    "hasClientScript": route_has_client_script(&route.blocks),
    "hasHead": has_meaningful_block(route.blocks.head.as_deref()),
    "hasStyle": has_meaningful_block(route.blocks.style.as_deref()),
    "layoutSourcePath": layout
      .map(|layout| to_project_relative_path(project_root, &layout.trs_path))
      .transpose()?,
    "layoutScopePath": layout.map(|layout| layout.scope_path.as_str()),
    "generatedTypesPath": route.types_export_path.as_ref().map(|path| format!("{TYPECHECK_TYPES_DIR}/{path}")),
    "generatedClientPath": route.types_export_path.as_ref().map(|path| format!("{TYPECHECK_CLIENT_DIR}/{path}")),
  }))
}

fn build_thebe_manifest_layouts(
  project_root: &Path,
  layouts: &HashMap<PathBuf, ParsedLayout>,
) -> anyhow::Result<Vec<serde_json::Value>> {
  let mut layouts = layouts.values().collect::<Vec<_>>();
  layouts.sort_by(|left, right| left.trs_path.cmp(&right.trs_path));

  layouts
    .into_iter()
    .map(|layout| {
      Ok(serde_json::json!({
        "sourcePath": to_project_relative_path(project_root, &layout.trs_path)?,
        "scopePath": &layout.scope_path,
        "templateBindings": &layout.template_bindings,
        "templateBindingSpans": layout
          .template_binding_spans
          .iter()
          .map(|binding| template_binding_span_json(&layout.source, binding))
          .collect::<Vec<_>>(),
        "hasHead": has_meaningful_block(layout.blocks.head.as_deref()),
        "hasStyle": has_meaningful_block(layout.blocks.style.as_deref()),
      }))
    })
    .collect()
}

fn collect_template_binding_occurrences(
  source: &str,
  template_spans: &[SourceSpan],
) -> anyhow::Result<Vec<thebe_codegen::TemplateBindingOccurrence>> {
  let mut bindings = Vec::new();

  for span in template_spans {
    let segment = &source[span.start..span.end];
    let occurrences = thebe_codegen::list_template_binding_occurrences(segment)
      .context("failed to list template binding occurrences")?;
    bindings.extend(occurrences.into_iter().map(|binding| {
      thebe_codegen::TemplateBindingOccurrence {
        name: binding.name,
        span: binding.span.offset(span.start),
      }
    }));
  }

  Ok(bindings)
}

fn template_binding_span_json(
  source: &str,
  binding: &thebe_codegen::TemplateBindingOccurrence,
) -> serde_json::Value {
  serde_json::json!({
    "name": binding.name,
    "sourceSpan": source_span_json(source, binding.span),
  })
}

fn source_span_json(source: &str, span: SourceSpan) -> serde_json::Value {
  let (start_line, start_column) = byte_offset_to_line_column(source, span.start);
  let (end_line, end_column) = byte_offset_to_line_column(source, span.end);

  serde_json::json!({
    "startByte": span.start,
    "endByte": span.end,
    "startLine": start_line,
    "startColumn": start_column,
    "endLine": end_line,
    "endColumn": end_column,
  })
}

fn byte_offset_to_line_column(source: &str, offset: usize) -> (usize, usize) {
  let mut line = 1usize;
  let mut column = 1usize;

  for (idx, ch) in source.char_indices() {
    if idx >= offset {
      break;
    }
    if ch == '\n' {
      line += 1;
      column = 1;
    } else {
      column += 1;
    }
  }

  (line, column)
}

fn has_meaningful_block(block: Option<&str>) -> bool {
  block.is_some_and(|block| !block.trim().is_empty())
}

fn to_project_relative_path(project_root: &Path, path: &Path) -> anyhow::Result<String> {
  Ok(
    path
      .strip_prefix(project_root)
      .with_context(|| format!("path {} is outside project root", path.display()))?
      .to_string_lossy()
      .replace('\\', "/"),
  )
}

fn load_app_html(project_root: &Path) -> anyhow::Result<LoadedAppHtml> {
  let app_html_path = project_root.join("app.html");

  if app_html_path.exists() {
    let app_html = std::fs::read_to_string(&app_html_path)
      .with_context(|| format!("failed to read {}", app_html_path.display()))?;
    thebe_codegen::validate_app_html(&app_html)
      .with_context(|| format!("invalid {}", app_html_path.display()))?;
    return Ok(LoadedAppHtml {
      contents: app_html,
      source_path: Some(app_html_path),
    });
  }

  let app_html = thebe_codegen::default_app_html().to_owned();
  thebe_codegen::validate_app_html(&app_html).context("internal default app.html is invalid")?;
  Ok(LoadedAppHtml {
    contents: app_html,
    source_path: None,
  })
}

/// Spawn `cargo run` in `project_root` as a background child process.
fn spawn_server(project_root: &Path) -> anyhow::Result<Child> {
  println!("thebe: running `cargo run`\u{2026}");
  Command::new("cargo")
    .arg("run")
    .current_dir(project_root)
    .spawn()
    .context("failed to spawn `cargo run`")
}

/// Terminate a server child and all processes it spawned.
///
/// On Unix we use `pkill -P <pid>` to kill the Axum binary that cargo spawned,
/// then kill cargo itself.  On other platforms we can only kill cargo directly.
fn kill_server(child: &mut Child) {
  #[cfg(unix)]
  {
    let _ = Command::new("pkill")
      .args(["-TERM", "-P", &child.id().to_string()])
      .status();
    // Give child processes a moment to exit cleanly.
    std::thread::sleep(Duration::from_millis(200));
  }
  let _ = child.kill();
  let _ = child.wait();
}

/// Watch `routes_dir` for `.trs` changes and restart the server on each one.
fn run_watch(project_root: &Path, src_dir: &Path, routes_dir: &Path) -> anyhow::Result<()> {
  use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
  use std::sync::mpsc;

  println!(
    "thebe: watch mode — watching {} for changes\u{2026}",
    routes_dir.display()
  );

  let mut child = spawn_server(project_root)?;

  let (tx, rx) = mpsc::channel();
  let mut watcher = RecommendedWatcher::new(
    move |res| {
      let _ = tx.send(res);
    },
    Config::default(),
  )
  .context("failed to create file watcher")?;

  watcher
    .watch(routes_dir, RecursiveMode::Recursive)
    .context("failed to watch routes directory")?;
  watcher
    .watch(project_root, RecursiveMode::NonRecursive)
    .context("failed to watch project root")?;

  while let Ok(first) = rx.recv() {
    // Only act on events that touch route `.trs` files or `app.html`.
    let codegen_changed = is_codegen_event(&first, project_root);

    // Drain any rapid follow-up events (debounce over 150 ms).
    let mut any_codegen_change = codegen_changed;
    loop {
      match rx.recv_timeout(Duration::from_millis(150)) {
        Ok(res) => {
          if is_codegen_event(&res, project_root) {
            any_codegen_change = true;
          }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => break,
        Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
      }
    }

    if !any_codegen_change {
      continue;
    }

    println!("thebe: change detected — rebuilding\u{2026}");

    kill_server(&mut child);

    match run_codegen(project_root, src_dir, routes_dir) {
      Err(e) => eprintln!("thebe: codegen error: {e:#}"),
      Ok(()) => match spawn_server(project_root) {
        Ok(new_child) => child = new_child,
        Err(e) => eprintln!("thebe: failed to restart server: {e:#}"),
      },
    }
  }

  Ok(())
}

/// Return `true` if a notify result should trigger route code generation.
fn is_codegen_event(res: &notify::Result<notify::Event>, project_root: &Path) -> bool {
  use notify::event::EventKind;
  match res {
    Ok(event) => {
      matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
      ) && event.paths.iter().any(|path| {
        path.extension().is_some_and(|ext| ext == "trs") || is_app_html_path(path, project_root)
      })
    }
    Err(_) => false,
  }
}

fn is_app_html_path(path: &Path, project_root: &Path) -> bool {
  path
    .file_name()
    .is_some_and(|file_name| file_name == "app.html")
    && path.parent().is_some_and(|parent| parent == project_root)
}

/// Walk up from the current directory to find the nearest `Cargo.toml`.
fn find_project_root() -> anyhow::Result<PathBuf> {
  let mut dir = std::env::current_dir().context("failed to get current directory")?;
  loop {
    if dir.join("Cargo.toml").exists() {
      return Ok(dir);
    }
    match dir.parent() {
      Some(parent) => dir = parent.to_path_buf(),
      None => anyhow::bail!("could not find a `Cargo.toml` in the current directory or any parent"),
    }
  }
}

/// Recursively collect `.trs` route files from `dir`, **excluding** any file
/// whose stem is `_layout` (those are collected separately by [`collect_layouts`]).
fn collect_trs_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
  let mut files = Vec::new();
  for entry in WalkDir::new(dir).min_depth(1) {
    let entry = entry.context("failed to read directory entry")?;
    if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "trs") {
      let stem = entry
        .path()
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
      if stem != "_layout" {
        files.push(entry.into_path());
      }
    }
  }
  files.sort();
  Ok(files)
}

/// Walk `routes_dir` and parse every `_layout.trs` file found.
///
/// Returns a map from **directory path** → parsed layout metadata. The scope
/// path is the layout file's path relative to `routes_dir` (without the `.trs`
/// extension), used as the CSS scope identifier.
fn collect_layouts(routes_dir: &Path) -> anyhow::Result<HashMap<PathBuf, ParsedLayout>> {
  let mut map = HashMap::new();
  for entry in WalkDir::new(routes_dir).min_depth(1) {
    let entry = entry.context("failed to read directory entry")?;
    if entry.file_type().is_file()
      && entry.path().extension().is_some_and(|e| e == "trs")
      && entry
        .path()
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        == "_layout"
    {
      let layout_path = entry.into_path();
      let source = std::fs::read_to_string(&layout_path)
        .with_context(|| format!("failed to read {}", layout_path.display()))?;
      let blocks = thebe_parser::parse_sfc(&source)
        .with_context(|| format!("parse error in {}", layout_path.display()))?;
      let template_bindings = thebe_codegen::list_template_bindings(&blocks.template)
        .with_context(|| {
          format!(
            "failed to inspect template bindings in {}",
            layout_path.display()
          )
        })?;
      let template_binding_spans =
        collect_template_binding_occurrences(&source, &blocks.template_spans).with_context(
          || {
            format!(
              "failed to inspect template binding spans in {}",
              layout_path.display()
            )
          },
        )?;

      // Derive a stable scope path: relative to routes_dir, no extension.
      let rel = layout_path.strip_prefix(routes_dir).unwrap_or(&layout_path);
      let scope_path = rel.with_extension("").to_string_lossy().replace('\\', "/");

      // Key by the directory that contains the layout file.
      let dir = layout_path.parent().unwrap_or(routes_dir).to_path_buf();
      map.insert(
        dir,
        ParsedLayout {
          source,
          blocks,
          scope_path,
          trs_path: layout_path.clone(),
          template_bindings,
          template_binding_spans,
        },
      );

      println!("thebe: found layout {}", layout_path.display());
    }
  }
  Ok(map)
}

/// Walk `components_dir` and compile every `.trs` file found into a
/// [`thebe_codegen::ComponentMacro`].
///
/// Component names are derived from the PascalCase file stem (e.g. `Card.trs`
/// → `Card`). Duplicate names across subdirectories are rejected to prevent
/// ambiguous macro collisions.
fn collect_components(components_dir: &Path) -> anyhow::Result<Vec<ParsedComponent>> {
  let mut components: Vec<ParsedComponent> = Vec::new();

  for entry in WalkDir::new(components_dir).min_depth(1) {
    let entry = entry.context("failed to read directory entry")?;
    if !entry.file_type().is_file() {
      continue;
    }
    if !entry.path().extension().is_some_and(|e| e == "trs") {
      continue;
    }

    let trs_path = entry.into_path();
    let raw_stem = trs_path
      .file_stem()
      .unwrap_or_default()
      .to_string_lossy()
      .into_owned();

    // Component names must start with an uppercase letter (PascalCase).
    if !raw_stem.starts_with(|c: char| c.is_ascii_uppercase()) {
      println!(
        "thebe: skipping component {} — name must be PascalCase",
        trs_path.display()
      );
      continue;
    }

    // Reject duplicate names.
    if components.iter().any(|c| c.component_macro.name == raw_stem) {
      anyhow::bail!(
        "duplicate component name `{raw_stem}` — each component must have a unique PascalCase name"
      );
    }

    let source = std::fs::read_to_string(&trs_path)
      .with_context(|| format!("failed to read {}", trs_path.display()))?;
    let blocks = thebe_parser::parse_component_sfc(&source)
      .with_context(|| format!("parse error in {}", trs_path.display()))?;
    let component_macro = thebe_codegen::generate_component(&blocks, &raw_stem)
      .with_context(|| format!("codegen error for component {}", trs_path.display()))?;

    println!(
      "thebe: component {} → {} macro",
      trs_path.display(),
      component_macro.macro_name
    );
    components.push(ParsedComponent {
      trs_path,
      component_macro,
    });
  }

  Ok(components)
}

/// Find the nearest `_layout.trs` that applies to `route_file`.
///
/// Walks up from the route file's directory towards (and including) `routes_dir`
/// and returns a reference to the first layout found, or `None`.
fn find_layout<'a>(
  layouts: &'a HashMap<PathBuf, ParsedLayout>,
  route_file: &Path,
  routes_dir: &Path,
) -> Option<&'a ParsedLayout> {
  let mut dir = route_file.parent().unwrap_or(routes_dir).to_path_buf();
  loop {
    if let Some(layout) = layouts.get(&dir) {
      return Some(layout);
    }
    // Stop after checking routes_dir itself.
    if dir == routes_dir {
      break;
    }
    match dir.parent() {
      Some(parent) => dir = parent.to_path_buf(),
      None => break,
    }
  }
  None
}

/// Derive the Axum route path from the file's position under `routes_dir`.
///
/// * `src/routes/index.trs` → `/`
/// * `src/routes/about.trs` → `/about`
/// * `src/routes/blog/[slug].trs` → `/blog/:slug`
fn file_to_route_path(trs_path: &Path, routes_dir: &Path) -> String {
  let rel = trs_path.strip_prefix(routes_dir).unwrap_or(trs_path);
  let stem = rel.file_stem().unwrap_or_default().to_string_lossy();
  let parent = rel.parent().unwrap_or(Path::new(""));

  let mut segments: Vec<String> = parent
    .components()
    .map(|c| c.as_os_str().to_string_lossy().into_owned())
    .collect();

  if stem != "index" {
    segments.push(stem.into_owned());
  }

  if segments.is_empty() {
    "/".to_owned()
  } else {
    let path = segments.join("/");
    // Convert file-system dynamic segments `[param]` → `{param}` (Axum v0.8+ syntax).
    let path = path.replace('[', "{").replace(']', "}");
    format!("/{path}")
  }
}

/// Derive the Rust module name from the `.trs` filename stem.
fn file_to_mod_name(trs_path: &Path, routes_dir: &Path) -> String {
  let rel = trs_path.strip_prefix(routes_dir).unwrap_or(trs_path);

  let mut parts: Vec<String> = rel
    .components()
    .map(|component| component.as_os_str().to_string_lossy().into_owned())
    .collect();
  if let Some(last) = parts.last_mut() {
    let stem = Path::new(last)
      .file_stem()
      .unwrap_or_default()
      .to_string_lossy()
      .into_owned();
    *last = stem;
  }

  let module = parts
    .iter()
    .map(|part| sanitize_module_segment(part))
    .collect::<Vec<_>>()
    .join("__");

  format!("route__{module}")
}

fn sanitize_module_segment(segment: &str) -> String {
  let raw = if let Some(dynamic) = segment
    .strip_prefix('[')
    .and_then(|value| value.strip_suffix(']'))
  {
    format!("dyn_{dynamic}")
  } else {
    segment.to_owned()
  };

  let mut out = String::new();
  let mut prev_was_underscore = false;
  for ch in raw.chars() {
    let mapped = if ch.is_ascii_alphanumeric() {
      prev_was_underscore = false;
      ch.to_ascii_lowercase()
    } else {
      if prev_was_underscore {
        continue;
      }
      prev_was_underscore = true;
      '_'
    };
    out.push(mapped);
  }

  while out.ends_with('_') {
    out.pop();
  }
  if out.is_empty() {
    out.push_str("route");
  }
  if out.starts_with(|c: char| c.is_ascii_digit()) {
    out.insert(0, '_');
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn file_to_route_path_maps_nested_and_dynamic_routes() {
    let routes_dir = Path::new("/tmp/app/src/routes");
    let path = Path::new("/tmp/app/src/routes/blog/[slug].trs");
    assert_eq!(file_to_route_path(path, routes_dir), "/blog/{slug}");
  }

  #[test]
  fn file_to_mod_name_uses_relative_path_segments() {
    let routes_dir = Path::new("/tmp/app/src/routes");
    let path = Path::new("/tmp/app/src/routes/blog/[slug].trs");
    assert_eq!(file_to_mod_name(path, routes_dir), "route__blog__dyn_slug");
  }

  #[test]
  fn sanitize_module_segment_normalizes_static_segments() {
    assert_eq!(sanitize_module_segment("My-page"), "my_page");
  }

  #[test]
  fn type_bridge_export_path_preserves_route_layout() {
    let routes_dir = Path::new("/tmp/app/src/routes");
    let path = Path::new("/tmp/app/src/routes/blog/[slug].trs");
    assert_eq!(
      type_bridge_export_path(path, routes_dir),
      "routes/blog/[slug].ts"
    );
  }

  #[test]
  fn mirror_props_import_path_matches_nested_route_depth() {
    let path = Path::new("routes/blog/[slug].ts");
    assert_eq!(
      mirror_props_import_path(path),
      "../../../types/routes/blog/[slug]"
    );
  }

  #[test]
  fn build_typecheck_mirror_imports_props_type() {
    let source =
      build_typecheck_mirror("let props = getProps<Props>();", "../../types/routes/index");

    assert!(source.contains("import type { Props } from \"../../types/routes/index\";"));
    assert!(source.contains("let props = getProps<Props>();"));
  }

  #[test]
  fn build_thebe_manifest_records_generated_paths_and_layouts() {
    let project_root = Path::new("/tmp/app");
    let routes_dir = Path::new("/tmp/app/src/routes");
    let mut layouts = HashMap::new();
    layouts.insert(
      PathBuf::from("/tmp/app/src/routes/blog"),
      ParsedLayout {
        source: "<div>{{ slot_title }}</div>".to_owned(),
        blocks: thebe_parser::SfcBlocks::default(),
        scope_path: "blog/_layout".to_owned(),
        trs_path: PathBuf::from("/tmp/app/src/routes/blog/_layout.trs"),
        template_bindings: vec!["slot_title".to_owned()],
        template_binding_spans: vec![thebe_codegen::TemplateBindingOccurrence {
          name: "slot_title".to_owned(),
          span: SourceSpan { start: 5, end: 21 },
        }],
      },
    );

    let route = ParsedRoute {
      source: "<script setup>#[thebe::get]\npub fn handler(State(state): State<crate::AppState>) -> Props { todo!() }</script>\n<h1>{{ post.title }}</h1>\n<p>{{ post.author.name }}</p>".to_owned(),
      trs_path: PathBuf::from("/tmp/app/src/routes/blog/[slug].trs"),
      blocks: thebe_parser::SfcBlocks {
        head: Some("<title>Blog</title>".to_owned()),
        script_ts: Some("let props = getProps<Props>();".to_owned()),
        style: Some(".blog { color: red; }".to_owned()),
        ..thebe_parser::SfcBlocks::default()
      },
      route_path: "/blog/:slug".to_owned(),
      mod_name: "route__blog__dyn_slug".to_owned(),
      handler_info: thebe_codegen::RouteHandlerInfo {
        method: "get",
        name: "handler".to_owned(),
        param_types: vec!["State<crate::AppState>".to_owned()],
        is_async: false,
        state_type: Some("crate::AppState".to_owned()),
        source_span: Some(SourceSpan { start: 28, end: 91 }),
      },
      state_type: Some("crate::AppState".to_owned()),
      template_bindings: vec!["post.title".to_owned(), "post.author.name".to_owned()],
      template_binding_spans: vec![
        thebe_codegen::TemplateBindingOccurrence {
          name: "post.title".to_owned(),
          span: SourceSpan {
            start: 113,
            end: 129,
          },
        },
        thebe_codegen::TemplateBindingOccurrence {
          name: "post.author.name".to_owned(),
          span: SourceSpan {
            start: 138,
            end: 160,
          },
        },
      ],
      types_export_path: Some("routes/blog/[slug].ts".to_owned()),
    };

    let manifest = build_thebe_manifest(
      project_root,
      routes_dir,
      &[route],
      &layouts,
      Some(Path::new("/tmp/app/app.html")),
    )
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();

    assert_eq!(manifest["version"], 3);
    assert_eq!(manifest["serverRouterPath"], THEBE_SERVER_ROUTES_FILE);
    assert_eq!(manifest["appHtml"]["sourcePath"], "app.html");
    assert_eq!(manifest["appHtml"]["usesDefault"], false);
    assert_eq!(
      manifest["layouts"][0]["sourcePath"],
      "src/routes/blog/_layout.trs"
    );
    assert_eq!(manifest["layouts"][0]["scopePath"], "blog/_layout");
    assert_eq!(
      manifest["layouts"][0]["templateBindings"],
      serde_json::json!(["slot_title"])
    );
    assert_eq!(
      manifest["layouts"][0]["templateBindingSpans"][0]["sourceSpan"]["startByte"],
      5
    );
    assert_eq!(
      manifest["routes"][0]["sourcePath"],
      "src/routes/blog/[slug].trs"
    );
    assert_eq!(
      manifest["routes"][0]["generatedServerPath"],
      ".thebe/server/routes/blog/[slug].rs"
    );
    assert_eq!(manifest["routes"][0]["handler"]["method"], "get");
    assert_eq!(manifest["routes"][0]["handler"]["name"], "handler");
    assert_eq!(manifest["routes"][0]["handler"]["isAsync"], false);
    assert_eq!(
      manifest["routes"][0]["handler"]["sourceSpan"]["startByte"],
      28
    );
    assert_eq!(
      manifest["routes"][0]["handler"]["paramTypes"],
      serde_json::json!(["State<crate::AppState>"])
    );
    assert_eq!(
      manifest["routes"][0]["templateBindings"],
      serde_json::json!(["post.title", "post.author.name"])
    );
    assert_eq!(
      manifest["routes"][0]["templateBindingSpans"][0]["name"],
      "post.title"
    );
    assert_eq!(
      manifest["routes"][0]["templateBindingSpans"][0]["sourceSpan"]["startByte"],
      113
    );
    assert_eq!(
      manifest["routes"][0]["layoutSourcePath"],
      "src/routes/blog/_layout.trs"
    );
    assert_eq!(manifest["routes"][0]["layoutScopePath"], "blog/_layout");
    assert_eq!(
      manifest["routes"][0]["generatedTypesPath"],
      ".thebe/types/routes/blog/[slug].ts"
    );
    assert_eq!(
      manifest["routes"][0]["generatedClientPath"],
      ".thebe/client/routes/blog/[slug].ts"
    );
    assert_eq!(manifest["routes"][0]["stateType"], "crate::AppState");
    assert_eq!(manifest["routes"][0]["hasClientScript"], true);
    assert_eq!(manifest["routes"][0]["hasHead"], true);
    assert_eq!(manifest["routes"][0]["hasStyle"], true);
  }

  #[test]
  fn build_thebe_manifest_marks_default_app_html_usage() {
    let manifest = build_thebe_manifest(
      Path::new("/tmp/app"),
      Path::new("/tmp/app/src/routes"),
      &[],
      &HashMap::new(),
      None,
    )
    .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();

    assert_eq!(manifest["version"], 3);
    assert_eq!(manifest["appHtml"]["sourcePath"], serde_json::Value::Null);
    assert_eq!(manifest["appHtml"]["usesDefault"], true);
    assert_eq!(manifest["layouts"], serde_json::json!([]));
    assert_eq!(manifest["routes"], serde_json::json!([]));
  }

  #[test]
  fn build_diagnostics_file_wraps_versioned_diagnostics() {
    let diagnostics = vec![project_diagnostic(
      "project",
      "missing-routes",
      "no `.trs` files found in `src/routes/`".to_owned(),
    )];

    let diagnostics = build_diagnostics_file(&diagnostics).unwrap();
    let diagnostics: serde_json::Value = serde_json::from_str(&diagnostics).unwrap();

    assert_eq!(diagnostics["version"], 1);
    assert_eq!(diagnostics["diagnostics"][0]["kind"], "project");
    assert_eq!(diagnostics["diagnostics"][0]["code"], "missing-routes");
  }

  #[test]
  fn file_diagnostic_records_relative_path_and_span() {
    let diagnostic = file_diagnostic(
      Path::new("/tmp/app"),
      Path::new("/tmp/app/src/routes/index.trs"),
      "abc\ndef",
      Some(SourceSpan { start: 4, end: 7 }),
      "template",
      "invalid-binding",
      "binding error".to_owned(),
    )
    .unwrap();

    assert_eq!(diagnostic["kind"], "file");
    assert_eq!(diagnostic["filePath"], "src/routes/index.trs");
    assert_eq!(diagnostic["sourceSpan"]["startLine"], 2);
    assert_eq!(diagnostic["sourceSpan"]["startColumn"], 1);
    assert_eq!(diagnostic["sourceSpan"]["endByte"], 7);
  }

  #[test]
  fn validate_app_html_for_diagnostics_reports_invalid_shell() {
    let app_html = Some(LoadedAppHtml {
      contents: "<!DOCTYPE html><html><body>%thebe.body%</body></html>".to_owned(),
      source_path: Some(PathBuf::from("/tmp/app/app.html")),
    });

    let diagnostic = validate_app_html_for_diagnostics(Path::new("/tmp/app"), &app_html)
      .unwrap()
      .unwrap();

    assert_eq!(diagnostic["kind"], "file");
    assert_eq!(diagnostic["category"], "app-html");
    assert_eq!(diagnostic["code"], "invalid-app-html");
    assert_eq!(diagnostic["filePath"], "app.html");
  }

  #[test]
  fn template_area_span_covers_all_template_segments() {
    let blocks = thebe_parser::SfcBlocks {
      template_spans: vec![
        SourceSpan { start: 5, end: 9 },
        SourceSpan { start: 14, end: 22 },
      ],
      ..thebe_parser::SfcBlocks::default()
    };

    assert_eq!(
      template_area_span(&blocks, "012345678901234567890123456789"),
      SourceSpan { start: 5, end: 22 }
    );
  }

  #[test]
  fn codegen_error_diagnostic_maps_template_errors_to_template_spans() {
    let blocks = thebe_parser::SfcBlocks {
      template_spans: vec![SourceSpan { start: 5, end: 24 }],
      ..thebe_parser::SfcBlocks::default()
    };

    let diagnostic = codegen_error_diagnostic(
      Path::new("/tmp/app"),
      Path::new("/tmp/app/src/routes/index.trs"),
      "<div>{{ invalid + 1 }}</div>",
      &blocks,
      &thebe_codegen::CodegenError::InvalidBinding("invalid + 1".to_owned()),
    )
    .unwrap();

    assert_eq!(diagnostic["category"], "template");
    assert_eq!(diagnostic["code"], "invalid-binding");
    assert_eq!(diagnostic["sourceSpan"]["startByte"], 5);
    assert_eq!(diagnostic["sourceSpan"]["endByte"], 24);
  }
}
