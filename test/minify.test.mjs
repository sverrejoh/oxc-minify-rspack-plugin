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

test("treats manually-pure calls as droppable, and keeps them otherwise", async () => {
  const source = `export function go(){ console.log("noise"); return 42; }`;

  const kept = await minify("k.js", source, undefined, {
    module: true,
    sourcemap: false,
  });
  assert.ok(kept.code.includes("console.log"), "console.log should survive by default");

  const dropped = await minify("d.js", source, undefined, {
    module: true,
    sourcemap: false,
    manualPureFunctions: ["console.log"],
  });
  assert.ok(
    !dropped.code.includes("console.log"),
    `manualPureFunctions did not reach the compressor: ${dropped.code}`
  );
});

test("rejects an unknown unused mode rather than silently ignoring it", async () => {
  await assert.rejects(
    () => minify("u.js", "export const a = 1;", undefined, { unused: "nope" }),
    /unknown `unused` value/
  );
});

// Composing must never invent a column. Looking a position up in the input
// map returns the nearest preceding token, and it is tempting to add the
// leftover offset to sharpen the result -- but a generated line assembled by
// a transform draws on many original columns, so the offset means nothing and
// the sharpened column can land past the end of the line it names. A mapping
// that points at a position which does not exist is worse than a coarse one:
// it sends a debugger somewhere the file cannot go. Regression test: this
// used to interpolate.
test("never maps a position past the end of its original line", async () => {
  // The original is one short line. Everything in the minified output must
  // therefore land inside it, however long the intermediate line is.
  const original = "const x = 1;";
  const source =
    `export function longFunctionName(alphaParam, betaParam) ` +
    `{ return alphaParam + betaParam + alphaParam * betaParam; }`;

  // A single token at the start of the line, pointing at column 6 of orig.js.
  const inputMap = JSON.stringify({
    version: 3,
    file: "in.js",
    sources: ["orig.js"],
    sourcesContent: [original],
    names: [],
    mappings: "AAAM",
  });

  const { map } = await minify("in.js", source, inputMap, { module: true });
  const parsed = JSON.parse(map);

  const B64 =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const decode = (s) => {
    const out = [];
    let i = 0;
    while (i < s.length) {
      let r = 0, sh = 0, c;
      do {
        c = B64.indexOf(s[i++]);
        r += (c & 31) << sh;
        sh += 5;
      } while (c & 32);
      out.push(r & 1 ? -(r >> 1) : r >> 1);
    }
    return out;
  };

  const lines = (parsed.sourcesContent?.[0] ?? original).split("\n");
  const bad = [];
  let srcLine = 0, srcCol = 0, n = 0;
  for (const line of parsed.mappings.split(";")) {
    for (const seg of line.split(",")) {
      if (!seg) continue;
      const f = decode(seg);
      if (f.length < 4) continue;
      srcLine += f[2];
      srcCol += f[3];
      n++;
      const len = lines[srcLine]?.length ?? 0;
      if (srcCol > len) bad.push(`${srcLine}:${srcCol} but line is ${len}`);
    }
  }

  assert.ok(n > 0, "expected the composed map to contain mappings");
  assert.equal(
    bad.length,
    0,
    `${bad.length} of ${n} mappings point past the end of their line: ` +
      bad.slice(0, 5).join(", ")
  );
});

// `minify` in oxc's codegen only removes whitespace. Comments survive it, and
// on code carrying jsdoc they can be most of the output: a real 58 KB module
// minified to 48 KB, of which 42 KB was comments, against 6.6 KB from
// oxc-minify. Regression test for that.
test("drops comments, keeping licence text unless told otherwise", async () => {
  const source = [
    `/*! Copyright Example Corp. @license MIT */`,
    `/** A jsdoc block that is pure documentation. */`,
    `// an ordinary comment`,
    `export function hello(someLongParameterName) {`,
    `  return someLongParameterName + 1;`,
    `}`,
  ].join("\n");

  const dflt = await minify("c.js", source, undefined, {
    module: true,
    sourcemap: false,
  });
  assert.ok(dflt.code.includes("@license"), "licence text must survive");
  assert.ok(!dflt.code.includes("jsdoc block"), "jsdoc must be dropped");
  assert.ok(!dflt.code.includes("ordinary comment"), "normal must be dropped");

  const none = await minify("c.js", source, undefined, {
    module: true,
    sourcemap: false,
    comments: "none",
  });
  assert.ok(!none.code.includes("@license"), `comments:none left ${none.code}`);
  assert.ok(!none.code.includes("/*"), "no comment should remain");

  const all = await minify("c.js", source, undefined, {
    module: true,
    sourcemap: false,
    comments: "all",
  });
  assert.ok(all.code.includes("jsdoc block"), "comments:all must keep jsdoc");

  assert.ok(
    none.code.length < dflt.code.length && dflt.code.length < all.code.length,
    `expected none < legal < all, got ${none.code.length}/` +
      `${dflt.code.length}/${all.code.length}`
  );

  await assert.rejects(
    () => minify("c.js", source, undefined, { comments: "bogus" }),
    /unknown `comments` value/
  );
});

test("treeshake annotations and propertyReadSideEffects change the output", async () => {
  // A pure-annotated call whose result is unused: droppable only when
  // annotations are respected.
  const annotated = `const unused = /* @__PURE__ */ sideEffecty(1);\nexport const kept = 2;\n`;

  const respected = await minify("a.js", annotated, undefined, {
    module: true,
    sourcemap: false,
    annotations: true,
  });
  const ignored = await minify("a.js", annotated, undefined, {
    module: true,
    sourcemap: false,
    annotations: false,
  });
  assert.ok(
    !respected.code.includes("sideEffecty"),
    `annotations:true should drop the pure call, got ${respected.code}`
  );
  assert.ok(
    ignored.code.includes("sideEffecty"),
    `annotations:false should keep the call, got ${ignored.code}`
  );

  // An unused property read: droppable only when reads are side-effect free.
  const propRead = `const o = globalThis.someObject;\nconst unused = o.a.b;\nexport const kept = 1;\n`;

  const free = await minify("b.js", propRead, undefined, {
    module: true,
    sourcemap: false,
    propertyReadSideEffects: false,
  });
  const unsafe = await minify("b.js", propRead, undefined, {
    module: true,
    sourcemap: false,
    propertyReadSideEffects: true,
  });
  assert.ok(
    free.code.length < unsafe.code.length,
    `propertyReadSideEffects:false should drop more, got ` +
      `${free.code.length} vs ${unsafe.code.length}`
  );
});
