// node --test app/ui-tests/
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  compact,
  dayKey,
  elapsed,
  hourName,
  longDate,
  pct,
  plural,
  resetPassed,
  severity,
  usageLevel,
  meterCells,
  span,
  timeAgo,
  timeUntil,
} from "../ui/scripts/format.js";

const isoIn = (ms) => new Date(Date.now() + ms).toISOString();

test("elapsed counts seconds, then minutes, hours and days, two digits after the first unit", () => {
  assert.equal(elapsed(0), "0s");
  assert.equal(elapsed(12.9), "12s");
  assert.equal(elapsed(62), "1m02s");
  assert.equal(elapsed(3900), "1h05m");
  assert.equal(elapsed(2 * 86400 + 3 * 3600), "2d03h");
  assert.equal(elapsed(-5), "0s");
});

test("span drops the seconds once there is a minute", () => {
  assert.equal(span(48), "48s");
  assert.equal(span(4 * 60 + 59), "4m");
  assert.equal(span(2 * 3600 + 24 * 60), "2h24m");
  assert.equal(span(4 * 86400 + 23 * 3600), "4d23h");
});

test("timeUntil is null for a missing, unreadable or past date and minutes at least otherwise", () => {
  assert.equal(timeUntil(null), null);
  assert.equal(timeUntil("not a date"), null);
  assert.equal(timeUntil(isoIn(-1000)), null);
  assert.equal(timeUntil(isoIn(5000)), "1m");
  assert.match(timeUntil(isoIn((2 * 3600 + 24 * 60 + 30) * 1000)), /^2h2[45]m$/);
});

test("timeAgo says just now, minutes, hours or days", () => {
  assert.equal(timeAgo(isoIn(-10_000)), "just now");
  assert.equal(timeAgo(isoIn(-5 * 60_000)), "5 min ago");
  assert.equal(timeAgo(isoIn(-3 * 3600_000)), "3 h ago");
  assert.equal(timeAgo(isoIn(-50 * 3600_000)), "2 d ago");
});

test("pct rounds and plural picks the noun", () => {
  assert.equal(pct(41.6), "42%");
  assert.equal(plural(1, "session", "sessions"), "1 session");
  assert.equal(plural(0, "session", "sessions"), "0 sessions");
  assert.equal(plural(5, "day", "days"), "5 days");
});

test("severity takes the server's word and falls back to the percentage", () => {
  assert.equal(severity({ severity: "Critical", percent: 10 }), "critical");
  assert.equal(severity({ severity: { Unknown: "critical" }, percent: 10 }), "critical");
  assert.equal(severity({ severity: "Warning", percent: 10 }), "warning");
  assert.equal(severity({ percent: 96 }), "critical");
  assert.equal(severity({ percent: 80 }), "warning");
  assert.equal(severity({ severity: "Normal", percent: 50 }), "normal");
});

test("compact writes counts as the status line does", () => {
  assert.equal(compact(940), "940");
  assert.equal(compact(577_000), "577K");
  assert.equal(compact(1_234_567), "1.2M");
  assert.equal(compact(12_345_678), "12M");
  assert.equal(compact(1_400_000_000), "1.4B");
});

test("longDate writes a report day in English and leaves anything else as it is", () => {
  assert.equal(longDate("2026-10-06"), "Oct 6, 2026");
  assert.equal(longDate("someday"), "someday");
});

test("dayKey is the local day, zero-padded", () => {
  assert.equal(dayKey(new Date(2026, 0, 5, 23, 59)), "2026-01-05");
  assert.equal(dayKey(new Date(2026, 9, 14)), "2026-10-14");
});

test("hourName writes the hour on a 12-hour clock", () => {
  assert.equal(hourName(0), "12 AM");
  assert.equal(hourName(9), "9 AM");
  assert.equal(hourName(12), "12 PM");
  assert.equal(hourName(17), "5 PM");
});

test("usageLevel: green under 50, amber from 50, red from 80, raised by the server", () => {
  assert.equal(usageLevel({ percent: 49 }), "ok");
  assert.equal(usageLevel({ percent: 50 }), "warn");
  assert.equal(usageLevel({ percent: 79.4 }), "warn");
  assert.equal(usageLevel({ percent: 80 }), "crit");
  assert.equal(usageLevel({ severity: "Warning", percent: 10 }), "warn");
  assert.equal(usageLevel({ severity: "Critical", percent: 60 }), "crit");
  assert.equal(usageLevel({ severity: "Normal", percent: 85 }), "crit");
});

test("meterCells rounds to tenths and stays within 0..10", () => {
  assert.equal(meterCells(0), 0);
  assert.equal(meterCells(39), 4);
  assert.equal(meterCells(71), 7);
  assert.equal(meterCells(88), 9);
  assert.equal(meterCells(130), 10);
  assert.equal(meterCells(-5), 0);
  assert.equal(meterCells(NaN), 0);
});

test("resetPassed is true once the reset date is reached, false for a missing or unreadable one", () => {
  const now = Date.parse("2026-10-15T12:00:00Z");
  assert.equal(resetPassed(null, now), false);
  assert.equal(resetPassed("not a date", now), false);
  assert.equal(resetPassed("2026-10-15T12:00:01Z", now), false);
  assert.equal(resetPassed("2026-10-15T12:00:00Z", now), true);
  assert.equal(resetPassed("2026-10-15T11:00:00Z", now), true);
  assert.equal(resetPassed(isoIn(-1000)), true);
});
