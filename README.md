# @sverrejoh/oxc-minify-rspack-plugin

Minify JavaScript with [oxc](https://oxc.rs), and compose the resulting source
map against the map you already have — without either map crossing into
JavaScript.

Ships prebuilt binaries for all major platforms, so there is no toolchain to
install and nothing to compile at install time.

## Why

A minifier used inside a bundler never sees original source. It sees code that
has already been through a loader pipeline, together with a map back to the
files a developer actually wrote. The minifier can only describe its own step,
so somebody has to compose those two maps into one.

Doing that in JavaScript is the expensive part. The bundler's map has to be
serialised, the minifier's map parsed, the two merged, and the result
serialised again — for a large bundle that is easily more time than the
minification itself, and all of it on the main thread.

`oxc-minify` on npm cannot help here: its options are `module`, `compress`,
`mangle`, `codegen` and `sourcemap`, with no way to pass an input map. So this
package does the composition in Rust, next to the minifier, and returns a
finished map that points straight at the original sources.

## Install

```sh
npm install --save-dev @sverrejoh/oxc-minify-rspack-plugin
```

## Use it as an rspack minimizer

```js
const { OxcMinifyRspackPlugin } = require("@sverrejoh/oxc-minify-rspack-plugin/plugin");

module.exports = {
  optimization: {
    minimizer: [new OxcMinifyRspackPlugin({ mangleToplevel: true })],
  },
};
```

| option | default | meaning |
| --- | --- | --- |
| `test` | `/\.js$/` | which assets to minify |
| `compress` | `true` | run the compressor |
| `mangle` | `true` | shorten identifiers |
| `mangleToplevel` | `false` | also mangle top-level names |
| `sourcemap` | `true` | emit source maps |
| `module` | inferred | force ES module parsing |
| `removeWhitespace` | `true` | strip whitespace from the output |
| `dropDebugger` | `true` | remove `debugger` statements |
| `dropConsole` | `false` | remove `console.*` calls |
| `sequences` | `true` | collapse statements with the comma operator |
| `joinVars` | `true` | merge consecutive variable declarations |
| `unused` | `remove` | unused bindings: `remove`, `keepAssign`, `keep` |
| `manualPureFunctions` | `[]` | calls whose unused results may be dropped |

Assets that arrive without a source map are minified but get no map back, so
the build does not gain `.map` files it never asked for.

## Use it directly

```js
const { minify } = require("@sverrejoh/oxc-minify-rspack-plugin");

const { code, map, warnings } = await minify(
  "bundle.js",
  intermediateCode,
  JSON.stringify(mapFromYourEarlierStep), // optional
  { mangleToplevel: true }
);
```

`map` is a JSON string that maps the minified output back to your original
sources in one hop. Pass no input map and you get the minifier's own map
instead.

`minify` resolves on the Node thread pool, so many assets can be in flight at
once without occupying the main thread. A blocking `minifySync` is exported for
scripts and tests.

## Supported platforms

| | x64 | arm64 | arm | ia32 |
| --- | :-: | :-: | :-: | :-: |
| Linux (glibc) | ✓ | ✓ | ✓ | |
| Linux (musl) | ✓ | ✓ | | |
| macOS | ✓ | ✓ | | |
| Windows | ✓ | ✓ | | ✓ |

npm picks the right binary through `optionalDependencies`.

## Developing

```sh
npm install
npm run build     # cargo build + generate the JS bindings
npm test
```

Releases are cut by pushing a tag that matches `package.json`:

```sh
npm version minor
git push --follow-tags
```

CI builds every target, runs the tests on Linux, macOS and Windows, and
publishes to npm with provenance. The publish job refuses a tag that disagrees
with `package.json`.

## License

MIT
