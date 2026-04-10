#[derive(Debug, thiserror::Error)]
pub enum CssError {
  #[error("CSS parse error: {0}")]
  Parse(String),

  #[error("CSS minify error: {0}")]
  Minify(String),

  #[error("CSS print error: {0}")]
  Print(String),
}
