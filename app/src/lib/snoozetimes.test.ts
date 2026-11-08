// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/snoozetimes.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { checkTime, fromLocalInput, MAX_AHEAD_MS, presets, toLocalInput, whenLabel } from "./snoozetimes.ts";

// Built from local components so the tests hold in any time zone. 2026-11-05 is a Thursday.
const d = (y: number, mo: number, da: number, h = 0, mi = 0) => new Date(y, mo - 1, da, h, mi, 0, 0);
const by = (now: Date) => Object.fromEntries(presets(now).map((p) => [p.id, p.at]));

test("presets on a Thursday morning", () => {
  const p = by(d(2026, 11, 5, 9, 30));
  assert.deepEqual(p.later, d(2026, 11, 5, 18));
  assert.deepEqual(p.tomorrow, d(2026, 11, 6, 8));
  assert.deepEqual(p.weekend, d(2026, 11, 7, 8));
  assert.deepEqual(p.nextweek, d(2026, 11, 9, 8));
});

test("later today after 15:00 is three hours on, rounded up, and absent when it would pass midnight", () => {
  assert.deepEqual(by(d(2026, 11, 5, 15, 5)).later, d(2026, 11, 5, 18, 15));
  assert.equal(by(d(2026, 11, 5, 21, 0)).later, undefined);
  assert.ok(presets(d(2026, 11, 5, 23, 59)).every((p) => p.id !== "later"));
});

test("weekend and next week on Saturday, Sunday and Monday", () => {
  const sat = by(d(2026, 11, 7, 10));
  assert.deepEqual(sat.weekend, d(2026, 11, 14, 8), "on Saturday the weekend is next week's");
  assert.deepEqual(sat.nextweek, d(2026, 11, 9, 8));
  const sun = by(d(2026, 11, 8, 10));
  assert.deepEqual(sun.weekend, d(2026, 11, 14, 8));
  assert.deepEqual(sun.nextweek, d(2026, 11, 9, 8));
  const mon = by(d(2026, 11, 9, 10));
  assert.deepEqual(mon.nextweek, d(2026, 11, 16, 8), "Monday means the following Monday");
  assert.deepEqual(mon.weekend, d(2026, 11, 14, 8));
});

test("every preset is in the future, across a month and year end", () => {
  for (const now of [d(2026, 12, 31, 23, 30), d(2026, 2, 28, 7, 0), d(2028, 2, 28, 16, 0)]) {
    for (const p of presets(now)) assert.ok(p.at.getTime() > now.getTime(), `${p.id} from ${now.toISOString()}`);
  }
  assert.deepEqual(by(d(2026, 12, 31, 9)).tomorrow, d(2027, 1, 1, 8));
});

test("datetime-local round trip and validation", () => {
  const x = d(2026, 11, 5, 9, 5);
  assert.equal(toLocalInput(x), "2026-11-05T09:05");
  assert.deepEqual(fromLocalInput("2026-11-05T09:05"), x);
  assert.equal(fromLocalInput(""), null);
  assert.equal(fromLocalInput("2026-02-31T10:00"), null, "rolled-over day rejected");
  assert.equal(fromLocalInput("garbage"), null);
});

test("checkTime enforces a minute minimum and a one year maximum", () => {
  const now = d(2026, 11, 5, 9);
  assert.match(checkTime(null, now)!, /Enter/);
  assert.match(checkTime(new Date(now.getTime() + 30_000), now)!, /minute/);
  assert.equal(checkTime(new Date(now.getTime() + 120_000), now), null);
  assert.match(checkTime(new Date(now.getTime() + MAX_AHEAD_MS + 86_400_000), now)!, /year/);
});

test("whenLabel", () => {
  const now = d(2026, 11, 5, 9);
  assert.equal(whenLabel(d(2026, 11, 5, 18).getTime(), now), "Today 18:00");
  assert.equal(whenLabel(d(2026, 11, 6, 8).getTime(), now), "Tomorrow 08:00");
  assert.equal(whenLabel(d(2026, 11, 7, 8).getTime(), now), "Sat 7 Nov 08:00");
  assert.equal(whenLabel(d(2027, 1, 2, 8).getTime(), now), "Sat 2 Jan 2027 08:00");
});
