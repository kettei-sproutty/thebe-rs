use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use thebe_parser::SourceSpan;
use walkdir::WalkDir;

pub const THEBE_DIR: &str = ".thebe";
pub const THEBE_DIAGNOSTICS_FILE: &str = ".thebe/diagnostics.json";
pub const THEBE_MANIFEST_FILE: &str = ".thebe/manifest.json";
pub const THEBE_SERVER_ROUTES_DIR: &str = ".thebe/server/routes";
pub const THEBE_SERVER_ROUTES_FILE: &str = ".thebe/server/routes.rs";
pub const TYPECHECK_CLIENT_DIR: &str = ".thebe/client";
pub const TYPECHECK_TYPES_DIR: &str = ".thebe/types";
pub const TYPECHECK_ENV_FILE: &str = ".thebe/thebe-env.d.ts";
pub const TYPECHECK_TSCONFIG_FILE: &str = ".thebe/tsconfig.json";

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

#[derive(Debug, Clone)]
pub struct ProjectArtifacts {
  pub manifest: ThebeManifest,
  pub diagnostics: ThebeDiagnosticsFile,
}

#[derive(Debug, Clone)]
pub enum EditorRefresh {
  Generated(ProjectArtifacts),
  Diagnostics(ThebeDiagnosticsFile),
}

impl EditorRefresh {
  #[must_use]
  pub fn diagnostics(&self) -> &ThebeDiagnosticsFile {
    match self {
      Self::Generated(artifacts) => &artifacts.diagnostics,
      Self::Diagnostics(diagnostics) => diagnostics,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThebeManifest {
  pub version: u32,
  pub server_router_path: String,
  pub app_html: AppHtmlMetadata,
  pub layouts: Vec<LayoutMetadata>,
  pub routes: Vec<RouteMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppHtmlMetadata {
  pub source_path: Option<String>,
  pub uses_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutMetadata {
  pub has_head: bool,
  pub has_style: bool,
  pub scope_path: String,
  pub source_path: String,
  pub template_binding_spans: Vec<TemplateBindingMetadata>,
  pub template_bindings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteMetadata {
  pub generated_client_path: Option<String>,
  pub generated_server_path: String,
  pub generated_types_path: Option<String>,
  pub handler: HandlerMetadata,
  pub has_client_script: bool,
  pub has_head: bool,
  pub has_style: bool,
  pub layout_scope_path: Option<String>,
  pub layout_source_path: Option<String>,
  pub module_name: String,
  pub route_path: String,
  pub source_path: String,
  pub state_type: Option<String>,
  pub template_binding_spans: Vec<TemplateBindingMetadata>,
  pub template_bindings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandlerMetadata {
  pub is_async: bool,
  pub method: String,
  pub name: String,
  pub param_types: Vec<String>,
  pub source_span: Option<SourceSpanMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateBindingMetadata {
  pub name: String,
  pub source_span: SourceSpanMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpanMetadata {
  pub start_byte: usize,
  pub end_byte: usize,
  pub start_line: usize,
  pub start_column: usize,
  pub end_line: usize,
  pub end_column: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThebeDiagnosticsFile {
  pub version: u32,
  pub diagnostics: Vec<ThebeDiagnostic>,
}

impl ThebeDiagnosticsFile {
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.diagnostics.is_empty()
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThebeDiagnostic {
  pub kind: String,
  pub severity: String,
  pub category: String,
  pub code: String,
  pub message: String,
  pub file_path: Option<String>,
  pub source_span: Option<SourceSpanMetadata>,
}

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

#[derive(Debug, Clone, Default)]
pub struct ProjectOverlay {
  files: HashMap<PathBuf, String>,
}

impl ProjectOverlay {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  pub fn insert(&mut self, path: PathBuf, contents: String) -> Option<String> {
    self.files.insert(path, contents)
  }

  pub fn remove(&mut self, path: &Path) -> Option<String> {
    self.files.remove(path)
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.files.is_empty()
  }

  fn contains_path(&self, path: &Path) -> bool {
    self.files.contains_key(path)
  }

  fn read_to_string(&self, path: &Path) -> anyhow::Result<String> {
    if let Some(source) = self.files.get(path) {
      return Ok(source.clone());
    }

    std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
  }

  fn paths_under<'a>(&'a self, dir: &'a Path) -> impl Iterator<Item = &'a PathBuf> + 'a {
    self.files.keys().filter(move |path| path.starts_with(dir))
  }
}

pub fn find_project_root_from(start: &Path) -> anyhow::Result<PathBuf> {
  let mut dir = if start.is_file() {
    start
      .parent()
      .map_or_else(|| start.to_path_buf(), Path::to_path_buf)
  } else {
    start.to_path_buf()
  };

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

pub fn generate_project(project_root: &Path) -> anyhow::Result<ProjectArtifacts> {
  let overlay = ProjectOverlay::default();
  generate_project_with_overlay(project_root, &overlay)
}

pub fn generate_project_with_overlay(
  project_root: &Path,
  overlay: &ProjectOverlay,
) -> anyhow::Result<ProjectArtifacts> {
  let src_dir = project_root.join("src");
  let routes_dir = src_dir.join("routes");
  anyhow::ensure!(
    routes_dir.exists() || overlay.paths_under(&routes_dir).next().is_some(),
    "no `src/routes/` directory found — create your route `.trs` files there"
  );

  let layouts = collect_layouts(&routes_dir, overlay)?;
  let app_html = load_app_html(project_root, overlay)?;
  let trs_files = collect_trs_files(&routes_dir, overlay)?;
  anyhow::ensure!(
    !trs_files.is_empty(),
    "no `.trs` files found in `src/routes/`"
  );

  let parsed_routes = collect_parsed_routes(&trs_files, &routes_dir, overlay)?;
  let needs_type_bridge = parsed_routes
    .iter()
    .any(|route| route.types_export_path.is_some());

  if needs_type_bridge {
    ensure_ts_rs_dependency(project_root, overlay)?;
  }
  prepare_generated_workspace(project_root, needs_type_bridge)?;

  let mut route_entries = Vec::new();

  for route in &parsed_routes {
    let layout_arg = find_layout(&layouts, &route.trs_path, &routes_dir)
      .map(|layout| (&layout.blocks, layout.scope_path.as_str()));

    let generated = thebe_codegen::generate_route(
      &route.blocks,
      &route.route_path,
      layout_arg,
      &app_html.contents,
      route.types_export_path.as_deref(),
    )
    .with_context(|| format!("codegen error for {}", route.trs_path.display()))?;

    let relative_rs_path = route
      .trs_path
      .strip_prefix(&routes_dir)
      .with_context(|| format!("route {} is outside src/routes/", route.trs_path.display()))?
      .with_extension("rs");
    let rs_path = project_root
      .join(THEBE_SERVER_ROUTES_DIR)
      .join(&relative_rs_path);
    if let Some(parent) = rs_path.parent() {
      std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&rs_path, &generated)
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

  let routes_file = thebe_codegen::generate_routes_file(&route_entries)
    .context("failed to generate .thebe/server/routes.rs")?;
  let routes_path = project_root.join(THEBE_SERVER_ROUTES_FILE);
  std::fs::write(&routes_path, &routes_file)
    .with_context(|| format!("failed to write {}", routes_path.display()))?;

  let manifest = build_thebe_manifest(
    project_root,
    &routes_dir,
    &parsed_routes,
    &layouts,
    app_html.source_path.as_deref(),
  )?;
  write_manifest_file(project_root, &manifest)?;

  let diagnostics = ThebeDiagnosticsFile {
    version: 1,
    diagnostics: Vec::new(),
  };
  write_diagnostics_file(project_root, &diagnostics)?;

  remove_legacy_generated_sources(&src_dir)?;

  Ok(ProjectArtifacts {
    manifest,
    diagnostics,
  })
}

pub fn check_project(project_root: &Path) -> anyhow::Result<ThebeDiagnosticsFile> {
  let overlay = ProjectOverlay::default();
  check_project_with_overlay(project_root, &overlay)
}

pub fn check_project_with_overlay(
  project_root: &Path,
  overlay: &ProjectOverlay,
) -> anyhow::Result<ThebeDiagnosticsFile> {
  let diagnostics = ThebeDiagnosticsFile {
    version: 1,
    diagnostics: collect_project_diagnostics(project_root, overlay)?,
  };
  write_diagnostics_file(project_root, &diagnostics)?;
  Ok(diagnostics)
}

pub fn refresh_project_for_editor(project_root: &Path) -> anyhow::Result<EditorRefresh> {
  let overlay = ProjectOverlay::default();
  refresh_project_for_editor_with_overlay(project_root, &overlay)
}

pub fn refresh_project_for_editor_with_overlay(
  project_root: &Path,
  overlay: &ProjectOverlay,
) -> anyhow::Result<EditorRefresh> {
  let diagnostics = check_project_with_overlay(project_root, overlay)?;
  if diagnostics.is_empty() {
    Ok(EditorRefresh::Generated(generate_project_with_overlay(
      project_root,
      overlay,
    )?))
  } else {
    Ok(EditorRefresh::Diagnostics(diagnostics))
  }
}

pub fn load_project_artifacts(project_root: &Path) -> anyhow::Result<ProjectArtifacts> {
  Ok(ProjectArtifacts {
    manifest: load_manifest(project_root)?,
    diagnostics: load_diagnostics(project_root)?,
  })
}

pub fn load_manifest(project_root: &Path) -> anyhow::Result<ThebeManifest> {
  load_json(&project_root.join(THEBE_MANIFEST_FILE))
}

pub fn load_diagnostics(project_root: &Path) -> anyhow::Result<ThebeDiagnosticsFile> {
  load_json(&project_root.join(THEBE_DIAGNOSTICS_FILE))
}

fn load_json<T>(path: &Path) -> anyhow::Result<T>
where
  T: for<'de> Deserialize<'de>,
{
  let source =
    std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
  serde_json::from_str(&source).with_context(|| format!("failed to parse {}", path.display()))
}

fn collect_parsed_routes(
  trs_files: &[PathBuf],
  routes_dir: &Path,
  overlay: &ProjectOverlay,
) -> anyhow::Result<Vec<ParsedRoute>> {
  let mut routes = Vec::with_capacity(trs_files.len());

  for trs_path in trs_files {
    let source = overlay.read_to_string(trs_path)?;

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

fn collect_project_diagnostics(
  project_root: &Path,
  overlay: &ProjectOverlay,
) -> anyhow::Result<Vec<ThebeDiagnostic>> {
  let mut diagnostics = Vec::new();
  let src_dir = project_root.join("src");
  let routes_dir = src_dir.join("routes");

  if !routes_dir.exists() && overlay.paths_under(&routes_dir).next().is_none() {
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

  let route_files = collect_trs_files(&routes_dir, overlay)?;
  let layout_files = collect_layout_files(&routes_dir, overlay)?;
  let cargo_has_ts_rs = has_ts_rs_dependency(project_root, overlay)?;

  if route_files.is_empty() {
    diagnostics.push(project_diagnostic(
      "project",
      "missing-routes",
      "no `.trs` files found in `src/routes/`".to_owned(),
    ));
  }

  let app_html = read_app_html_for_diagnostics(project_root, overlay)?;
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
    match parse_layout_for_diagnostics(project_root, &layout_path, &routes_dir, overlay)? {
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
      overlay,
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
    let cargo_toml = overlay.read_to_string(&cargo_toml_path)?;
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

  if let Err(err) = thebe_codegen::generate_routes_file(&route_entries) {
    diagnostics.push(project_diagnostic(
      "project",
      "mixed-route-state-types",
      err.to_string(),
    ));
  }

  Ok(diagnostics)
}

fn collect_layout_files(
  routes_dir: &Path,
  overlay: &ProjectOverlay,
) -> anyhow::Result<Vec<PathBuf>> {
  let mut files = Vec::new();

  if routes_dir.exists() {
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
  }

  files.extend(
    overlay
      .paths_under(routes_dir)
      .filter(|path| path.extension().is_some_and(|ext| ext == "trs"))
      .filter(|path| path.file_stem().unwrap_or_default().to_string_lossy() == "_layout")
      .cloned(),
  );

  files.sort();
  files.dedup();
  Ok(files)
}

fn analyze_route_for_diagnostics(
  project_root: &Path,
  routes_dir: &Path,
  layouts: &HashMap<PathBuf, ParsedLayout>,
  app_html: &str,
  app_html_valid: bool,
  route_path: &Path,
  overlay: &ProjectOverlay,
  diagnostics: &mut Vec<ThebeDiagnostic>,
) -> anyhow::Result<Option<ParsedRoute>> {
  let source = overlay.read_to_string(route_path)?;

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
  overlay: &ProjectOverlay,
) -> anyhow::Result<Result<ParsedLayout, ThebeDiagnostic>> {
  let source = overlay.read_to_string(layout_path)?;

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

fn write_manifest_file(project_root: &Path, manifest: &ThebeManifest) -> anyhow::Result<()> {
  let manifest_path = project_root.join(THEBE_MANIFEST_FILE);
  if let Some(parent) = manifest_path.parent() {
    std::fs::create_dir_all(parent)
      .with_context(|| format!("failed to create {}", parent.display()))?;
  }
  let contents =
    serde_json::to_string_pretty(manifest).context("failed to serialize .thebe/manifest.json")?;
  std::fs::write(&manifest_path, contents)
    .with_context(|| format!("failed to write {}", manifest_path.display()))
}

fn write_diagnostics_file(
  project_root: &Path,
  diagnostics: &ThebeDiagnosticsFile,
) -> anyhow::Result<()> {
  let diagnostics_path = project_root.join(THEBE_DIAGNOSTICS_FILE);
  if let Some(parent) = diagnostics_path.parent() {
    std::fs::create_dir_all(parent)
      .with_context(|| format!("failed to create {}", parent.display()))?;
  }

  let diagnostics_contents = serde_json::to_string_pretty(diagnostics)
    .context("failed to serialize .thebe/diagnostics.json")?;
  std::fs::write(&diagnostics_path, diagnostics_contents)
    .with_context(|| format!("failed to write {}", diagnostics_path.display()))
}

fn project_diagnostic(category: &str, code: &str, message: String) -> ThebeDiagnostic {
  ThebeDiagnostic {
    kind: "project".to_owned(),
    severity: "error".to_owned(),
    category: category.to_owned(),
    code: code.to_owned(),
    message,
    file_path: None,
    source_span: None,
  }
}

fn file_diagnostic(
  project_root: &Path,
  file_path: &Path,
  source: &str,
  span: Option<SourceSpan>,
  category: &str,
  code: &str,
  message: String,
) -> anyhow::Result<ThebeDiagnostic> {
  Ok(ThebeDiagnostic {
    kind: "file".to_owned(),
    severity: "error".to_owned(),
    category: category.to_owned(),
    code: code.to_owned(),
    message,
    file_path: Some(to_project_relative_path(project_root, file_path)?),
    source_span: span
      .or_else(|| whole_source_span(source))
      .map(|span| source_span_metadata(source, span)),
  })
}

fn codegen_error_diagnostic(
  project_root: &Path,
  file_path: &Path,
  source: &str,
  blocks: &thebe_parser::SfcBlocks,
  err: &thebe_codegen::CodegenError,
) -> anyhow::Result<ThebeDiagnostic> {
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

fn read_app_html_for_diagnostics(
  project_root: &Path,
  overlay: &ProjectOverlay,
) -> anyhow::Result<Option<LoadedAppHtml>> {
  let app_html_path = project_root.join("app.html");
  if !app_html_path.exists() && !overlay.contains_path(&app_html_path) {
    return Ok(None);
  }

  let contents = overlay.read_to_string(&app_html_path)?;

  Ok(Some(LoadedAppHtml {
    contents,
    source_path: Some(app_html_path),
  }))
}

fn validate_app_html_for_diagnostics(
  project_root: &Path,
  app_html: &Option<LoadedAppHtml>,
) -> anyhow::Result<Option<ThebeDiagnostic>> {
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

fn ensure_ts_rs_dependency(project_root: &Path, overlay: &ProjectOverlay) -> anyhow::Result<()> {
  anyhow::ensure!(
    has_ts_rs_dependency(project_root, overlay)?,
    "typed `<script lang=\"ts\">` routes require `ts-rs` in {} — add `ts-rs = \"12\"` under `[dependencies]`",
    project_root.join("Cargo.toml").display()
  );

  Ok(())
}

fn has_ts_rs_dependency(project_root: &Path, overlay: &ProjectOverlay) -> anyhow::Result<bool> {
  let cargo_toml_path = project_root.join("Cargo.toml");
  let cargo_toml = overlay.read_to_string(&cargo_toml_path)?;

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
) -> anyhow::Result<ThebeManifest> {
  let layout_entries = build_thebe_manifest_layouts(project_root, layouts)?;
  let routes = routes
    .iter()
    .map(|route| build_thebe_manifest_route(project_root, routes_dir, route, layouts))
    .collect::<anyhow::Result<Vec<_>>>()?;

  Ok(ThebeManifest {
    version: 3,
    server_router_path: THEBE_SERVER_ROUTES_FILE.to_owned(),
    app_html: AppHtmlMetadata {
      source_path: app_html_path
        .map(|path| to_project_relative_path(project_root, path))
        .transpose()?,
      uses_default: app_html_path.is_none(),
    },
    layouts: layout_entries,
    routes,
  })
}

fn build_thebe_manifest_route(
  project_root: &Path,
  routes_dir: &Path,
  route: &ParsedRoute,
  layouts: &HashMap<PathBuf, ParsedLayout>,
) -> anyhow::Result<RouteMetadata> {
  let layout = find_layout(layouts, &route.trs_path, routes_dir);
  let relative_rs_path = route
    .trs_path
    .strip_prefix(routes_dir)
    .with_context(|| format!("route {} is outside src/routes/", route.trs_path.display()))?
    .with_extension("rs");

  Ok(RouteMetadata {
    generated_client_path: route
      .types_export_path
      .as_ref()
      .map(|path| format!("{TYPECHECK_CLIENT_DIR}/{path}")),
    generated_server_path: format!(
      "{THEBE_SERVER_ROUTES_DIR}/{}",
      relative_rs_path.to_string_lossy().replace('\\', "/")
    ),
    generated_types_path: route
      .types_export_path
      .as_ref()
      .map(|path| format!("{TYPECHECK_TYPES_DIR}/{path}")),
    handler: HandlerMetadata {
      is_async: route.handler_info.is_async,
      method: route.handler_info.method.to_owned(),
      name: route.handler_info.name.clone(),
      param_types: route.handler_info.param_types.clone(),
      source_span: route
        .handler_info
        .source_span
        .map(|span| source_span_metadata(&route.source, span)),
    },
    has_client_script: route_has_client_script(&route.blocks),
    has_head: has_meaningful_block(route.blocks.head.as_deref()),
    has_style: has_meaningful_block(route.blocks.style.as_deref()),
    layout_scope_path: layout.map(|layout| layout.scope_path.clone()),
    layout_source_path: layout
      .map(|layout| to_project_relative_path(project_root, &layout.trs_path))
      .transpose()?,
    module_name: route.mod_name.clone(),
    route_path: route.route_path.clone(),
    source_path: to_project_relative_path(project_root, &route.trs_path)?,
    state_type: route.state_type.clone(),
    template_binding_spans: route
      .template_binding_spans
      .iter()
      .map(|binding| template_binding_span_metadata(&route.source, binding))
      .collect(),
    template_bindings: route.template_bindings.clone(),
  })
}

fn build_thebe_manifest_layouts(
  project_root: &Path,
  layouts: &HashMap<PathBuf, ParsedLayout>,
) -> anyhow::Result<Vec<LayoutMetadata>> {
  let mut layouts = layouts.values().collect::<Vec<_>>();
  layouts.sort_by(|left, right| left.trs_path.cmp(&right.trs_path));

  layouts
    .into_iter()
    .map(|layout| {
      Ok(LayoutMetadata {
        has_head: has_meaningful_block(layout.blocks.head.as_deref()),
        has_style: has_meaningful_block(layout.blocks.style.as_deref()),
        scope_path: layout.scope_path.clone(),
        source_path: to_project_relative_path(project_root, &layout.trs_path)?,
        template_binding_spans: layout
          .template_binding_spans
          .iter()
          .map(|binding| template_binding_span_metadata(&layout.source, binding))
          .collect(),
        template_bindings: layout.template_bindings.clone(),
      })
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

fn template_binding_span_metadata(
  source: &str,
  binding: &thebe_codegen::TemplateBindingOccurrence,
) -> TemplateBindingMetadata {
  TemplateBindingMetadata {
    name: binding.name.clone(),
    source_span: source_span_metadata(source, binding.span),
  }
}

fn source_span_metadata(source: &str, span: SourceSpan) -> SourceSpanMetadata {
  let (start_line, start_column) = byte_offset_to_line_column(source, span.start);
  let (end_line, end_column) = byte_offset_to_line_column(source, span.end);

  SourceSpanMetadata {
    start_byte: span.start,
    end_byte: span.end,
    start_line,
    start_column,
    end_line,
    end_column,
  }
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

fn load_app_html(project_root: &Path, overlay: &ProjectOverlay) -> anyhow::Result<LoadedAppHtml> {
  let app_html_path = project_root.join("app.html");

  if app_html_path.exists() || overlay.contains_path(&app_html_path) {
    let app_html = overlay.read_to_string(&app_html_path)?;
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

fn collect_trs_files(dir: &Path, overlay: &ProjectOverlay) -> anyhow::Result<Vec<PathBuf>> {
  let mut files = Vec::new();
  if dir.exists() {
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
  }

  files.extend(
    overlay
      .paths_under(dir)
      .filter(|path| path.extension().is_some_and(|ext| ext == "trs"))
      .filter(|path| path.file_stem().unwrap_or_default().to_string_lossy() != "_layout")
      .cloned(),
  );

  files.sort();
  files.dedup();
  Ok(files)
}

fn collect_layouts(
  routes_dir: &Path,
  overlay: &ProjectOverlay,
) -> anyhow::Result<HashMap<PathBuf, ParsedLayout>> {
  let mut map = HashMap::new();
  for layout_path in collect_layout_files(routes_dir, overlay)? {
    let source = overlay.read_to_string(&layout_path)?;
    let blocks = thebe_parser::parse_sfc(&source)
      .with_context(|| format!("parse error in {}", layout_path.display()))?;
    let template_bindings =
      thebe_codegen::list_template_bindings(&blocks.template).with_context(|| {
        format!(
          "failed to inspect template bindings in {}",
          layout_path.display()
        )
      })?;
    let template_binding_spans =
      collect_template_binding_occurrences(&source, &blocks.template_spans).with_context(|| {
        format!(
          "failed to inspect template binding spans in {}",
          layout_path.display()
        )
      })?;

    let rel = layout_path.strip_prefix(routes_dir).unwrap_or(&layout_path);
    let scope_path = rel.with_extension("").to_string_lossy().replace('\\', "/");

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
  }
  Ok(map)
}

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
    let path = path.replace('[', ":").replace(']', "");
    format!("/{path}")
  }
}

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
  use std::fs;
  use std::time::{SystemTime, UNIX_EPOCH};

  struct TestProject {
    root: PathBuf,
  }

  impl TestProject {
    fn new(name: &str) -> Self {
      let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after unix epoch")
        .as_nanos();
      let root = std::env::temp_dir().join(format!("thebe-project-{name}-{suffix}"));
      fs::create_dir_all(&root).expect("failed to create test project root");
      Self { root }
    }

    fn path(&self) -> &Path {
      &self.root
    }

    fn write(&self, relative_path: &str, contents: &str) {
      let path = self.root.join(relative_path);
      if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("failed to create parent directory");
      }
      fs::write(path, contents).expect("failed to write fixture file");
    }
  }

  impl Drop for TestProject {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.root);
    }
  }

  fn fixture_cargo_toml(include_ts_rs: bool) -> String {
    let dependencies = if include_ts_rs {
      "[dependencies]\nts-rs = \"12\"\n"
    } else {
      "[dependencies]\n"
    };

    format!(
      "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n{dependencies}"
    )
  }

  #[test]
  fn file_to_route_path_maps_nested_and_dynamic_routes() {
    let routes_dir = Path::new("/tmp/app/src/routes");
    let path = Path::new("/tmp/app/src/routes/blog/[slug].trs");
    assert_eq!(file_to_route_path(path, routes_dir), "/blog/:slug");
  }

  #[test]
  fn file_to_mod_name_uses_relative_path_segments() {
    let routes_dir = Path::new("/tmp/app/src/routes");
    let path = Path::new("/tmp/app/src/routes/blog/[slug].trs");
    assert_eq!(file_to_mod_name(path, routes_dir), "route__blog__dyn_slug");
  }

  #[test]
  fn build_typecheck_mirror_imports_props_type() {
    let source =
      build_typecheck_mirror("let props = getProps<Props>();", "../../types/routes/index");

    assert!(source.contains("import type { Props } from \"../../types/routes/index\";"));
    assert!(source.contains("let props = getProps<Props>();"));
  }

  #[test]
  fn build_manifest_records_generated_paths() {
    let manifest = ThebeManifest {
      version: 3,
      server_router_path: THEBE_SERVER_ROUTES_FILE.to_owned(),
      app_html: AppHtmlMetadata {
        source_path: Some("app.html".to_owned()),
        uses_default: false,
      },
      layouts: vec![LayoutMetadata {
        has_head: true,
        has_style: true,
        scope_path: "_layout".to_owned(),
        source_path: "src/routes/_layout.trs".to_owned(),
        template_binding_spans: Vec::new(),
        template_bindings: Vec::new(),
      }],
      routes: vec![RouteMetadata {
        generated_client_path: Some(".thebe/client/routes/index.ts".to_owned()),
        generated_server_path: ".thebe/server/routes/index.rs".to_owned(),
        generated_types_path: Some(".thebe/types/routes/index.ts".to_owned()),
        handler: HandlerMetadata {
          is_async: false,
          method: "get".to_owned(),
          name: "handler".to_owned(),
          param_types: vec!["State<crate::AppState>".to_owned()],
          source_span: Some(SourceSpanMetadata {
            start_byte: 10,
            end_byte: 20,
            start_line: 2,
            start_column: 1,
            end_line: 2,
            end_column: 11,
          }),
        },
        has_client_script: true,
        has_head: true,
        has_style: true,
        layout_scope_path: Some("_layout".to_owned()),
        layout_source_path: Some("src/routes/_layout.trs".to_owned()),
        module_name: "route__index".to_owned(),
        route_path: "/".to_owned(),
        source_path: "src/routes/index.trs".to_owned(),
        state_type: Some("crate::AppState".to_owned()),
        template_binding_spans: vec![TemplateBindingMetadata {
          name: "count".to_owned(),
          source_span: SourceSpanMetadata {
            start_byte: 100,
            end_byte: 110,
            start_line: 8,
            start_column: 5,
            end_line: 8,
            end_column: 15,
          },
        }],
        template_bindings: vec!["count".to_owned()],
      }],
    };

    let json = serde_json::to_value(&manifest).unwrap();
    assert_eq!(json["serverRouterPath"], THEBE_SERVER_ROUTES_FILE);
    assert_eq!(
      json["routes"][0]["generatedServerPath"],
      ".thebe/server/routes/index.rs"
    );
    assert_eq!(
      json["routes"][0]["generatedTypesPath"],
      ".thebe/types/routes/index.ts"
    );
  }

  #[test]
  fn diagnostics_file_serializes_with_version() {
    let diagnostics = ThebeDiagnosticsFile {
      version: 1,
      diagnostics: vec![project_diagnostic(
        "project",
        "missing-routes",
        "no `.trs` files found in `src/routes/`".to_owned(),
      )],
    };

    let json = serde_json::to_value(&diagnostics).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["diagnostics"][0]["code"], "missing-routes");
  }

  #[test]
  fn refresh_project_for_editor_with_overlay_reports_unsaved_analyzer_errors() {
    let project = TestProject::new("overlay-diagnostics");
    project.write("Cargo.toml", &fixture_cargo_toml(true));
    project.write(
      "src/routes/index.trs",
      r#"<script setup>
struct Props {
  username: String,
}

#[thebe::get]
fn handler() -> Props {
  Props {
    username: String::from("Thebe"),
  }
}
</script>

<script lang="ts">
let props = getProps<Props>();

function handleChange(event: Event) {
  const input = event.target;
  props.username = input.value;
}
</script>

<main>{{ username }}</main>
"#,
    );

    let mut overlay = ProjectOverlay::new();
    overlay.insert(
      project.path().join("src/routes/index.trs"),
      r#"<script setup>
struct Props {
  username: String,
}

#[thebe::get]
fn handler() -> Props {
  Props {
    username: String::from("Thebe"),
  }
}
</script>

<script lang="ts">
let props = getProps<Props>();

function handleChange(event: Event) {
  props.username = ;
}
</script>

<main>{{ username }}</main>
"#
      .to_owned(),
    );

    let refresh = refresh_project_for_editor_with_overlay(project.path(), &overlay)
      .expect("overlay refresh should succeed");
    let EditorRefresh::Diagnostics(diagnostics) = refresh else {
      panic!("expected diagnostics for invalid unsaved client script");
    };

    assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
      diagnostic.category == "client-script"
        && diagnostic.code == "analyzer-error"
        && diagnostic.file_path.as_deref() == Some("src/routes/index.trs")
    }));
  }

  #[test]
  fn generate_project_with_overlay_discovers_unsaved_route_files() {
    let project = TestProject::new("overlay-generation");
    project.write("Cargo.toml", &fixture_cargo_toml(false));

    let mut overlay = ProjectOverlay::new();
    overlay.insert(
      project.path().join("src/routes/index.trs"),
      r#"<script setup>
struct Props {
  title: String,
}

#[thebe::get]
fn handler() -> Props {
  Props {
    title: String::from("Overlay"),
  }
}
</script>

<main>{{ title }}</main>
"#
      .to_owned(),
    );

    let artifacts = generate_project_with_overlay(project.path(), &overlay)
      .expect("overlay generation should succeed");

    assert_eq!(artifacts.manifest.routes.len(), 1);
    assert_eq!(
      artifacts.manifest.routes[0].source_path,
      "src/routes/index.trs"
    );
    assert_eq!(artifacts.manifest.routes[0].handler.name, "handler");
  }
}
