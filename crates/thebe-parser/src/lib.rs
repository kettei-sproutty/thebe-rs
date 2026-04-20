mod error;
mod tokenizer;

pub use error::ParseError;
pub use tokenizer::{
  SfcBlocks, SourceSpan, TemplateAttr, TemplateToken, parse_component_sfc, parse_sfc,
  tokenize_template,
};
