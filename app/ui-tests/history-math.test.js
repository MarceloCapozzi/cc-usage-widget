// node --test app/ui/tests/
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  abbreviate,
  buildSeries,
  gapLimit,
  HOUR_MS,
  MINUTE_MS,
  noHistoryText,
  RANGE_HOURS,
  runs,
  timeTicks,
} from "../ui/scripts/history-math.js";

test("abbreviate names the series at the line's tip", () => {
  assert.equal(abbreviate("5-hour session"), "5h");
  assert.equal(abbreviate("All models · weekly"), "7d");
  assert.equal(abbreviate("Opus · weekly"), "Opus");
});

test("buildSeries orders the 5-hour session, the overall weekly, then the models by name", () => {
  const samples = [
    { t: 1, v: [{ k: "Sonnet · weekly", p: 1 }, { k: "All models · weekly", p: 2 }, { k: "5-hour session", p: 3 }] },
    { t: 2, v: [{ k: "Opus · weekly", p: 4 }, { k: "5-hour session", p: 5, r: 9 }] },
  ];
  const colours = [];
  const series = buildSeries(samples, (name, k) => {
    colours.push([name, k]);
    return `c-${k}`;
  });
  assert.deepEqual(series.map((s) => s.name), ["5-hour session", "All models · weekly", "Opus · weekly", "Sonnet · weekly"]);
  // The per-model ones count from 0 for their colour.
  assert.deepEqual(colours, [["5-hour session", 0], ["All models · weekly", 0], ["Opus · weekly", 0], ["Sonnet · weekly", 1]]);
  assert.deepEqual(series[0].points, [{ i: 0, t: 1, p: 3, r: undefined }, { i: 1, t: 2, p: 5, r: 9 }]);
  assert.deepEqual(series[2].points, [{ i: 1, t: 2, p: 4, r: undefined }]);
});

test("gapLimit is three typical steps, 20 minutes at least", () => {
  const every = (step, n) => Array.from({ length: n }, (_, i) => ({ t: i * step }));
  assert.equal(gapLimit([]), 20 * MINUTE_MS);
  assert.equal(gapLimit(every(MINUTE_MS, 10)), 20 * MINUTE_MS);
  assert.equal(gapLimit(every(10 * MINUTE_MS, 10)), 30 * MINUTE_MS);
  // The median step: one long outage does not stretch it.
  const withOutage = [...every(10 * MINUTE_MS, 6), { t: 50 * MINUTE_MS + 5 * HOUR_MS }];
  assert.equal(gapLimit(withOutage), 30 * MINUTE_MS);
});

test("runs cut where nothing was recorded", () => {
  const p = (t) => ({ t: t * MINUTE_MS, p: 10 });
  const out = runs([p(0), p(5), p(10), p(60), p(65)], 20 * MINUTE_MS);
  assert.deepEqual(out.map((r) => r.length), [3, 2]);
  assert.deepEqual(runs([], 1), []);
});

test("runs cut where the window reset, not where it only moved a little", () => {
  const at = (t, r) => ({ t: t * MINUTE_MS, r: r * MINUTE_MS, p: 50 });
  // The first window reset at minute 300; the next sample has a later one.
  const reset = runs([at(290, 300), at(295, 300), at(305, 600)], HOUR_MS);
  assert.deepEqual(reset.map((r) => r.length), [2, 1]);
  // A reset time that moved by under a minute is the same window.
  assert.equal(runs([at(0, 300), { t: 5 * MINUTE_MS, r: 300 * MINUTE_MS + 30e3, p: 50 }], HOUR_MS).length, 1);
  // A later reset time while the old one has not passed yet: no cut.
  assert.equal(runs([at(0, 300), at(5, 600)], HOUR_MS).length, 1);
});

test("timeTicks fall on round local times inside the span", () => {
  const t1 = new Date(2026, 9, 14, 15, 37).getTime();
  const span = (range) => ({ t0: t1 - RANGE_HOURS[range] * HOUR_MS, t1 });
  const check = (range, round) => {
    const s = span(range);
    const ticks = timeTicks(range, s);
    assert.ok(ticks.length >= 2 && ticks.length <= 13, `${range}: ${ticks.length} ticks`);
    for (const [i, tick] of ticks.entries()) {
      assert.ok(tick.t >= s.t0 && tick.t <= s.t1, `${range}: tick inside the span`);
      assert.ok(round(new Date(tick.t)), `${range}: ${new Date(tick.t)} is not round`);
      assert.equal(typeof tick.label, "string");
      if (i) assert.ok(tick.t > ticks[i - 1].t);
    }
    return ticks;
  };
  check("1h", (d) => d.getMinutes() % 15 === 0 && d.getSeconds() === 0);
  check("6h", (d) => d.getHours() % 2 === 0 && d.getMinutes() === 0);
  const day = check("1d", (d) => d.getHours() % 6 === 0 && d.getMinutes() === 0);
  // Midnight is labelled with the weekday.
  const midnight = day.find((t) => new Date(t.t).getHours() === 0);
  assert.equal(midnight.label, "Wed");
  check("7d", (d) => d.getHours() === 0 && d.getMinutes() === 0);
  const month = check("30d", (d) => d.getDay() === 1 && d.getHours() === 0);
  assert.equal(month.length, 4);
  assert.match(month[0].label, /^[A-Z][a-z]{2} \d{1,2}$/);
});

test("noHistoryText names the refresh interval in force, never a fixed one", () => {
  assert.equal(
    noHistoryText(0, 15),
    "No history yet. One sample is stored per usage refresh (every 15 min), so this range fills in by itself."
  );
  assert.match(noHistoryText(0, 2), /\(every 2 min\)/);
  // Settings not loaded yet: no interval at all rather than a made-up one.
  assert.equal(
    noHistoryText(0, null),
    "No history yet. One sample is stored per usage refresh, so this range fills in by itself."
  );
  assert.doesNotMatch(noHistoryText(0, undefined), /every/);
  assert.equal(noHistoryText(1, 5), "Only one sample so far. The line appears with the second.");
});
