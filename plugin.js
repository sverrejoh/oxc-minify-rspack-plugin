"use strict";

const { minify } = require("./index.js");

const PLUGIN = "OxcMinifyRspackPlugin";

/**
 * Minifies a bundler's JavaScript assets with oxc.
 *
 * The interesting part is what it does *not* do. A minimizer receives code the
 * bundler has already transformed, plus a map back to the original sources, so
 * the minifier's own map has to be composed against that one. Doing it in
 * JavaScript means serialising both maps, merging them, and serialising the
 * result — which on a large bundle costs more than the minification. Here the
 * asset's map goes into Rust, the composition happens there, and what comes
 * back is the finished map.
 */
class OxcMinifyRspackPlugin {
  /**
   * @param {object} [options]
   * @param {RegExp} [options.test] Which assets to minify. Defaults to `.js`.
   * @param {boolean} [options.compress] Run the compressor. Default `true`.
   * @param {boolean} [options.mangle] Shorten identifiers. Default `true`.
   * @param {boolean} [options.mangleToplevel] Also mangle top-level names.
   * @param {boolean} [options.sourcemap] Emit source maps. Default `true`.
   * @param {boolean} [options.module] Force ES module parsing.
   */
  constructor(options = {}) {
    this.options = { test: /\.js$/, sourcemap: true, ...options };
  }

  apply(compiler) {
    const { test, ...minifyOptions } = this.options;

    compiler.hooks.compilation.tap(PLUGIN, (compilation) => {
      compilation.hooks.processAssets.tapPromise(
        {
          name: PLUGIN,
          stage: compiler.webpack.Compilation.PROCESS_ASSETS_STAGE_OPTIMIZE_SIZE,
        },
        async (assets) => {
          const { sources } = compiler.webpack;
          const files = Object.keys(assets).filter((file) => test.test(file));

          await Promise.all(
            files.map(async (file) => {
              const asset = assets[file];
              const source = asset.source();
              if (!source) return;
              const code = typeof source === "string" ? source : source.toString();

              // Only assets that already carry a map get one back. Generating a
              // map for an asset that never had one would add .map files the
              // build never asked for.
              const inputMap =
                this.options.sourcemap && typeof asset.map === "function"
                  ? asset.map({ columns: true })
                  : undefined;

              try {
                const result = await minify(
                  file,
                  code,
                  inputMap ? JSON.stringify(inputMap) : undefined,
                  minifyOptions
                );

                for (const warning of result.warnings ?? []) {
                  compilation.warnings.push(new Error(`${PLUGIN} [${file}]: ${warning}`));
                }
                if (!result.code) return;

                // result.map is already composed, so handing it over as the
                // outer map means the bundler has no merging left to do.
                compilation.updateAsset(
                  file,
                  inputMap && result.map
                    ? new sources.SourceMapSource(result.code, file, result.map)
                    : new sources.RawSource(result.code)
                );
              } catch (error) {
                compilation.errors.push(new Error(`${PLUGIN} [${file}]: ${error.message}`));
              }
            })
          );
        }
      );
    });
  }
}

module.exports = { OxcMinifyRspackPlugin };
module.exports.default = OxcMinifyRspackPlugin;
