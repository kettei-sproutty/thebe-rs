use anyhow::Result;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use thebe_project::{
  EditorRefresh, LayoutMetadata, ProjectArtifacts, ProjectOverlay, RouteMetadata,
  SourceSpanMetadata, THEBE_DIAGNOSTICS_FILE, THEBE_MANIFEST_FILE, TemplateBindingMetadata,
  ThebeDiagnostic, ThebeDiagnosticsFile, ThebeManifest,
};
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
  Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
  DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentSymbol, DocumentSymbolParams,
  DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents,
  HoverParams, HoverProviderCapability, InitializeParams, InitializeResult, Location,
  MarkupContent, MarkupKind, MessageType, NumberOrString, OneOf, Position, Range, ReferenceParams,
  ServerCapabilities, ServerInfo, SymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind,
  TextDocumentSyncOptions, TextDocumentSyncSaveOptions, Url,
};
use tower_lsp::{Client, LanguageServer};

const CHANGE_REFRESH_DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Debug, Default)]
struct OpenDocuments {
  files: HashMap<PathBuf, String>,
}

impl OpenDocuments {
  fn set(&mut self, path: PathBuf, text: String) -> bool {
    if self
      .files
      .get(&path)
      .is_some_and(|existing| existing == &text)
    {
      return false;
    }

    self.files.insert(path, text);
    true
  }

  fn remove(&mut self, path: &Path) -> bool {
    self.files.remove(path).is_some()
  }

  fn overlay_for_project(&self, project_root: &Path) -> ProjectOverlay {
    let mut overlay = ProjectOverlay::new();

    for (path, text) in &self.files {
      if is_project_input_file(project_root, path) {
        overlay.insert(path.clone(), text.clone());
      }
    }

    overlay
  }
}

#[derive(Debug, Default)]
struct BackendState {
  open_documents: OpenDocuments,
  projects: HashMap<PathBuf, ProjectState>,
}

impl BackendState {
  fn set_document(&mut self, project_root: PathBuf, path: PathBuf, text: String) -> Option<u64> {
    self
      .open_documents
      .set(path, text)
      .then(|| self.bump_revision(project_root))
  }

  fn clear_document(&mut self, project_root: PathBuf, path: &Path) -> Option<u64> {
    self
      .open_documents
      .remove(path)
      .then(|| self.bump_revision(project_root))
  }

  fn snapshot(&self, project_root: &Path) -> ProjectSnapshot {
    let project = self.projects.get(project_root);

    ProjectSnapshot {
      overlay: self.open_documents.overlay_for_project(project_root),
      revision: project.map_or(0, |project| project.revision),
      cached_artifacts: project.and_then(ProjectState::cached_artifacts),
    }
  }

  fn note_refresh_requested(&mut self, project_root: &Path, revision: u64) {
    self
      .projects
      .entry(project_root.to_path_buf())
      .or_default()
      .scheduled_refresh_revision = Some(revision);
  }

  fn should_run_debounced_refresh(&self, project_root: &Path, revision: u64) -> bool {
    self
      .projects
      .get(project_root)
      .is_some_and(|project| project.scheduled_refresh_revision == Some(revision))
  }

  fn remember_generated(
    &mut self,
    project_root: &Path,
    revision: u64,
    artifacts: &ProjectArtifacts,
  ) -> bool {
    self
      .projects
      .entry(project_root.to_path_buf())
      .or_default()
      .remember_generated(revision, artifacts)
  }

  fn remember_diagnostics(
    &mut self,
    project_root: &Path,
    revision: u64,
    diagnostics: &ThebeDiagnosticsFile,
  ) -> (bool, Option<ProjectArtifacts>) {
    self
      .projects
      .entry(project_root.to_path_buf())
      .or_default()
      .remember_diagnostics(revision, diagnostics)
  }

  fn is_current_revision(&self, project_root: &Path, revision: u64) -> bool {
    self
      .projects
      .get(project_root)
      .is_some_and(|project| project.revision == revision)
  }

  fn bump_revision(&mut self, project_root: PathBuf) -> u64 {
    let project = self.projects.entry(project_root).or_default();
    project.revision += 1;
    project.revision
  }
}

#[derive(Debug, Clone)]
struct ProjectSnapshot {
  overlay: ProjectOverlay,
  revision: u64,
  cached_artifacts: Option<ProjectArtifacts>,
}

#[derive(Debug, Default)]
struct ProjectState {
  revision: u64,
  scheduled_refresh_revision: Option<u64>,
  cached_artifacts: Option<ProjectArtifacts>,
  last_good_artifacts: Option<ProjectArtifacts>,
  last_good_revision: Option<u64>,
}

impl ProjectState {
  fn cached_artifacts(&self) -> Option<ProjectArtifacts> {
    self
      .cached_artifacts
      .clone()
      .or_else(|| self.last_good_artifacts.clone())
  }

  fn remember_generated(&mut self, revision: u64, artifacts: &ProjectArtifacts) -> bool {
    if self
      .last_good_revision
      .map_or(true, |last_good_revision| revision >= last_good_revision)
    {
      self.last_good_revision = Some(revision);
      self.last_good_artifacts = Some(artifacts.clone());
    }

    let is_current = self.revision == revision;
    if is_current {
      self.cached_artifacts = Some(artifacts.clone());
    }

    is_current
  }

  fn remember_diagnostics(
    &mut self,
    revision: u64,
    diagnostics: &ThebeDiagnosticsFile,
  ) -> (bool, Option<ProjectArtifacts>) {
    let artifacts = self
      .last_good_artifacts
      .as_ref()
      .or(self.cached_artifacts.as_ref())
      .map(|artifacts| {
        let mut artifacts = artifacts.clone();
        artifacts.diagnostics = diagnostics.clone();
        artifacts
      });

    let is_current = self.revision == revision;
    if is_current {
      self.cached_artifacts = artifacts.clone();
    }

    (is_current, artifacts)
  }
}

#[derive(Debug)]
pub struct Backend {
  client: Client,
  state: Arc<RwLock<BackendState>>,
}

impl Backend {
  #[must_use]
  pub fn new(client: Client) -> Self {
    Self {
      client,
      state: Arc::new(RwLock::new(BackendState::default())),
    }
  }

  fn set_document_overlay(&self, uri: &Url, text: String) -> Option<(PathBuf, u64)> {
    let (project_root, path) = tracked_project_file_from_uri(uri)?;
    let revision =
      write_backend_state(&self.state).set_document(project_root.clone(), path, text)?;
    Some((project_root, revision))
  }

  fn clear_document_overlay(&self, uri: &Url) -> Option<(PathBuf, u64)> {
    let (project_root, path) = tracked_project_file_from_uri(uri)?;
    let revision = write_backend_state(&self.state).clear_document(project_root.clone(), &path)?;
    Some((project_root, revision))
  }

  fn project_snapshot(&self, project_root: &Path) -> ProjectSnapshot {
    read_backend_state(&self.state).snapshot(project_root)
  }

  fn note_refresh_requested(&self, project_root: &Path, revision: u64) {
    write_backend_state(&self.state).note_refresh_requested(project_root, revision);
  }

  fn schedule_refresh(&self, uri: &Url, project_root: PathBuf, revision: u64) {
    self.note_refresh_requested(&project_root, revision);

    let client = self.client.clone();
    let state = Arc::clone(&self.state);
    let uri = uri.clone();

    tokio::spawn(async move {
      tokio::time::sleep(CHANGE_REFRESH_DEBOUNCE).await;

      let snapshot = {
        let state_guard = read_backend_state(&state);
        if !state_guard.should_run_debounced_refresh(&project_root, revision) {
          return;
        }
        state_guard.snapshot(&project_root)
      };

      if snapshot.revision != revision {
        return;
      }

      refresh_project(client, state, project_root, uri, snapshot).await;
    });
  }

  async fn refresh_project_for_uri_now(&self, uri: &Url, project_root: PathBuf, revision: u64) {
    self.note_refresh_requested(&project_root, revision);

    let snapshot = self.project_snapshot(&project_root);
    if snapshot.revision != revision {
      return;
    }

    refresh_project(
      self.client.clone(),
      Arc::clone(&self.state),
      project_root,
      uri.clone(),
      snapshot,
    )
    .await;
  }

  fn load_or_refresh_artifacts(&self, uri: &Url) -> Option<(PathBuf, ProjectArtifacts)> {
    let project_root = find_project_root_from_uri(uri)?;
    let snapshot = self.project_snapshot(&project_root);

    if let Some(artifacts) = snapshot.cached_artifacts {
      return Some((project_root, artifacts));
    }

    if let Ok(artifacts) = thebe_project::load_project_artifacts(&project_root) {
      return Some((project_root, artifacts));
    }

    match thebe_project::refresh_project_for_editor_with_overlay(&project_root, &snapshot.overlay)
      .ok()?
    {
      EditorRefresh::Generated(artifacts) => {
        let _ = write_backend_state(&self.state).remember_generated(
          &project_root,
          snapshot.revision,
          &artifacts,
        );
        Some((project_root, artifacts))
      }
      EditorRefresh::Diagnostics(diagnostics) => {
        let (_, cached_artifacts) = write_backend_state(&self.state).remember_diagnostics(
          &project_root,
          snapshot.revision,
          &diagnostics,
        );

        cached_artifacts
          .map(|artifacts| (project_root.clone(), artifacts))
          .or_else(|| {
            thebe_project::load_project_artifacts(&project_root)
              .ok()
              .map(|artifacts| (project_root, artifacts))
          })
      }
    }
  }
}

async fn refresh_project(
  client: Client,
  state: Arc<RwLock<BackendState>>,
  project_root: PathBuf,
  uri: Url,
  snapshot: ProjectSnapshot,
) {
  match thebe_project::refresh_project_for_editor_with_overlay(&project_root, &snapshot.overlay) {
    Ok(EditorRefresh::Generated(artifacts)) => {
      let should_publish = write_backend_state(&state).remember_generated(
        &project_root,
        snapshot.revision,
        &artifacts,
      );
      if !should_publish {
        return;
      }

      if let Err(err) = publish_project_diagnostics(&client, &project_root, &artifacts).await {
        client
          .log_message(MessageType::ERROR, format!("thebe-lsp: {err:#}"))
          .await;
      }
    }
    Ok(EditorRefresh::Diagnostics(diagnostics)) => {
      let (should_publish, cached_artifacts) = write_backend_state(&state).remember_diagnostics(
        &project_root,
        snapshot.revision,
        &diagnostics,
      );
      if !should_publish {
        return;
      }

      if let Some(artifacts) = cached_artifacts {
        if let Err(err) = publish_project_diagnostics(&client, &project_root, &artifacts).await {
          client
            .log_message(MessageType::ERROR, format!("thebe-lsp: {err:#}"))
            .await;
        }
      } else if let Ok(mut artifacts) = thebe_project::load_project_artifacts(&project_root) {
        artifacts.diagnostics = diagnostics.clone();
        if let Err(err) = publish_project_diagnostics(&client, &project_root, &artifacts).await {
          client
            .log_message(MessageType::ERROR, format!("thebe-lsp: {err:#}"))
            .await;
        }
      } else if let Err(err) =
        publish_diagnostics_without_manifest(&client, &project_root, &diagnostics).await
      {
        client
          .log_message(MessageType::ERROR, format!("thebe-lsp: {err:#}"))
          .await;
      }
    }
    Err(err) => {
      let should_publish =
        read_backend_state(&state).is_current_revision(&project_root, snapshot.revision);
      if !should_publish {
        return;
      }

      client
        .log_message(
          MessageType::WARNING,
          format!(
            "thebe-lsp: failed to refresh {} or {} for {}: {err:#}",
            THEBE_MANIFEST_FILE,
            THEBE_DIAGNOSTICS_FILE,
            project_root.display()
          ),
        )
        .await;
      client.publish_diagnostics(uri, Vec::new(), None).await;
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
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
          TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::FULL),
            save: Some(TextDocumentSyncSaveOptions::Supported(true)),
            ..TextDocumentSyncOptions::default()
          },
        )),
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
        "thebe-lsp initialized — compiler state now refreshes through the shared thebe-project crate",
      )
      .await;
  }

  async fn shutdown(&self) -> LspResult<()> {
    Ok(())
  }

  async fn did_open(&self, params: DidOpenTextDocumentParams) {
    if let Some((project_root, revision)) =
      self.set_document_overlay(&params.text_document.uri, params.text_document.text)
    {
      self
        .refresh_project_for_uri_now(&params.text_document.uri, project_root, revision)
        .await;
    }
  }

  async fn did_change(&self, params: DidChangeTextDocumentParams) {
    let Some(text) = params
      .content_changes
      .into_iter()
      .last()
      .map(|change| change.text)
    else {
      return;
    };

    if let Some((project_root, revision)) =
      self.set_document_overlay(&params.text_document.uri, text)
    {
      self.schedule_refresh(&params.text_document.uri, project_root, revision);
    }
  }

  async fn did_save(&self, params: DidSaveTextDocumentParams) {
    if let Some((project_root, revision)) = self.clear_document_overlay(&params.text_document.uri) {
      self
        .refresh_project_for_uri_now(&params.text_document.uri, project_root, revision)
        .await;
    }
  }

  async fn did_close(&self, params: DidCloseTextDocumentParams) {
    if let Some((project_root, revision)) = self.clear_document_overlay(&params.text_document.uri) {
      self
        .refresh_project_for_uri_now(&params.text_document.uri, project_root, revision)
        .await;
    }
  }

  async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some((project_root, artifacts)) = self.load_or_refresh_artifacts(uri) else {
      return Ok(None);
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
    let Some((project_root, artifacts)) = self.load_or_refresh_artifacts(uri) else {
      return Ok(None);
    };
    let Some(relative_path) = relative_path_from_uri(&project_root, uri) else {
      return Ok(None);
    };

    Ok(document_symbols_for_manifest_file(
      &artifacts.manifest,
      &relative_path,
    ))
  }

  async fn goto_definition(
    &self,
    params: GotoDefinitionParams,
  ) -> LspResult<Option<GotoDefinitionResponse>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some((project_root, artifacts)) = self.load_or_refresh_artifacts(uri) else {
      return Ok(None);
    };
    let Some(relative_path) = relative_path_from_uri(&project_root, uri) else {
      return Ok(None);
    };

    definition_for_manifest_file(
      &project_root,
      &artifacts.manifest,
      &relative_path,
      params.text_document_position_params.position,
    )
    .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())
  }

  async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
    let uri = &params.text_document_position.text_document.uri;
    let Some((project_root, artifacts)) = self.load_or_refresh_artifacts(uri) else {
      return Ok(None);
    };
    let Some(relative_path) = relative_path_from_uri(&project_root, uri) else {
      return Ok(None);
    };

    references_for_manifest_file(
      &project_root,
      &artifacts.manifest,
      &relative_path,
      params.text_document_position.position,
      params.context.include_declaration,
    )
    .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())
  }
}

fn find_project_root_from_uri(uri: &Url) -> Option<PathBuf> {
  let path = uri.to_file_path().ok()?;
  thebe_project::find_project_root_from(&path).ok()
}

fn read_backend_state(
  state: &Arc<RwLock<BackendState>>,
) -> std::sync::RwLockReadGuard<'_, BackendState> {
  state
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_backend_state(
  state: &Arc<RwLock<BackendState>>,
) -> std::sync::RwLockWriteGuard<'_, BackendState> {
  state
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn tracked_project_file_from_uri(uri: &Url) -> Option<(PathBuf, PathBuf)> {
  let path = uri.to_file_path().ok()?;
  let project_root = thebe_project::find_project_root_from(&path).ok()?;
  is_project_input_file(&project_root, &path).then_some((project_root, path))
}

fn is_project_input_file(project_root: &Path, path: &Path) -> bool {
  if path == project_root.join("Cargo.toml") || path == project_root.join("app.html") {
    return true;
  }

  let routes_dir = project_root.join("src/routes");
  path.starts_with(&routes_dir) && path.extension().is_some_and(|ext| ext == "trs")
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
        binding,
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
      binding,
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

fn definition_for_manifest_file(
  project_root: &Path,
  manifest: &ThebeManifest,
  relative_path: &str,
  position: Position,
) -> Result<Option<GotoDefinitionResponse>> {
  match find_route_document_match(manifest, relative_path) {
    Some(RouteDocumentMatch::Source(route)) => {
      if let Some(source_span) = route.handler.source_span
        && position_in_span(position, &source_span)
      {
        return file_start_definition(project_root, &route.generated_server_path).map(Some);
      }

      if let Some(binding) = route
        .template_binding_spans
        .iter()
        .find(|binding| position_in_span(position, &binding.source_span))
      {
        if let Some(generated_types_path) = route.generated_types_path.as_deref() {
          return file_start_definition(project_root, generated_types_path).map(Some);
        }
        if let Some(generated_client_path) = route.generated_client_path.as_deref() {
          return file_start_definition(project_root, generated_client_path).map(Some);
        }

        return location_for_relative_path(project_root, &route.source_path, binding.source_span)
          .map(|location| Some(GotoDefinitionResponse::Scalar(location)));
      }
    }
    Some(RouteDocumentMatch::GeneratedServer(route)) => {
      if let Some(source_span) = route.handler.source_span {
        let location = location_for_relative_path(project_root, &route.source_path, source_span)?;
        return Ok(Some(GotoDefinitionResponse::Scalar(location)));
      }
    }
    Some(RouteDocumentMatch::GeneratedClient(route))
    | Some(RouteDocumentMatch::GeneratedTypes(route)) => {
      let location = if let Some(source_span) = route.handler.source_span {
        location_for_relative_path(project_root, &route.source_path, source_span)?
      } else {
        file_start_location(project_root, &route.source_path)?
      };
      return Ok(Some(GotoDefinitionResponse::Scalar(location)));
    }
    None => {}
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
    let definition_span = layout
      .template_binding_spans
      .iter()
      .find(|candidate| candidate.name == binding.name)
      .map(|candidate| candidate.source_span)
      .unwrap_or(binding.source_span);
    let location = location_for_relative_path(project_root, &layout.source_path, definition_span)?;
    return Ok(Some(GotoDefinitionResponse::Scalar(location)));
  }

  Ok(None)
}

fn references_for_manifest_file(
  project_root: &Path,
  manifest: &ThebeManifest,
  relative_path: &str,
  position: Position,
  include_declaration: bool,
) -> Result<Option<Vec<Location>>> {
  let mut locations = Vec::new();

  match find_route_document_match(manifest, relative_path) {
    Some(RouteDocumentMatch::Source(route)) => {
      if let Some(source_span) = route.handler.source_span
        && position_in_span(position, &source_span)
      {
        if include_declaration {
          locations.push(location_for_relative_path(
            project_root,
            &route.source_path,
            source_span,
          )?);
        }
        locations.push(file_start_location(
          project_root,
          &route.generated_server_path,
        )?);
        return Ok((!locations.is_empty()).then_some(locations));
      }

      if let Some(binding) = route
        .template_binding_spans
        .iter()
        .find(|binding| position_in_span(position, &binding.source_span))
      {
        locations.extend(binding_reference_locations(
          project_root,
          &route.source_path,
          &route.template_binding_spans,
          &binding.name,
          include_declaration,
          Some(binding.source_span),
        )?);
        if let Some(generated_client_path) = route.generated_client_path.as_deref() {
          locations.push(file_start_location(project_root, generated_client_path)?);
        }
        if let Some(generated_types_path) = route.generated_types_path.as_deref() {
          locations.push(file_start_location(project_root, generated_types_path)?);
        }
        dedup_locations(&mut locations);
        return Ok((!locations.is_empty()).then_some(locations));
      }
    }
    Some(RouteDocumentMatch::GeneratedServer(route)) => {
      if include_declaration {
        locations.push(file_start_location(
          project_root,
          &route.generated_server_path,
        )?);
      }
      if let Some(source_span) = route.handler.source_span {
        locations.push(location_for_relative_path(
          project_root,
          &route.source_path,
          source_span,
        )?);
      }
      return Ok((!locations.is_empty()).then_some(locations));
    }
    Some(RouteDocumentMatch::GeneratedClient(route)) => {
      if include_declaration {
        if let Some(generated_client_path) = route.generated_client_path.as_deref() {
          locations.push(file_start_location(project_root, generated_client_path)?);
        }
      }
      if let Some(source_span) = route.handler.source_span {
        locations.push(location_for_relative_path(
          project_root,
          &route.source_path,
          source_span,
        )?);
      }
      if let Some(generated_types_path) = route.generated_types_path.as_deref() {
        locations.push(file_start_location(project_root, generated_types_path)?);
      }
      dedup_locations(&mut locations);
      return Ok((!locations.is_empty()).then_some(locations));
    }
    Some(RouteDocumentMatch::GeneratedTypes(route)) => {
      if include_declaration {
        if let Some(generated_types_path) = route.generated_types_path.as_deref() {
          locations.push(file_start_location(project_root, generated_types_path)?);
        }
      }
      if let Some(source_span) = route.handler.source_span {
        locations.push(location_for_relative_path(
          project_root,
          &route.source_path,
          source_span,
        )?);
      }
      if let Some(generated_client_path) = route.generated_client_path.as_deref() {
        locations.push(file_start_location(project_root, generated_client_path)?);
      }
      dedup_locations(&mut locations);
      return Ok((!locations.is_empty()).then_some(locations));
    }
    None => {}
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
    locations.extend(binding_reference_locations(
      project_root,
      &layout.source_path,
      &layout.template_binding_spans,
      &binding.name,
      include_declaration,
      Some(binding.source_span),
    )?);
  }

  dedup_locations(&mut locations);
  Ok((!locations.is_empty()).then_some(locations))
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

async fn publish_diagnostics_without_manifest(
  client: &Client,
  project_root: &Path,
  diagnostics: &ThebeDiagnosticsFile,
) -> Result<()> {
  let mut diagnostics_by_file = BTreeMap::<Url, Vec<Diagnostic>>::new();

  for diagnostic in &diagnostics.diagnostics {
    let Some(relative_path) = diagnostic.file_path.as_deref() else {
      continue;
    };
    let file_url = file_url(project_root, relative_path)?;
    diagnostics_by_file
      .entry(file_url)
      .or_default()
      .push(to_lsp_diagnostic(diagnostic));
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

fn find_route_document_match<'a>(
  manifest: &'a ThebeManifest,
  relative_path: &str,
) -> Option<RouteDocumentMatch<'a>> {
  for route in &manifest.routes {
    if route.source_path == relative_path {
      return Some(RouteDocumentMatch::Source(route));
    }
    if route.generated_server_path == relative_path {
      return Some(RouteDocumentMatch::GeneratedServer(route));
    }
    if route.generated_client_path.as_deref() == Some(relative_path) {
      return Some(RouteDocumentMatch::GeneratedClient(route));
    }
    if route.generated_types_path.as_deref() == Some(relative_path) {
      return Some(RouteDocumentMatch::GeneratedTypes(route));
    }
  }

  None
}

fn binding_reference_locations(
  project_root: &Path,
  relative_path: &str,
  binding_spans: &[TemplateBindingMetadata],
  binding_name: &str,
  include_declaration: bool,
  current_span: Option<SourceSpanMetadata>,
) -> Result<Vec<Location>> {
  binding_spans
    .iter()
    .filter(|binding| binding.name == binding_name)
    .filter(|binding| include_declaration || Some(binding.source_span) != current_span)
    .map(|binding| location_for_relative_path(project_root, relative_path, binding.source_span))
    .collect()
}

fn file_start_definition(
  project_root: &Path,
  relative_path: &str,
) -> Result<GotoDefinitionResponse> {
  Ok(GotoDefinitionResponse::Scalar(file_start_location(
    project_root,
    relative_path,
  )?))
}

fn file_start_location(project_root: &Path, relative_path: &str) -> Result<Location> {
  Ok(Location {
    uri: file_url(project_root, relative_path)?,
    range: full_document_range(),
  })
}

fn location_for_relative_path(
  project_root: &Path,
  relative_path: &str,
  span: SourceSpanMetadata,
) -> Result<Location> {
  Ok(Location {
    uri: file_url(project_root, relative_path)?,
    range: range_from_span(&span),
  })
}

fn dedup_locations(locations: &mut Vec<Location>) {
  locations.sort_by(|left, right| {
    left
      .uri
      .as_str()
      .cmp(right.uri.as_str())
      .then_with(|| compare_positions(left.range.start, right.range.start))
      .then_with(|| compare_positions(left.range.end, right.range.end))
  });
  locations.dedup_by(|left, right| left.uri == right.uri && left.range == right.range);
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

fn binding_hover(binding: &TemplateBindingMetadata, owner: &str) -> Hover {
  Hover {
    contents: HoverContents::Markup(MarkupContent {
      kind: MarkupKind::Markdown,
      value: format!("**Template binding** `{}`\n\n{owner}", binding.name),
    }),
    range: Some(range_from_span(&binding.source_span)),
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

enum RouteDocumentMatch<'a> {
  Source(&'a RouteMetadata),
  GeneratedServer(&'a RouteMetadata),
  GeneratedClient(&'a RouteMetadata),
  GeneratedTypes(&'a RouteMetadata),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn backend_state_only_bumps_revision_when_overlay_changes() {
    let project_root = PathBuf::from("/tmp/app");
    let route_path = project_root.join("src/routes/index.trs");
    let mut state = BackendState::default();

    assert_eq!(
      state.set_document(project_root.clone(), route_path.clone(), "one".to_owned()),
      Some(1)
    );
    assert_eq!(
      state.set_document(project_root.clone(), route_path.clone(), "one".to_owned()),
      None
    );
    assert_eq!(
      state.set_document(project_root.clone(), route_path.clone(), "two".to_owned()),
      Some(2)
    );
    assert_eq!(
      state.clear_document(project_root.clone(), &route_path),
      Some(3)
    );
    assert_eq!(state.clear_document(project_root, &route_path), None);
  }

  #[test]
  fn backend_state_only_runs_latest_debounced_refresh() {
    let project_root = PathBuf::from("/tmp/app");
    let mut state = BackendState::default();

    state.note_refresh_requested(&project_root, 1);
    assert!(state.should_run_debounced_refresh(&project_root, 1));

    state.note_refresh_requested(&project_root, 2);
    assert!(!state.should_run_debounced_refresh(&project_root, 1));
    assert!(state.should_run_debounced_refresh(&project_root, 2));
  }

  #[test]
  fn project_state_reuses_last_good_artifacts_for_current_diagnostics() {
    let mut project_state = ProjectState {
      revision: 2,
      ..ProjectState::default()
    };
    let good_artifacts = ProjectArtifacts {
      manifest: fixture_manifest(),
      diagnostics: ThebeDiagnosticsFile {
        version: 1,
        diagnostics: Vec::new(),
      },
    };
    let diagnostics = ThebeDiagnosticsFile {
      version: 1,
      diagnostics: vec![ThebeDiagnostic {
        kind: "file".to_owned(),
        severity: "error".to_owned(),
        category: "client-script".to_owned(),
        code: "analyzer-error".to_owned(),
        message: "parse error".to_owned(),
        file_path: Some("src/routes/profile.trs".to_owned()),
        source_span: None,
      }],
    };

    assert!(!project_state.remember_generated(1, &good_artifacts));

    let (is_current, cached_artifacts) = project_state.remember_diagnostics(2, &diagnostics);
    let cached_artifacts = cached_artifacts.expect("expected cached artifacts");

    assert!(is_current);
    assert_eq!(
      cached_artifacts.manifest.routes[0].source_path,
      "src/routes/profile.trs"
    );
    assert_eq!(
      cached_artifacts.diagnostics.diagnostics[0].code,
      "analyzer-error"
    );
  }

  #[test]
  fn project_input_files_include_routes_and_shared_inputs() {
    let project_root = Path::new("/tmp/app");

    assert!(is_project_input_file(
      project_root,
      &project_root.join("src/routes/index.trs"),
    ));
    assert!(is_project_input_file(
      project_root,
      &project_root.join("src/routes/blog/_layout.trs"),
    ));
    assert!(is_project_input_file(
      project_root,
      &project_root.join("Cargo.toml"),
    ));
    assert!(is_project_input_file(
      project_root,
      &project_root.join("app.html"),
    ));
  }

  #[test]
  fn project_input_files_ignore_generated_and_unrelated_paths() {
    let project_root = Path::new("/tmp/app");

    assert!(!is_project_input_file(
      project_root,
      &project_root.join("src/main.rs"),
    ));
    assert!(!is_project_input_file(
      project_root,
      &project_root.join(".thebe/server/routes/index.rs"),
    ));
    assert!(!is_project_input_file(
      project_root,
      &project_root.join("README.md"),
    ));
  }

  fn fixture_manifest() -> ThebeManifest {
    ThebeManifest {
      version: 3,
      server_router_path: ".thebe/server/routes.rs".to_owned(),
      app_html: thebe_project::AppHtmlMetadata {
        source_path: Some("app.html".to_owned()),
        uses_default: false,
      },
      layouts: vec![LayoutMetadata {
        has_head: true,
        has_style: true,
        scope_path: "_layout".to_owned(),
        source_path: "src/routes/_layout.trs".to_owned(),
        template_bindings: vec!["nav_title".to_owned()],
        template_binding_spans: vec![TemplateBindingMetadata {
          name: "nav_title".to_owned(),
          source_span: SourceSpanMetadata {
            start_byte: 20,
            end_byte: 33,
            start_line: 4,
            start_column: 3,
            end_line: 4,
            end_column: 16,
          },
        }],
      }],
      routes: vec![RouteMetadata {
        generated_client_path: Some(".thebe/client/routes/profile.ts".to_owned()),
        generated_server_path: ".thebe/server/routes/profile.rs".to_owned(),
        generated_types_path: Some(".thebe/types/routes/profile.ts".to_owned()),
        handler: thebe_project::HandlerMetadata {
          is_async: false,
          method: "get".to_owned(),
          name: "handler".to_owned(),
          param_types: vec!["State<crate::AppState>".to_owned()],
          source_span: Some(SourceSpanMetadata {
            start_byte: 67,
            end_byte: 94,
            start_line: 7,
            start_column: 1,
            end_line: 7,
            end_column: 28,
          }),
        },
        has_client_script: true,
        has_head: false,
        has_style: true,
        layout_scope_path: Some("_layout".to_owned()),
        layout_source_path: Some("src/routes/_layout.trs".to_owned()),
        module_name: "route__profile".to_owned(),
        route_path: "/profile".to_owned(),
        source_path: "src/routes/profile.trs".to_owned(),
        state_type: Some("crate::AppState".to_owned()),
        template_bindings: vec!["username".to_owned()],
        template_binding_spans: vec![
          TemplateBindingMetadata {
            name: "username".to_owned(),
            source_span: SourceSpanMetadata {
              start_byte: 367,
              end_byte: 381,
              start_line: 25,
              start_column: 17,
              end_line: 25,
              end_column: 31,
            },
          },
          TemplateBindingMetadata {
            name: "username".to_owned(),
            source_span: SourceSpanMetadata {
              start_byte: 403,
              end_byte: 417,
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
      start_byte: 1,
      end_byte: 11,
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
  fn definition_for_handler_points_to_generated_server_file() {
    let manifest = fixture_manifest();
    let response = definition_for_manifest_file(
      Path::new("/tmp/app"),
      &manifest,
      "src/routes/profile.trs",
      Position::new(6, 3),
    )
    .unwrap()
    .unwrap();
    let GotoDefinitionResponse::Scalar(location) = response else {
      panic!("expected scalar definition");
    };

    assert!(
      location
        .uri
        .path()
        .ends_with("/.thebe/server/routes/profile.rs")
    );
  }

  #[test]
  fn definition_for_generated_server_points_back_to_source_handler() {
    let manifest = fixture_manifest();
    let response = definition_for_manifest_file(
      Path::new("/tmp/app"),
      &manifest,
      ".thebe/server/routes/profile.rs",
      Position::new(0, 0),
    )
    .unwrap()
    .unwrap();
    let GotoDefinitionResponse::Scalar(location) = response else {
      panic!("expected scalar definition");
    };

    assert!(location.uri.path().ends_with("/src/routes/profile.trs"));
    assert_eq!(location.range.start, Position::new(6, 0));
  }

  #[test]
  fn references_for_binding_include_all_occurrences_and_generated_files() {
    let manifest = fixture_manifest();
    let locations = references_for_manifest_file(
      Path::new("/tmp/app"),
      &manifest,
      "src/routes/profile.trs",
      Position::new(24, 18),
      true,
    )
    .unwrap()
    .unwrap();

    assert_eq!(locations.len(), 4);
    assert!(
      locations
        .iter()
        .any(|location| location.uri.path().ends_with("/src/routes/profile.trs"))
    );
    assert!(locations.iter().any(|location| {
      location
        .uri
        .path()
        .ends_with("/.thebe/client/routes/profile.ts")
    }));
    assert!(locations.iter().any(|location| {
      location
        .uri
        .path()
        .ends_with("/.thebe/types/routes/profile.ts")
    }));
  }
}
