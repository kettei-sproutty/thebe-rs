#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
  #[error("template compile error: {0}")]
  TemplateCompile(String),
  #[error("template render error: {0}")]
  TemplateRender(String),
}
