#[derive(Debug, thiserror::Error)]
pub enum CodegenError {
  #[error("parse error: {0}")]
  Parse(#[from] thebe_parser::ParseError),

  #[error(
    "unsupported HTTP method attribute `#[thebe::{0}]` — supported: get, post, put, patch, delete, head, options"
  )]
  UnsupportedMethod(String),

  #[error("could not parse the handler signature after `#[thebe::{0}]`")]
  InvalidHandlerSignature(String),

  #[error("unclosed binding — every `{{{{` must be closed with `}}}}`")]
  UnclosedBinding,

  #[error("invalid binding `{0}` — only simple identifiers and dotted field access are allowed")]
  InvalidBinding(String),

  #[error(
    "no handler found — add `#[thebe::get]` (or another HTTP method attribute) \
         before the handler function in `<script setup>`"
  )]
  MissingHandler,

  #[error("`<script setup>` block is missing")]
  MissingScriptSetup,

  #[error("CSS error: {0}")]
  CssError(String),
}
