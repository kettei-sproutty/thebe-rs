use anyhow::Result;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use thebe_parser::SfcBlocks;
use thebe_project::{
  ComponentMetadata, ComponentPropMetadata, EditorRefresh, LayoutMetadata, ProjectArtifacts,
  ProjectOverlay, RouteMetadata, SourceSpanMetadata, THEBE_DIAGNOSTICS_FILE,
  THEBE_MANIFEST_FILE, TemplateBindingMetadata, TemplateSymbolMetadata, ThebeDiagnostic,
  ThebeDiagnosticsFile, ThebeManifest,
};
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
  CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
  CodeActionProviderCapability, CompletionItem, CompletionItemKind, CompletionOptions,
  CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic, DiagnosticSeverity,
  DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
  DidSaveTextDocumentParams, DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams,
  DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover,
  HoverContents, HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
  InsertTextFormat, Location, MarkupContent, MarkupKind, MessageType, NumberOrString, OneOf,
  Position, PrepareRenameResponse, Range, ReferenceParams, RenameOptions, RenameParams,
  SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
  SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
  SemanticTokensParams, SemanticTokensResult, SemanticTokensServerCapabilities,
  ServerCapabilities, ServerInfo, SymbolKind, TextDocumentSyncCapability,
  TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Url,
  WorkspaceEdit,
};
use tower_lsp::{Client, LanguageServer};

const CHANGE_REFRESH_DEBOUNCE: Duration = Duration::from_millis(150);

type DiagnosticsByFile = BTreeMap<Url, Vec<Diagnostic>>;
type DiagnosticPublishBatch = Vec<(Url, Vec<Diagnostic>)>;

const COMPLETION_TRIGGER_CHARACTERS: &[&str] = &["<", "{", ".", "\"", ":", "/"];
const SEMANTIC_TOKEN_TYPES: &[SemanticTokenType] = &[
  SemanticTokenType::CLASS,
  SemanticTokenType::FUNCTION,
  SemanticTokenType::KEYWORD,
  SemanticTokenType::PROPERTY,
  SemanticTokenType::VARIABLE,
];
const SEMANTIC_TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[];
const TOKEN_TYPE_CLASS: u32 = 0;
const TOKEN_TYPE_FUNCTION: u32 = 1;
const TOKEN_TYPE_KEYWORD: u32 = 2;
const TOKEN_TYPE_PROPERTY: u32 = 3;
const TOKEN_TYPE_VARIABLE: u32 = 4;
const EVENT_ATTRIBUTE_COMPLETIONS: &[&str] = &[
  "onclick",
  "onchange",
  "oninput",
  "onsubmit",
  "onkeydown",
  "onkeyup",
  "onfocus",
  "onblur",
];

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

  fn coalesce_diagnostic_updates(
    &mut self,
    project_root: &Path,
    next_diagnostics: DiagnosticsByFile,
  ) -> DiagnosticPublishBatch {
    self
      .projects
      .entry(project_root.to_path_buf())
      .or_default()
      .coalesce_diagnostic_updates(next_diagnostics)
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

#[derive(Debug, Clone)]
struct DocumentContext {
  project_root: PathBuf,
  relative_path: String,
  source: String,
  cached_artifacts: Option<ProjectArtifacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRange {
  start: usize,
  end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionContext {
  BlockTag { prefix: String, replace: ByteRange },
  ComponentTag { prefix: String, replace: ByteRange },
  AttributeName {
    tag_name: String,
    prefix: String,
    replace: ByteRange,
  },
  TemplateBinding { prefix: String, replace: ByteRange },
  EventHandler { prefix: String, replace: ByteRange },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComponentImport {
  module_path: String,
  tag_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SemanticTokenSpan {
  end: usize,
  start: usize,
  token_type: u32,
}

#[derive(Debug, Default)]
struct ProjectState {
  revision: u64,
  scheduled_refresh_revision: Option<u64>,
  cached_artifacts: Option<ProjectArtifacts>,
  last_good_artifacts: Option<ProjectArtifacts>,
  last_good_revision: Option<u64>,
  published_diagnostics: DiagnosticsByFile,
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

  fn coalesce_diagnostic_updates(
    &mut self,
    mut next_diagnostics: DiagnosticsByFile,
  ) -> DiagnosticPublishBatch {
    for (uri, diagnostics) in &self.published_diagnostics {
      if diagnostics.is_empty() && !next_diagnostics.contains_key(uri) {
        next_diagnostics.insert(uri.clone(), Vec::new());
      }
    }

    let mut updates = Vec::new();

    for (uri, diagnostics) in &next_diagnostics {
      if self.published_diagnostics.get(uri) != Some(diagnostics) {
        updates.push((uri.clone(), diagnostics.clone()));
      }
    }

    for (uri, diagnostics) in &self.published_diagnostics {
      if diagnostics.is_empty() || next_diagnostics.contains_key(uri) {
        continue;
      }

      updates.push((uri.clone(), Vec::new()));
    }

    self.published_diagnostics = next_diagnostics;
    updates
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

  fn document_context(&self, uri: &Url) -> Option<DocumentContext> {
    let path = uri.to_file_path().ok()?;
    let project_root = thebe_project::find_project_root_from(&path).ok()?;
    let relative_path = relative_path_from_uri(&project_root, uri)?;
    let source = read_backend_state(&self.state)
      .open_documents
      .files
      .get(&path)
      .cloned()
      .or_else(|| std::fs::read_to_string(&path).ok())?;
    let cached_artifacts = self
      .load_or_refresh_artifacts(uri)
      .map(|(_, artifacts)| artifacts);

    Some(DocumentContext {
      project_root,
      relative_path,
      source,
      cached_artifacts,
    })
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

      if let Err(err) =
        publish_project_diagnostics(&client, &state, &project_root, &artifacts).await
      {
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
        if let Err(err) =
          publish_project_diagnostics(&client, &state, &project_root, &artifacts).await
        {
          client
            .log_message(MessageType::ERROR, format!("thebe-lsp: {err:#}"))
            .await;
        }
      } else if let Ok(mut artifacts) = thebe_project::load_project_artifacts(&project_root) {
        artifacts.diagnostics = diagnostics.clone();
        if let Err(err) =
          publish_project_diagnostics(&client, &state, &project_root, &artifacts).await
        {
          client
            .log_message(MessageType::ERROR, format!("thebe-lsp: {err:#}"))
            .await;
        }
      } else if let Err(err) =
        publish_diagnostics_without_manifest(&client, &state, &project_root, &diagnostics).await
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
        rename_provider: Some(OneOf::Right(RenameOptions {
          prepare_provider: Some(true),
          work_done_progress_options: Default::default(),
        })),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
          resolve_provider: Some(false),
          trigger_characters: Some(
            COMPLETION_TRIGGER_CHARACTERS
              .iter()
              .map(|character| (*character).to_owned())
              .collect(),
          ),
          ..CompletionOptions::default()
        }),
        semantic_tokens_provider: Some(
          SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
            legend: SemanticTokensLegend {
              token_types: SEMANTIC_TOKEN_TYPES.to_vec(),
              token_modifiers: SEMANTIC_TOKEN_MODIFIERS.to_vec(),
            },
            full: Some(SemanticTokensFullOptions::Bool(true)),
            range: None,
            work_done_progress_options: Default::default(),
          }),
        ),
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

  async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
    let uri = &params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };

    if !document.relative_path.ends_with(".trs") {
      return Ok(None);
    }

    let Some(context) = classify_completion_context(&document.source, position) else {
      return Ok(None);
    };

    let items = completion_items_for_context(&document, &context);
    if items.is_empty() {
      Ok(None)
    } else {
      Ok(Some(CompletionResponse::Array(items)))
    }
  }

  async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };
    let Some(artifacts) = document.cached_artifacts.as_ref() else {
      return Ok(None);
    };

    Ok(hover_for_document(
      &document.source,
      &artifacts.manifest,
      &document.relative_path,
      params.text_document_position_params.position,
    ))
  }

  async fn document_symbol(
    &self,
    params: DocumentSymbolParams,
  ) -> LspResult<Option<DocumentSymbolResponse>> {
    let uri = &params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };
    let Some(artifacts) = document.cached_artifacts.as_ref() else {
      return Ok(None);
    };

    Ok(document_symbols_for_document(
      &document.source,
      &artifacts.manifest,
      &document.relative_path,
    ))
  }

  async fn goto_definition(
    &self,
    params: GotoDefinitionParams,
  ) -> LspResult<Option<GotoDefinitionResponse>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };
    let Some(artifacts) = document.cached_artifacts.as_ref() else {
      return Ok(None);
    };

    definition_for_document(
      &document.project_root,
      &document.source,
      &artifacts.manifest,
      &document.relative_path,
      params.text_document_position_params.position,
    )
    .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())
  }

  async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
    let uri = &params.text_document_position.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };
    let Some(artifacts) = document.cached_artifacts.as_ref() else {
      return Ok(None);
    };

    references_for_document(
      &document.project_root,
      &document.source,
      &artifacts.manifest,
      &document.relative_path,
      params.text_document_position.position,
      params.context.include_declaration,
    )
    .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())
  }

  async fn prepare_rename(
    &self,
    params: tower_lsp::lsp_types::TextDocumentPositionParams,
  ) -> LspResult<Option<PrepareRenameResponse>> {
    let uri = &params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };

    Ok(prepare_rename_for_document(&document, params.position))
  }

  async fn rename(&self, params: RenameParams) -> LspResult<Option<WorkspaceEdit>> {
    let uri = &params.text_document_position.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };

    Ok(rename_for_document(
      &document,
      params.text_document_position.position,
      &params.new_name,
    ))
  }

  async fn code_action(
    &self,
    params: CodeActionParams,
  ) -> LspResult<Option<Vec<CodeActionOrCommand>>> {
    let uri = &params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };

    Ok(code_actions_for_document(&document, &params))
  }

  async fn formatting(&self, params: DocumentFormattingParams) -> LspResult<Option<Vec<TextEdit>>> {
    let uri = &params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };

    Ok(format_document(&document))
  }

  async fn semantic_tokens_full(
    &self,
    params: SemanticTokensParams,
  ) -> LspResult<Option<SemanticTokensResult>> {
    let uri = &params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };

    Ok(semantic_tokens_for_document(&document).map(SemanticTokensResult::Tokens))
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
  let components_dir = project_root.join("src/components");
  (path.starts_with(&routes_dir) || path.starts_with(&components_dir))
    && path.extension().is_some_and(|ext| ext == "trs")
}

fn classify_completion_context(source: &str, position: Position) -> Option<CompletionContext> {
  let offset = byte_offset_from_position(source, position)?;

  event_handler_context(source, offset)
    .or_else(|| template_binding_context(source, offset))
    .or_else(|| attribute_name_context(source, offset))
    .or_else(|| component_tag_context(source, offset))
    .or_else(|| block_tag_context(source, offset))
}

fn completion_items_for_context(
  document: &DocumentContext,
  context: &CompletionContext,
) -> Vec<CompletionItem> {
  match context {
    CompletionContext::BlockTag { prefix, replace } => {
      block_completion_items(document, prefix, *replace)
    }
    CompletionContext::ComponentTag { prefix, replace } => {
      component_tag_completion_items(document, prefix, *replace)
    }
    CompletionContext::AttributeName {
      tag_name,
      prefix,
      replace,
    } => attribute_completion_items(document, tag_name, prefix, *replace),
    CompletionContext::TemplateBinding { prefix, replace } => {
      template_binding_completion_items(document, prefix, *replace)
    }
    CompletionContext::EventHandler { prefix, replace } => {
      event_handler_completion_items(document, prefix, *replace)
    }
  }
}

fn block_completion_items(
  document: &DocumentContext,
  prefix: &str,
  replace: ByteRange,
) -> Vec<CompletionItem> {
  let blocks = parse_document_blocks(document).ok();
  let mut items = vec![(
    "head",
    "<head>\n  $0\n</head>",
    CompletionItemKind::SNIPPET,
    blocks
      .as_ref()
      .is_none_or(|blocks| blocks.head_span.is_none()),
  )];

  if is_component_path(&document.relative_path) {
    items.push((
      "script",
      "<script>\n$0\n</script>",
      CompletionItemKind::SNIPPET,
      blocks.as_ref().is_none_or(|blocks| blocks.script_span.is_none()),
    ));
  } else {
    items.push((
      "script setup",
      "<script setup>\n$0\n</script>",
      CompletionItemKind::SNIPPET,
      blocks
        .as_ref()
        .is_none_or(|blocks| blocks.script_setup_span.is_none()),
    ));
  }

  items.extend([
    (
      "script lang=\"ts\"",
      "<script lang=\"ts\">\n$0\n</script>",
      CompletionItemKind::SNIPPET,
      blocks
        .as_ref()
        .is_none_or(|blocks| blocks.script_ts_span.is_none()),
    ),
    (
      "style",
      "<style>\n$0\n</style>",
      CompletionItemKind::SNIPPET,
      blocks
        .as_ref()
        .is_none_or(|blocks| blocks.style_span.is_none()),
    ),
  ]);

  items
  .into_iter()
  .filter(|(label, _, _, enabled)| *enabled && label.starts_with(prefix))
  .map(|(label, snippet, kind, _)| {
    snippet_completion_item(&document.source, label, snippet, kind, replace)
  })
  .collect()
}

fn component_tag_completion_items(
  document: &DocumentContext,
  prefix: &str,
  replace: ByteRange,
) -> Vec<CompletionItem> {
  let mut imports = current_component_imports(document);
  imports.sort_by(|left, right| left.tag_name.cmp(&right.tag_name));
  imports.dedup_by(|left, right| left.tag_name == right.tag_name);

  imports
    .into_iter()
    .filter(|component| component.tag_name.starts_with(prefix))
    .map(|component| CompletionItem {
      label: component.tag_name.clone(),
      kind: Some(CompletionItemKind::CLASS),
      detail: Some(component.module_path),
      text_edit: Some(CompletionTextEdit::Edit(TextEdit {
        range: range_from_offsets(&document.source, replace.start, replace.end),
        new_text: component.tag_name,
      })),
      ..CompletionItem::default()
    })
    .collect()
}

fn attribute_completion_items(
  document: &DocumentContext,
  tag_name: &str,
  prefix: &str,
  replace: ByteRange,
) -> Vec<CompletionItem> {
  let mut items = vec![
    snippet_completion_item(
      &document.source,
      ":if",
      ":if=\"$1\"",
      CompletionItemKind::KEYWORD,
      replace,
    ),
    snippet_completion_item(
      &document.source,
      ":class",
      ":class=\"$1\"",
      CompletionItemKind::KEYWORD,
      replace,
    ),
    snippet_completion_item(
      &document.source,
      ":attr",
      ":${1:attr}=\"$2\"",
      CompletionItemKind::SNIPPET,
      replace,
    ),
  ];

  items.extend(EVENT_ATTRIBUTE_COMPLETIONS.iter().map(|attribute| {
    snippet_completion_item(
      &document.source,
      attribute,
      &format!("{attribute}=\"$1\""),
      CompletionItemKind::EVENT,
      replace,
    )
  }));

  if let Some(component) = component_metadata_for_tag(document, tag_name) {
    items.extend(component.props.iter().map(|prop| CompletionItem {
      label: format!(":{}", prop.name),
      kind: Some(CompletionItemKind::PROPERTY),
      detail: Some(prop.type_name.clone()),
      insert_text_format: Some(InsertTextFormat::SNIPPET),
      text_edit: Some(CompletionTextEdit::Edit(TextEdit {
        range: range_from_offsets(&document.source, replace.start, replace.end),
        new_text: format!(":{}=\"$1\"", prop.name),
      })),
      ..CompletionItem::default()
    }));
  }

  items
    .into_iter()
    .filter(|item| item.label.starts_with(prefix))
    .collect()
}

fn template_binding_completion_items(
  document: &DocumentContext,
  prefix: &str,
  replace: ByteRange,
) -> Vec<CompletionItem> {
  let mut symbols = current_template_symbols(document);
  symbols.sort();
  symbols.dedup();

  symbols
    .into_iter()
    .filter(|symbol| symbol.starts_with(prefix))
    .map(|symbol| {
      plain_completion_item(
        &document.source,
        &symbol,
        CompletionItemKind::VARIABLE,
        replace,
      )
    })
    .collect()
}

fn event_handler_completion_items(
  document: &DocumentContext,
  prefix: &str,
  replace: ByteRange,
) -> Vec<CompletionItem> {
  let mut handlers = current_event_handlers(document);
  handlers.sort();
  handlers.dedup();

  handlers
    .into_iter()
    .filter(|handler| handler.starts_with(prefix))
    .map(|handler| {
      plain_completion_item(
        &document.source,
        &handler,
        CompletionItemKind::FUNCTION,
        replace,
      )
    })
    .collect()
}

fn current_template_symbols(document: &DocumentContext) -> Vec<String> {
  let mut symbols = document
    .cached_artifacts
    .as_ref()
    .map(|artifacts| template_symbols_for_path(&artifacts.manifest, &document.relative_path))
    .unwrap_or_default();

  if is_component_path(&document.relative_path) {
    if let Ok(blocks) = parse_component_source_blocks(&document.source) {
      if let Some(script) = blocks.script.as_deref()
        && let Ok(current_symbols) = thebe_codegen::props_symbol_definitions(script)
      {
        symbols.extend(
          current_symbols
            .into_iter()
            .map(|definition| format!("props.{}", definition.path)),
        );
      }

      if let Ok(current_bindings) = thebe_codegen::list_template_bindings(&blocks.template) {
        symbols.extend(current_bindings);
      }
    }
  } else if let Ok(blocks) = parse_source_blocks(&document.source) {
    if let Ok(current_symbols) = thebe_codegen::route_template_symbols(&blocks) {
      symbols.extend(current_symbols);
    }

    if let Ok(current_bindings) = thebe_codegen::list_template_bindings(&blocks.template) {
      symbols.extend(current_bindings);
    }
  }

  symbols
}

fn current_event_handlers(document: &DocumentContext) -> Vec<String> {
  let Ok(blocks) = parse_document_blocks(document) else {
    return Vec::new();
  };
  let Some(script_ts) = blocks.script_ts.as_deref() else {
    return Vec::new();
  };

  thebe_analyzer::analyze(script_ts, false)
    .map(|module| module.event_fns)
    .unwrap_or_default()
}

fn template_symbols_for_path(manifest: &ThebeManifest, relative_path: &str) -> Vec<String> {
  if let Some(route) = manifest
    .routes
    .iter()
    .find(|route| route.source_path == relative_path)
  {
    let mut symbols = route.template_symbols.clone();
    symbols.extend(route.template_bindings.clone());
    return symbols;
  }

  if let Some(layout) = manifest
    .layouts
    .iter()
    .find(|layout| layout.source_path == relative_path)
  {
    return layout.template_bindings.clone();
  }

  if let Some(component) = manifest
    .components
    .iter()
    .find(|component| component.source_path == relative_path)
  {
    return component
      .prop_names
      .iter()
      .map(|name| format!("props.{name}"))
      .collect();
  }

  Vec::new()
}

fn parse_source_blocks(source: &str) -> std::result::Result<SfcBlocks, thebe_parser::ParseError> {
  thebe_parser::parse_sfc(source)
}

fn parse_component_source_blocks(
  source: &str,
) -> std::result::Result<SfcBlocks, thebe_parser::ParseError> {
  thebe_parser::parse_component_sfc(source)
}

fn parse_document_blocks(
  document: &DocumentContext,
) -> std::result::Result<SfcBlocks, thebe_parser::ParseError> {
  if is_component_path(&document.relative_path) {
    parse_component_source_blocks(&document.source)
  } else {
    parse_source_blocks(&document.source)
  }
}

fn template_binding_context(source: &str, offset: usize) -> Option<CompletionContext> {
  let open = source[..offset].rfind("{{")?;
  if source[..offset]
    .rfind("}}")
    .is_some_and(|close| close > open)
  {
    return None;
  }

  let close = source[offset..]
    .find("}}")
    .map_or(source.len(), |relative| offset + relative);
  let content_start = trim_ascii_whitespace_start(source, open + 2, close);
  let content_end = trim_ascii_whitespace_end(source, content_start, close);
  if offset < content_start || offset > close {
    return None;
  }

  Some(CompletionContext::TemplateBinding {
    prefix: source[content_start..offset].trim().to_owned(),
    replace: ByteRange {
      start: content_start,
      end: content_end.max(content_start),
    },
  })
}

fn event_handler_context(source: &str, offset: usize) -> Option<CompletionContext> {
  let tag_start = source[..offset].rfind('<')?;
  if source[..offset]
    .rfind('>')
    .is_some_and(|end| end > tag_start)
  {
    return None;
  }

  let tag_prefix = &source[tag_start..offset];
  let (attribute_name, value_start) = open_attribute_value(tag_prefix)?;
  if !attribute_name.starts_with("on") {
    return None;
  }

  let absolute_value_start = tag_start + value_start;
  Some(CompletionContext::EventHandler {
    prefix: source[absolute_value_start..offset].trim().to_owned(),
    replace: ByteRange {
      start: absolute_value_start,
      end: offset,
    },
  })
}

fn open_attribute_value(tag_prefix: &str) -> Option<(String, usize)> {
  let bytes = tag_prefix.as_bytes();
  let mut idx = 1usize;

  while idx < bytes.len() {
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
      idx += 1;
    }

    if idx >= bytes.len() || matches!(bytes[idx], b'>' | b'/') {
      return None;
    }

    let name_start = idx;
    while idx < bytes.len()
      && (bytes[idx].is_ascii_alphanumeric() || matches!(bytes[idx], b':' | b'-' | b'_'))
    {
      idx += 1;
    }

    if name_start == idx {
      idx += 1;
      continue;
    }

    let name = tag_prefix[name_start..idx].to_owned();
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
      idx += 1;
    }

    if idx >= bytes.len() || bytes[idx] != b'=' {
      continue;
    }
    idx += 1;

    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
      idx += 1;
    }

    if idx >= bytes.len() {
      return None;
    }

    let quote = bytes[idx];
    if quote != b'\'' && quote != b'"' {
      while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() && bytes[idx] != b'>' {
        idx += 1;
      }
      continue;
    }

    let value_start = idx + 1;
    idx += 1;
    if tag_prefix[idx..].contains(quote as char) {
      let next = tag_prefix[idx..]
        .find(quote as char)
        .expect("quoted value close exists");
      idx += next + 1;
      continue;
    }

    return Some((name, value_start));
  }

  None
}

fn attribute_name_context(source: &str, offset: usize) -> Option<CompletionContext> {
  let tag_start = source[..offset].rfind('<')?;
  if source[..offset]
    .rfind('>')
    .is_some_and(|end| end > tag_start)
  {
    return None;
  }

  let tag_prefix = &source[tag_start..offset];
  if open_attribute_value(tag_prefix).is_some() {
    return None;
  }

  let (tag_name, tag_name_end) = open_tag_name(tag_prefix)?;
  let after_tag_name = &tag_prefix[tag_name_end..];
  if !after_tag_name.contains(char::is_whitespace) && !after_tag_name.is_empty() {
    return None;
  }

  let (prefix, start) = current_attribute_name_prefix(tag_prefix, tag_name_end)?;
  Some(CompletionContext::AttributeName {
    tag_name,
    prefix,
    replace: ByteRange {
      start: tag_start + start,
      end: offset,
    },
  })
}

fn component_tag_context(source: &str, offset: usize) -> Option<CompletionContext> {
  let tag_start = source[..offset].rfind('<')?;
  if source[..offset]
    .rfind('>')
    .is_some_and(|end| end > tag_start)
  {
    return None;
  }

  let tag_prefix = &source[tag_start..offset];
  let trimmed = tag_prefix[1..].trim_start_matches('/');
  if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
    return None;
  }
  if !trimmed.chars().next()?.is_ascii_uppercase() {
    return None;
  }

  Some(CompletionContext::ComponentTag {
    prefix: trimmed.to_owned(),
    replace: ByteRange {
      start: offset - trimmed.len(),
      end: offset,
    },
  })
}

fn open_tag_name(tag_prefix: &str) -> Option<(String, usize)> {
  let bytes = tag_prefix.as_bytes();
  let mut idx = 1usize;

  if bytes.get(idx).is_some_and(|byte| *byte == b'/') {
    idx += 1;
  }
  while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
    idx += 1;
  }

  let start = idx;
  while idx < bytes.len()
    && (bytes[idx].is_ascii_alphanumeric() || matches!(bytes[idx], b':' | b'-' | b'_'))
  {
    idx += 1;
  }

  (start < idx).then(|| (tag_prefix[start..idx].to_owned(), idx))
}

fn current_attribute_name_prefix(tag_prefix: &str, tag_name_end: usize) -> Option<(String, usize)> {
  let bytes = tag_prefix.as_bytes();
  let mut idx = bytes.len();

  while idx > tag_name_end && bytes[idx - 1].is_ascii_whitespace() {
    idx -= 1;
  }

  if idx == tag_name_end {
    return Some((String::new(), bytes.len()));
  }

  let mut start = idx;
  while start > tag_name_end
    && !bytes[start - 1].is_ascii_whitespace()
    && bytes[start - 1] != b'<'
  {
    start -= 1;
  }

  let token = &tag_prefix[start..idx];
  if token.contains('=') || token.ends_with('"') || token.ends_with('\'') {
    return Some((String::new(), bytes.len()));
  }

  Some((token.to_owned(), start))
}

fn block_tag_context(source: &str, offset: usize) -> Option<CompletionContext> {
  let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
  let prefix = &source[line_start..offset];
  let trimmed_prefix = prefix.trim_start();
  let trimmed_start = line_start + prefix.len() - trimmed_prefix.len();
  if !trimmed_prefix.starts_with('<') || trimmed_prefix.contains('>') {
    return None;
  }

  let tag_prefix = trimmed_prefix[1..].trim_start_matches('/');
  if tag_prefix.contains(char::is_whitespace) {
    return None;
  }

  Some(CompletionContext::BlockTag {
    prefix: tag_prefix.to_owned(),
    replace: ByteRange {
      start: trimmed_start,
      end: offset,
    },
  })
}

fn is_component_path(relative_path: &str) -> bool {
  relative_path.starts_with("src/components/") && relative_path.ends_with(".trs")
}

fn current_component_imports(document: &DocumentContext) -> Vec<ComponentImport> {
  let mut imports = Vec::new();

  if !is_component_path(&document.relative_path)
    && let Ok(blocks) = parse_source_blocks(&document.source)
    && let Some(script_setup) = blocks.script_setup.as_deref()
  {
    for line in script_setup.lines() {
      let line = line.trim();
      if !line.starts_with("use ") || !line.ends_with(';') || !line.contains("crate::components::")
      {
        continue;
      }

      let import = line.trim_start_matches("use ").trim_end_matches(';').trim();
      if import.contains('{') {
        continue;
      }

      let (module_path, tag_name) = if let Some((path, alias)) = import.split_once(" as ") {
        (path.trim().to_owned(), alias.trim().to_owned())
      } else {
        let Some(tag_name) = import.rsplit("::").next() else {
          continue;
        };
        (import.to_owned(), tag_name.to_owned())
      };

      imports.push(ComponentImport {
        module_path,
        tag_name,
      });
    }
  }

  if imports.is_empty()
    && let Some(artifacts) = document.cached_artifacts.as_ref()
  {
    imports.extend(artifacts.manifest.components.iter().map(|component| ComponentImport {
      module_path: component.module_path.clone(),
      tag_name: component.tag_name.clone(),
    }));
  }

  imports
}

fn component_metadata_for_tag<'a>(
  document: &'a DocumentContext,
  tag_name: &str,
) -> Option<&'a ComponentMetadata> {
  let manifest = &document.cached_artifacts.as_ref()?.manifest;
  let imports = current_component_imports(document);

  if let Some(imported) = imports.iter().find(|component| component.tag_name == tag_name) {
    return manifest
      .components
      .iter()
      .find(|component| component.module_path == imported.module_path);
  }

  manifest
    .components
    .iter()
    .find(|component| component.tag_name == tag_name)
}

fn snippet_completion_item(
  source: &str,
  label: &str,
  snippet: &str,
  kind: CompletionItemKind,
  replace: ByteRange,
) -> CompletionItem {
  CompletionItem {
    label: label.to_owned(),
    kind: Some(kind),
    insert_text_format: Some(InsertTextFormat::SNIPPET),
    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
      range: range_from_offsets(source, replace.start, replace.end),
      new_text: snippet.to_owned(),
    })),
    ..CompletionItem::default()
  }
}

fn plain_completion_item(
  source: &str,
  label: &str,
  kind: CompletionItemKind,
  replace: ByteRange,
) -> CompletionItem {
  CompletionItem {
    label: label.to_owned(),
    kind: Some(kind),
    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
      range: range_from_offsets(source, replace.start, replace.end),
      new_text: label.to_owned(),
    })),
    ..CompletionItem::default()
  }
}

fn range_from_offsets(source: &str, start: usize, end: usize) -> Range {
  Range::new(
    position_from_byte_offset(source, start),
    position_from_byte_offset(source, end),
  )
}

fn position_from_byte_offset(source: &str, offset: usize) -> Position {
  let mut line = 0u32;
  let mut column = 0u32;

  for (index, ch) in source.char_indices() {
    if index >= offset {
      break;
    }
    if ch == '\n' {
      line += 1;
      column = 0;
    } else {
      column += 1;
    }
  }

  Position::new(line, column)
}

fn byte_offset_from_position(source: &str, position: Position) -> Option<usize> {
  let mut current_line = 0u32;
  let mut line_start = 0usize;

  for line in source.split_inclusive('\n') {
    let line_end = line_start + line.len();
    if current_line == position.line {
      let column = position.character as usize;
      if column > line.len() {
        return None;
      }
      return Some(line_start + column.min(line.trim_end_matches(['\r', '\n']).len()));
    }
    line_start = line_end;
    current_line += 1;
  }

  (current_line == position.line && position.character == 0).then_some(source.len())
}

fn trim_ascii_whitespace_start(source: &str, start: usize, end: usize) -> usize {
  let mut index = start;
  while index < end && source.as_bytes()[index].is_ascii_whitespace() {
    index += 1;
  }
  index
}

fn trim_ascii_whitespace_end(source: &str, start: usize, end: usize) -> usize {
  let mut index = end;
  while index > start && source.as_bytes()[index - 1].is_ascii_whitespace() {
    index -= 1;
  }
  index
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

fn hover_for_document(
  source: &str,
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
      if let Some(path_match) = binding_path_match_at_position(source, binding, position)
        && let Some(definition) = template_symbol_definition(route, &path_match.path)
      {
        return Some(template_symbol_hover(definition, path_match.range(source)));
      }

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

  if let Some(component) = manifest
    .components
    .iter()
    .find(|component| component.source_path == relative_path)
    && let Some(prop) = component
      .props
      .iter()
      .find(|prop| position_in_span(position, &prop.source_span))
  {
    return Some(component_prop_hover(component, prop));
  }

  None
}

fn document_symbols_for_document(
  _source: &str,
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

  if let Some(component) = manifest
    .components
    .iter()
    .find(|component| component.source_path == relative_path)
  {
    return Some(DocumentSymbolResponse::Nested(document_symbols_for_component(
      component,
    )));
  }

  if manifest.app_html.source_path.as_deref() == Some(relative_path) {
    return Some(DocumentSymbolResponse::Nested(Vec::new()));
  }

  None
}

fn definition_for_document(
  project_root: &Path,
  source: &str,
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
        if let Some(path_match) = binding_path_match_at_position(source, binding, position)
          && let Some(definition) = template_symbol_definition(route, &path_match.path)
        {
          let location = location_for_relative_path(
            project_root,
            &route.source_path,
            definition.source_span,
          )?;
          return Ok(Some(GotoDefinitionResponse::Scalar(location)));
        }

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

  if let Some((component, prop)) = component_prop_at_position(source, manifest, position) {
    let location = location_for_relative_path(project_root, &component.source_path, prop.source_span)?;
    return Ok(Some(GotoDefinitionResponse::Scalar(location)));
  }

  if let Some(component) = component_tag_at_position(source, manifest, position) {
    let location = file_start_location(project_root, &component.source_path)?;
    return Ok(Some(GotoDefinitionResponse::Scalar(location)));
  }

  Ok(None)
}

fn references_for_document(
  project_root: &Path,
  source: &str,
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
        if let Some(path_match) = binding_path_match_at_position(source, binding, position) {
          if let Some(definition) = template_symbol_definition(route, &path_match.path)
            && include_declaration
          {
            locations.push(location_for_relative_path(
              project_root,
              &route.source_path,
              definition.source_span,
            )?);
          }

          locations.extend(binding_symbol_reference_locations(
            project_root,
            &route.source_path,
            &route.template_binding_spans,
            &path_match.path,
            Some((binding.source_span, path_match.path.as_str())),
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

fn template_symbol_definition<'a>(
  route: &'a RouteMetadata,
  path: &str,
) -> Option<&'a TemplateSymbolMetadata> {
  route
    .template_symbol_definitions
    .iter()
    .find(|definition| definition.path == path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplatePathMatch {
  end: usize,
  path: String,
  start: usize,
}

impl TemplatePathMatch {
  fn range(&self, source: &str) -> Range {
    range_from_offsets(source, self.start, self.end)
  }
}

fn binding_path_match_at_position(
  source: &str,
  binding: &TemplateBindingMetadata,
  position: Position,
) -> Option<TemplatePathMatch> {
  let offset = byte_offset_from_position(source, position)?;
  binding_path_match_at_offset(source, binding, offset)
}

fn binding_path_match_at_offset(
  source: &str,
  binding: &TemplateBindingMetadata,
  offset: usize,
) -> Option<TemplatePathMatch> {
  if offset < binding.source_span.start_byte || offset > binding.source_span.end_byte {
    return None;
  }

  let token = &source[binding.source_span.start_byte..binding.source_span.end_byte];
  let name_start = token.find(&binding.name)? + binding.source_span.start_byte;

  let mut current_start = name_start;
  let mut current_path = String::new();
  for segment in binding.name.split('.') {
    let current_end = current_start + segment.len();
    if !current_path.is_empty() {
      current_path.push('.');
    }
    current_path.push_str(segment);

    if offset >= current_start && offset <= current_end {
      return Some(TemplatePathMatch {
        end: current_end,
        path: current_path,
        start: current_start,
      });
    }

    current_start = current_end + 1;
  }

  None
}

fn binding_symbol_reference_locations(
  project_root: &Path,
  relative_path: &str,
  binding_spans: &[TemplateBindingMetadata],
  symbol_path: &str,
  current: Option<(SourceSpanMetadata, &str)>,
) -> Result<Vec<Location>> {
  binding_spans
    .iter()
    .filter(|binding| {
      binding.name == symbol_path || binding.name.starts_with(&format!("{symbol_path}."))
    })
    .filter(|binding| {
      current.is_none_or(|(current_span, current_path)| {
        !(binding.source_span == current_span && binding.name == current_path)
      })
    })
    .map(|binding| location_for_relative_path(project_root, relative_path, binding.source_span))
    .collect()
}

fn component_tag_at_position<'a>(
  source: &str,
  manifest: &'a ThebeManifest,
  position: Position,
) -> Option<&'a ComponentMetadata> {
  let offset = byte_offset_from_position(source, position)?;
  let (tag_name, start, end) = tag_name_at_offset(source, offset)?;
  if offset < start || offset > end {
    return None;
  }
  manifest
    .components
    .iter()
    .find(|component| component.tag_name == tag_name)
}

fn component_prop_at_position<'a>(
  source: &str,
  manifest: &'a ThebeManifest,
  position: Position,
) -> Option<(&'a ComponentMetadata, &'a ComponentPropMetadata)> {
  let offset = byte_offset_from_position(source, position)?;
  let (tag_name, _, _) = tag_name_at_offset(source, offset)?;
  let component = manifest
    .components
    .iter()
    .find(|component| component.tag_name == tag_name)?;
  let (attribute_name, _, _) = attribute_name_at_offset(source, offset)?;
  let prop_name = attribute_name.trim_start_matches(':');
  let prop = component.props.iter().find(|prop| prop.name == prop_name)?;
  Some((component, prop))
}

fn tag_name_at_offset(source: &str, offset: usize) -> Option<(String, usize, usize)> {
  let tag_start = source[..offset].rfind('<')?;
  if source[..offset]
    .rfind('>')
    .is_some_and(|end| end > tag_start)
  {
    return None;
  }

  let tag_prefix = &source[tag_start..];
  let (tag_name, end_rel) = open_tag_name(tag_prefix)?;
  Some((tag_name, tag_start + 1, tag_start + end_rel))
}

fn attribute_name_at_offset(source: &str, offset: usize) -> Option<(String, usize, usize)> {
  let tag_start = source[..offset].rfind('<')?;
  if source[..offset]
    .rfind('>')
    .is_some_and(|end| end > tag_start)
  {
    return None;
  }

  let tag_end = source[offset..]
    .find('>')
    .map_or(source.len(), |relative| offset + relative);
  let tag = &source[tag_start..tag_end];
  let bytes = tag.as_bytes();
  let mut idx = open_tag_name(tag)?.1;

  while idx < bytes.len() {
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
      idx += 1;
    }

    let start = idx;
    while idx < bytes.len()
      && (bytes[idx].is_ascii_alphanumeric() || matches!(bytes[idx], b':' | b'-' | b'_'))
    {
      idx += 1;
    }
    if start == idx {
      idx += 1;
      continue;
    }

    let end = idx;
    let absolute_start = tag_start + start;
    let absolute_end = tag_start + end;
    if offset >= absolute_start && offset <= absolute_end {
      return Some((tag[start..end].to_owned(), absolute_start, absolute_end));
    }

    while idx < bytes.len() && bytes[idx] != b'>' && !bytes[idx].is_ascii_whitespace() {
      idx += 1;
    }
  }

  None
}

fn prepare_rename_for_document(
  document: &DocumentContext,
  position: Position,
) -> Option<PrepareRenameResponse> {
  let target = rename_target(document, position)?;
  Some(PrepareRenameResponse::RangeWithPlaceholder {
    placeholder: target.current_name,
    range: target.range,
  })
}

fn rename_for_document(
  document: &DocumentContext,
  position: Position,
  new_name: &str,
) -> Option<WorkspaceEdit> {
  if !is_valid_identifier(new_name) {
    return None;
  }

  let target = rename_target(document, position)?;
  let uri = file_url(&document.project_root, &document.relative_path).ok()?;

  Some(WorkspaceEdit {
    changes: Some(HashMap::from([(
      uri,
      target
        .edits
        .into_iter()
        .map(|edit| TextEdit {
          range: edit.range,
          new_text: new_name.to_owned(),
        })
        .collect(),
    )])),
    document_changes: None,
    change_annotations: None,
  })
}

fn code_actions_for_document(
  document: &DocumentContext,
  params: &CodeActionParams,
) -> Option<Vec<CodeActionOrCommand>> {
  let uri = file_url(&document.project_root, &document.relative_path).ok()?;
  let mut actions = Vec::new();

  if document.relative_path == "Cargo.toml"
    && params.context.diagnostics.iter().any(|diagnostic| {
      matches!(
        diagnostic.code.as_ref(),
        Some(NumberOrString::String(code)) if code == "missing-ts-rs"
      )
    })
    && let Some(edit) = ts_rs_dependency_edit(&document.source)
  {
    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
      title: "Add ts-rs dependency".to_owned(),
      kind: Some(CodeActionKind::QUICKFIX),
      diagnostics: Some(params.context.diagnostics.clone()),
      edit: Some(WorkspaceEdit {
        changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
        document_changes: None,
        change_annotations: None,
      }),
      command: None,
      is_preferred: Some(true),
      disabled: None,
      data: None,
    }));
  }

  if document.relative_path.ends_with(".trs") {
    let blocks = parse_document_blocks(document).ok();
    let insert_range = Range::new(Position::new(0, 0), Position::new(0, 0));
    let mut missing_blocks = Vec::new();

    if blocks.as_ref().is_none_or(|blocks| blocks.head_span.is_none()) {
      missing_blocks.push(("Add <head>", "<head>\n  \n</head>\n\n"));
    }
    if is_component_path(&document.relative_path) {
      if blocks.as_ref().is_none_or(|blocks| blocks.script_span.is_none()) {
        missing_blocks.push(("Add <script>", "<script>\n\n</script>\n\n"));
      }
    } else if blocks
      .as_ref()
      .is_none_or(|blocks| blocks.script_setup_span.is_none())
    {
      missing_blocks.push(("Add <script setup>", "<script setup>\n\n</script>\n\n"));
    }
    if blocks
      .as_ref()
      .is_none_or(|blocks| blocks.script_ts_span.is_none())
    {
      missing_blocks.push((
        "Add <script lang=\"ts\">",
        "<script lang=\"ts\">\n\n</script>\n\n",
      ));
    }
    if blocks.as_ref().is_none_or(|blocks| blocks.style_span.is_none()) {
      missing_blocks.push(("Add <style>", "<style>\n\n</style>\n\n"));
    }

    actions.extend(missing_blocks.into_iter().map(|(title, snippet)| {
      CodeActionOrCommand::CodeAction(CodeAction {
        title: title.to_owned(),
        kind: Some(CodeActionKind::SOURCE),
        diagnostics: None,
        edit: Some(WorkspaceEdit {
          changes: Some(HashMap::from([(
            uri.clone(),
            vec![TextEdit {
              range: insert_range,
              new_text: snippet.to_owned(),
            }],
          )])),
          document_changes: None,
          change_annotations: None,
        }),
        command: None,
        is_preferred: None,
        disabled: None,
        data: None,
      })
    }));
  }

  (!actions.is_empty()).then_some(actions)
}

fn format_document(document: &DocumentContext) -> Option<Vec<TextEdit>> {
  if !document.relative_path.ends_with(".trs") {
    return None;
  }

  let blocks = parse_document_blocks(document).ok()?;
  let formatted = format_trs_document(document, &blocks);
  if formatted == document.source {
    return None;
  }

  Some(vec![TextEdit {
    range: range_from_offsets(&document.source, 0, document.source.len()),
    new_text: formatted,
  }])
}

fn semantic_tokens_for_document(document: &DocumentContext) -> Option<SemanticTokens> {
  if !document.relative_path.ends_with(".trs") {
    return None;
  }

  let blocks = parse_document_blocks(document).ok()?;
  let mut spans = Vec::new();

  if !is_component_path(&document.relative_path)
    && let Ok(handler_info) = thebe_codegen::route_handler_info(&blocks)
    && let Some(source_span) = handler_info.source_span
    && let Some(name_range) =
      identifier_range_within_source_span(&document.source, &handler_info.name, source_span)
  {
    spans.push(SemanticTokenSpan {
      start: name_range.start,
      end: name_range.end,
      token_type: TOKEN_TYPE_FUNCTION,
    });
  }

  spans.extend(template_binding_token_spans(&document.source, &blocks.template_spans));
  spans.extend(template_tag_token_spans(&document.source, &blocks.template_spans));
  spans.sort_by_key(|span| (span.start, span.end));
  spans.dedup_by(|left, right| {
    left.start == right.start && left.end == right.end && left.token_type == right.token_type
  });

  let mut data = Vec::new();
  let mut previous_line = 0u32;
  let mut previous_start = 0u32;
  for span in spans {
    let start = position_from_byte_offset(&document.source, span.start);
    let length = document.source[span.start..span.end].chars().count() as u32;
    let delta_line = start.line.saturating_sub(previous_line);
    let delta_start = if delta_line == 0 {
      start.character.saturating_sub(previous_start)
    } else {
      start.character
    };
    data.push(SemanticToken {
      delta_line,
      delta_start,
      length,
      token_type: span.token_type,
      token_modifiers_bitset: 0,
    });
    previous_line = start.line;
    previous_start = start.character;
  }

  Some(SemanticTokens {
    result_id: None,
    data,
  })
}

#[derive(Debug, Clone)]
struct RenameTarget {
  current_name: String,
  edits: Vec<TextEdit>,
  range: Range,
}

fn rename_target(document: &DocumentContext, position: Position) -> Option<RenameTarget> {
  route_handler_rename_target(document, position)
    .or_else(|| event_handler_rename_target(document, position))
}

fn route_handler_rename_target(
  document: &DocumentContext,
  position: Position,
) -> Option<RenameTarget> {
  if is_component_path(&document.relative_path) {
    return None;
  }

  let blocks = parse_source_blocks(&document.source).ok()?;
  let handler_info = thebe_codegen::route_handler_info(&blocks).ok()?;
  let source_span = handler_info.source_span?;
  let name_range =
    identifier_range_within_source_span(&document.source, &handler_info.name, source_span)?;
  let range = range_from_offsets(&document.source, name_range.start, name_range.end);
  if !position_in_range(position, range) {
    return None;
  }

  Some(RenameTarget {
    current_name: handler_info.name,
    edits: vec![TextEdit {
      range,
      new_text: String::new(),
    }],
    range,
  })
}

fn event_handler_rename_target(
  document: &DocumentContext,
  position: Position,
) -> Option<RenameTarget> {
  let handlers = current_event_handlers(document);
  if handlers.is_empty() {
    return None;
  }

  let definition_ranges = event_handler_definition_ranges(document);
  for name in handlers {
    let definition = definition_ranges.get(&name)?;
    let usage_ranges = event_handler_reference_ranges(&document.source, &name);
    let all_ranges = std::iter::once(*definition).chain(usage_ranges.iter().copied());
    if all_ranges.clone().any(|range| position_in_range(position, range_from_offsets(&document.source, range.start, range.end))) {
      let mut edits = Vec::new();
      edits.push(TextEdit {
        range: range_from_offsets(&document.source, definition.start, definition.end),
        new_text: String::new(),
      });
      edits.extend(usage_ranges.into_iter().map(|range| TextEdit {
        range: range_from_offsets(&document.source, range.start, range.end),
        new_text: String::new(),
      }));
      let range = range_from_offsets(&document.source, definition.start, definition.end);
      return Some(RenameTarget {
        current_name: name,
        edits,
        range,
      });
    }
  }

  None
}

fn event_handler_definition_ranges(document: &DocumentContext) -> HashMap<String, ByteRange> {
  let Ok(blocks) = parse_document_blocks(document) else {
    return HashMap::new();
  };
  let Some(script_ts) = blocks.script_ts.as_deref() else {
    return HashMap::new();
  };
  let Some(script_span) = blocks.script_ts_span else {
    return HashMap::new();
  };

  current_event_handlers(document)
    .into_iter()
    .filter_map(|handler| {
      let needle = format!("function {handler}");
      let start = script_ts.find(&needle)? + script_span.start + "function ".len();
      Some((
        handler.clone(),
        ByteRange {
          start,
          end: start + handler.len(),
        },
      ))
    })
    .collect()
}

fn event_handler_reference_ranges(source: &str, handler_name: &str) -> Vec<ByteRange> {
  let bytes = source.as_bytes();
  let mut ranges = Vec::new();
  let mut idx = 0usize;

  while idx < bytes.len() {
    if bytes[idx] != b'<' {
      idx += 1;
      continue;
    }

    let tag_start = idx;
    let Some(tag_end) = source[idx..].find('>').map(|relative| idx + relative) else {
      break;
    };
    let tag = &source[tag_start..=tag_end];
    let tag_bytes = tag.as_bytes();
    let mut cursor = open_tag_name(tag).map_or(1, |(_, end)| end);

    while cursor < tag_bytes.len() {
      while cursor < tag_bytes.len() && tag_bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
      }
      let name_start = cursor;
      while cursor < tag_bytes.len()
        && (tag_bytes[cursor].is_ascii_alphanumeric()
          || matches!(tag_bytes[cursor], b':' | b'-' | b'_'))
      {
        cursor += 1;
      }
      if name_start == cursor {
        cursor += 1;
        continue;
      }

      let attribute_name = &tag[name_start..cursor];
      while cursor < tag_bytes.len() && tag_bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
      }
      if !attribute_name.starts_with("on") || cursor >= tag_bytes.len() || tag_bytes[cursor] != b'=' {
        continue;
      }
      cursor += 1;
      while cursor < tag_bytes.len() && tag_bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
      }
      if cursor >= tag_bytes.len() || !matches!(tag_bytes[cursor], b'\'' | b'"') {
        continue;
      }

      let quote = tag_bytes[cursor];
      let value_start = cursor + 1;
      cursor += 1;
      while cursor < tag_bytes.len() && tag_bytes[cursor] != quote {
        cursor += 1;
      }
      let value_end = cursor.min(tag_bytes.len());
      let value = &tag[value_start..value_end];
      if let Some(rest) = value.strip_prefix(handler_name)
        && (rest.is_empty() || rest.starts_with('('))
      {
        let absolute_start = tag_start + value_start;
        ranges.push(ByteRange {
          start: absolute_start,
          end: absolute_start + handler_name.len(),
        });
      }
    }

    idx = tag_end + 1;
  }

  ranges
}

fn ts_rs_dependency_edit(source: &str) -> Option<TextEdit> {
  if source.contains("ts-rs") {
    return None;
  }

  if let Some(index) = source.find("[dependencies]") {
    let insert = source[index..]
      .find('\n')
      .map_or(source.len(), |relative| index + relative + 1);
    return Some(TextEdit {
      range: range_from_offsets(source, insert, insert),
      new_text: "ts-rs = \"12\"\n".to_owned(),
    });
  }

  Some(TextEdit {
    range: range_from_offsets(source, source.len(), source.len()),
    new_text: "\n[dependencies]\nts-rs = \"12\"\n".to_owned(),
  })
}

fn format_trs_document(document: &DocumentContext, blocks: &SfcBlocks) -> String {
  let mut sections = Vec::new();

  if let Some(head) = blocks.head.as_deref() {
    sections.push(render_block("head", head));
  }
  if is_component_path(&document.relative_path) {
    if let Some(script) = blocks.script.as_deref() {
      sections.push(render_block("script", script));
    }
  } else if let Some(script_setup) = blocks.script_setup.as_deref() {
    sections.push(render_block("script setup", script_setup));
  }
  if let Some(script_ts) = blocks.script_ts.as_deref() {
    sections.push(render_block("script lang=\"ts\"", script_ts));
  }
  if !blocks.template.trim().is_empty() {
    sections.push(dedent_block(&blocks.template, 0));
  }
  if let Some(style) = blocks.style.as_deref() {
    sections.push(render_block("style", style));
  }

  let mut output = sections.join("\n\n");
  output.push('\n');
  output
}

fn render_block(tag: &str, contents: &str) -> String {
  let close = if tag.starts_with("script") {
    "script"
  } else {
    tag
  };
  format!("<{tag}>\n{}\n</{close}>", dedent_block(contents, 2))
}

fn dedent_block(contents: &str, indent: usize) -> String {
  let lines = contents.trim().lines().collect::<Vec<_>>();
  let min_indent = lines
    .iter()
    .filter(|line| !line.trim().is_empty())
    .map(|line| line.chars().take_while(|ch| ch.is_whitespace()).count())
    .min()
    .unwrap_or(0);
  let prefix = " ".repeat(indent);

  lines
    .into_iter()
    .map(|line| {
      if line.trim().is_empty() {
        String::new()
      } else {
        format!("{prefix}{}", &line[min_indent..])
      }
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn template_binding_token_spans(source: &str, template_spans: &[thebe_parser::SourceSpan]) -> Vec<SemanticTokenSpan> {
  let mut spans = Vec::new();

  for span in template_spans {
    let segment = &source[span.start..span.end];
    let Ok(bindings) = thebe_codegen::list_template_binding_occurrences(segment) else {
      continue;
    };
    for binding in bindings {
      let absolute = TemplateBindingMetadata {
        name: binding.name,
        source_span: SourceSpanMetadata {
          start_byte: span.start + binding.span.start,
          end_byte: span.start + binding.span.end,
          start_line: 0,
          start_column: 0,
          end_line: 0,
          end_column: 0,
        },
      };
      let token = &source[absolute.source_span.start_byte..absolute.source_span.end_byte];
      if let Some(name_offset) = token.find(&absolute.name) {
        let mut current_start = absolute.source_span.start_byte + name_offset;
        for (index, segment) in absolute.name.split('.').enumerate() {
          let current_end = current_start + segment.len();
          spans.push(SemanticTokenSpan {
            start: current_start,
            end: current_end,
            token_type: if index == 0 {
              TOKEN_TYPE_VARIABLE
            } else {
              TOKEN_TYPE_PROPERTY
            },
          });
          current_start = current_end + 1;
        }
      }
    }
  }

  spans
}

fn template_tag_token_spans(source: &str, template_spans: &[thebe_parser::SourceSpan]) -> Vec<SemanticTokenSpan> {
  let mut spans = Vec::new();

  for span in template_spans {
    let bytes = source.as_bytes();
    let mut idx = span.start;
    while idx < span.end {
      if bytes[idx] != b'<' {
        idx += 1;
        continue;
      }

      let Some(tag_end) = source[idx..span.end].find('>').map(|relative| idx + relative) else {
        break;
      };
      let tag = &source[idx..=tag_end];
      if let Some((tag_name, _)) = open_tag_name(tag)
        && let Some(name_start) = tag.find(&tag_name)
      {
        let absolute_start = idx + name_start;
        let absolute_end = absolute_start + tag_name.len();
        if tag_name.chars().next().is_some_and(|ch| ch.is_ascii_uppercase()) {
          spans.push(SemanticTokenSpan {
            start: absolute_start,
            end: absolute_end,
            token_type: TOKEN_TYPE_CLASS,
          });
        } else if matches!(tag_name.as_str(), "head" | "script" | "style") {
          spans.push(SemanticTokenSpan {
            start: absolute_start,
            end: absolute_end,
            token_type: TOKEN_TYPE_KEYWORD,
          });
        }

        let tag_bytes = tag.as_bytes();
        let mut cursor = open_tag_name(tag).map_or(1, |(_, end)| end);
        while cursor < tag_bytes.len() {
          while cursor < tag_bytes.len() && tag_bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
          }
          let attr_start = cursor;
          while cursor < tag_bytes.len()
            && (tag_bytes[cursor].is_ascii_alphanumeric()
              || matches!(tag_bytes[cursor], b':' | b'-' | b'_'))
          {
            cursor += 1;
          }
          if attr_start == cursor {
            cursor += 1;
            continue;
          }

          let attr = &tag[attr_start..cursor];
          let token_type = if attr.starts_with(':') {
            Some(TOKEN_TYPE_KEYWORD)
          } else if attr.starts_with("on") {
            Some(TOKEN_TYPE_FUNCTION)
          } else if tag_name.chars().next().is_some_and(|ch| ch.is_ascii_uppercase()) {
            Some(TOKEN_TYPE_PROPERTY)
          } else {
            None
          };

          if let Some(token_type) = token_type {
            spans.push(SemanticTokenSpan {
              start: idx + attr_start,
              end: idx + cursor,
              token_type,
            });
          }

          while cursor < tag_bytes.len() && tag_bytes[cursor] != b'>' && !tag_bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
          }
        }
      }

      idx = tag_end + 1;
    }
  }

  spans
}

fn identifier_range_within_source_span(
  source: &str,
  name: &str,
  span: thebe_parser::SourceSpan,
) -> Option<ByteRange> {
  let slice = &source[span.start..span.end];
  let start = slice.find(name)? + span.start;
  Some(ByteRange {
    start,
    end: start + name.len(),
  })
}

fn position_in_range(position: Position, range: Range) -> bool {
  compare_positions(position, range.start) != Ordering::Less
    && compare_positions(position, range.end) != Ordering::Greater
}

fn is_valid_identifier(name: &str) -> bool {
  let mut chars = name.chars();
  let Some(first) = chars.next() else {
    return false;
  };
  if !first.is_alphabetic() && first != '_' {
    return false;
  }
  chars.all(|ch| ch.is_alphanumeric() || ch == '_')
}

async fn publish_project_diagnostics(
  client: &Client,
  state: &Arc<RwLock<BackendState>>,
  project_root: &Path,
  artifacts: &ProjectArtifacts,
) -> Result<()> {
  let diagnostics_by_file = artifact_diagnostics_by_file(project_root, artifacts)?;
  publish_diagnostic_batch(client, state, project_root, diagnostics_by_file).await;

  Ok(())
}

async fn publish_diagnostics_without_manifest(
  client: &Client,
  state: &Arc<RwLock<BackendState>>,
  project_root: &Path,
  diagnostics: &ThebeDiagnosticsFile,
) -> Result<()> {
  let diagnostics_by_file = diagnostics_by_file(project_root, diagnostics)?;
  publish_diagnostic_batch(client, state, project_root, diagnostics_by_file).await;

  Ok(())
}

async fn publish_diagnostic_batch(
  client: &Client,
  state: &Arc<RwLock<BackendState>>,
  project_root: &Path,
  diagnostics_by_file: DiagnosticsByFile,
) {
  let updates =
    write_backend_state(state).coalesce_diagnostic_updates(project_root, diagnostics_by_file);

  for (uri, diagnostics) in updates {
    client.publish_diagnostics(uri, diagnostics, None).await;
  }
}

fn artifact_diagnostics_by_file(
  project_root: &Path,
  artifacts: &ProjectArtifacts,
) -> Result<DiagnosticsByFile> {
  let mut diagnostics_by_file = diagnostics_by_file(project_root, &artifacts.diagnostics)?;

  for relative_path in known_source_paths(&artifacts.manifest) {
    let file_url = file_url(project_root, &relative_path)?;
    diagnostics_by_file.entry(file_url).or_default();
  }

  Ok(diagnostics_by_file)
}

fn diagnostics_by_file(
  project_root: &Path,
  diagnostics: &ThebeDiagnosticsFile,
) -> Result<DiagnosticsByFile> {
  let mut diagnostics_by_file = DiagnosticsByFile::new();

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

  Ok(diagnostics_by_file)
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
  paths.extend(
    manifest
      .components
      .iter()
      .map(|component| component.source_path.clone()),
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

fn template_symbol_hover(definition: &TemplateSymbolMetadata, range: Range) -> Hover {
  Hover {
    contents: HoverContents::Markup(MarkupContent {
      kind: MarkupKind::Markdown,
      value: format!(
        "**Template symbol** `{}`\n\n- Field: `{}`\n- Owner: `{}`\n- Type: `{}`",
        definition.path, definition.field_name, definition.owner_type, definition.type_name,
      ),
    }),
    range: Some(range),
  }
}

fn component_prop_hover(component: &ComponentMetadata, prop: &ComponentPropMetadata) -> Hover {
  Hover {
    contents: HoverContents::Markup(MarkupContent {
      kind: MarkupKind::Markdown,
      value: format!(
        "**Component prop** `{}`\n\n- Component: `{}`\n- Type: `{}`",
        prop.name, component.tag_name, prop.type_name,
      ),
    }),
    range: Some(range_from_span(&prop.source_span)),
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

fn document_symbols_for_component(component: &ComponentMetadata) -> Vec<DocumentSymbol> {
  component_prop_symbols(&component.props)
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

#[expect(
  deprecated,
  reason = "lsp-types 0.94 still requires populating DocumentSymbol::deprecated"
)]
fn component_prop_symbols(props: &[ComponentPropMetadata]) -> Vec<DocumentSymbol> {
  props
    .iter()
    .map(|prop| {
      let range = range_from_span(&prop.source_span);
      DocumentSymbol {
        name: prop.name.clone(),
        detail: Some(format!("component prop: {}", prop.type_name)),
        kind: SymbolKind::PROPERTY,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
      }
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
  fn project_state_only_publishes_changed_diagnostics() {
    let mut project_state = ProjectState::default();
    let route_uri = Url::parse("file:///tmp/app/src/routes/index.trs").expect("valid route url");
    let layout_uri =
      Url::parse("file:///tmp/app/src/routes/_layout.trs").expect("valid layout url");
    let diagnostic = Diagnostic {
      range: full_document_range(),
      severity: Some(DiagnosticSeverity::ERROR),
      code: Some(NumberOrString::String("parse-error".to_owned())),
      code_description: None,
      source: Some("thebe/parser".to_owned()),
      message: "parse error".to_owned(),
      related_information: None,
      tags: None,
      data: None,
    };

    let first_updates = project_state.coalesce_diagnostic_updates(BTreeMap::from([
      (route_uri.clone(), vec![diagnostic.clone()]),
      (layout_uri.clone(), Vec::new()),
    ]));
    assert_eq!(first_updates.len(), 2);

    let second_updates = project_state.coalesce_diagnostic_updates(BTreeMap::from([
      (route_uri.clone(), vec![diagnostic.clone()]),
      (layout_uri.clone(), Vec::new()),
    ]));
    assert!(second_updates.is_empty());

    let third_updates =
      project_state.coalesce_diagnostic_updates(BTreeMap::from([(layout_uri, Vec::new())]));
    assert_eq!(third_updates.len(), 1);
    assert_eq!(third_updates[0].0, route_uri);
    assert!(third_updates[0].1.is_empty());
  }

  #[test]
  fn template_binding_context_detects_binding_prefix() {
    let source = "<main>{{ user.na }}</main>";
    let offset = source.find("na").expect("binding prefix exists") + 2;

    let context = template_binding_context(source, offset).expect("binding context");

    assert_eq!(
      context,
      CompletionContext::TemplateBinding {
        prefix: "user.na".to_owned(),
        replace: ByteRange { start: 9, end: 16 },
      }
    );
  }

  #[test]
  fn event_handler_context_detects_open_event_attribute() {
    let source = r#"<button onclick="inc"></button>"#;
    let offset = source.find("inc").expect("handler prefix exists") + 3;

    let context = event_handler_context(source, offset).expect("event handler context");

    assert_eq!(
      context,
      CompletionContext::EventHandler {
        prefix: "inc".to_owned(),
        replace: ByteRange { start: 17, end: 20 },
      }
    );
  }

  #[test]
  fn block_tag_context_detects_top_level_block_prefix() {
    let source = "<sc";
    let offset = source.len();

    let context = block_tag_context(source, offset).expect("block tag context");

    assert_eq!(
      context,
      CompletionContext::BlockTag {
        prefix: "sc".to_owned(),
        replace: ByteRange { start: 0, end: 3 },
      }
    );
  }

  #[test]
  fn event_handler_completion_items_use_current_script_functions() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: r#"<script setup>
struct Props {
  count: i64,
}

#[thebe::get]
fn handler() -> Props {
  Props { count: 0 }
}
</script>

<script lang="ts">
function increment() {
  return 1;
}

function decrement() {
  return 0;
}
</script>

<button onclick="inc"></button>
"#
      .to_owned(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };

    let items = event_handler_completion_items(&document, "inc", ByteRange { start: 0, end: 3 });

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "increment");
  }

  #[test]
  fn template_binding_completion_items_merge_cached_and_current_bindings() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/profile.trs".to_owned(),
      source: "<main>{{ user }}</main>".to_owned(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };

    let items = template_binding_completion_items(&document, "u", ByteRange { start: 0, end: 1 });

    assert!(items.iter().any(|item| item.label == "user"));
    assert!(items.iter().any(|item| item.label == "username"));
  }

  #[test]
  fn template_binding_completion_items_use_cached_template_symbols() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/profile.trs".to_owned(),
      source: "<main>{{ profile.d }}</main>".to_owned(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };

    let items =
      template_binding_completion_items(&document, "profile.d", ByteRange { start: 0, end: 9 });

    assert!(
      items
        .iter()
        .any(|item| item.label == "profile.display_name")
    );
  }

  #[test]
  fn template_binding_completion_items_use_current_props_fields() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/profile.trs".to_owned(),
      source: r#"<script setup>
struct Props {
  user: User,
  posts: Vec<Post>,
}

struct User {
  name: String,
}

struct Post {
  title: String,
}
</script>

<main>{{ user.n }}</main>
"#
      .to_owned(),
      cached_artifacts: None,
    };

    let items =
      template_binding_completion_items(&document, "user.n", ByteRange { start: 0, end: 6 });

    assert!(items.iter().any(|item| item.label == "user.name"));
    assert!(!items.iter().any(|item| item.label == "posts.title"));
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
    assert!(is_project_input_file(
      project_root,
      &project_root.join("src/components/Card.trs"),
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
    let source = fixture_route_source();
    let username_field = source.find("username: String").expect("username field");
    let profile_field = source.find("profile: Profile").expect("profile field");
    let display_name_field = source
      .find("display_name: String")
      .expect("display name field");
    let handler_start = source.find("fn handler").expect("handler start");
    let handler_end = handler_start
      + source[handler_start..]
        .find("{\n")
        .expect("handler signature end")
        + 1;
    let username_binding_1 = source.find("{{ username }}").expect("first username binding");
    let username_binding_2 = source[username_binding_1 + 1..]
      .find("{{ username }}")
      .map(|offset| username_binding_1 + 1 + offset)
      .expect("second username binding");
    let profile_binding = source
      .find("{{ profile.display_name }}")
      .expect("profile binding");

    ThebeManifest {
      version: 5,
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
          source_span: Some(span_from_offsets(source, handler_start, handler_end)),
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
        template_bindings: vec!["username".to_owned(), "profile.display_name".to_owned()],
        template_symbols: vec![
          "username".to_owned(),
          "profile".to_owned(),
          "profile.display_name".to_owned(),
        ],
        template_binding_spans: vec![
          TemplateBindingMetadata {
            name: "username".to_owned(),
            source_span: span_from_offsets(source, username_binding_1, username_binding_1 + 14),
          },
          TemplateBindingMetadata {
            name: "username".to_owned(),
            source_span: span_from_offsets(source, username_binding_2, username_binding_2 + 14),
          },
          TemplateBindingMetadata {
            name: "profile.display_name".to_owned(),
            source_span: span_from_offsets(
              source,
              profile_binding,
              profile_binding + 26,
            ),
          },
        ],
        template_symbol_definitions: vec![
          TemplateSymbolMetadata {
            field_name: "username".to_owned(),
            owner_type: "Props".to_owned(),
            path: "username".to_owned(),
            source_span: span_from_offsets(source, username_field, username_field + 8),
            type_name: "String".to_owned(),
          },
          TemplateSymbolMetadata {
            field_name: "profile".to_owned(),
            owner_type: "Props".to_owned(),
            path: "profile".to_owned(),
            source_span: span_from_offsets(source, profile_field, profile_field + 7),
            type_name: "Profile".to_owned(),
          },
          TemplateSymbolMetadata {
            field_name: "display_name".to_owned(),
            owner_type: "Profile".to_owned(),
            path: "profile.display_name".to_owned(),
            source_span: span_from_offsets(source, display_name_field, display_name_field + 12),
            type_name: "String".to_owned(),
          },
        ],
      }],
      components: vec![ComponentMetadata {
        has_client_script: false,
        has_style: false,
        module_path: "crate::components::Card".to_owned(),
        prop_names: vec!["title".to_owned()],
        props: vec![ComponentPropMetadata {
          name: "title".to_owned(),
          source_span: SourceSpanMetadata {
            start_byte: 1,
            end_byte: 6,
            start_line: 1,
            start_column: 2,
            end_line: 1,
            end_column: 7,
          },
          type_name: "String".to_owned(),
        }],
        source_path: "src/components/Card.trs".to_owned(),
        tag_name: "Card".to_owned(),
      }],
    }
  }

  fn fixture_route_source() -> &'static str {
    r#"<script setup>
struct Props {
  username: String,
  profile: Profile,
}

struct Profile {
  display_name: String,
}

#[thebe::get]
fn handler(State(state): State<crate::AppState>) -> Props {
  let _ = state;
  Props {
    username: String::from("Ada"),
    profile: Profile {
      display_name: String::from("Ada Lovelace"),
    },
  }
}
</script>

<main>
  <h1>{{ username }}</h1>
  <p>{{ username }}</p>
  <span>{{ profile.display_name }}</span>
</main>
"#
  }

  fn span_from_offsets(source: &str, start: usize, end: usize) -> SourceSpanMetadata {
    let start = position_from_byte_offset(source, start);
    let end = position_from_byte_offset(source, end);

    SourceSpanMetadata {
      start_byte: start_byte_from_position(source, start),
      end_byte: start_byte_from_position(source, end),
      start_line: start.line as usize + 1,
      start_column: start.character as usize + 1,
      end_line: end.line as usize + 1,
      end_column: end.character as usize + 1,
    }
  }

  fn start_byte_from_position(source: &str, position: Position) -> usize {
    byte_offset_from_position(source, position).expect("valid fixture position")
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
  fn hover_for_document_returns_handler_hover() {
    let manifest = fixture_manifest();
    let source = fixture_route_source();
    let hover = hover_for_document(
      source,
      &manifest,
      "src/routes/profile.trs",
      position_from_byte_offset(source, source.find("handler").expect("handler name") + 2),
    )
    .unwrap();
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
      document_symbols_for_document(fixture_route_source(), &manifest, "src/routes/profile.trs")
    else {
      panic!("expected nested document symbols");
    };

    assert_eq!(symbols.len(), 3);
    assert_eq!(symbols[0].name, "handler");
    assert_eq!(symbols[1].name, "username");
    assert_eq!(symbols[2].name, "profile.display_name");
  }

  #[test]
  fn definition_for_handler_points_to_generated_server_file() {
    let manifest = fixture_manifest();
    let source = fixture_route_source();
    let response = definition_for_document(
      Path::new("/tmp/app"),
      source,
      &manifest,
      "src/routes/profile.trs",
      position_from_byte_offset(source, source.find("handler").expect("handler name") + 1),
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
    let response = definition_for_document(
      Path::new("/tmp/app"),
      fixture_route_source(),
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
    assert_eq!(location.range.start, Position::new(11, 0));
  }

  #[test]
  fn references_for_binding_include_all_occurrences_and_generated_files() {
    let manifest = fixture_manifest();
    let source = fixture_route_source();
    let binding_position = position_from_byte_offset(
      source,
      source.find("{{ username }}").expect("username binding") + 4,
    );
    let locations = references_for_document(
      Path::new("/tmp/app"),
      source,
      &manifest,
      "src/routes/profile.trs",
      binding_position,
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

  #[test]
  fn nested_template_definition_points_to_exact_field() {
    let manifest = fixture_manifest();
    let source = fixture_route_source();
    let position = position_from_byte_offset(
      source,
      source
        .find("profile.display_name")
        .expect("profile path in binding")
        + "profile.".len()
        + 2,
    );
    let response = definition_for_document(
      Path::new("/tmp/app"),
      source,
      &manifest,
      "src/routes/profile.trs",
      position,
    )
    .unwrap()
    .unwrap();
    let GotoDefinitionResponse::Scalar(location) = response else {
      panic!("expected scalar definition");
    };

    assert!(location.uri.path().ends_with("/src/routes/profile.trs"));
    assert_eq!(location.range.start, Position::new(7, 2));
  }
}
