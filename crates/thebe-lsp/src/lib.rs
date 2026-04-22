use anyhow::Result;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
  DidSaveTextDocumentParams, DocumentFormattingParams, DocumentHighlight,
  DocumentHighlightKind, DocumentHighlightParams, DocumentSymbol, DocumentSymbolParams,
  DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover,
  HoverContents, HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
  InsertTextFormat, LinkedEditingRangeParams, LinkedEditingRangeServerCapabilities,
  LinkedEditingRanges, Location, MarkupContent, MarkupKind, MessageType,
  NumberOrString, OneOf, Position, PrepareRenameResponse, Range, ReferenceParams,
  RenameOptions, RenameParams, SemanticToken, SemanticTokenModifier,
  SemanticTokenType, SemanticTokens, SemanticTokensFullOptions, SemanticTokensLegend,
  SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
  SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, SymbolInformation,
  SymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind,
  TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Url,
  WorkspaceEdit, WorkspaceSymbolParams,
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

  fn project_roots(&self) -> Vec<PathBuf> {
    let mut roots = self.projects.keys().cloned().collect::<Vec<_>>();
    roots.sort();
    roots
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComponentImportOccurrence {
  imported_name: String,
  local_name: String,
  local_range: ByteRange,
  module_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SemanticTokenSpan {
  end: usize,
  start: usize,
  token_type: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TagNameOccurrence {
  is_closing: bool,
  name: String,
  range: ByteRange,
  self_closing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplateTagPair {
  close: ByteRange,
  open: ByteRange,
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

  fn known_project_roots(&self) -> Vec<PathBuf> {
    read_backend_state(&self.state).project_roots()
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

  fn load_or_refresh_artifacts_for_project(&self, project_root: &Path) -> Option<ProjectArtifacts> {
    let snapshot = self.project_snapshot(project_root);

    if let Some(artifacts) = snapshot.cached_artifacts {
      return Some(artifacts);
    }

    if let Ok(artifacts) = thebe_project::load_project_artifacts(project_root) {
      return Some(artifacts);
    }

    match thebe_project::refresh_project_for_editor_with_overlay(project_root, &snapshot.overlay)
      .ok()?
    {
      EditorRefresh::Generated(artifacts) => {
        let _ =
          write_backend_state(&self.state).remember_generated(project_root, snapshot.revision, &artifacts);
        Some(artifacts)
      }
      EditorRefresh::Diagnostics(diagnostics) => {
        let (_, cached_artifacts) =
          write_backend_state(&self.state).remember_diagnostics(project_root, snapshot.revision, &diagnostics);

        cached_artifacts.or_else(|| {
          thebe_project::load_project_artifacts(project_root)
            .ok()
            .map(|mut artifacts| {
              artifacts.diagnostics = diagnostics.clone();
              artifacts
            })
        })
      }
    }
  }

  fn load_or_refresh_artifacts(&self, uri: &Url) -> Option<(PathBuf, ProjectArtifacts)> {
    let project_root = find_project_root_from_uri(uri)?;
    let artifacts = self.load_or_refresh_artifacts_for_project(&project_root)?;
    Some((project_root, artifacts))
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
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
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
        linked_editing_range_provider: Some(LinkedEditingRangeServerCapabilities::Simple(true)),
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

  async fn document_highlight(
    &self,
    params: DocumentHighlightParams,
  ) -> LspResult<Option<Vec<DocumentHighlight>>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };

    Ok(document_highlights_for_document(
      &document,
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

  async fn symbol(&self, params: WorkspaceSymbolParams) -> LspResult<Option<Vec<SymbolInformation>>> {
    let mut symbols = Vec::new();

    for project_root in self.known_project_roots() {
      let Some(artifacts) = self.load_or_refresh_artifacts_for_project(&project_root) else {
        continue;
      };

      symbols.extend(
        workspace_symbols_for_manifest(&project_root, &artifacts.manifest, &params.query)
          .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?,
      );
    }

    sort_symbol_information(&mut symbols);

    if symbols.is_empty() {
      Ok(None)
    } else {
      Ok(Some(symbols))
    }
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

  async fn linked_editing_range(
    &self,
    params: LinkedEditingRangeParams,
  ) -> LspResult<Option<LinkedEditingRanges>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let Some(document) = self.document_context(uri) else {
      return Ok(None);
    };

    Ok(linked_editing_ranges_for_document(
      &document,
      params.text_document_position_params.position,
    ))
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
  let current_imports = current_component_imports(document);
  let import_insert_offset = component_import_insert_offset(document);
  let mut imports = available_component_imports(document, &current_imports);
  imports.sort_by(|left, right| left.tag_name.cmp(&right.tag_name));
  imports.dedup_by(|left, right| left.tag_name == right.tag_name);

  imports
    .into_iter()
    .filter(|component| component.tag_name.starts_with(prefix))
    .map(|component| CompletionItem {
      label: component.tag_name.clone(),
      kind: Some(CompletionItemKind::CLASS),
      detail: Some(component.module_path.clone()),
      text_edit: Some(CompletionTextEdit::Edit(TextEdit {
        range: range_from_offsets(&document.source, replace.start, replace.end),
        new_text: component.tag_name.clone(),
      })),
      additional_text_edits: missing_component_import_edit(
        &document.source,
        &component,
        &current_imports,
        import_insert_offset,
      ),
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
  component_import_occurrences(document)
    .into_iter()
    .map(|occurrence| ComponentImport {
      module_path: occurrence.module_path,
      tag_name: occurrence.local_name,
    })
    .collect()
}

fn component_import_occurrences(document: &DocumentContext) -> Vec<ComponentImportOccurrence> {
  let Ok(blocks) = parse_document_blocks(document) else {
    return Vec::new();
  };

  let (import_block, import_span) = if is_component_path(&document.relative_path) {
    (blocks.script.as_deref(), blocks.script_span)
  } else {
    (blocks.script_setup.as_deref(), blocks.script_setup_span)
  };
  let (Some(import_block), Some(import_span)) = (import_block, import_span) else {
    return Vec::new();
  };

  let mut occurrences = Vec::new();
  let mut line_offset = 0usize;

  for line in import_block.split_inclusive('\n') {
    let line_text = line.trim_end_matches(['\r', '\n']);
    let trimmed = line_text.trim();
    if !trimmed.starts_with("use ")
      || !trimmed.ends_with(';')
      || !trimmed.contains("crate::components::")
    {
      line_offset += line.len();
      continue;
    }

    let import = trimmed.trim_start_matches("use ").trim_end_matches(';').trim();
    if import.contains('{') {
      line_offset += line.len();
      continue;
    }

    let (module_path, local_name) = if let Some((path, alias)) = import.split_once(" as ") {
      (path.trim().to_owned(), alias.trim().to_owned())
    } else {
      let Some(tag_name) = import.rsplit("::").next() else {
        line_offset += line.len();
        continue;
      };
      (import.to_owned(), tag_name.to_owned())
    };
    let Some(imported_name) = module_path.rsplit("::").next().map(str::to_owned) else {
      line_offset += line.len();
      continue;
    };

    let statement = line_text.split_once(';').map_or(line_text, |(before, _)| before);
    let Some(local_name_start) = statement.rfind(&local_name) else {
      line_offset += line.len();
      continue;
    };

    let absolute_start = import_span.start + line_offset + local_name_start;
    occurrences.push(ComponentImportOccurrence {
      imported_name,
      local_name: local_name.clone(),
      local_range: ByteRange {
        start: absolute_start,
        end: absolute_start + local_name.len(),
      },
      module_path,
    });

    line_offset += line.len();
  }

  occurrences
}

fn available_component_imports(
  document: &DocumentContext,
  current_imports: &[ComponentImport],
) -> Vec<ComponentImport> {
  let Some(artifacts) = document.cached_artifacts.as_ref() else {
    return current_imports.to_vec();
  };

  let mut imports = artifacts
    .manifest
    .components
    .iter()
    .map(|component| {
      let tag_name = current_imports
        .iter()
        .find(|import| import.module_path == component.module_path)
        .map_or_else(|| component.tag_name.clone(), |import| import.tag_name.clone());

      ComponentImport {
        module_path: component.module_path.clone(),
        tag_name,
      }
    })
    .collect::<Vec<_>>();

  for import in current_imports {
    if imports.iter().any(|candidate| candidate.module_path == import.module_path) {
      continue;
    }

    imports.push(import.clone());
  }

  imports
}

fn component_import_insert_offset(document: &DocumentContext) -> Option<usize> {
  let blocks = parse_document_blocks(document).ok()?;
  if is_component_path(&document.relative_path) {
    return blocks.script_span.map(|span| span.start);
  }

  blocks.script_setup_span.map(|span| span.start)
}

fn missing_component_import_edit(
  source: &str,
  component: &ComponentImport,
  current_imports: &[ComponentImport],
  import_insert_offset: Option<usize>,
) -> Option<Vec<TextEdit>> {
  if current_imports
    .iter()
    .any(|import| import.module_path == component.module_path)
  {
    return None;
  }

  let offset = import_insert_offset?;
  Some(vec![TextEdit {
    range: range_from_offsets(source, offset, offset),
    new_text: format!("use {};\n", component.module_path),
  }])
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

  if let Some((component, prop)) = component_prop_at_position_in_document(
    source,
    manifest,
    relative_path,
    position,
  ) {
    let range = current_component_prop_usage_range(source, position)
      .map(|range| range_from_offsets(source, range.start, range.end))
      .unwrap_or_else(|| range_from_span(&prop.source_span));
    return Some(component_prop_hover_at_range(component, prop, range));
  }

  if let Some(component) = component_tag_at_position_in_document(
    source,
    manifest,
    relative_path,
    position,
  ) {
    let range = current_component_tag_usage_range(source, position)
      .map(|range| range_from_offsets(source, range.start, range.end))
      .unwrap_or_else(full_document_range);
    return Some(component_hover(component, range));
  }

  if let Some((component, range)) = component_import_hover_target(
    source,
    manifest,
    relative_path,
    position,
  ) {
    return Some(component_hover(component, range));
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

  if let Some((component, prop)) = component_prop_at_position_in_document(
    source,
    manifest,
    relative_path,
    position,
  ) {
    let location = location_for_relative_path(project_root, &component.source_path, prop.source_span)?;
    return Ok(Some(GotoDefinitionResponse::Scalar(location)));
  }

  if let Some(component) = component_tag_at_position_in_document(
    source,
    manifest,
    relative_path,
    position,
  ) {
    let location = file_start_location(project_root, &component.source_path)?;
    return Ok(Some(GotoDefinitionResponse::Scalar(location)));
  }

  if let Some(component) = component_import_at_position(
    source,
    manifest,
    relative_path,
    position,
  ) {
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

  if let Some((component, prop)) = component_prop_at_position_in_document(
    source,
    manifest,
    relative_path,
    position,
  ) {
    let current_usage = current_component_prop_usage_range(source, position);
    locations.extend(component_prop_reference_locations(
      project_root,
      manifest,
      relative_path,
      source,
      component,
      prop,
      include_declaration,
      current_usage,
    )?);
    dedup_locations(&mut locations);
    return Ok((!locations.is_empty()).then_some(locations));
  }

  if let Some((component, prop)) = component_prop_definition_at_position(
    manifest,
    relative_path,
    position,
  ) {
    locations.extend(component_prop_reference_locations(
      project_root,
      manifest,
      relative_path,
      source,
      component,
      prop,
      include_declaration,
      None,
    )?);
    dedup_locations(&mut locations);
    return Ok((!locations.is_empty()).then_some(locations));
  }

  if let Some(component) = component_tag_at_position_in_document(
    source,
    manifest,
    relative_path,
    position,
  ) {
    let current_usage = current_component_tag_usage_range(source, position);
    locations.extend(component_tag_reference_locations(
      project_root,
      manifest,
      relative_path,
      source,
      component,
      include_declaration,
      current_usage,
    )?);
    dedup_locations(&mut locations);
    return Ok((!locations.is_empty()).then_some(locations));
  }

  if let Some(component) = component_import_at_position(
    source,
    manifest,
    relative_path,
    position,
  ) {
    locations.extend(component_tag_reference_locations(
      project_root,
      manifest,
      relative_path,
      source,
      component,
      include_declaration,
      None,
    )?);
    dedup_locations(&mut locations);
    return Ok((!locations.is_empty()).then_some(locations));
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

fn component_tag_at_position_in_document<'a>(
  source: &str,
  manifest: &'a ThebeManifest,
  relative_path: &str,
  position: Position,
) -> Option<&'a ComponentMetadata> {
  let offset = byte_offset_from_position(source, position)?;
  let (tag_name, start, end) = tag_name_at_offset(source, offset)?;
  if offset < start || offset > end {
    return None;
  }
  resolve_component_by_tag_name(source, manifest, relative_path, &tag_name)
}

fn component_prop_at_position_in_document<'a>(
  source: &str,
  manifest: &'a ThebeManifest,
  relative_path: &str,
  position: Position,
) -> Option<(&'a ComponentMetadata, &'a ComponentPropMetadata)> {
  let offset = byte_offset_from_position(source, position)?;
  let (tag_name, _, _) = tag_name_at_offset(source, offset)?;
  let component = resolve_component_by_tag_name(source, manifest, relative_path, &tag_name)?;
  let (attribute_name, _, _) = attribute_name_at_offset(source, offset)?;
  let prop_name = attribute_name.trim_start_matches(':');
  let prop = component.props.iter().find(|prop| prop.name == prop_name)?;
  Some((component, prop))
}

fn component_prop_definition_at_position<'a>(
  manifest: &'a ThebeManifest,
  relative_path: &str,
  position: Position,
) -> Option<(&'a ComponentMetadata, &'a ComponentPropMetadata)> {
  let component = manifest
    .components
    .iter()
    .find(|component| component.source_path == relative_path)?;
  let prop = component
    .props
    .iter()
    .find(|prop| position_in_span(position, &prop.source_span))?;
  Some((component, prop))
}

fn component_import_at_position<'a>(
  source: &str,
  manifest: &'a ThebeManifest,
  relative_path: &str,
  position: Position,
) -> Option<&'a ComponentMetadata> {
  component_import_hover_target(source, manifest, relative_path, position).map(|(component, _)| component)
}

fn component_import_hover_target<'a>(
  source: &str,
  manifest: &'a ThebeManifest,
  relative_path: &str,
  position: Position,
) -> Option<(&'a ComponentMetadata, Range)> {
  let document = DocumentContext {
    project_root: PathBuf::new(),
    relative_path: relative_path.to_owned(),
    source: source.to_owned(),
    cached_artifacts: None,
  };
  let import = component_import_occurrences(&document).into_iter().find(|import| {
    position_in_range(
      position,
      range_from_offsets(source, import.local_range.start, import.local_range.end),
    )
  })?;
  let range = range_from_offsets(source, import.local_range.start, import.local_range.end);

  let component = manifest
    .components
    .iter()
    .find(|component| component.module_path == import.module_path)?;
  Some((component, range))
}

fn resolve_component_by_tag_name<'a>(
  source: &str,
  manifest: &'a ThebeManifest,
  relative_path: &str,
  tag_name: &str,
) -> Option<&'a ComponentMetadata> {
  if let Some(component) = manifest
    .components
    .iter()
    .find(|component| component.tag_name == tag_name)
  {
    return Some(component);
  }

  let document = DocumentContext {
    project_root: PathBuf::new(),
    relative_path: relative_path.to_owned(),
    source: source.to_owned(),
    cached_artifacts: None,
  };
  let import = current_component_imports(&document)
    .into_iter()
    .find(|import| import.tag_name == tag_name)?;
  manifest
    .components
    .iter()
    .find(|component| component.module_path == import.module_path)
}

fn current_component_prop_usage_range(source: &str, position: Position) -> Option<ByteRange> {
  let offset = byte_offset_from_position(source, position)?;
  let (attribute_name, start, end) = attribute_name_at_offset(source, offset)?;
  let prop_name = attribute_name.trim_start_matches(':');
  let prefix = attribute_name.len().saturating_sub(prop_name.len());

  Some(ByteRange {
    start: start + prefix,
    end,
  })
}

fn current_component_tag_usage_range(source: &str, position: Position) -> Option<ByteRange> {
  let offset = byte_offset_from_position(source, position)?;
  let (_, start, end) = tag_name_at_offset(source, offset)?;
  Some(ByteRange { start, end })
}

fn component_tag_reference_locations(
  project_root: &Path,
  manifest: &ThebeManifest,
  current_relative_path: &str,
  current_source: &str,
  component: &ComponentMetadata,
  include_declaration: bool,
  current_usage: Option<ByteRange>,
) -> Result<Vec<Location>> {
  let mut locations = Vec::new();

  if include_declaration {
    locations.push(file_start_location(project_root, &component.source_path)?);
  }

  for relative_path in known_trs_source_paths(manifest, current_relative_path)
    .into_iter()
  {
    let Some(source) = source_for_relative_path(
      project_root,
      current_relative_path,
      current_source,
      &relative_path,
    ) else {
      continue;
    };

    for range in component_tag_reference_ranges_in_source(&relative_path, &source, component) {
      if relative_path == current_relative_path && current_usage == Some(range) {
        continue;
      }
      locations.push(location_for_byte_range(project_root, &relative_path, &source, range)?);
    }
  }

  Ok(locations)
}

fn component_prop_reference_locations(
  project_root: &Path,
  manifest: &ThebeManifest,
  current_relative_path: &str,
  current_source: &str,
  component: &ComponentMetadata,
  prop: &ComponentPropMetadata,
  include_declaration: bool,
  current_usage: Option<ByteRange>,
) -> Result<Vec<Location>> {
  let mut locations = Vec::new();

  if include_declaration {
    locations.push(location_for_relative_path(
      project_root,
      &component.source_path,
      prop.source_span,
    )?);
  }

  for relative_path in known_trs_source_paths(manifest, current_relative_path)
    .into_iter()
  {
    let Some(source) = source_for_relative_path(
      project_root,
      current_relative_path,
      current_source,
      &relative_path,
    ) else {
      continue;
    };

    for range in component_prop_reference_ranges_in_source(&relative_path, &source, component, &prop.name) {
      if relative_path == current_relative_path && current_usage == Some(range) {
        continue;
      }
      locations.push(location_for_byte_range(project_root, &relative_path, &source, range)?);
    }
  }

  Ok(locations)
}

fn component_prop_reference_ranges_in_source(
  relative_path: &str,
  source: &str,
  component: &ComponentMetadata,
  prop_name: &str,
) -> Vec<ByteRange> {
  let blocks = parse_blocks_for_path(relative_path, source).ok();
  let template_spans = blocks
    .as_ref()
    .map(|blocks| blocks.template_spans.as_slice())
    .unwrap_or(&[]);
  let tag_names = component_local_tag_names_for_source(relative_path, source, component);
  if tag_names.is_empty() {
    return Vec::new();
  }

  let ranges = component_prop_reference_ranges(source, template_spans, &tag_names, prop_name);
  if !ranges.is_empty() || template_spans.is_empty() {
    return ranges;
  }

  component_prop_reference_ranges(
    source,
    &[thebe_parser::SourceSpan {
      start: 0,
      end: source.len(),
    }],
    &tag_names,
    prop_name,
  )
}

fn component_tag_reference_ranges_in_source(
  relative_path: &str,
  source: &str,
  component: &ComponentMetadata,
) -> Vec<ByteRange> {
  let blocks = parse_blocks_for_path(relative_path, source).ok();
  let template_spans = blocks
    .as_ref()
    .map(|blocks| blocks.template_spans.as_slice())
    .unwrap_or(&[]);
  let tag_names = component_local_tag_names_for_source(relative_path, source, component);
  if tag_names.is_empty() {
    return Vec::new();
  }

  let ranges = tag_names
    .iter()
    .flat_map(|tag_name| template_tag_name_ranges(source, template_spans, tag_name))
    .collect::<Vec<_>>();
  if !ranges.is_empty() || template_spans.is_empty() {
    return ranges;
  }

  tag_names
    .iter()
    .flat_map(|tag_name| {
      template_tag_name_ranges(
        source,
        &[thebe_parser::SourceSpan {
          start: 0,
          end: source.len(),
        }],
        tag_name,
      )
    })
    .collect()
}

fn component_local_tag_names_for_source(
  relative_path: &str,
  source: &str,
  component: &ComponentMetadata,
) -> Vec<String> {
  let document = DocumentContext {
    project_root: PathBuf::new(),
    relative_path: relative_path.to_owned(),
    source: source.to_owned(),
    cached_artifacts: None,
  };
  let mut tag_names = current_component_imports(&document)
    .into_iter()
    .filter(|import| import.module_path == component.module_path)
    .map(|import| import.tag_name)
    .collect::<Vec<_>>();

  if tag_names.is_empty() {
    tag_names.push(component.tag_name.clone());
  }

  tag_names.sort();
  tag_names.dedup();
  tag_names
}

fn component_prop_reference_ranges(
  source: &str,
  template_spans: &[thebe_parser::SourceSpan],
  tag_names: &[String],
  prop_name: &str,
) -> Vec<ByteRange> {
  let spans = if template_spans.is_empty() {
    vec![thebe_parser::SourceSpan {
      start: 0,
      end: source.len(),
    }]
  } else {
    template_spans.to_vec()
  };
  let mut ranges = Vec::new();

  for span in spans {
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
      let is_closing = tag.as_bytes().get(1).is_some_and(|byte| *byte == b'/');
      let Some((tag_name, mut cursor)) = open_tag_name(tag) else {
        idx = tag_end + 1;
        continue;
      };
      if is_closing || !tag_names.iter().any(|candidate| candidate == &tag_name) {
        idx = tag_end + 1;
        continue;
      }

      let tag_bytes = tag.as_bytes();
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
        let attribute_prop = attribute_name.trim_start_matches(':');
        if attribute_prop == prop_name {
          let prefix = attribute_name.len().saturating_sub(attribute_prop.len());
          let start = idx + name_start + prefix;
          ranges.push(ByteRange {
            start,
            end: start + prop_name.len(),
          });
        }

        while cursor < tag_bytes.len() && tag_bytes[cursor].is_ascii_whitespace() {
          cursor += 1;
        }
        if cursor < tag_bytes.len() && tag_bytes[cursor] == b'=' {
          cursor += 1;
          while cursor < tag_bytes.len() && tag_bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
          }
          if cursor < tag_bytes.len() && matches!(tag_bytes[cursor], b'\'' | b'"') {
            let quote = tag_bytes[cursor];
            cursor += 1;
            while cursor < tag_bytes.len() && tag_bytes[cursor] != quote {
              cursor += 1;
            }
            cursor = cursor.saturating_add(1);
          } else {
            while cursor < tag_bytes.len()
              && !tag_bytes[cursor].is_ascii_whitespace()
              && tag_bytes[cursor] != b'>'
            {
              cursor += 1;
            }
          }
        }
      }

      idx = tag_end + 1;
    }
  }

  ranges
}

fn parse_blocks_for_path(
  relative_path: &str,
  source: &str,
) -> std::result::Result<SfcBlocks, thebe_parser::ParseError> {
  if is_component_path(relative_path) {
    parse_component_source_blocks(source)
  } else {
    parse_source_blocks(source)
  }
}

fn source_for_relative_path(
  project_root: &Path,
  current_relative_path: &str,
  current_source: &str,
  relative_path: &str,
) -> Option<String> {
  if relative_path == current_relative_path {
    return Some(current_source.to_owned());
  }

  std::fs::read_to_string(project_root.join(relative_path)).ok()
}

fn location_for_byte_range(
  project_root: &Path,
  relative_path: &str,
  source: &str,
  range: ByteRange,
) -> Result<Location> {
  Ok(Location {
    uri: file_url(project_root, relative_path)?,
    range: range_from_offsets(source, range.start, range.end),
  })
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
  let mut changes = HashMap::new();

  for edit in target.edits {
    let uri = file_url(&document.project_root, &edit.relative_path).ok()?;
    changes
      .entry(uri)
      .or_insert_with(Vec::new)
      .push(edit.edit.into_text_edit(new_name));
  }

  Some(WorkspaceEdit {
    changes: Some(changes),
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

fn document_highlights_for_document(
  document: &DocumentContext,
  position: Position,
) -> Option<Vec<DocumentHighlight>> {
  let target = rename_target(document, position)?;
  let local_edits = target
    .edits
    .iter()
    .filter(|edit| edit.relative_path == document.relative_path)
    .cloned()
    .collect::<Vec<_>>();
  let write_range = local_edits.first().map(LocatedRenameEdit::range)?;
  let mut highlights = local_edits
    .into_iter()
    .map(|edit| DocumentHighlight {
      range: edit.range(),
      kind: Some(if edit.range() == write_range {
        DocumentHighlightKind::WRITE
      } else {
        DocumentHighlightKind::READ
      }),
    })
    .collect::<Vec<_>>();

  highlights.sort_by(|left, right| {
    compare_positions(left.range.start, right.range.start)
      .then_with(|| compare_positions(left.range.end, right.range.end))
  });
  highlights.dedup_by(|left, right| left.range == right.range && left.kind == right.kind);

  Some(highlights)
}

fn linked_editing_ranges_for_document(
  document: &DocumentContext,
  position: Position,
) -> Option<LinkedEditingRanges> {
  if !document.relative_path.ends_with(".trs") {
    return None;
  }

  let template_spans = parse_document_blocks(document)
    .ok()
    .map(|blocks| blocks.template_spans)
    .unwrap_or_default();
  let pair = template_tag_pair_at_position(&document.source, &template_spans, position).or_else(
    || {
      template_tag_pair_at_position(
        &document.source,
        &[thebe_parser::SourceSpan {
          start: 0,
          end: document.source.len(),
        }],
        position,
      )
    },
  )?;
  Some(LinkedEditingRanges {
    ranges: vec![
      range_from_offsets(&document.source, pair.open.start, pair.open.end),
      range_from_offsets(&document.source, pair.close.start, pair.close.end),
    ],
    word_pattern: Some("[A-Za-z][A-Za-z0-9:_-]*".to_owned()),
  })
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
  edits: Vec<LocatedRenameEdit>,
  range: Range,
}

#[derive(Debug, Clone)]
enum RenameEdit {
  ImportAlias { imported_name: String, range: Range },
  Replace { range: Range },
}

impl RenameEdit {
  fn range(&self) -> Range {
    match self {
      Self::ImportAlias { range, .. } | Self::Replace { range } => range.clone(),
    }
  }

  fn into_text_edit(self, new_name: &str) -> TextEdit {
    match self {
      Self::ImportAlias {
        imported_name,
        range,
      } => TextEdit {
        range,
        new_text: format!("{imported_name} as {new_name}"),
      },
      Self::Replace { range } => TextEdit {
        range,
        new_text: new_name.to_owned(),
      },
    }
  }
}

#[derive(Debug, Clone)]
struct LocatedRenameEdit {
  edit: RenameEdit,
  relative_path: String,
}

impl LocatedRenameEdit {
  fn local(document: &DocumentContext, edit: RenameEdit) -> Self {
    Self {
      edit,
      relative_path: document.relative_path.clone(),
    }
  }

  fn range(&self) -> Range {
    self.edit.range()
  }
}

fn rename_target(document: &DocumentContext, position: Position) -> Option<RenameTarget> {
  route_handler_rename_target(document, position)
    .or_else(|| template_symbol_rename_target(document, position))
    .or_else(|| component_prop_rename_target(document, position))
    .or_else(|| component_tag_rename_target(document, position))
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
    edits: vec![LocatedRenameEdit::local(
      document,
      RenameEdit::Replace {
        range,
      },
    )],
    range,
  })
}

fn template_symbol_rename_target(
  document: &DocumentContext,
  position: Position,
) -> Option<RenameTarget> {
  let manifest = &document.cached_artifacts.as_ref()?.manifest;
  let route = manifest
    .routes
    .iter()
    .find(|route| route.source_path == document.relative_path)?;

  if let Some(definition) = route
    .template_symbol_definitions
    .iter()
    .find(|definition| position_in_span(position, &definition.source_span))
  {
    let range = range_from_span(&definition.source_span);
    return Some(RenameTarget {
      current_name: definition.field_name.clone(),
      edits: template_symbol_rename_edits(document, route, &definition.path),
      range,
    });
  }

  let binding = route
    .template_binding_spans
    .iter()
    .find(|binding| position_in_span(position, &binding.source_span))?;
  let path_match = binding_path_match_at_position(&document.source, binding, position)?;
  let definition = template_symbol_definition(route, &path_match.path)?;

  Some(RenameTarget {
    current_name: definition.field_name.clone(),
    edits: template_symbol_rename_edits(document, route, &path_match.path),
    range: path_match.range(&document.source),
  })
}

fn template_symbol_rename_edits(
  document: &DocumentContext,
  route: &RouteMetadata,
  symbol_path: &str,
) -> Vec<LocatedRenameEdit> {
  let mut edits = Vec::new();

  if let Some(definition) = template_symbol_definition(route, symbol_path) {
    edits.push(LocatedRenameEdit::local(
      document,
      RenameEdit::Replace {
        range: range_from_span(&definition.source_span),
      },
    ));
  }

  edits.extend(route.template_binding_spans.iter().filter_map(|binding| {
    template_symbol_binding_edit_range(&document.source, binding, symbol_path).map(|range| {
      LocatedRenameEdit::local(
        document,
        RenameEdit::Replace {
          range: range_from_offsets(&document.source, range.start, range.end),
        },
      )
    })
  }));

  edits
}

fn template_symbol_binding_edit_range(
  source: &str,
  binding: &TemplateBindingMetadata,
  symbol_path: &str,
) -> Option<ByteRange> {
  if binding.name != symbol_path && !binding.name.starts_with(&format!("{symbol_path}.")) {
    return None;
  }

  let token = &source[binding.source_span.start_byte..binding.source_span.end_byte];
  let binding_name_start = token.find(&binding.name)? + binding.source_span.start_byte;
  let field_name = symbol_path.rsplit('.').next()?;
  let field_offset = symbol_path.rfind('.').map_or(0, |index| index + 1);
  let start = binding_name_start + field_offset;

  Some(ByteRange {
    start,
    end: start + field_name.len(),
  })
}

fn component_prop_rename_target(
  document: &DocumentContext,
  position: Position,
) -> Option<RenameTarget> {
  let manifest = &document.cached_artifacts.as_ref()?.manifest;
  let (component, prop, range) = current_component_prop_target(document, manifest, position)?;

  Some(RenameTarget {
    current_name: prop.name.clone(),
    edits: component_prop_rename_edits(document, manifest, component, prop),
    range,
  })
}

fn current_component_prop_target<'a>(
  document: &'a DocumentContext,
  manifest: &'a ThebeManifest,
  position: Position,
) -> Option<(&'a ComponentMetadata, &'a ComponentPropMetadata, Range)> {
  if let Some((component, prop)) = component_prop_at_position_in_document(
    &document.source,
    manifest,
    &document.relative_path,
    position,
  ) {
    let usage_range = current_component_prop_usage_range(&document.source, position)?;
    return Some((
      component,
      prop,
      range_from_offsets(&document.source, usage_range.start, usage_range.end),
    ));
  }

  let (component, prop) = component_prop_definition_at_position(
    manifest,
    &document.relative_path,
    position,
  )?;
  Some((component, prop, range_from_span(&prop.source_span)))
}

fn component_prop_rename_edits(
  document: &DocumentContext,
  manifest: &ThebeManifest,
  component: &ComponentMetadata,
  prop: &ComponentPropMetadata,
) -> Vec<LocatedRenameEdit> {
  let mut edits = vec![LocatedRenameEdit {
    edit: RenameEdit::Replace {
      range: range_from_span(&prop.source_span),
    },
    relative_path: component.source_path.clone(),
  }];

  for relative_path in known_trs_source_paths(manifest, &document.relative_path)
    .into_iter()
  {
    let Some(source) = source_for_relative_path(
      &document.project_root,
      &document.relative_path,
      &document.source,
      &relative_path,
    ) else {
      continue;
    };

    edits.extend(
      component_prop_reference_ranges_in_source(&relative_path, &source, component, &prop.name)
        .into_iter()
        .map(|range| LocatedRenameEdit {
          edit: RenameEdit::Replace {
            range: range_from_offsets(&source, range.start, range.end),
          },
          relative_path: relative_path.clone(),
        }),
    );
  }

  edits.sort_by(|left, right| {
    left
      .relative_path
      .cmp(&right.relative_path)
      .then_with(|| compare_positions(left.range().start, right.range().start))
      .then_with(|| compare_positions(left.range().end, right.range().end))
  });
  edits.dedup_by(|left, right| left.relative_path == right.relative_path && left.range() == right.range());
  edits
}

fn component_tag_rename_target(
  document: &DocumentContext,
  position: Position,
) -> Option<RenameTarget> {
  let manifest = &document.cached_artifacts.as_ref()?.manifest;
  parse_document_blocks(document).ok()?;
  let imports = component_import_occurrences(document);

  for import in imports {
    let usage_ranges = component_tag_usage_ranges_for_local_name(
      &document.relative_path,
      &document.source,
      &import.local_name,
    );
    let import_range = range_from_offsets(
      &document.source,
      import.local_range.start,
      import.local_range.end,
    );
    let current_usage = usage_ranges.iter().find_map(|range| {
      let range = range_from_offsets(&document.source, range.start, range.end);
      position_in_range(position, range.clone()).then_some(range)
    });

    if !position_in_range(position, import_range.clone()) && current_usage.is_none() {
      continue;
    }
    let component = manifest
      .components
      .iter()
      .find(|component| component.module_path == import.module_path)?;

    return Some(RenameTarget {
      current_name: import.local_name,
      edits: component_tag_rename_edits(document, manifest, component),
      range: current_usage.unwrap_or(import_range),
    });
  }

  None
}

fn component_tag_rename_edits(
  document: &DocumentContext,
  manifest: &ThebeManifest,
  component: &ComponentMetadata,
) -> Vec<LocatedRenameEdit> {
  let mut edits = Vec::new();

  for relative_path in known_trs_source_paths(manifest, &document.relative_path)
    .into_iter()
  {
    let Some(source) = source_for_relative_path(
      &document.project_root,
      &document.relative_path,
      &document.source,
      &relative_path,
    ) else {
      continue;
    };
    let source_document = DocumentContext {
      project_root: PathBuf::new(),
      relative_path: relative_path.clone(),
      source: source.clone(),
      cached_artifacts: None,
    };

    edits.extend(
      component_import_occurrences(&source_document)
        .into_iter()
        .filter(|import| import.module_path == component.module_path)
        .flat_map(|import| {
          let import_range = range_from_offsets(
            &source,
            import.local_range.start,
            import.local_range.end,
          );
          let import_edit = if import.imported_name == import.local_name {
            LocatedRenameEdit {
              edit: RenameEdit::ImportAlias {
                imported_name: import.imported_name,
                range: import_range,
              },
              relative_path: relative_path.clone(),
            }
          } else {
            LocatedRenameEdit {
              edit: RenameEdit::Replace { range: import_range },
              relative_path: relative_path.clone(),
            }
          };

          std::iter::once(import_edit).chain(
            component_tag_usage_ranges_for_local_name(
              &relative_path,
              &source,
              &import.local_name,
            )
            .into_iter()
            .map(|range| LocatedRenameEdit {
              edit: RenameEdit::Replace {
                range: range_from_offsets(&source, range.start, range.end),
              },
              relative_path: relative_path.clone(),
            }),
          )
        }),
    );
  }

  edits.sort_by(|left, right| {
    left
      .relative_path
      .cmp(&right.relative_path)
      .then_with(|| compare_positions(left.range().start, right.range().start))
      .then_with(|| compare_positions(left.range().end, right.range().end))
  });
  edits.dedup_by(|left, right| left.relative_path == right.relative_path && left.range() == right.range());
  edits
}

fn component_tag_usage_ranges_for_local_name(
  relative_path: &str,
  source: &str,
  local_name: &str,
) -> Vec<ByteRange> {
  let blocks = parse_blocks_for_path(relative_path, source).ok();
  let template_spans = blocks
    .as_ref()
    .map(|blocks| blocks.template_spans.as_slice())
    .unwrap_or(&[]);
  let ranges = template_tag_name_ranges(source, template_spans, local_name);
  if !ranges.is_empty() || template_spans.is_empty() {
    return ranges;
  }

  template_tag_name_ranges(
    source,
    &[thebe_parser::SourceSpan {
      start: 0,
      end: source.len(),
    }],
    local_name,
  )
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
      edits.push(LocatedRenameEdit::local(
        document,
        RenameEdit::Replace {
          range: range_from_offsets(&document.source, definition.start, definition.end),
        },
      ));
      edits.extend(usage_ranges.into_iter().map(|range| {
        LocatedRenameEdit::local(
          document,
          RenameEdit::Replace {
            range: range_from_offsets(&document.source, range.start, range.end),
          },
        )
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

fn template_tag_occurrences(
  source: &str,
  template_spans: &[thebe_parser::SourceSpan],
) -> Vec<TagNameOccurrence> {
  let mut occurrences = Vec::new();

  if template_spans.is_empty() {
    scan_template_tag_occurrences_in_span(
      source,
      thebe_parser::SourceSpan {
        start: 0,
        end: source.len(),
      },
      &mut occurrences,
    );
    return occurrences;
  }

  for span in template_spans {
    scan_template_tag_occurrences_in_span(source, *span, &mut occurrences);
  }

  occurrences
}

fn scan_template_tag_occurrences_in_span(
  source: &str,
  span: thebe_parser::SourceSpan,
  occurrences: &mut Vec<TagNameOccurrence>,
) {
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
    if let Some(occurrence) = tag_name_occurrence(tag, idx) {
      occurrences.push(occurrence);
    }
    idx = tag_end + 1;
  }
}

fn tag_name_occurrence(tag: &str, absolute_start: usize) -> Option<TagNameOccurrence> {
  let bytes = tag.as_bytes();
  let mut idx = 1usize;
  let mut is_closing = false;

  if bytes.get(idx).is_some_and(|byte| *byte == b'/') {
    is_closing = true;
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
  if start == idx {
    return None;
  }

  Some(TagNameOccurrence {
    is_closing,
    name: tag[start..idx].to_owned(),
    range: ByteRange {
      start: absolute_start + start,
      end: absolute_start + idx,
    },
    self_closing: !is_closing && tag.trim_end().ends_with("/>") ,
  })
}

fn template_tag_name_ranges(
  source: &str,
  template_spans: &[thebe_parser::SourceSpan],
  tag_name: &str,
) -> Vec<ByteRange> {
  template_tag_occurrences(source, template_spans)
    .into_iter()
    .filter(|occurrence| occurrence.name == tag_name)
    .map(|occurrence| occurrence.range)
    .collect()
}

fn template_tag_pair_at_position(
  source: &str,
  template_spans: &[thebe_parser::SourceSpan],
  position: Position,
) -> Option<TemplateTagPair> {
  let offset = byte_offset_from_position(source, position)?;
  template_tag_pairs(source, template_spans)
    .into_iter()
    .find(|pair| {
      (offset >= pair.open.start && offset <= pair.open.end)
        || (offset >= pair.close.start && offset <= pair.close.end)
    })
}

fn template_tag_pairs(
  source: &str,
  template_spans: &[thebe_parser::SourceSpan],
) -> Vec<TemplateTagPair> {
  let mut stack = Vec::new();
  let mut pairs = Vec::new();

  for occurrence in template_tag_occurrences(source, template_spans) {
    if occurrence.is_closing {
      if let Some(index) = stack
        .iter()
        .rposition(|open: &TagNameOccurrence| open.name == occurrence.name)
      {
        let open = stack.remove(index);
        pairs.push(TemplateTagPair {
          close: occurrence.range,
          open: open.range,
        });
      }
    } else if !occurrence.self_closing {
      stack.push(occurrence);
    }
  }

  pairs
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
  format_trs_document_with(
    document,
    blocks,
    try_format_rust_source,
    try_format_typescript_source,
    try_format_css_source,
  )
}

fn format_trs_document_with<R, T, C>(
  document: &DocumentContext,
  blocks: &SfcBlocks,
  rust_formatter: R,
  script_ts_formatter: T,
  style_formatter: C,
) -> String
where
  R: Fn(&str) -> Option<String>,
  T: Fn(&str) -> Option<String>,
  C: Fn(&str) -> Option<String>,
{
  let mut sections = Vec::new();

  if let Some(head) = blocks.head.as_deref() {
    sections.push(render_block("head", head));
  }
  if is_component_path(&document.relative_path) {
    if let Some(script) = blocks.script.as_deref() {
      sections.push(render_formatted_block("script", script, &rust_formatter));
    }
  } else if let Some(script_setup) = blocks.script_setup.as_deref() {
    sections.push(render_formatted_block(
      "script setup",
      script_setup,
      &rust_formatter,
    ));
  }
  if let Some(script_ts) = blocks.script_ts.as_deref() {
    sections.push(render_formatted_block(
      "script lang=\"ts\"",
      script_ts,
      &script_ts_formatter,
    ));
  }
  if !blocks.template.trim().is_empty() {
    sections.push(dedent_block(&blocks.template, 0));
  }
  if let Some(style) = blocks.style.as_deref() {
    sections.push(render_formatted_block("style", style, &style_formatter));
  }

  let mut output = sections.join("\n\n");
  output.push('\n');
  output
}

fn render_formatted_block<F>(tag: &str, contents: &str, formatter: &F) -> String
where
  F: Fn(&str) -> Option<String>,
{
  let formatted = formatter(contents).unwrap_or_else(|| contents.to_owned());
  render_block(tag, &formatted)
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

#[cfg(test)]
fn no_embedded_formatter(_: &str) -> Option<String> {
  None
}

fn try_format_rust_source(source: &str) -> Option<String> {
  if source.trim().is_empty() {
    return Some(String::new());
  }

  let mut child = Command::new("rustfmt")
    .args(["--emit", "stdout", "--edition", "2024"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .spawn()
    .ok()?;

  let mut stdin = child.stdin.take()?;
  stdin.write_all(source.as_bytes()).ok()?;
  drop(stdin);

  let output = child.wait_with_output().ok()?;
  if !output.status.success() {
    return None;
  }

  String::from_utf8(output.stdout).ok()
}

fn try_format_typescript_source(source: &str) -> Option<String> {
  if source.trim().is_empty() {
    return Some(String::new());
  }

  thebe_analyzer::format_typescript(source).ok()
}

fn try_format_css_source(source: &str) -> Option<String> {
  if source.trim().is_empty() {
    return Some(String::new());
  }

  thebe_css::format_style_block(source).ok()
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

fn known_trs_source_paths(manifest: &ThebeManifest, current_relative_path: &str) -> Vec<String> {
  let mut paths = known_source_paths(manifest)
    .into_iter()
    .filter(|path| path.ends_with(".trs"))
    .collect::<Vec<_>>();

  if current_relative_path.ends_with(".trs") {
    paths.push(current_relative_path.to_owned());
  }

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

fn component_hover(component: &ComponentMetadata, range: Range) -> Hover {
  let props = if component.prop_names.is_empty() {
    "none".to_owned()
  } else {
    component.prop_names.join(", ")
  };

  Hover {
    contents: HoverContents::Markup(MarkupContent {
      kind: MarkupKind::Markdown,
      value: format!(
        "**Component** `{}`\n\n- Module: `{}`\n- Props: `{}`",
        component.tag_name, component.module_path, props,
      ),
    }),
    range: Some(range),
  }
}

fn component_prop_hover(component: &ComponentMetadata, prop: &ComponentPropMetadata) -> Hover {
  component_prop_hover_at_range(component, prop, range_from_span(&prop.source_span))
}

fn component_prop_hover_at_range(
  component: &ComponentMetadata,
  prop: &ComponentPropMetadata,
  range: Range,
) -> Hover {
  Hover {
    contents: HoverContents::Markup(MarkupContent {
      kind: MarkupKind::Markdown,
      value: format!(
        "**Component prop** `{}`\n\n- Component: `{}`\n- Type: `{}`",
        prop.name, component.tag_name, prop.type_name,
      ),
    }),
    range: Some(range),
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
  reason = "lsp-types 0.94 still requires populating SymbolInformation::deprecated"
)]
fn workspace_symbols_for_manifest(
  project_root: &Path,
  manifest: &ThebeManifest,
  query: &str,
) -> Result<Vec<SymbolInformation>> {
  let mut symbols = Vec::new();

  for route in &manifest.routes {
    let route_location = file_start_location(project_root, &route.source_path)?;
    if workspace_symbol_matches(
      query,
      [&route.route_path, &route.source_path, &route.module_name],
    ) {
      symbols.push(SymbolInformation {
        name: route.route_path.clone(),
        kind: SymbolKind::MODULE,
        tags: None,
        deprecated: None,
        location: route_location.clone(),
        container_name: Some(route.source_path.clone()),
      });
    }

    let handler_location = route.handler.source_span.map_or_else(
      || file_start_location(project_root, &route.source_path),
      |span| location_for_relative_path(project_root, &route.source_path, span),
    )?;
    if workspace_symbol_matches(
      query,
      [&route.handler.name, &route.route_path, &route.source_path],
    ) {
      symbols.push(SymbolInformation {
        name: route.handler.name.clone(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        deprecated: None,
        location: handler_location,
        container_name: Some(route.route_path.clone()),
      });
    }

    for definition in &route.template_symbol_definitions {
      if !workspace_symbol_matches(
        query,
        [
          definition.path.as_str(),
          definition.field_name.as_str(),
          route.route_path.as_str(),
          definition.type_name.as_str(),
        ],
      ) {
        continue;
      }

      symbols.push(SymbolInformation {
        name: definition.path.clone(),
        kind: SymbolKind::VARIABLE,
        tags: None,
        deprecated: None,
        location: location_for_relative_path(
          project_root,
          &route.source_path,
          definition.source_span,
        )?,
        container_name: Some(route.route_path.clone()),
      });
    }
  }

  for layout in &manifest.layouts {
    let layout_location = file_start_location(project_root, &layout.source_path)?;
    if workspace_symbol_matches(query, [&layout.source_path, &layout.scope_path]) {
      symbols.push(SymbolInformation {
        name: layout.source_path.clone(),
        kind: SymbolKind::MODULE,
        tags: None,
        deprecated: None,
        location: layout_location,
        container_name: Some("layout".to_owned()),
      });
    }

    for binding in &layout.template_binding_spans {
      if !workspace_symbol_matches(query, [&binding.name, &layout.source_path]) {
        continue;
      }

      symbols.push(SymbolInformation {
        name: binding.name.clone(),
        kind: SymbolKind::VARIABLE,
        tags: None,
        deprecated: None,
        location: location_for_relative_path(project_root, &layout.source_path, binding.source_span)?,
        container_name: Some(layout.source_path.clone()),
      });
    }
  }

  for component in &manifest.components {
    let component_location = file_start_location(project_root, &component.source_path)?;
    if workspace_symbol_matches(
      query,
      [&component.tag_name, &component.module_path, &component.source_path],
    ) {
      symbols.push(SymbolInformation {
        name: component.tag_name.clone(),
        kind: SymbolKind::CLASS,
        tags: None,
        deprecated: None,
        location: component_location,
        container_name: Some(component.module_path.clone()),
      });
    }

    for prop in &component.props {
      if !workspace_symbol_matches(
        query,
        [&prop.name, &component.tag_name, &component.module_path],
      ) {
        continue;
      }

      symbols.push(SymbolInformation {
        name: prop.name.clone(),
        kind: SymbolKind::PROPERTY,
        tags: None,
        deprecated: None,
        location: location_for_relative_path(project_root, &component.source_path, prop.source_span)?,
        container_name: Some(component.tag_name.clone()),
      });
    }
  }

  Ok(symbols)
}

fn workspace_symbol_matches<T>(query: &str, candidates: impl IntoIterator<Item = T>) -> bool
where
  T: AsRef<str>,
{
  let query = query.trim();
  if query.is_empty() {
    return true;
  }

  let query = query.to_ascii_lowercase();
  candidates
    .into_iter()
    .any(|candidate| candidate.as_ref().to_ascii_lowercase().contains(&query))
}

fn sort_symbol_information(symbols: &mut [SymbolInformation]) {
  symbols.sort_by(|left, right| {
    left
      .name
      .cmp(&right.name)
      .then_with(|| left.location.uri.as_str().cmp(right.location.uri.as_str()))
      .then_with(|| compare_positions(left.location.range.start, right.location.range.start))
      .then_with(|| compare_positions(left.location.range.end, right.location.range.end))
  });
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
  fn format_trs_document_uses_rust_formatter_for_route_script_setup() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: r#"<script setup>
fn   handler()->Props{Props{count:0}}
</script>

<main>{{ count }}</main>
"#
      .to_owned(),
      cached_artifacts: None,
    };
    let blocks = parse_document_blocks(&document).expect("valid blocks");

    let formatted = format_trs_document_with(
      &document,
      &blocks,
      |_| Some("fn handler() -> Props {\n    Props { count: 0 }\n}".to_owned()),
      no_embedded_formatter,
      no_embedded_formatter,
    );

    assert!(formatted.contains(
      "<script setup>\n  fn handler() -> Props {\n      Props { count: 0 }\n  }\n</script>"
    ));
  }

  #[test]
  fn format_trs_document_uses_rust_formatter_for_component_script() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/components/Counter.trs".to_owned(),
      source: r#"<script>
pub   struct Props{pub count:i64}
</script>

<main>{{ props.count }}</main>
"#
      .to_owned(),
      cached_artifacts: None,
    };
    let blocks = parse_document_blocks(&document).expect("valid blocks");

    let formatted = format_trs_document_with(
      &document,
      &blocks,
      |_| Some("pub struct Props {\n    pub count: i64,\n}".to_owned()),
      no_embedded_formatter,
      no_embedded_formatter,
    );

    assert!(formatted.contains(
      "<script>\n  pub struct Props {\n      pub count: i64,\n  }\n</script>"
    ));
  }

  #[test]
  fn format_trs_document_falls_back_when_rust_formatter_is_unavailable() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: r#"<script setup>
    fn handler() -> Props {
        Props { count: 0 }
    }
</script>
"#
      .to_owned(),
      cached_artifacts: None,
    };
    let blocks = parse_document_blocks(&document).expect("valid blocks");

    let formatted = format_trs_document_with(
      &document,
      &blocks,
      |_| None,
      no_embedded_formatter,
      no_embedded_formatter,
    );

    assert_eq!(
      formatted,
      "<script setup>\n  fn handler() -> Props {\n          Props { count: 0 }\n      }\n</script>\n"
    );
  }

  #[test]
  fn format_trs_document_uses_typescript_formatter_for_client_script() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: r#"<script lang="ts">
function increment(step:number){return step+1}
</script>
"#
      .to_owned(),
      cached_artifacts: None,
    };
    let blocks = parse_document_blocks(&document).expect("valid blocks");

    let formatted = format_trs_document_with(
      &document,
      &blocks,
      no_embedded_formatter,
      |_| Some("function increment(step: number) {\n  return step + 1;\n}".to_owned()),
      no_embedded_formatter,
    );

    assert!(formatted.contains(
      "<script lang=\"ts\">\n  function increment(step: number) {\n    return step + 1;\n  }\n</script>"
    ));
  }

  #[test]
  fn format_trs_document_uses_css_formatter_for_style_block() {
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: r#"<style>
button{color:red}
</style>
"#
      .to_owned(),
      cached_artifacts: None,
    };
    let blocks = parse_document_blocks(&document).expect("valid blocks");

    let formatted = format_trs_document_with(
      &document,
      &blocks,
      no_embedded_formatter,
      no_embedded_formatter,
      |_| Some("button {\n  color: red;\n}".to_owned()),
    );

    assert!(formatted.contains("<style>\n  button {\n    color: red;\n  }\n</style>"));
  }

  #[test]
  fn rename_for_document_updates_top_level_template_symbol_definition_and_bindings() {
    let source = fixture_route_source().to_owned();
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/profile.trs".to_owned(),
      source: source.clone(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };
    let position = position_from_byte_offset(
      &source,
      source.find("{{ username }}").expect("username binding") + 5,
    );

    let edit = rename_for_document(&document, position, "handle").expect("rename edit");
    let changes = edit.changes.expect("rename changes");
    let edits = changes.values().next().expect("rename file edits");

    assert_eq!(edits.len(), 3);
    assert!(edits.iter().all(|edit| edit.new_text == "handle"));
    assert!(edits.iter().any(|edit| edit.range.start == Position::new(2, 2)));
  }

  #[test]
  fn rename_for_document_updates_nested_template_symbol_definition_and_binding_segment() {
    let source = fixture_route_source().to_owned();
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/profile.trs".to_owned(),
      source: source.clone(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };
    let position = position_from_byte_offset(
      &source,
      source
        .find("profile.display_name")
        .expect("nested template binding")
        + "profile.".len()
        + 2,
    );

    let edit = rename_for_document(&document, position, "name").expect("rename edit");
    let changes = edit.changes.expect("rename changes");
    let edits = changes.values().next().expect("rename file edits");

    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|edit| edit.new_text == "name"));
    assert!(edits.iter().any(|edit| edit.range.start == Position::new(7, 2)));
    assert!(edits.iter().any(|edit| edit.range.start == Position::new(25, 19)));
  }

  #[test]
  fn prepare_rename_for_document_uses_nested_template_symbol_segment_range() {
    let source = fixture_route_source().to_owned();
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/profile.trs".to_owned(),
      source: source.clone(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };
    let position = position_from_byte_offset(
      &source,
      source
        .find("profile.display_name")
        .expect("nested template binding")
        + "profile.".len()
        + 2,
    );

    let response = prepare_rename_for_document(&document, position).expect("prepare rename");
    let PrepareRenameResponse::RangeWithPlaceholder { placeholder, range } = response else {
      panic!("expected range-with-placeholder response");
    };

    assert_eq!(placeholder, "display_name");
    assert_eq!(range.start, Position::new(25, 19));
    assert_eq!(range.end, Position::new(25, 31));
  }

  #[test]
  fn document_highlights_for_document_return_template_symbol_reads_and_write() {
    let source = fixture_route_source().to_owned();
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/profile.trs".to_owned(),
      source: source.clone(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };
    let position = position_from_byte_offset(
      &source,
      source.find("{{ username }}").expect("username binding") + 5,
    );

    let highlights = document_highlights_for_document(&document, position).expect("highlights");

    assert_eq!(highlights.len(), 3);
    assert_eq!(highlights[0].kind, Some(DocumentHighlightKind::WRITE));
    assert!(highlights[1..]
      .iter()
      .all(|highlight| highlight.kind == Some(DocumentHighlightKind::READ)));
  }

  #[test]
  fn linked_editing_ranges_for_document_match_nested_template_tags() {
    let source = "<main><div><div>x</div></div></main>".to_owned();
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: source.clone(),
      cached_artifacts: None,
    };
    let position = position_from_byte_offset(&source, source.find("<div><div").expect("inner div") + 7);
    let pair = template_tag_pair_at_position(
      &source,
      &[thebe_parser::SourceSpan {
        start: 0,
        end: source.len(),
      }],
      position,
    )
    .expect("template tag pair");

    let ranges = linked_editing_ranges_for_document(&document, position)
      .expect("linked editing ranges");

    assert_eq!(pair.open, ByteRange { start: 12, end: 15 });
    assert_eq!(pair.close, ByteRange { start: 19, end: 22 });
    assert_eq!(ranges.ranges.len(), 2);
    assert_eq!(ranges.ranges[0], Range::new(Position::new(0, 12), Position::new(0, 15)));
    assert_eq!(ranges.ranges[1], Range::new(Position::new(0, 19), Position::new(0, 22)));
  }

  #[test]
  fn rename_for_document_rewrites_component_import_without_alias_and_tag_usages() {
    let source = r#"<script setup>
use crate::components::Card;

struct Props {
  count: i64,
}

#[thebe::get]
fn handler() -> Props {
  Props { count: 0 }
}
</script>

<main>
  <Card></Card>
</main>
"#
    .to_owned();
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: source.clone(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };
    let position = position_from_byte_offset(&source, source.find("<Card>").expect("component tag") + 2);

    let edit = rename_for_document(&document, position, "SummaryCard").expect("rename edit");
    let changes = edit.changes.expect("rename changes");
    let edits = changes.values().next().expect("rename file edits");

    assert_eq!(edits.len(), 3);
    assert!(edits.iter().any(|edit| edit.new_text == "Card as SummaryCard"));
    assert_eq!(
      edits
        .iter()
        .filter(|edit| edit.new_text == "SummaryCard")
        .count(),
      2
    );
  }

  #[test]
  fn rename_for_document_rewrites_component_imports_and_tag_usages_across_workspace() {
    let project = TempProject::new();
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let route_source = r#"<script setup>
use crate::components::Card;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <Card title=\"primary\" />
</main>
"#;
    let other_route_source = r#"<script setup>
use crate::components::Card;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<section>
  <Card :title=\"secondary\" />
</section>
"#;
    project.write("src/components/Card.trs", component_source);
    project.write("src/routes/index.trs", route_source);
    project.write("src/routes/other.trs", other_route_source);

    let document = DocumentContext {
      project_root: project.root.clone(),
      relative_path: "src/routes/index.trs".to_owned(),
      source: route_source.to_owned(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: component_prop_fixture_manifest(
          component_source,
          &["src/routes/index.trs", "src/routes/other.trs"],
        ),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };
    let position = position_from_byte_offset(
      &document.source,
      document.source.find("<Card").expect("component tag") + 2,
    );

    let edit = rename_for_document(&document, position, "SummaryCard").expect("rename edit");
    let changes = edit.changes.expect("rename changes");

    assert_eq!(changes.len(), 2);
    assert!(changes.iter().any(|(uri, edits)| {
      uri.path().ends_with("/src/routes/index.trs")
        && edits.iter().any(|edit| edit.new_text == "Card as SummaryCard")
        && edits.iter().filter(|edit| edit.new_text == "SummaryCard").count() == 1
    }));
    assert!(changes.iter().any(|(uri, edits)| {
      uri.path().ends_with("/src/routes/other.trs")
        && edits.iter().any(|edit| edit.new_text == "Card as SummaryCard")
        && edits.iter().filter(|edit| edit.new_text == "SummaryCard").count() == 1
    }));
  }

  #[test]
  fn document_highlights_for_document_include_component_import_and_tag_usages() {
    let source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {
  count: i64,
}

#[thebe::get]
fn handler() -> Props {
  Props { count: 0 }
}
</script>

<main>
  <SummaryCard></SummaryCard>
</main>
"#
    .to_owned();
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: source.clone(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };
    let position = position_from_byte_offset(
      &source,
      source.find("SummaryCard").expect("component import alias") + 2,
    );

    let highlights = document_highlights_for_document(&document, position).expect("highlights");

    assert_eq!(highlights.len(), 3);
    assert_eq!(highlights[0].kind, Some(DocumentHighlightKind::WRITE));
    assert!(highlights[1..]
      .iter()
      .all(|highlight| highlight.kind == Some(DocumentHighlightKind::READ)));
  }

  #[test]
  fn references_for_document_return_component_prop_definition_and_other_usages() {
    let project = TempProject::new();
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let route_source = r#"<script setup>
use crate::components::Card;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <Card title=\"primary\" />
</main>
"#;
    let aliased_route_source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <SummaryCard :title=\"secondary\" />
</main>
"#;
    project.write("src/components/Card.trs", component_source);
    project.write("src/routes/index.trs", route_source);
    project.write("src/routes/alias.trs", aliased_route_source);

    let manifest = component_prop_fixture_manifest(
      component_source,
      &["src/routes/index.trs", "src/routes/alias.trs"],
    );
    let position = position_from_byte_offset(
      aliased_route_source,
      aliased_route_source.find(":title").expect("aliased prop") + 2,
    );

    let locations = references_for_document(
      &project.root,
      aliased_route_source,
      &manifest,
      "src/routes/alias.trs",
      position,
      true,
    )
    .expect("references result")
    .expect("prop references");

    assert_eq!(locations.len(), 2);
    assert!(locations.iter().any(|location| {
      location
        .uri
        .path()
        .ends_with("/src/components/Card.trs")
        && location.range.start == Position::new(2, 2)
    }));
    assert!(locations.iter().any(|location| {
      location.uri.path().ends_with("/src/routes/index.trs")
        && location.range.start == Position::new(12, 8)
    }));
  }

  #[test]
  fn references_for_document_return_component_definition_and_other_tag_usages() {
    let project = TempProject::new();
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let route_source = r#"<script setup>
use crate::components::Card;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <Card title=\"primary\" />
</main>
"#;
    let aliased_route_source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <SummaryCard :title=\"secondary\" />
</main>
"#;
    project.write("src/components/Card.trs", component_source);
    project.write("src/routes/index.trs", route_source);
    project.write("src/routes/alias.trs", aliased_route_source);

    let manifest = component_prop_fixture_manifest(
      component_source,
      &["src/routes/index.trs", "src/routes/alias.trs"],
    );
    let position = position_from_byte_offset(
      aliased_route_source,
      aliased_route_source.find("<SummaryCard").expect("aliased component tag") + 2,
    );
    let other_usage_start = position_from_byte_offset(
      route_source,
      route_source.find("<Card").expect("component tag") + 1,
    );

    let locations = references_for_document(
      &project.root,
      aliased_route_source,
      &manifest,
      "src/routes/alias.trs",
      position,
      true,
    )
    .expect("references result")
    .expect("component references");

    assert_eq!(locations.len(), 2);
    assert!(locations.iter().any(|location| {
      location.uri.path().ends_with("/src/components/Card.trs")
        && location.range.start == Position::new(0, 0)
    }));
    assert!(locations.iter().any(|location| {
      location.uri.path().ends_with("/src/routes/index.trs")
        && location.range.start == other_usage_start
    }));
  }

  #[test]
  fn references_for_document_return_component_definition_and_tag_usages_from_import_alias() {
    let project = TempProject::new();
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let route_source = r#"<script setup>
use crate::components::Card;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <Card title=\"primary\" />
</main>
"#;
    let aliased_route_source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <SummaryCard :title=\"secondary\" />
</main>
"#;
    project.write("src/components/Card.trs", component_source);
    project.write("src/routes/index.trs", route_source);
    project.write("src/routes/alias.trs", aliased_route_source);

    let manifest = component_prop_fixture_manifest(
      component_source,
      &["src/routes/index.trs", "src/routes/alias.trs"],
    );
    let position = position_from_byte_offset(
      aliased_route_source,
      aliased_route_source.find("SummaryCard").expect("aliased import") + 2,
    );
    let local_usage_start = position_from_byte_offset(
      aliased_route_source,
      aliased_route_source.find("<SummaryCard").expect("local component tag") + 1,
    );
    let other_usage_start = position_from_byte_offset(
      route_source,
      route_source.find("<Card").expect("component tag") + 1,
    );

    let locations = references_for_document(
      &project.root,
      aliased_route_source,
      &manifest,
      "src/routes/alias.trs",
      position,
      true,
    )
    .expect("references result")
    .expect("component references");

    assert_eq!(locations.len(), 3);
    assert!(locations.iter().any(|location| {
      location.uri.path().ends_with("/src/components/Card.trs")
        && location.range.start == Position::new(0, 0)
    }));
    assert!(locations.iter().any(|location| {
      location.uri.path().ends_with("/src/routes/alias.trs")
        && location.range.start == local_usage_start
    }));
    assert!(locations.iter().any(|location| {
      location.uri.path().ends_with("/src/routes/index.trs")
        && location.range.start == other_usage_start
    }));
  }

  #[test]
  fn definition_for_document_returns_component_file_from_import_alias() {
    let project = TempProject::new();
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let aliased_route_source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <SummaryCard :title=\"secondary\" />
</main>
"#;
    project.write("src/components/Card.trs", component_source);
    project.write("src/routes/alias.trs", aliased_route_source);

    let manifest = component_prop_fixture_manifest(component_source, &["src/routes/alias.trs"]);
    let position = position_from_byte_offset(
      aliased_route_source,
      aliased_route_source.find("SummaryCard").expect("aliased import") + 2,
    );

    let response = definition_for_document(
      &project.root,
      aliased_route_source,
      &manifest,
      "src/routes/alias.trs",
      position,
    )
    .expect("definition result")
    .expect("definition");
    let GotoDefinitionResponse::Scalar(location) = response else {
      panic!("expected scalar definition");
    };

    assert!(location.uri.path().ends_with("/src/components/Card.trs"));
    assert_eq!(location.range.start, Position::new(0, 0));
  }

  #[test]
  fn rename_for_document_updates_component_prop_definition_and_workspace_usages() {
    let project = TempProject::new();
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let route_source = r#"<script setup>
use crate::components::Card;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <Card title=\"primary\" />
</main>
"#;
    let aliased_route_source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <SummaryCard :title=\"secondary\" />
</main>
"#;
    project.write("src/components/Card.trs", component_source);
    project.write("src/routes/index.trs", route_source);
    project.write("src/routes/alias.trs", aliased_route_source);

    let document = DocumentContext {
      project_root: project.root.clone(),
      relative_path: "src/routes/alias.trs".to_owned(),
      source: aliased_route_source.to_owned(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: component_prop_fixture_manifest(
          component_source,
          &["src/routes/index.trs", "src/routes/alias.trs"],
        ),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };
    let position = position_from_byte_offset(
      &document.source,
      document.source.find(":title").expect("aliased prop") + 2,
    );

    let edit = rename_for_document(&document, position, "heading").expect("rename edit");
    let changes = edit.changes.expect("rename changes");

    assert_eq!(changes.len(), 3);
    assert!(changes.iter().any(|(uri, edits)| {
      uri.path().ends_with("/src/components/Card.trs")
        && edits.len() == 1
        && edits[0].new_text == "heading"
        && edits[0].range.start == Position::new(2, 2)
    }));
    assert!(changes.iter().any(|(uri, edits)| {
      uri.path().ends_with("/src/routes/index.trs")
        && edits.len() == 1
        && edits[0].new_text == "heading"
        && edits[0].range.start == Position::new(12, 8)
    }));
    assert!(changes.iter().any(|(uri, edits)| {
      uri.path().ends_with("/src/routes/alias.trs")
        && edits.len() == 1
        && edits[0].new_text == "heading"
        && edits[0].range.start == Position::new(12, 16)
    }));
  }

  #[test]
  fn component_tag_completion_items_add_import_edits_for_unimported_components() {
    let source = r#"<script setup>
use crate::components::Badge;

struct Props {
  count: i64,
}

#[thebe::get]
fn handler() -> Props {
  Props { count: 0 }
}
</script>

<Ca
"#;
    let replace_start = source.find("Ca").expect("component tag prefix");
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: source.to_owned(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };

    let items = component_tag_completion_items(
      &document,
      "Ca",
      ByteRange {
        start: replace_start,
        end: replace_start + 2,
      },
    );

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "Card");
    let edits = items[0]
      .additional_text_edits
      .as_ref()
      .expect("missing import edit");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "use crate::components::Card;\n");
    assert_eq!(edits[0].range.start, Position::new(1, 0));
  }

  #[test]
  fn component_tag_completion_items_preserve_existing_component_aliases() {
    let source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {
  count: i64,
}

#[thebe::get]
fn handler() -> Props {
  Props { count: 0 }
}
</script>

<Sum
"#;
    let replace_start = source.find("Sum").expect("component tag prefix");
    let document = DocumentContext {
      project_root: PathBuf::from("/tmp/app"),
      relative_path: "src/routes/index.trs".to_owned(),
      source: source.to_owned(),
      cached_artifacts: Some(ProjectArtifacts {
        manifest: fixture_manifest(),
        diagnostics: ThebeDiagnosticsFile {
          version: 1,
          diagnostics: Vec::new(),
        },
      }),
    };

    let items = component_tag_completion_items(
      &document,
      "Sum",
      ByteRange {
        start: replace_start,
        end: replace_start + 3,
      },
    );

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "SummaryCard");
    assert!(items[0].additional_text_edits.is_none());
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

  fn component_prop_fixture_manifest(
    component_source: &str,
    route_paths: &[&str],
  ) -> ThebeManifest {
    let title_field = component_source.find("title: String").expect("component prop field");

    ThebeManifest {
      version: 5,
      server_router_path: ".thebe/server/routes.rs".to_owned(),
      app_html: thebe_project::AppHtmlMetadata {
        source_path: Some("app.html".to_owned()),
        uses_default: false,
      },
      layouts: Vec::new(),
      routes: route_paths
        .iter()
        .map(|source_path| RouteMetadata {
          generated_client_path: None,
          generated_server_path: format!(".thebe/server/{}.rs", source_path.replace('/', "__")),
          generated_types_path: None,
          handler: thebe_project::HandlerMetadata {
            is_async: false,
            method: "get".to_owned(),
            name: "handler".to_owned(),
            param_types: Vec::new(),
            source_span: None,
          },
          has_client_script: false,
          has_head: false,
          has_style: false,
          layout_scope_path: None,
          layout_source_path: None,
          module_name: source_path.replace(['/', '.'], "_"),
          route_path: source_path.to_string(),
          source_path: (*source_path).to_owned(),
          state_type: None,
          template_bindings: Vec::new(),
          template_symbols: Vec::new(),
          template_binding_spans: Vec::new(),
          template_symbol_definitions: Vec::new(),
        })
        .collect(),
      components: vec![ComponentMetadata {
        has_client_script: false,
        has_style: false,
        module_path: "crate::components::Card".to_owned(),
        prop_names: vec!["title".to_owned()],
        props: vec![ComponentPropMetadata {
          name: "title".to_owned(),
          source_span: span_from_offsets(component_source, title_field, title_field + 5),
          type_name: "String".to_owned(),
        }],
        source_path: "src/components/Card.trs".to_owned(),
        tag_name: "Card".to_owned(),
      }],
    }
  }

  struct TempProject {
    root: PathBuf,
  }

  impl TempProject {
    fn new() -> Self {
      let unique = format!(
        "thebe-lsp-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .expect("system time")
          .as_nanos()
      );
      let root = std::env::temp_dir().join(unique);
      std::fs::create_dir_all(root.join("src/routes")).expect("create route fixture dir");
      std::fs::create_dir_all(root.join("src/components")).expect("create component fixture dir");
      Self { root }
    }

    fn write(&self, relative_path: &str, source: &str) {
      let path = self.root.join(relative_path);
      if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture parent dir");
      }
      std::fs::write(path, source).expect("write fixture source");
    }
  }

  impl Drop for TempProject {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.root);
    }
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
  fn hover_for_document_returns_component_hover_for_import_alias() {
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let aliased_route_source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <SummaryCard :title=\"secondary\" />
</main>
"#;
    let manifest = component_prop_fixture_manifest(component_source, &["src/routes/alias.trs"]);
    let alias_start = aliased_route_source.find("SummaryCard").expect("aliased import");

    let hover = hover_for_document(
      aliased_route_source,
      &manifest,
      "src/routes/alias.trs",
      position_from_byte_offset(aliased_route_source, alias_start + 2),
    )
    .expect("hover");
    let HoverContents::Markup(contents) = hover.contents else {
      panic!("expected markdown hover");
    };

    assert!(contents.value.contains("**Component** `Card`"));
    assert!(contents.value.contains("crate::components::Card"));
    assert_eq!(
      hover.range.expect("hover range").start,
      position_from_byte_offset(aliased_route_source, alias_start),
    );
  }

  #[test]
  fn hover_for_document_returns_component_hover_for_tag_usage() {
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let aliased_route_source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <SummaryCard :title=\"secondary\" />
</main>
"#;
    let manifest = component_prop_fixture_manifest(component_source, &["src/routes/alias.trs"]);
    let tag_name_start = aliased_route_source
      .find("<SummaryCard")
      .expect("component tag")
      + 1;

    let hover = hover_for_document(
      aliased_route_source,
      &manifest,
      "src/routes/alias.trs",
      position_from_byte_offset(aliased_route_source, tag_name_start + 1),
    )
    .expect("hover");
    let HoverContents::Markup(contents) = hover.contents else {
      panic!("expected markdown hover");
    };

    assert!(contents.value.contains("**Component** `Card`"));
    assert_eq!(
      hover.range.expect("hover range").start,
      position_from_byte_offset(aliased_route_source, tag_name_start),
    );
  }

  #[test]
  fn hover_for_document_returns_component_prop_hover_for_prop_usage() {
    let component_source = r#"<script>
pub struct Props {
  title: String,
}
</script>

<article>{{ props.title }}</article>
"#;
    let aliased_route_source = r#"<script setup>
use crate::components::Card as SummaryCard;

struct Props {}

#[thebe::get]
fn handler() -> Props {
  Props {}
}
</script>

<main>
  <SummaryCard :title=\"secondary\" />
</main>
"#;
    let manifest = component_prop_fixture_manifest(component_source, &["src/routes/alias.trs"]);
    let prop_name_start = aliased_route_source.find(":title").expect("component prop") + 1;

    let hover = hover_for_document(
      aliased_route_source,
      &manifest,
      "src/routes/alias.trs",
      position_from_byte_offset(aliased_route_source, prop_name_start + 1),
    )
    .expect("hover");
    let HoverContents::Markup(contents) = hover.contents else {
      panic!("expected markdown hover");
    };

    assert!(contents.value.contains("**Component prop** `title`"));
    assert!(contents.value.contains("Component: `Card`"));
    assert_eq!(
      hover.range.expect("hover range").start,
      position_from_byte_offset(aliased_route_source, prop_name_start),
    );
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
  fn workspace_symbols_include_route_handlers_and_template_definitions() {
    let symbols = workspace_symbols_for_manifest(
      Path::new("/tmp/app"),
      &fixture_manifest(),
      "profile",
    )
    .expect("workspace symbols");

    assert!(symbols.iter().any(|symbol| {
      symbol.name == "/profile" && symbol.kind == SymbolKind::MODULE
    }));
    assert!(symbols.iter().any(|symbol| {
      symbol.name == "handler" && symbol.container_name.as_deref() == Some("/profile")
    }));
    assert!(symbols.iter().any(|symbol| symbol.name == "profile.display_name"));
  }

  #[test]
  fn workspace_symbols_match_component_props_case_insensitively() {
    let symbols = workspace_symbols_for_manifest(Path::new("/tmp/app"), &fixture_manifest(), "TITLE")
      .expect("workspace symbols");

    assert!(symbols.iter().any(|symbol| {
      symbol.name == "title" && symbol.container_name.as_deref() == Some("Card")
    }));
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
