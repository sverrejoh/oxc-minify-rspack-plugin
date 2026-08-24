import type { Compiler } from "@rspack/core";

export interface OxcMinifyRspackPluginOptions {
  /** Which assets to minify. Defaults to `/\.js$/`. */
  test?: RegExp;
  /** Run the compressor. Defaults to `true`. */
  compress?: boolean;
  /** Shorten identifiers. Defaults to `true`. */
  mangle?: boolean;
  /** Also mangle top-level identifiers. */
  mangleToplevel?: boolean;
  /** Emit source maps. Defaults to `true`. */
  sourcemap?: boolean;
  /** Force ES module parsing rather than inferring from the file extension. */
  module?: boolean;
}

export declare class OxcMinifyRspackPlugin {
  constructor(options?: OxcMinifyRspackPluginOptions);
  apply(compiler: Compiler): void;
}

export default OxcMinifyRspackPlugin;
