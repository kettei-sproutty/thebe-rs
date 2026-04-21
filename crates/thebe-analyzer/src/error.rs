use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnalyzerError {
  #[error("script_ts block is missing")]
  MissingScript,

  #[error("failed to parse client script: {0}")]
  Parse(String),

  #[error("failed to strip client TypeScript: {0}")]
  Strip(String),

  #[error("failed to emit client JavaScript: {0}")]
  Emit(String),
}
