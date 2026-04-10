mod error;
mod extract;
mod strip;
pub use error::AnalyzerError;
use std::fmt::Write as _;

/// The result of analysing a `<script lang="ts">` block.
#[derive(Debug)]
pub struct ClientModule {
  /// Browser-runnable JavaScript with TypeScript types stripped.
  ///
  /// Includes synthesised `__thebe_register("name", name)` calls for every
  /// top-level function so the runtime onclick wiring can look them up.
  pub js: String,

  /// Names of the top-level functions found (i.e. potential onclick targets).
  pub event_fns: Vec<String>,
}

/// Analyse a `<script lang="ts">` block and produce a [`ClientModule`].
///
/// Steps:
/// 1. Strip TypeScript type annotations → browser-runnable JS.
/// 2. Extract top-level function names.
/// 3. Append `__thebe_register` calls so the onclick wiring can find them.
///
/// # Errors
///
/// Returns [`AnalyzerError`] if the TypeScript block cannot be processed.
pub fn analyze(script_ts: &str) -> Result<ClientModule, AnalyzerError> {
  let js_raw = strip::strip_ts_types(script_ts);
  let event_fns = extract::extract_function_names(&js_raw);

  let mut js = js_raw;
  if !event_fns.is_empty() {
    js.push_str("\n// thebe: register event handlers\n");
    for name in &event_fns {
      writeln!(js, r#"__thebe_register("{name}", {name});"#)
        .expect("writing to String is infallible");
    }
  }

  Ok(ClientModule { js, event_fns })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn analyze_produces_registration_calls() {
    let ts = "let props = getProps<Props>();\n\
                  function increment() { props.counter += 1; }\n\
                  function decrement() { props.counter -= 1; }";
    let module = analyze(ts).unwrap();
    assert_eq!(module.event_fns, &["increment", "decrement"]);
    assert!(
      module
        .js
        .contains("__thebe_register(\"increment\", increment)")
    );
    assert!(
      module
        .js
        .contains("__thebe_register(\"decrement\", decrement)")
    );
    // Type-stripped call
    assert!(module.js.contains("getProps()"));
    assert!(!module.js.contains("getProps<Props>()"));
  }
}
