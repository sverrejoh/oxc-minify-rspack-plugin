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
use oxc::codegen::{Codegen, CodegenOptions};
use oxc::minifier::{CompressOptions, MangleOptions, Minifier, MinifierOptions};
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
  let warnings: Vec<String> = parsed.diagnostics.iter().map(|e| e.to_string()).collect();
  if parsed.panicked {
    return Err(Error::new(
      Status::GenericFailure,
      format!("failed to parse {filename}: {}", warnings.join("; ")),
    ));
  }

  let mut program = parsed.program;

  let minifier_options = MinifierOptions {
    compress: if options.compress.unwrap_or(true) {
      Some(CompressOptions::default())
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
    mangle_properties: None,
  };
  let minified = Minifier::new(minifier_options).minify(&allocator, &mut program);

  let codegen_options = CodegenOptions {
    minify: true,
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
