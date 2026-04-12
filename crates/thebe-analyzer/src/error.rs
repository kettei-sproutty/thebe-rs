use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnalyzerError {
  #[error("script_ts block is missing")]
  MissingScript,

  #[error("failed to parse client TypeScript: {0}")]
  Parse(String),

  #[error("failed to strip client TypeScript: {0}")]
  Strip(String),
}
