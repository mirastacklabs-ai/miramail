// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { openable, previewKind, MAX_PREVIEW_IMAGE } from "./attach.ts";

test("only documents and pictures are openable", () => {
  for (const ok of ["a.pdf", "Photo.JPG", "sheet.xlsx", "x.y.txt"]) assert.ok(openable(ok), ok);
  for (const bad of ["run.command", "x.app", "a.sh", "i.dmg", "p.html", "v.svg", "x.exe", "noext", ".pdf", "evil.pdf.app", "z.zip", "a.terminal", "a.webloc"]) assert.ok(!openable(bad), bad);
});

test("preview kinds", () => {
  assert.deepEqual(previewKind("image/PNG", 10), { kind: "image", mime: "image/png" });
  assert.deepEqual(previewKind("image/jpg", 10), { kind: "image", mime: "image/jpeg" });
  assert.deepEqual(previewKind("application/pdf", 10), { kind: "pdf" });
  assert.equal(previewKind("image/svg+xml", 10), null, "svg is never previewed");
  assert.equal(previewKind("text/html", 10), null);
  assert.equal(previewKind("image/png", MAX_PREVIEW_IMAGE + 1), null);
  assert.equal(previewKind("application/pdf", 26 * 1024 * 1024), null);
});
