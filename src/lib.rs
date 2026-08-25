//! Minify JavaScript with [oxc] and compose the resulting source map against
//! the map the caller already has, without either map crossing into JavaScript.
//!
//! Bundlers hand a minifier code that has *already* been transformed, together
//! with a map back to the original sources. A minifier alone can only describe
//! its own step, so somebody has to compose the two. Doing that in JavaScript
//! means serialising both maps, merging them, and serialising the result again;
//! for a large bundle that is the dominant cost. Here the composition happens
//! in Rust and only the finished map is returned.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use oxc::allocator::Allocator;
use oxc::codegen::{Codegen, CodegenOptions, CommentOptions, LegalComment};
use oxc::minifier::{
  CompressOptions, CompressOptionsUnused, MangleOptions, Minifier, MinifierOptions,
  PropertyReadSideEffects, TreeShakeOptions,
};
use oxc::parser::Parser;
use oxc::span::SourceType;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[napi(object)]
#[derive(Default)]
pub struct MinifyOptions {
  /// Run the compressor. Defaults to `true`.
  pub compress: Option<bool>,
  /// Shorten identifiers. Defaults to `true`.
  pub mangle: Option<bool>,
  /// Also mangle top-level identifiers. Only meaningful with `mangle`.
  pub mangle_toplevel: Option<bool>,
  /// Produce a source map. Defaults to `true`.
  pub sourcemap: Option<bool>,
  /// Parse as an ES module rather than inferring from the file extension.
  pub module: Option<bool>,
  /// Strip whitespace from the output. Defaults to `true`.
  pub remove_whitespace: Option<bool>,
  /// Drop `debugger` statements. Defaults to `true`.
  pub drop_debugger: Option<bool>,
  /// Drop `console.*` calls. Defaults to `false`.
  pub drop_console: Option<bool>,
  /// Collapse consecutive statements with the comma operator. Defaults to
  /// `true`.
  pub sequences: Option<bool>,
  /// Merge consecutive variable declarations. Defaults to `true`.
  pub join_vars: Option<bool>,
  /// What to do with unused bindings: `remove`, `keepAssign` or `keep`.
  /// Defaults to `remove`.
  pub unused: Option<String>,
  /// Calls to treat as side-effect free, so an unused result can be dropped.
  /// Names are matched as written, e.g. `console.log`.
  pub manual_pure_functions: Option<Vec<String>>,
  /// Respect pure annotations such as `/* @__PURE__ */` and
  /// `/* #__NO_SIDE_EFFECTS__ */`. Defaults to `true`.
  pub annotations: Option<bool>,
  /// Whether reading a property can have side effects. `false` lets unused
  /// property reads be dropped, which matters for generated GraphQL/Relay
  /// code. Defaults to `true`.
  ///
  /// `oxc-minify` spells this `boolean | "always"`; normalise `"always"` to
  /// `true` before calling, since both mean the same thing.
  pub property_read_side_effects: Option<bool>,
  /// Which comments to keep: `legal`, `none` or `all`.
  ///
  /// Defaults to `legal`, which drops ordinary and jsdoc comments but keeps
  /// licence text - comments starting `/*!` or containing `@license` or
  /// `@preserve`. Note that `oxc-minify` drops those too, so `none` is the
  /// setting that reproduces it exactly.
  pub comments: Option<String>,
}

#[napi(object)]
pub struct MinifyResult {
  /// The minified code.
  pub code: String,
  /// The source map, as JSON. Already composed against `inputMap` when one was
  /// supplied, so it maps minified output straight back to original sources.
  /// `None` when source maps are switched off.
  pub map: Option<String>,
  /// Non-fatal problems encountered while parsing.
  pub warnings: Vec<String>,
}

/// Runs a minify job on the Node thread pool.
///
/// Bundlers call this once per asset, and a large bundle means thousands of
/// assets: doing that synchronously would block the very thread the caller is
/// trying to keep free, so the default export is the asynchronous one.
pub struct MinifyTask {
  filename: String,
  source: String,
  input_map: Option<String>,
  options: Option<MinifyOptions>,
}

impl Task for MinifyTask {
  type Output = MinifyResult;
  type JsValue = MinifyResult;

  fn compute(&mut self) -> Result<Self::Output> {
    minify_sync(
      std::mem::take(&mut self.filename),
      std::mem::take(&mut self.source),
      self.input_map.take(),
      self.options.take(),
    )
  }

  fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
    Ok(output)
  }
}

/// Minify `source`, optionally composing the result against `input_map`.
///
/// Resolves with a [`MinifyResult`]. The work happens on the Node thread pool,
/// so many assets can be in flight at once without occupying the main thread.
#[napi(ts_return_type = "Promise<MinifyResult>")]
pub fn minify(
  filename: String,
  source: String,
  input_map: Option<String>,
  options: Option<MinifyOptions>,
) -> AsyncTask<MinifyTask> {
  AsyncTask::new(MinifyTask {
    filename,
    source,
    input_map,
    options,
  })
}

/// Blocking version of [`minify`]. Useful in scripts and tests; prefer the
/// asynchronous form anywhere throughput matters.
///
/// `input_map` is the caller's existing map, as a JSON string: it must describe
/// how `source` relates to the original files. When present, the returned map
/// maps the minified output back to those original files in one hop.
#[napi(js_name = "minifySync")]
pub fn minify_sync(
  filename: String,
  source: String,
  input_map: Option<String>,
  options: Option<MinifyOptions>,
) -> Result<MinifyResult> {
  let options = options.unwrap_or_default();
  let want_map = options.sourcemap.unwrap_or(true);

  let allocator = Allocator::default();
  let source_type = match options.module {
    Some(true) => SourceType::mjs(),
    Some(false) => SourceType::cjs(),
    None => SourceType::from_path(&filename).unwrap_or(SourceType::mjs()),
  };

  let parsed = Parser::new(&allocator, &source, source_type).parse();
  // Recoverable problems are reported rather than thrown: a bundler generally
  // prefers a warning and unminified output to a failed build.
  let warnings: Vec<String> = parsed.errors.iter().map(|e| e.to_string()).collect();
  if parsed.panicked {
    return Err(Error::new(
      Status::GenericFailure,
      format!("failed to parse {filename}: {}", warnings.join("; ")),
    ));
  }

  let mut program = parsed.program;

  let minifier_options = MinifierOptions {
    compress: if options.compress.unwrap_or(true) {
      // oxc's own defaults already describe the smallest useful output, so
      // each field is overridden only when the caller asked for something
      // else rather than being restated here.
      let defaults = CompressOptions::default();
      Some(CompressOptions {
        drop_debugger: options.drop_debugger.unwrap_or(defaults.drop_debugger),
        drop_console: options.drop_console.unwrap_or(defaults.drop_console),
        sequences: options.sequences.unwrap_or(defaults.sequences),
        join_vars: options.join_vars.unwrap_or(defaults.join_vars),
        unused: match options.unused.as_deref() {
          Some("keep") => CompressOptionsUnused::Keep,
          Some("keepAssign") => CompressOptionsUnused::KeepAssign,
          Some("remove") | None => CompressOptionsUnused::Remove,
          Some(other) => {
            return Err(Error::new(
              Status::InvalidArg,
              format!("unknown `unused` value {other:?}; expected remove, keepAssign or keep"),
            ))
          }
        },
        treeshake: TreeShakeOptions {
          manual_pure_functions: options.manual_pure_functions.clone().unwrap_or_default(),
          annotations: options.annotations.unwrap_or(defaults.treeshake.annotations),
          // `oxc-minify` exposes this as `boolean | "always"`; both `true` and
          // `"always"` mean `All`, so the caller normalises to a bool.
          property_read_side_effects: match options.property_read_side_effects {
            Some(true) => PropertyReadSideEffects::All,
            Some(false) => PropertyReadSideEffects::None,
            None => defaults.treeshake.property_read_side_effects,
          },
          ..defaults.treeshake.clone()
        },
        ..defaults
      })
    } else {
      None
    },
    mangle: if options.mangle.unwrap_or(true) {
      Some(MangleOptions {
        top_level: options.mangle_toplevel,
        ..MangleOptions::default()
      })
    } else {
      None
    },
  };
  let minified = Minifier::new(minifier_options).minify(&allocator, &mut program);

  let codegen_options = CodegenOptions {
    minify: options.remove_whitespace.unwrap_or(true),
    // `minify` only removes whitespace: comments survive it, and on code
    // carrying jsdoc that can be most of the output. They have to be turned
    // off explicitly.
    comments: match options.comments.as_deref() {
      Some("all") => CommentOptions::default(),
      Some("none") => CommentOptions::disabled(),
      Some("legal") | None => CommentOptions {
        normal: false,
        jsdoc: false,
        annotation: false,
        legal: LegalComment::Inline,
      },
      Some(other) => {
        return Err(Error::new(
          Status::InvalidArg,
          format!("unknown `comments` value {other:?}; expected legal, none or all"),
        ))
      }
    },
    source_map_path: want_map.then(|| PathBuf::from(&filename)),
    ..CodegenOptions::default()
  };
  let generated = Codegen::new()
    .with_options(codegen_options)
    .with_scoping(minified.scoping)
    .build(&program);

  let map = match generated.map {
    Some(map) if want_map => Some(compose(&map, input_map.as_deref(), &filename)?),
    _ => None,
  };

  Ok(MinifyResult {
    code: generated.code,
    map,
    warnings,
  })
}

/// Chain the minifier's map onto the caller's, so the result points at the
/// original sources instead of at the minifier's input.
///
/// This is a genuine composition, not a positional fixup: every token in the
/// minifier's map is looked up in the caller's map, and the token that comes
/// out carries the caller's source file, position and name. Tokens that the
/// caller's map cannot explain are dropped, which is what makes the result a
/// single hop from minified output to original source.
fn compose(
  generated: &oxc_sourcemap::SourceMap,
  input_map: Option<&str>,
  filename: &str,
) -> Result<String> {
  let generated_json = generated.to_json_string();

  let Some(input_map) = input_map else {
    return Ok(generated_json);
  };

  let fail = |e: sourcemap::Error| Error::new(Status::GenericFailure, format!("{filename}: {e}"));

  let minified = sourcemap::SourceMap::from_slice(generated_json.as_bytes()).map_err(fail)?;
  let original = sourcemap::SourceMap::from_slice(input_map.as_bytes()).map_err(fail)?;

  let mut tokens: Vec<sourcemap::RawToken> = Vec::with_capacity(minified.get_token_count() as usize);
  let mut sources: Vec<Arc<str>> = Vec::new();
  let mut sources_content: Vec<Option<Arc<str>>> = Vec::new();
  let mut names: Vec<Arc<str>> = Vec::new();
  let mut source_ids: HashMap<String, u32> = HashMap::new();
  let mut name_ids: HashMap<String, u32> = HashMap::new();

  for token in minified.tokens() {
    // Ask the caller's map what this position in *its* output really was.
    let Some(source_token) = original.lookup_token(token.get_src_line(), token.get_src_col())
    else {
      continue;
    };
    let Some(source) = source_token.get_source() else {
      continue;
    };

    let src_id = *source_ids.entry(source.to_owned()).or_insert_with(|| {
      sources.push(Arc::from(source));
      sources_content.push(
        original
          .get_source_contents(source_token.get_src_id())
          .map(Arc::from),
      );
      (sources.len() - 1) as u32
    });

    // Prefer the name the minifier recorded; it knows what it renamed.
    let name = token.get_name().or_else(|| source_token.get_name());
    let name_id = match name {
      Some(name) => *name_ids.entry(name.to_owned()).or_insert_with(|| {
        names.push(Arc::from(name));
        (names.len() - 1) as u32
      }),
      None => !0,
    };

    // Take the token's own column rather than interpolating from the query
    // position. Adding `query_col - token_col` looks right, and reproduces
    // `webpack-sources` exactly when the generated line is a verbatim copy of
    // the original -- but that equality is a coincidence of copied text, not
    // a rule. `webpack-sources` gets its precision by emitting a token per
    // identifier, not by shifting one. On a transformed line, one generated
    // line draws on many original columns, so the shift runs off the end:
    // measured over 5.2M mappings of a production build, interpolating put
    // 1.627% of columns past the end of their own source line, against
    // 0.005% for `webpack-sources`.
    tokens.push(sourcemap::RawToken {
      dst_line: token.get_dst_line(),
      dst_col: token.get_dst_col(),
      src_line: source_token.get_src_line(),
      src_col: source_token.get_src_col(),
      src_id,
      name_id,
      is_range: false,
    });
  }

  let composed = sourcemap::SourceMap::new(
    Some(Arc::from(filename)),
    tokens,
    names,
    sources,
    Some(sources_content),
  );

  let mut out = Vec::with_capacity(generated_json.len());
  composed.to_writer(&mut out).map_err(fail)?;
  String::from_utf8(out)
    .map_err(|e| Error::new(Status::GenericFailure, format!("{filename}: {e}")))
}
