mod error;
mod tokenizer;

pub use error::ParseError;
pub use tokenizer::{SfcBlocks, SourceSpan, parse_component_sfc, parse_sfc};
