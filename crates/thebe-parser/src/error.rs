#[derive(Debug, thiserror::Error)]
pub enum ParseError {
  #[error("unclosed `{0}` block — missing closing tag")]
  UnclosedBlock(String),
  #[error("duplicate `{0}` block — each block may only appear once")]
  DuplicateBlock(String),
}
