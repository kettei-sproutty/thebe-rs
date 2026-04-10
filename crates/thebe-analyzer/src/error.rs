use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnalyzerError {
  #[error("script_ts block is missing")]
  MissingScript,
}
