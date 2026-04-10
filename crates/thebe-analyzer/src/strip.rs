/// Strip TypeScript-specific syntax from a `<script lang="ts">` block so the
/// result is valid browser JavaScript.
///
/// ## v0 grammar handled
///
/// | Input                              | Output                  |
/// |------------------------------------|-------------------------|
/// | `getProps<Props>()`                | `getProps()`            |
/// | `derived<string>(`                 | `derived(`              |
/// | any `ident<SimpleType>`            | `ident`                 |
/// | `): void {`                        | `) {`                   |
/// | `function f(x: number)`            | `function f(x)`         |
/// | `let x: string = `                 | `let x = `              |
///
/// String literals and `//` / `/* */` comments are passed through unchanged.
pub fn strip_ts_types(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // ── String literals ───────────────────────────────────────────────
        if ch == '"' || ch == '\'' || ch == '`' {
            let delim = ch;
            out.push(ch);
            i += 1;
            while i < len {
                let c = chars[i];
                out.push(c);
                i += 1;
                if c == '\\' && i < len {
                    out.push(chars[i]);
                    i += 1;
                } else if c == delim {
                    break;
                }
            }
            continue;
        }

        // ── Line comment ──────────────────────────────────────────────────
        if ch == '/' && i + 1 < len && chars[i + 1] == '/' {
            while i < len && chars[i] != '\n' {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }

        // ── Block comment ─────────────────────────────────────────────────
        if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
            out.push('/');
            out.push('*');
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                out.push(chars[i]);
                i += 1;
            }
            if i + 1 < len {
                out.push('*');
                out.push('/');
                i += 2;
            }
            continue;
        }

        // ── Generic type parameter: `ident<Type>` ─────────────────────────
        if ch == '<' {
            let prev_is_ident = out
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if prev_is_ident
                && let Some(end) = try_match_type_params(&chars, i)
            {
                i = end; // skip the entire `<…>`
                continue;
            }
            out.push(ch);
            i += 1;
            continue;
        }

        // ── Colon type annotation: `: Type` ──────────────────────────────
        if ch == ':' {
            let prev = out.chars().next_back();
            let prev_is_valid = prev.is_some_and(|c| {
                c.is_alphanumeric() || c == '_' || c == ')' || c == '?'
            });
            if prev_is_valid
                && let Some(end) = try_match_type_annotation(&chars, i + 1)
            {
                i = end; // skip `: TypeExpr`
                continue;
            }
            out.push(ch);
            i += 1;
            continue;
        }

        out.push(ch);
        i += 1;
    }

    out
}

/// Try to match `<SimpleTypeList>` starting at index `i` (which is `<`).
///
/// Accepts identifiers, commas, spaces, `[]`, `|`, `&`, and nested `<>`.
/// Returns the index **after** the closing `>` on success.
fn try_match_type_params(chars: &[char], i: usize) -> Option<usize> {
    debug_assert_eq!(chars[i], '<');
    let mut j = i + 1;
    let len = chars.len();
    let mut depth: i32 = 1;

    while j < len && depth > 0 {
        match chars[j] {
            '<' => depth += 1,
            '>' => depth -= 1,
            c if c.is_alphanumeric()
                || c == '_'
                || c == ','
                || c == ' '
                || c == '\n'
                || c == '\t'
                || c == '['
                || c == ']'
                || c == '|'
                || c == '&'
                || c == '.' => {}
            _ => return None,
        }
        j += 1;
    }

    if depth == 0 { Some(j) } else { None }
}

/// Try to match `: TypeAnnotation` where `start` points to the first char
/// **after** the colon.
///
/// Returns the index after the type expression on success.
fn try_match_type_annotation(chars: &[char], start: usize) -> Option<usize> {
    let len = chars.len();
    let mut j = start;

    // Skip leading whitespace (space / tab only — not newlines).
    while j < len && (chars[j] == ' ' || chars[j] == '\t') {
        j += 1;
    }

    // Must start with an identifier character.
    if j >= len || !(chars[j].is_alphabetic() || chars[j] == '_') {
        return None;
    }

    let type_start = j;
    let mut depth: i32 = 0;

    while j < len {
        match chars[j] {
            '<' => {
                depth += 1;
                j += 1;
            }
            '>' if depth > 0 => {
                depth -= 1;
                j += 1;
            }
            c if c.is_alphanumeric()
                || c == '_'
                || c == '.'
                || c == '['
                || c == ']' =>
            {
                j += 1;
            }
            ' ' | '\t' if depth > 0 => {
                j += 1;
            }
            '|' | '&' => {
                j += 1;
            }
            _ => break,
        }
    }

    if j == type_start || depth != 0 {
        return None;
    }

    // What follows the type must be a "terminator" that makes sense after a
    // type annotation in those contexts.
    let mut k = j;
    while k < len && chars[k] == ' ' {
        k += 1;
    }
    let next = if k < len { chars[k] } else { '\0' };
    match next {
        '=' | '{' | ',' | ')' | ';' | '\n' | '\0' => Some(j),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_generic_on_get_props() {
        let src = "let props = getProps<Props>();";
        let got = strip_ts_types(src);
        assert_eq!(got, "let props = getProps();");
    }

    #[test]
    fn strip_generic_on_derived() {
        let src = "const d = derived<string>(() => props.name);";
        let got = strip_ts_types(src);
        assert_eq!(got, "const d = derived(() => props.name);");
    }

    #[test]
    fn strip_variable_type_annotation() {
        let src = "let count: number = 0;";
        let got = strip_ts_types(src);
        assert_eq!(got, "let count = 0;");
    }

    #[test]
    fn strip_function_return_type() {
        let src = "function greet(): void {\n  console.log('hi');\n}";
        let got = strip_ts_types(src);
        assert_eq!(got, "function greet() {\n  console.log('hi');\n}");
    }

    #[test]
    fn strip_param_type_annotation() {
        let src = "function add(a: number, b: number) { return a + b; }";
        let got = strip_ts_types(src);
        assert_eq!(got, "function add(a, b) { return a + b; }");
    }

    #[test]
    fn preserves_object_literal_colon() {
        let src = "const x = { key: 'value' };";
        let got = strip_ts_types(src);
        assert_eq!(got, "const x = { key: 'value' };");
    }

    #[test]
    fn preserves_string_literals() {
        let src = r#"const s = "hello: world <ts>";"#;
        let got = strip_ts_types(src);
        assert_eq!(got, src);
    }

    #[test]
    fn preserves_comparison_operator() {
        let src = "if (a < b) { return a; }";
        let got = strip_ts_types(src);
        assert_eq!(got, src);
    }
}
