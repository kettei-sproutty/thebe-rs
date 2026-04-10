#[derive(Debug, thiserror::Error)]
pub enum CodegenError {
    #[error("parse error: {0}")]
    Parse(#[from] thebe_parser::ParseError),

    #[error("unclosed binding — every `{{{{` must be closed with `}}}}`")]
    UnclosedBinding,

    #[error(
        "invalid binding `{0}` — only simple identifiers and dotted field access are allowed"
    )]
    InvalidBinding(String),

    #[error(
        "no handler found — add `#[thebe::get]` (or another HTTP method attribute) \
         before the handler function in `<script setup>`"
    )]
    MissingHandler,

    #[error("`<script setup>` block is missing")]
    MissingScriptSetup,
}
