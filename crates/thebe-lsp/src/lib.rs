use anyhow::{Context, Result};
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
  Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
  DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Hover, HoverContents, HoverParams,
  HoverProviderCapability, InitializeParams, InitializeResult, MarkupContent, MarkupKind,
  MessageType, NumberOrString, OneOf, Position, Range, ServerCapabilities, ServerInfo, SymbolKind,
  TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{Client, LanguageServer};

const THEBE_MANIFEST_FILE: &str = ".thebe/manifest.json";
const THEBE_DIAGNOSTICS_FILE: &str = ".thebe/diagnostics.json";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThebeManifest {
  app_html: AppHtmlMetadata,
  layouts: Vec<LayoutMetadata>,
  routes: Vec<RouteMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppHtmlMetadata {
  source_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutMetadata {
  scope_path: String,
  source_path: String,
  template_binding_spans: Vec<TemplateBindingMetadata>,
  template_bindings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteMetadata {
  route_path: String,
  source_path: String,
  state_type: Option<String>,
  handler: HandlerMetadata,
  template_binding_spans: Vec<TemplateBindingMetadata>,
  template_bindings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HandlerMetadata {
  method: String,
  name: String,
  is_async: bool,
  param_types: Vec<String>,
  source_span: Option<SourceSpanMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TemplateBindingMetadata {
  name: String,
  source_span: SourceSpanMetadata,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SourceSpanMetadata {
  start_line: usize,
  start_column: usize,
  end_line: usize,
  end_column: usize,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct ThebeDiagnosticsFile {
  diagnostics: Vec<ThebeDiagnostic>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThebeDiagnostic {
  severity: String,
  category: String,
  code: String,
  message: String,
  file_path: Option<String>,
  source_span: Option<SourceSpanMetadata>,
}

#[derive(Debug, Clone)]
struct ProjectArtifacts {
  manifest: ThebeManifest,
  diagnostics: ThebeDiagnosticsFile,
}

#[derive(Debug)]
pub struct Backend {
  client: Client,
}

impl Backend {
  #[must_use]
  pub fn new(client: Client) -> Self {
    Self { client }
  }

  async fn refresh_diagnostics_for_uri(&self, uri: &Url) {
    let Some(project_root) = find_project_root_from_uri(uri) else {
      return;
    };

    match ProjectArtifacts::load(&project_root) {
      Ok(artifacts) => {
        if let Err(err) = publish_project_diagnostics(&self.client, &project_root, &artifacts).await
        {
          self
            .client
            .log_message(MessageType::ERROR, format!("thebe-lsp: {err:#}"))
            .await;
        }
      }
      Err(err) => {
        self
          .client
          .log_message(
            MessageType::WARNING,
            format!(
              "thebe-lsp: failed to load {} or {} for {}: {err:#}",
              THEBE_MANIFEST_FILE,
              THEBE_DIAGNOSTICS_FILE,
              project_root.display()
            ),
          )
          .await;
        self
          .client
          .publish_diagnostics(uri.clone(), Vec::new(), None)
          .await;
      }
    }
  }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
  async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
    Ok(InitializeResult {
      server_info: Some(ServerInfo {
        name: "thebe-lsp".to_owned(),
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
      }),
      capabilities: ServerCapabilities {
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        ..ServerCapabilities::default()
      },
      ..InitializeResult::default()
    })
  }

  async fn initialized(&self, _: tower_lsp::lsp_types::InitializedParams) {
    self
      .client
      .log_message(
        MessageType::INFO,
        "thebe-lsp initialized — run `thebe check` or `thebe dev` to refresh .thebe artifacts",
      )
      .await;
  }

  async fn shutdown(&self) -> LspResult<()> {
    Ok(())
  }

  async fn did_open(&self, params: DidOpenTextDocumentParams) {
    self
      .refresh_diagnostics_for_uri(&params.text_document.uri)
      .await;
  }

  async fn did_save(&self, params: DidSaveTextDocumentParams) {
    self
      .refresh_diagnostics_for_uri(&params.text_document.uri)
      .await;
  }

  async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(project_root) = find_project_root_from_uri(uri) else {
      return Ok(None);
    };
    let artifacts = match ProjectArtifacts::load(&project_root) {
      Ok(artifacts) => artifacts,
      Err(_) => return Ok(None),
    };
    let Some(relative_path) = relative_path_from_uri(&project_root, uri) else {
      return Ok(None);
    };

    Ok(hover_for_manifest_file(
      &artifacts.manifest,
      &relative_path,
      params.text_document_position_params.position,
    ))
  }

  async fn document_symbol(
    &self,
    params: DocumentSymbolParams,
  ) -> LspResult<Option<DocumentSymbolResponse>> {
    let uri = &params.text_document.uri;
    let Some(project_root) = find_project_root_from_uri(uri) else {
      return Ok(None);
    };
    let artifacts = match ProjectArtifacts::load(&project_root) {
      Ok(artifacts) => artifacts,
      Err(_) => return Ok(None),
    };
    let Some(relative_path) = relative_path_from_uri(&project_root, uri) else {
      return Ok(None);
    };

    Ok(document_symbols_for_manifest_file(
      &artifacts.manifest,
      &relative_path,
    ))
  }
}

impl ProjectArtifacts {
  fn load(project_root: &Path) -> Result<Self> {
    let manifest_path = project_root.join(THEBE_MANIFEST_FILE);
    let diagnostics_path = project_root.join(THEBE_DIAGNOSTICS_FILE);
    let manifest = load_json::<ThebeManifest>(&manifest_path)?;
    let diagnostics = load_json::<ThebeDiagnosticsFile>(&diagnostics_path)?;

    Ok(Self {
      manifest,
      diagnostics,
    })
  }
}

fn load_json<T>(path: &Path) -> Result<T>
where
  T: for<'de> Deserialize<'de>,
{
  let source =
    std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
  serde_json::from_str(&source).with_context(|| format!("failed to parse {}", path.display()))
}

fn find_project_root_from_uri(uri: &Url) -> Option<PathBuf> {
  let path = uri.to_file_path().ok()?;
  let mut current = if path.is_file() {
    path.parent()?.to_path_buf()
  } else {
    path
  };

  loop {
    if current.join("Cargo.toml").exists() {
      return Some(current);
    }
    current = current.parent()?.to_path_buf();
  }
}

fn relative_path_from_uri(project_root: &Path, uri: &Url) -> Option<String> {
  let path = uri.to_file_path().ok()?;
  Some(
    path
      .strip_prefix(project_root)
      .ok()?
      .to_string_lossy()
      .replace('\\', "/"),
  )
}

fn hover_for_manifest_file(
  manifest: &ThebeManifest,
  relative_path: &str,
  position: Position,
) -> Option<Hover> {
  if let Some(route) = manifest
    .routes
    .iter()
    .find(|route| route.source_path == relative_path)
  {
    if let Some(source_span) = route.handler.source_span
      && position_in_span(position, &source_span)
    {
      return Some(handler_hover(route));
    }

    if let Some(binding) = route
      .template_binding_spans
      .iter()
      .find(|binding| position_in_span(position, &binding.source_span))
    {
      return Some(binding_hover(
        &binding.name,
        &format!("Route `{}`", route.route_path),
      ));
    }
  }

  if let Some(layout) = manifest
    .layouts
    .iter()
    .find(|layout| layout.source_path == relative_path)
    && let Some(binding) = layout
      .template_binding_spans
      .iter()
      .find(|binding| position_in_span(position, &binding.source_span))
  {
    return Some(binding_hover(
      &binding.name,
      &format!("Layout `{}`", layout.scope_path),
    ));
  }

  None
}

fn document_symbols_for_manifest_file(
  manifest: &ThebeManifest,
  relative_path: &str,
) -> Option<DocumentSymbolResponse> {
  if let Some(route) = manifest
    .routes
    .iter()
    .find(|route| route.source_path == relative_path)
  {
    return Some(DocumentSymbolResponse::Nested(document_symbols_for_route(
      route,
    )));
  }

  if let Some(layout) = manifest
    .layouts
    .iter()
    .find(|layout| layout.source_path == relative_path)
  {
    return Some(DocumentSymbolResponse::Nested(document_symbols_for_layout(
      layout,
    )));
  }

  if manifest.app_html.source_path.as_deref() == Some(relative_path) {
    return Some(DocumentSymbolResponse::Nested(Vec::new()));
  }

  None
}

async fn publish_project_diagnostics(
  client: &Client,
  project_root: &Path,
  artifacts: &ProjectArtifacts,
) -> Result<()> {
  let mut diagnostics_by_file = BTreeMap::<Url, Vec<Diagnostic>>::new();

  for diagnostic in &artifacts.diagnostics.diagnostics {
    let Some(relative_path) = diagnostic.file_path.as_deref() else {
      continue;
    };
    let file_url = file_url(project_root, relative_path)?;
    diagnostics_by_file
      .entry(file_url)
      .or_default()
      .push(to_lsp_diagnostic(diagnostic));
  }

  for relative_path in known_source_paths(&artifacts.manifest) {
    let file_url = file_url(project_root, &relative_path)?;
    diagnostics_by_file.entry(file_url).or_default();
  }

  for (uri, diagnostics) in diagnostics_by_file {
    client.publish_diagnostics(uri, diagnostics, None).await;
  }

  Ok(())
}

fn known_source_paths(manifest: &ThebeManifest) -> Vec<String> {
  let mut paths = Vec::new();

  if let Some(source_path) = &manifest.app_html.source_path {
    paths.push(source_path.clone());
  }

  paths.extend(
    manifest
      .layouts
      .iter()
      .map(|layout| layout.source_path.clone()),
  );
  paths.extend(
    manifest
      .routes
      .iter()
      .map(|route| route.source_path.clone()),
  );

  paths.sort();
  paths.dedup();
  paths
}

fn file_url(project_root: &Path, relative_path: &str) -> Result<Url> {
  Url::from_file_path(project_root.join(relative_path)).map_err(|()| {
    anyhow::anyhow!(
      "failed to convert {} into a file:// URL",
      project_root.join(relative_path).display()
    )
  })
}

fn to_lsp_diagnostic(diagnostic: &ThebeDiagnostic) -> Diagnostic {
  Diagnostic {
    range: diagnostic
      .source_span
      .map_or_else(full_document_range, |source_span| {
        range_from_span(&source_span)
      }),
    severity: Some(severity_from_str(&diagnostic.severity)),
    code: Some(NumberOrString::String(diagnostic.code.clone())),
    code_description: None,
    source: Some(format!("thebe/{}", diagnostic.category)),
    message: diagnostic.message.clone(),
    related_information: None,
    tags: None,
    data: None,
  }
}

fn full_document_range() -> Range {
  Range::new(Position::new(0, 0), Position::new(0, 0))
}

fn severity_from_str(severity: &str) -> DiagnosticSeverity {
  match severity {
    "warning" => DiagnosticSeverity::WARNING,
    "information" => DiagnosticSeverity::INFORMATION,
    "hint" => DiagnosticSeverity::HINT,
    _ => DiagnosticSeverity::ERROR,
  }
}

fn handler_hover(route: &RouteMetadata) -> Hover {
  let params = if route.handler.param_types.is_empty() {
    "none".to_owned()
  } else {
    route.handler.param_types.join(", ")
  };
  let state = route.state_type.as_deref().unwrap_or("none");
  let value = format!(
    "**{} {}**\n\nHandler `{}`\n\n- Async: {}\n- Params: {}\n- State: {}",
    route.handler.method.to_uppercase(),
    route.route_path,
    route.handler.name,
    if route.handler.is_async { "yes" } else { "no" },
    params,
    state,
  );

  Hover {
    contents: HoverContents::Markup(MarkupContent {
      kind: MarkupKind::Markdown,
      value,
    }),
    range: route.handler.source_span.map(|span| range_from_span(&span)),
  }
}

fn binding_hover(binding_name: &str, owner: &str) -> Hover {
  Hover {
    contents: HoverContents::Markup(MarkupContent {
      kind: MarkupKind::Markdown,
      value: format!("**Template binding** `{binding_name}`\n\n{owner}"),
    }),
    range: None,
  }
}

#[expect(
  deprecated,
  reason = "lsp-types 0.94 still requires populating DocumentSymbol::deprecated"
)]
fn document_symbols_for_route(route: &RouteMetadata) -> Vec<DocumentSymbol> {
  let mut symbols = Vec::new();

  if let Some(source_span) = route.handler.source_span {
    let range = range_from_span(&source_span);
    symbols.push(DocumentSymbol {
      name: route.handler.name.clone(),
      detail: Some(format!(
        "{} {}",
        route.handler.method.to_uppercase(),
        route.route_path
      )),
      kind: SymbolKind::FUNCTION,
      tags: None,
      deprecated: None,
      range,
      selection_range: range,
      children: None,
    });
  }

  symbols.extend(binding_symbols(
    &route.template_bindings,
    &route.template_binding_spans,
  ));
  symbols
}

fn document_symbols_for_layout(layout: &LayoutMetadata) -> Vec<DocumentSymbol> {
  binding_symbols(&layout.template_bindings, &layout.template_binding_spans)
}

#[expect(
  deprecated,
  reason = "lsp-types 0.94 still requires populating DocumentSymbol::deprecated"
)]
fn binding_symbols(
  binding_names: &[String],
  binding_spans: &[TemplateBindingMetadata],
) -> Vec<DocumentSymbol> {
  binding_names
    .iter()
    .filter_map(|binding_name| {
      let source_span = binding_spans
        .iter()
        .find(|binding| binding.name == *binding_name)
        .map(|binding| binding.source_span)?;
      let range = range_from_span(&source_span);

      Some(DocumentSymbol {
        name: binding_name.clone(),
        detail: Some("template binding".to_owned()),
        kind: SymbolKind::VARIABLE,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
      })
    })
    .collect()
}

fn range_from_span(span: &SourceSpanMetadata) -> Range {
  Range::new(
    Position::new(
      span.start_line.saturating_sub(1) as u32,
      span.start_column.saturating_sub(1) as u32,
    ),
    Position::new(
      span.end_line.saturating_sub(1) as u32,
      span.end_column.saturating_sub(1) as u32,
    ),
  )
}

fn position_in_span(position: Position, span: &SourceSpanMetadata) -> bool {
  let start = Position::new(
    span.start_line.saturating_sub(1) as u32,
    span.start_column.saturating_sub(1) as u32,
  );
  let end = Position::new(
    span.end_line.saturating_sub(1) as u32,
    span.end_column.saturating_sub(1) as u32,
  );

  compare_positions(position, start) != Ordering::Less
    && compare_positions(position, end) == Ordering::Less
}

fn compare_positions(left: Position, right: Position) -> Ordering {
  match left.line.cmp(&right.line) {
    Ordering::Equal => left.character.cmp(&right.character),
    ordering => ordering,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn fixture_manifest() -> ThebeManifest {
    ThebeManifest {
      app_html: AppHtmlMetadata {
        source_path: Some("app.html".to_owned()),
      },
      layouts: vec![LayoutMetadata {
        scope_path: "_layout".to_owned(),
        source_path: "src/routes/_layout.trs".to_owned(),
        template_bindings: vec!["nav_title".to_owned()],
        template_binding_spans: vec![TemplateBindingMetadata {
          name: "nav_title".to_owned(),
          source_span: SourceSpanMetadata {
            start_line: 4,
            start_column: 3,
            end_line: 4,
            end_column: 16,
          },
        }],
      }],
      routes: vec![RouteMetadata {
        route_path: "/profile".to_owned(),
        source_path: "src/routes/profile.trs".to_owned(),
        state_type: Some("crate::AppState".to_owned()),
        handler: HandlerMetadata {
          method: "get".to_owned(),
          name: "handler".to_owned(),
          is_async: false,
          param_types: vec!["State<crate::AppState>".to_owned()],
          source_span: Some(SourceSpanMetadata {
            start_line: 7,
            start_column: 1,
            end_line: 7,
            end_column: 28,
          }),
        },
        template_bindings: vec!["username".to_owned()],
        template_binding_spans: vec![
          TemplateBindingMetadata {
            name: "username".to_owned(),
            source_span: SourceSpanMetadata {
              start_line: 25,
              start_column: 17,
              end_line: 25,
              end_column: 31,
            },
          },
          TemplateBindingMetadata {
            name: "username".to_owned(),
            source_span: SourceSpanMetadata {
              start_line: 26,
              start_column: 17,
              end_line: 26,
              end_column: 31,
            },
          },
        ],
      }],
    }
  }

  #[test]
  fn range_from_span_converts_to_zero_based_positions() {
    let range = range_from_span(&SourceSpanMetadata {
      start_line: 8,
      start_column: 2,
      end_line: 8,
      end_column: 12,
    });

    assert_eq!(range.start, Position::new(7, 1));
    assert_eq!(range.end, Position::new(7, 11));
  }

  #[test]
  fn hover_for_manifest_file_returns_handler_hover() {
    let manifest = fixture_manifest();
    let hover =
      hover_for_manifest_file(&manifest, "src/routes/profile.trs", Position::new(6, 4)).unwrap();
    let HoverContents::Markup(contents) = hover.contents else {
      panic!("expected markdown hover");
    };

    assert!(contents.value.contains("GET /profile"));
    assert!(contents.value.contains("State<crate::AppState>"));
  }

  #[test]
  fn hover_for_manifest_file_returns_binding_hover() {
    let manifest = fixture_manifest();
    let hover =
      hover_for_manifest_file(&manifest, "src/routes/profile.trs", Position::new(24, 18)).unwrap();
    let HoverContents::Markup(contents) = hover.contents else {
      panic!("expected markdown hover");
    };

    assert!(contents.value.contains("Template binding"));
    assert!(contents.value.contains("username"));
  }

  #[test]
  fn document_symbols_for_route_use_first_binding_span() {
    let manifest = fixture_manifest();
    let Some(DocumentSymbolResponse::Nested(symbols)) =
      document_symbols_for_manifest_file(&manifest, "src/routes/profile.trs")
    else {
      panic!("expected nested document symbols");
    };

    assert_eq!(symbols.len(), 2);
    assert_eq!(symbols[0].name, "handler");
    assert_eq!(symbols[1].name, "username");
    assert_eq!(symbols[1].range.start, Position::new(24, 16));
  }

  #[test]
  fn known_source_paths_cover_manifest_files() {
    let manifest = fixture_manifest();
    let paths = known_source_paths(&manifest);

    assert_eq!(
      paths,
      vec![
        "app.html".to_owned(),
        "src/routes/_layout.trs".to_owned(),
        "src/routes/profile.trs".to_owned(),
      ]
    );
  }
}
