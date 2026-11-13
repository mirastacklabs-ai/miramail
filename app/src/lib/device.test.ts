// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/device.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { thisDevice, deviceWord, keychainName, osName } from "./device.ts";

const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
test("device name", () => {
  assert.equal(thisDevice("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)", 5), "this iPhone");
  assert.equal(thisDevice(MAC, 0), "this Mac");
  assert.equal(thisDevice(MAC, 5), "this iPad", "iPadOS reports a Mac user agent with touch points");
  assert.equal(thisDevice("Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X)", 5), "this iPad");
});

test("wording helpers: Mac text is unchanged, phone text is neutral", () => {
  const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)";
  assert.equal(deviceWord(MAC, 0), "Mac");
  assert.equal(keychainName(MAC, 0), "macOS Keychain");
  assert.equal(osName(MAC, 0), "macOS");
  assert.equal(deviceWord(IPHONE, 5), "iPhone");
  assert.equal(keychainName(IPHONE, 5), "Keychain");
  assert.equal(osName(IPHONE, 5), "iOS");
  assert.equal(osName(MAC, 5), "iPadOS");
});

test("deviceMsg maps backend 'on this Mac' wording, Mac unchanged", async () => {
  const { deviceMsg } = await import("./device.ts");
  const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)";
  assert.equal(deviceMsg("Saved on this Mac, but not on the server.", MAC, 0), "Saved on this Mac, but not on the server.");
  assert.equal(deviceMsg("Saved on this Mac, but not on the server.", IPHONE, 5), "Saved on this iPhone, but not on the server.");
  assert.equal(deviceMsg("Deleted on this Mac, but not on the calendar server: x", IPHONE, 5), "Deleted on this iPhone, but not on the calendar server: x");
});

test("exact Mac strings of the wording constants (node reports no touch: Mac)", async () => {
  const { TONE_LOCAL_NOTICE } = await import("./aivm.ts");
  const { PRIVACY_NOTE } = await import("./safety.ts");
  const air = await import("./airules.ts");
  const { t } = await import("./i18n.ts");
  assert.equal(TONE_LOCAL_NOTICE, "Nothing leaves this Mac: the provider runs locally.");
  assert.equal(PRIVACY_NOTE, "Checked on this Mac. Nothing was sent anywhere. This check cannot prove a message is safe.");
  assert.equal(t("vac.intro"), "MiraMail asks your mail server to answer incoming mail for you, so it keeps working while this Mac is off. It uses your mail server's ManageSieve service; nothing goes anywhere else.");
  const all = Object.values(air).flat().filter((v) => typeof v === "string");
  assert.ok(all.some((s) => s.startsWith("Rules and automatic replies run only while MiraMail is open on this Mac. For the times it is closed")));
  assert.ok(all.some((s) => s.startsWith("It only works while MiraMail is open on this Mac. Mail that arrives")));
});
