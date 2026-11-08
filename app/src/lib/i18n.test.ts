// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { en, t, registerLocale, setLocale, getLocale } from "./i18n.ts";

test("english lookup and placeholders", () => {
  assert.equal(t("prefs.tab.general"), "General");
  assert.equal(t("vac.status.on", { account: "a@x.org" }), "Automatic replies are ON for a@x.org.");
  assert.equal(t("vac.restored", {}), "Turned off. Your previous mail filter \"{name}\" is active again.");
});

test("partial locale falls back to English", () => {
  registerLocale("xx", { "prefs.tab.general": "Allgemein" });
  setLocale("xx");
  assert.equal(t("prefs.tab.general"), "Allgemein");
  assert.equal(t("prefs.tab.about"), "About");
  setLocale("nope");
  assert.equal(getLocale(), "xx");
  setLocale("en");
  assert.equal(t("prefs.tab.general"), "General");
});

test("every key has text", () => {
  for (const [k, v] of Object.entries(en)) assert.ok(v.length > 0, k);
});
