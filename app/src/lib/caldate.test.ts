// @ts-nocheck -- run with: node --test src/lib/caldate.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { layoutColumns, rangeFor, startOfWeek, dayLengthMinutes, presetRrule, presetFromRrule, validateRrule, weekStartDay, wallToRfc, rfcToWall, tzOffsetMinutes, occSpan, clipToDay, addMonths } from "./caldate.ts";

test("overlap layout: cluster shares columns, gaps reset", () => {
  const p = layoutColumns([
    { item: "a", start: 0, end: 60 }, { item: "b", start: 30, end: 90 }, { item: "c", start: 60, end: 120 }, { item: "d", start: 200, end: 230 },
  ]);
  const m = Object.fromEntries(p.map((x) => [x.item, x]));
  assert.deepEqual([m.a.col, m.b.col, m.c.col], [0, 1, 0]);
  assert.equal(m.a.cols, 2); assert.equal(m.c.cols, 2);
  assert.deepEqual([m.d.col, m.d.cols], [0, 1]);
});
test("three-way overlap uses three columns", () => {
  const p = layoutColumns([{ item: 1, start: 0, end: 100 }, { item: 2, start: 10, end: 100 }, { item: 3, start: 20, end: 100 }]);
  assert.ok(p.every((x) => x.cols === 3));
  assert.deepEqual(p.map((x) => x.col).sort(), [0, 1, 2]);
});
test("week range respects week start", () => {
  const wed = new Date(2026, 10, 4); // Wed 4 Nov 2026
  const [s, e] = rangeFor("week", wed, 1);
  assert.equal(s.getDay(), 1); assert.equal(s.getDate(), 2); assert.equal((e - s) / 864e5 | 0, 7);
  assert.equal(startOfWeek(wed, 0).getDate(), 1);
});
test("month range: whole weeks covering the month", () => {
  const [s, e] = rangeFor("month", new Date(2026, 10, 15), 0); // Nov 2026 starts Sunday
  assert.equal(s.getDate(), 1); assert.equal(Math.round((e - s) / 864e5), 35);
  const [s2, e2] = rangeFor("month", new Date(2026, 7, 10), 1); // Aug 2026 starts Saturday, Mon start -> 6 weeks
  assert.equal(s2.getDay(), 1); assert.equal(Math.round((e2 - s2) / 864e5), 42);
});
test("addMonths clamps", () => { assert.equal(addMonths(new Date(2026, 0, 31), 1).getDate(), 28); });
test("DST day lengths (zone-dependent; only asserted when TZ has DST)", () => {
  const lens = new Set();
  for (let i = 0; i < 366; i++) lens.add(dayLengthMinutes(new Date(2026, 0, 1 + i)));
  if (process.env.TZ === "America/New_York") { assert.ok(lens.has(1380)); assert.ok(lens.has(1500)); }
  assert.ok(lens.has(1440));
});
test("tz conversion incl. DST", () => {
  assert.equal(tzOffsetMinutes("Asia/Kolkata", Date.UTC(2026, 0, 1)), 330);
  assert.equal(wallToRfc("2026-11-06", "15:00", "Asia/Kolkata"), "2026-11-06T15:00:00+05:30");
  assert.equal(wallToRfc("2026-07-01", "09:00", "America/New_York"), "2026-07-01T09:00:00-04:00");
  assert.equal(wallToRfc("2026-12-01", "09:00", "America/New_York"), "2026-12-01T09:00:00-05:00");
  assert.equal(wallToRfc("2026-03-08", "02:30", "America/New_York"), "2026-03-08T03:30:00-04:00"); // spring-forward gap resolves forward
  assert.deepEqual(rfcToWall("2026-11-06T15:00:00+05:30", "Asia/Kolkata"), { date: "2026-11-06", time: "15:00" });
  assert.deepEqual(rfcToWall("2026-11-06T09:30:00Z", "Asia/Kolkata"), { date: "2026-11-06", time: "15:00" });
});
test("rrule presets", () => {
  const mon = new Date(2026, 10, 2);
  assert.equal(presetRrule("weekly", mon), "FREQ=WEEKLY;BYDAY=MO");
  assert.equal(presetRrule("monthly", mon), "FREQ=MONTHLY;BYMONTHDAY=2");
  assert.equal(presetRrule("yearly", mon), "FREQ=YEARLY;BYMONTH=11;BYMONTHDAY=2");
  assert.equal(presetRrule("weekdays", mon), "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR");
  assert.equal(presetRrule("none", mon), null);
  assert.equal(presetFromRrule("RRULE:FREQ=DAILY", mon), "daily");
  assert.equal(presetFromRrule("FREQ=WEEKLY;BYDAY=TU", mon), "custom");
  assert.equal(presetFromRrule(null, mon), "none");
});
test("rrule validation", () => {
  assert.equal(validateRrule("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10"), null);
  assert.equal(validateRrule("rrule:freq=monthly;byday=1fr;interval=2"), null);
  for (const bad of ["", "BYDAY=MO", "FREQ=HOURLY", "FREQ=DAILY;COUNT=0", "FREQ=DAILY;COUNT=2;UNTIL=20271231", "FREQ=WEEKLY;BYDAY=XX", "FREQ=DAILY;FOO=1", "FREQ=MONTHLY;BYMONTHDAY=0", "FREQ=DAILY;UNTIL=tomorrow"])
    assert.ok(validateRrule(bad), bad);
});
test("week start fallback", () => {
  assert.equal(weekStartDay("en-US") === 0, true);
  assert.equal(weekStartDay("de-DE"), 1);
});
test("all-day exclusive end and clipping", () => {
  const sp = occSpan({ start: "2026-11-05", end: "2026-11-07", allDay: true });
  assert.equal(sp.end.getDate(), 7);
  const t = occSpan({ start: "2026-11-05T23:00:00Z", end: "2026-11-06T01:00:00Z", allDay: false });
  const d = new Date(t.start); 
  assert.ok(clipToDay(t, d));
  assert.equal(clipToDay(t, new Date(2030, 0, 1)), null);
});
