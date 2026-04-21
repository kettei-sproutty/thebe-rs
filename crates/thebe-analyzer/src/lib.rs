mod error;
pub use error::AnalyzerError;
use std::fmt::Write as _;
use swc_common::{FileName, Globals, Mark, SourceMap, errors::Handler, sync::Lrc, GLOBALS};
use swc_ecma_ast::{Decl, EsVersion, Program, Script, Stmt};
use swc_ecma_codegen::{Config as CodegenConfig, Emitter, text_writer::JsWriter};
use swc_ecma_minifier::{
  optimize,
  option::{CompressOptions, ExtraOptions, MinifyOptions},
};
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

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
pub fn analyze(script_ts: &str, minify: bool) -> Result<ClientModule, AnalyzerError> {
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

  if minify {
    js = minify_javascript(&js)?;
  }

  Ok(ClientModule { js, event_fns })
}

const CLIENT_SCRIPT_FILENAME: &str = "thebe-client.ts";
const CLIENT_JAVASCRIPT_FILENAME: &str = "thebe-client.js";

/// Minify plain JavaScript source using SWC's parser and code generator.
///
/// This preserves semantics better than line-based trimming while still
/// keeping the current pipeline lightweight.
///
/// # Errors
///
/// Returns [`AnalyzerError`] if the JavaScript cannot be parsed or emitted.
pub fn minify_javascript(script_js: &str) -> Result<String, AnalyzerError> {
  let script = parse_script(
    script_js,
    Syntax::Es(EsSyntax::default()),
    CLIENT_JAVASCRIPT_FILENAME,
  )?;
  let script = compress_script(script)?;
  emit_script(&script, true)
}

fn parse_typescript_script(script_ts: &str) -> Result<Script, AnalyzerError> {
  parse_script(
    script_ts,
    Syntax::Typescript(ts_syntax()),
    CLIENT_SCRIPT_FILENAME,
  )
}

fn parse_script(source: &str, syntax: Syntax, filename: &str) -> Result<Script, AnalyzerError> {
  let source_map: Lrc<SourceMap> = Default::default();
  let source_file = source_map.new_source_file(
    FileName::Custom(filename.into()).into(),
    source.to_owned(),
  );
  let lexer = Lexer::new(
    syntax,
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

fn emit_script(script: &Script, minify: bool) -> Result<String, AnalyzerError> {
  let source_map: Lrc<SourceMap> = Default::default();
  let mut buffer = Vec::new();

  {
    let writer = JsWriter::new(source_map.clone(), "\n", &mut buffer, None);
    let mut emitter = Emitter {
      cfg: CodegenConfig::default().with_minify(minify),
      cm: source_map,
      comments: None,
      wr: Box::new(writer),
    };
    emitter
      .emit_script(script)
      .map_err(|error| AnalyzerError::Emit(error.to_string()))?;
  }

  String::from_utf8(buffer).map_err(|error| AnalyzerError::Emit(error.to_string()))
}

fn compress_script(script: Script) -> Result<Script, AnalyzerError> {
  let source_map: Lrc<SourceMap> = Default::default();
  let globals = Globals::default();
  let mut compress = CompressOptions::default();
  compress.collapse_vars = false;
  compress.inline = 0;
  compress.join_vars = false;
  compress.reduce_fns = false;
  compress.unused = false;
  let options = MinifyOptions {
    compress: Some(compress),
    mangle: None,
    ..Default::default()
  };

  let program = GLOBALS.set(&globals, || {
    let extra = ExtraOptions {
      unresolved_mark: Mark::new(),
      top_level_mark: Mark::new(),
      mangle_name_cache: None,
    };
    optimize(Program::Script(script), source_map, None, None, &options, &extra)
  });

  match program {
    Program::Script(script) => Ok(script),
    Program::Module(_) => Err(AnalyzerError::Emit(
      "expected script output from minifier".to_owned(),
    )),
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

      let module = analyze(ts, false).unwrap();

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

      let module = analyze(ts, false).unwrap();

      assert_eq!(module.event_fns, &["decrement"]);
    }

    #[test]
    fn preserves_object_literal_colons_while_stripping_types() {
      let ts = "const payload: { key: string } = { key: 'value' };";

      let module = analyze(ts, false).unwrap();

      assert!(!module.js.contains("payload: { key: string }"));
      assert!(module.js.contains("key: 'value'"));
    }

    #[test]
    fn minify_removes_comments_and_compacts_output() {
      let ts = "let props = getProps<Props>();\n\
                // thebe test comment\n\
                function increment(step: number) {\n\
                  return step + 1;\n\
                }";

      let readable = analyze(ts, false).unwrap();
      let module = analyze(ts, true).unwrap();

      assert!(!module.js.contains("thebe test comment"), "output: {}", module.js);
      assert!(!module.js.contains("step: number"), "output: {}", module.js);
      assert!(
        module.js.contains("function increment(step){"),
        "output: {}",
        module.js
      );
      assert!(
        module.js.contains("__thebe_register(\"increment\",increment);"),
        "output: {}",
        module.js
      );
      assert!(module.js.len() < readable.js.len());
    }

    #[test]
    fn minify_eliminates_obvious_dead_branches() {
      let ts = "function increment(step: number) {\n\
                if (false) {\n\
                  console.log('dead');\n\
                }\n\
                return step + 1;\n\
              }";

      let module = analyze(ts, true).unwrap();

      assert!(!module.js.contains("console.log"), "output: {}", module.js);
      assert!(!module.js.contains("if(false)"), "output: {}", module.js);
    }
  }
}
