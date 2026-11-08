// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/csshide.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { cssNum, styleHides, opacityHides, fontSizeHides } from "./csshide.ts";

test("cssNum semantics", () => {
  assert.deepEqual(cssNum(" 12px "), { n: 12, unit: "px" });
  assert.deepEqual(cssNum("-.5em"), { n: -0.5, unit: "em" });
  assert.deepEqual(cssNum("1e2px"), { n: 100, unit: "px" });
  assert.deepEqual(cssNum("1.5E-1%"), { n: 0.15, unit: "%" });
  assert.deepEqual(cssNum("2 PX"), { n: 2, unit: "px" });
  assert.deepEqual(cssNum("0"), { n: 0, unit: "" });
  for (const bad of ["", "small", "inherit", "px", ".", "+", "1 a b", "1px 2px", "1.2.3", "--1"]) assert.equal(cssNum(bad), null, bad);
});

test("hide / visible cases", () => {
  assert.ok(styleHides("font-size:0"));
  assert.ok(styleHides("FONT-SIZE: 1px"));
  assert.ok(!styleHides("font-size:12px"));
  assert.ok(!styleHides("font-size:small"));
  assert.ok(styleHides("opacity:0"));
  assert.ok(!styleHides("opacity:0.5"));
  assert.ok(styleHides("display:none"));
  assert.ok(styleHides("color:rgba(0,0,0,0)"));
  assert.ok(styleHides("margin-left:-9999px", true));
  assert.ok(!styleHides("margin-left:-9999px", false));
  assert.ok(styleHides("height:0;overflow:hidden", true));
  assert.ok(opacityHides("5%") && !opacityHides("50%"));
  assert.ok(fontSizeHides("1pt") && !fontSizeHides("12pt"));
});

test("hostile long values are fast and not hidden", () => {
  const cases = [
    "font-size:" + "9".repeat(200_000) + " a b",
    "font-size:" + "9".repeat(200_000),
    "opacity:0" + "0".repeat(200_000),
    "font-size:" + "a".repeat(200_000),
    "margin-left:-" + "9".repeat(200_000) + "px",
    "font:" + "1 ".repeat(100_000),
  ];
  for (const c of cases) {
    const t = performance.now();
    assert.equal(styleHides(c, true), false);
    assert.ok(performance.now() - t < 100, "slow: " + c.slice(0, 20));
  }
  const w = performance.now();
  assert.equal(styleHides("font-size:" + " ".repeat(200_000) + "1px"), true); // padding is trimmed, the value itself is short
  assert.ok(performance.now() - w < 100);
  const t = performance.now();
  assert.equal(cssNum("9".repeat(200_000) + " a b"), null);
  assert.equal(cssNum("9".repeat(150)).n, Number("9".repeat(150)));
  assert.equal(cssNum("9".repeat(300)), null);
  assert.ok(performance.now() - t < 100);
});
