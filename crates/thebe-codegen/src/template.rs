use crate::error::CodegenError;

/// A segment of a parsed Thebe template.
///
/// Used during validation; the field contents are intentionally not read by
/// the codegen (which passes the raw template string to minijinja at runtime).
#[derive(Debug)]
pub enum TemplatePart {
    /// A run of static HTML text.
    Literal(#[allow(dead_code)] String),
    /// A `{{ ident }}` or `{{ ident.field }}` binding.
    Binding(#[allow(dead_code)] String),
}

/// Parse a Thebe template string into a flat list of [`TemplatePart`]s.
///
/// Only simple identifier and dotted-field bindings are supported (v0 grammar).
/// Anything else (arithmetic, function calls, ternaries) is rejected.
pub fn parse_template(template: &str) -> Result<Vec<TemplatePart>, CodegenError> {
    let mut parts: Vec<TemplatePart> = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' && chars.peek().copied() == Some('{') {
            chars.next(); // consume second '{'

            // Collect the binding content until `}}`.
            let mut binding = String::new();
            loop {
                match chars.next() {
                    None => return Err(CodegenError::UnclosedBinding),
                    Some('}') if chars.peek().copied() == Some('}') => {
                        chars.next(); // consume second '}'
                        break;
                    }
                    Some(c) => binding.push(c),
                }
            }

            let ident = binding.trim().to_owned();
            validate_binding(&ident)?;

            if !literal.is_empty() {
                parts.push(TemplatePart::Literal(std::mem::take(&mut literal)));
            }
            parts.push(TemplatePart::Binding(ident));
        } else {
            literal.push(ch);
        }
    }

    if !literal.is_empty() {
        parts.push(TemplatePart::Literal(literal));
    }

    Ok(parts)
}

/// Validate that a binding is a simple identifier or dotted field path.
///
/// Valid: `title`, `post`, `post.author`, `post.author.name`
/// Invalid: `a + b`, `fn()`, `0bad`, `.leading_dot`
fn validate_binding(ident: &str) -> Result<(), CodegenError> {
    if ident.is_empty() {
        return Err(CodegenError::InvalidBinding(ident.to_owned()));
    }
    for part in ident.split('.') {
        if part.is_empty() {
            return Err(CodegenError::InvalidBinding(ident.to_owned()));
        }
        let mut chars = part.chars();
        let first = chars.next().expect("part is non-empty");
        if !first.is_alphabetic() && first != '_' {
            return Err(CodegenError::InvalidBinding(ident.to_owned()));
        }
        for c in chars {
            if !c.is_alphanumeric() && c != '_' {
                return Err(CodegenError::InvalidBinding(ident.to_owned()));
            }
        }
    }
    Ok(())
}

/// Escape a string for use inside a Rust double-quoted string literal.
#[allow(dead_code)] // retained for potential future use
pub fn escape_rust_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_binding() {
        let parts = parse_template("Hello {{ name }}!").unwrap();
        assert!(matches!(parts[0], TemplatePart::Literal(ref s) if s == "Hello "));
        assert!(matches!(parts[1], TemplatePart::Binding(ref s) if s == "name"));
        assert!(matches!(parts[2], TemplatePart::Literal(ref s) if s == "!"));
    }

    #[test]
    fn parse_dotted_binding() {
        let parts = parse_template("{{ post.author.name }}").unwrap();
        assert!(
            matches!(parts[0], TemplatePart::Binding(ref s) if s == "post.author.name")
        );
    }

    #[test]
    fn parse_unclosed_binding_returns_error() {
        assert!(parse_template("{{ oops").is_err());
    }

    #[test]
    fn validate_rejects_expressions() {
        assert!(parse_template("{{ a + b }}").is_err());
        assert!(parse_template("{{ fn() }}").is_err());
        assert!(parse_template("{{ }}").is_err());
    }
}
