/// Extract the names of top-level `function` declarations from JavaScript
/// source.
///
/// Only handles the `function name(` form that is expected in v0 client
/// scripts.  Arrow functions and function expressions assigned to `const` /
/// `let` are not included (they are not typically used as onclick targets).
pub fn extract_function_names(js: &str) -> Vec<String> {
  let mut names = Vec::new();
  let chars: Vec<char> = js.chars().collect();
  let len = chars.len();
  let mut i = 0;

  while i < len {
    // Skip string literals (same logic as strip.rs to avoid false matches).
    if chars[i] == '"' || chars[i] == '\'' || chars[i] == '`' {
      let delim = chars[i];
      i += 1;
      while i < len {
        if chars[i] == '\\' && i + 1 < len {
          i += 2;
        } else if chars[i] == delim {
          i += 1;
          break;
        } else {
          i += 1;
        }
      }
      continue;
    }

    // Skip line comment.
    if chars[i] == '/' && i + 1 < len && chars[i + 1] == '/' {
      while i < len && chars[i] != '\n' {
        i += 1;
      }
      continue;
    }

    // Skip block comment.
    if chars[i] == '/' && i + 1 < len && chars[i + 1] == '*' {
      i += 2;
      while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
        i += 1;
      }
      if i + 1 < len {
        i += 2;
      }
      continue;
    }

    // Try to collect a word.
    if chars[i].is_alphabetic() || chars[i] == '_' {
      let start = i;
      while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
        i += 1;
      }
      let word: String = chars[start..i].iter().collect();

      if word == "function" {
        // Skip whitespace between `function` and the name.
        while i < len && chars[i].is_whitespace() {
          i += 1;
        }
        // Collect the function name (if present; skip anonymous fns).
        if i < len && (chars[i].is_alphabetic() || chars[i] == '_') {
          let name_start = i;
          while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
            i += 1;
          }
          names.push(chars[name_start..i].iter().collect());
        }
      }
      continue;
    }

    i += 1;
  }

  names
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn finds_simple_functions() {
    let js = "function increment() {}\nfunction decrement() {}";
    let names = extract_function_names(js);
    assert_eq!(names, &["increment", "decrement"]);
  }

  #[test]
  fn ignores_function_in_string() {
    let js = r#"const s = "function fake() {}"; function real() {}"#;
    let names = extract_function_names(js);
    assert_eq!(names, &["real"]);
  }

  #[test]
  fn ignores_anonymous_function() {
    let js = "const x = function() {}; function named() {}";
    let names = extract_function_names(js);
    assert_eq!(names, &["named"]);
  }

  #[test]
  fn ignores_function_in_line_comment() {
    let js = "// function commented() {}\nfunction real() {}";
    let names = extract_function_names(js);
    assert_eq!(names, &["real"]);
  }
}
