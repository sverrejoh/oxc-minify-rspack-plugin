import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { rspack } = require("@rspack/core");
const { OxcMinifyRspackPlugin } = require("../plugin.js");

// A named export the minifier will rename, on a line the bundler will move.
const ENTRY = `import { describeTotal } from "./math.js";
console.log(describeTotal([1, 2, 3]));
`;

const MATH = `export function describeTotal(numbers) {
  const total = numbers.reduce((sum, value) => sum + value, 0);
  return "total is " + total;
}
`;

function build(dir) {
  return new Promise((resolve, reject) => {
    rspack(
      {
        mode: "production",
        context: dir,
        entry: path.join(dir, "src/index.js"),
        devtool: "source-map",
        output: { path: path.join(dir, "dist"), filename: "bundle.js" },
        optimization: { minimizer: [new OxcMinifyRspackPlugin()] },
      },
      (err, stats) => {
        if (err) return reject(err);
        if (stats.hasErrors()) {
          return reject(new Error(stats.toString({ all: false, errors: true })));
        }
        resolve(stats);
      }
    );
  });
}

test("minifies a real rspack build and maps back to original sources", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "oxc-plugin-"));
  try {
    fs.mkdirSync(path.join(dir, "src"));
    fs.writeFileSync(path.join(dir, "src/index.js"), ENTRY);
    fs.writeFileSync(path.join(dir, "src/math.js"), MATH);

    await build(dir);

    const code = fs.readFileSync(path.join(dir, "dist/bundle.js"), "utf8");
    const map = JSON.parse(fs.readFileSync(path.join(dir, "dist/bundle.js.map"), "utf8"));

    // Minified: the source text is gone and the whole bundle is a couple of lines.
    assert.ok(!code.includes("describeTotal"), "top-level name survived minification");
    assert.ok(code.split("\n").length < 5, `expected minified output, got:\n${code}`);
    assert.ok(code.includes("total is"), "the program's own string literal is missing");

    // Composed: the map names the files the developer wrote, not an
    // intermediate produced by the bundler.
    const originals = map.sources.filter((s) => s.includes("src/math.js"));
    assert.equal(originals.length, 1, `sources were ${JSON.stringify(map.sources)}`);
    assert.ok(
      map.sourcesContent?.[map.sources.indexOf(originals[0])]?.includes("describeTotal"),
      "sourcesContent does not carry the original text"
    );

    // And the mappings survived the composition rather than being emptied out.
    assert.ok(map.mappings.length > 50, "mappings look truncated");
    assert.ok(map.names.includes("describeTotal"), "original names were lost");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
