import assert from "node:assert/strict";
import test from "node:test";
import { GenMapping, addMapping, toEncodedMap } from "@jridgewell/gen-mapping";
import { TraceMap, originalPositionFor } from "@jridgewell/trace-mapping";
import { minify, minifySync } from "../index.js";

/**
 * The point of this package is collapsing two hops into one, so the tests build
 * a genuine two-hop situation: an "original" file, an intermediate produced by
 * some earlier transform, and a map describing that first hop. A result that
 * merely points back at the intermediate would be useless, so that is exactly
 * what is asserted against.
 */
const ORIGINAL = `export function computeTotalPrice(items, taxRate) {
  const subtotal = items.reduce((sum, item) => sum + item.price, 0);
  const tax = subtotal * taxRate;
  return subtotal + tax;
}
`;

// Pretend an earlier tool prefixed a banner, shifting every line down by 3.
const BANNER_LINES = 3;
const INTERMEDIATE = "\n".repeat(BANNER_LINES) + ORIGINAL;

function buildFirstHopMap() {
  const gen = new GenMapping({ file: "intermediate.js" });
  const lineCount = ORIGINAL.split("\n").length;
  for (let line = 0; line < lineCount; line++) {
    addMapping(gen, {
      generated: { line: line + 1 + BANNER_LINES, column: 0 },
      original: { line: line + 1, column: 0 },
      source: "original.js",
    });
  }
  const map = toEncodedMap(gen);
  map.sourcesContent = [ORIGINAL];
  return map;
}

test("composes the minifier's map onto the caller's, in one hop", async () => {
  const inputMap = buildFirstHopMap();
  const result = await minify("intermediate.js", INTERMEDIATE, JSON.stringify(inputMap), {
    module: true,
  });

  assert.ok(result.code.length > 0, "expected minified output");
  assert.ok(result.code.length < INTERMEDIATE.length, "expected it to be smaller");
  assert.ok(result.map, "expected a source map");

  const composed = JSON.parse(result.map);

  // The decisive assertion: the composed map must name the *original* file.
  // If composition were skipped it would name intermediate.js instead.
  assert.deepEqual(
    composed.sources,
    ["original.js"],
    "composed map should point at the original source, not the intermediate"
  );

  // And a real position must resolve back through both hops.
  const trace = new TraceMap(composed);
  const idx = result.code.indexOf("computeTotalPrice");
  assert.ok(idx >= 0, "exported name should survive minification");
  const line = result.code.slice(0, idx).split("\n").length;
  const column = idx - (result.code.lastIndexOf("\n", idx) + 1);

  const original = originalPositionFor(trace, { line, column });
  assert.equal(original.source, "original.js");
  assert.equal(
    original.line,
    1,
    "computeTotalPrice is declared on line 1 of the original, despite the banner"
  );
});

test("returns the minifier's own map when no input map is given", async () => {
  const result = await minify("standalone.js", ORIGINAL, undefined, { module: true });
  assert.ok(result.map);
  const map = JSON.parse(result.map);
  assert.deepEqual(map.sources, ["standalone.js"]);
});

test("omits the map when source maps are switched off", async () => {
  const result = await minify("x.js", ORIGINAL, undefined, { module: true, sourcemap: false });
  assert.ok(!result.map, "no map expected");
  assert.ok(result.code.length > 0);
});

test("mangles and compresses by default", async () => {
  const source = `function add(firstOperand, secondOperand) {
  const unusedBinding = 1;
  return firstOperand + secondOperand;
}
console.log(add(1, 2));
`;
  const result = await minify("m.js", source, undefined, { sourcemap: false });
  assert.ok(!result.code.includes("firstOperand"), "long parameter names should be mangled");
  assert.ok(!result.code.includes("unusedBinding"), "unused bindings should be dropped");
});

test("reports a parse failure rather than emitting broken output", async () => {
  await assert.rejects(() => minify("bad.js", "function ( {", undefined, {}), /bad\.js/);
});

test("minifySync does the same work without a promise", () => {
  const result = minifySync("s.js", ORIGINAL, undefined, { module: true });
  assert.ok(result.code.length > 0);
  assert.deepEqual(JSON.parse(result.map).sources, ["s.js"]);
});
