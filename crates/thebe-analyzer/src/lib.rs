mod error;
pub use error::AnalyzerError;
use std::fmt::Write as _;
use swc_common::{FileName, SourceMap, errors::Handler, sync::Lrc};
use swc_ecma_ast::{Decl, EsVersion, Script, Stmt};
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

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
/// 1. Parse the TypeScript with SWC.
/// 2. Extract top-level function names from the SWC AST.
/// 3. Strip TypeScript syntax with SWC's fast strip pipeline.
/// 3. Append `__thebe_register` calls so the onclick wiring can find them.
///
/// # Errors
///
/// Returns [`AnalyzerError`] if the TypeScript block cannot be processed.
pub fn analyze(script_ts: &str) -> Result<ClientModule, AnalyzerError> {
  let script = parse_typescript_script(script_ts)?;
  let event_fns = collect_top_level_function_names(&script);
  let js_raw = strip_typescript(script_ts)?;

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

const CLIENT_SCRIPT_FILENAME: &str = "thebe-client.ts";

fn parse_typescript_script(script_ts: &str) -> Result<Script, AnalyzerError> {
  let source_map: Lrc<SourceMap> = Default::default();
  let source_file = source_map.new_source_file(
    FileName::Custom(CLIENT_SCRIPT_FILENAME.into()).into(),
    script_ts.to_owned(),
  );
  let lexer = Lexer::new(
    Syntax::Typescript(ts_syntax()),
    EsVersion::latest(),
    StringInput::from(&*source_file),
    None,
  );
  let mut parser = Parser::new_from(lexer);
  let script = parser
    .parse_script()
    .map_err(|error| AnalyzerError::Parse(format!("{:?}", error.kind())))?;

  let errors = parser.take_errors();
  if errors.is_empty() {
    Ok(script)
  } else {
    Err(AnalyzerError::Parse(
      errors
        .into_iter()
        .map(|error| format!("{:?}", error.kind()))
        .collect::<Vec<_>>()
        .join("\n"),
    ))
  }
}

fn strip_typescript(script_ts: &str) -> Result<String, AnalyzerError> {
  let source_map: Lrc<SourceMap> = Default::default();
  let handler = Handler::with_emitter_writer(Box::new(std::io::sink()), Some(source_map.clone()));
  let result = swc_ts_fast_strip::operate(
    &source_map,
    &handler,
    script_ts.to_owned(),
    swc_ts_fast_strip::Options {
      filename: Some(CLIENT_SCRIPT_FILENAME.to_owned()),
      mode: swc_ts_fast_strip::Mode::StripOnly,
      module: Some(false),
      parser: ts_syntax(),
      ..Default::default()
    },
  )
  .map_err(|error| AnalyzerError::Strip(error.to_string()))?;

  Ok(result.code)
}

fn ts_syntax() -> TsSyntax {
  TsSyntax {
    decorators: true,
    ..Default::default()
  }
}

fn collect_top_level_function_names(script: &Script) -> Vec<String> {
  script
    .body
    .iter()
    .filter_map(|statement| match statement {
      Stmt::Decl(Decl::Fn(function)) => Some(function.ident.sym.to_string()),
      _ => None,
    })
    .collect()
}

#[cfg(test)]
mod tests {
  mod analyze {
    use crate::analyze;

    #[test]
    fn strips_types_and_registers_top_level_functions() {
      let ts = "let props = getProps<Props>();\n\
                function increment(step: number): void { props.counter += step; }\n\
                function decrement(step: number): void { props.counter -= step; }";

      let module = analyze(ts).unwrap();

      assert_eq!(module.event_fns, &["increment", "decrement"]);
      assert!(!module.js.contains("getProps<Props>()"));
      assert!(!module.js.contains("step: number"));
      assert!(!module.js.contains("): void"));
      assert!(module.js.contains("function increment("));
      assert!(module.js.contains("function decrement("));
      assert!(
        module
          .js
          .contains("__thebe_register(\"increment\", increment);")
      );
      assert!(
        module
          .js
          .contains("__thebe_register(\"decrement\", decrement);")
      );
    }

    #[test]
    fn ignores_arrow_functions_for_event_registration() {
      let ts = "const increment = () => 1;\nfunction decrement() { return 0; }";

      let module = analyze(ts).unwrap();

      assert_eq!(module.event_fns, &["decrement"]);
    }

    #[test]
    fn preserves_object_literal_colons_while_stripping_types() {
      let ts = "const payload: { key: string } = { key: 'value' };";

      let module = analyze(ts).unwrap();

      assert!(!module.js.contains("payload: { key: string }"));
      assert!(module.js.contains("key: 'value'"));
    }
  }
}
