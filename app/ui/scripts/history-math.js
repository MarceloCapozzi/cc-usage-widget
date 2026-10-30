// The history chart's arithmetic: series, gaps, runs and time ticks
// (history-chart.js draws them).
//
// Pure: nothing here touches the DOM or the backend, so the unit tests in
// app/ui-tests/ import it under Node. Samples are what the `history` command
// returns: `{t: ms, v: [{k: label, p: percent, r: resets-at ms}]}`.

export const MINUTE_MS = 60e3, HOUR_MS = 3600e3, DAY_MS = 24 * HOUR_MS;
export const RANGE_HOURS = { "1h": 1, "6h": 6, "1d": 24, "7d": 168, "30d": 720 };

/** Series order: the 5-hour session, the overall weekly, then the per-model ones. */
export const seriesRank = (name) => (name.startsWith("5-hour") ? 0 : name.startsWith("All models") ? 1 : 2);

/**
 * One series per limit: the 5-hour session, the overall weekly, then the
 * per-model ones. `colorFor(name, k)` gives a series its colour, `k`
 * counting the per-model ones from 0.
 */
export function buildSeries(samples, colorFor) {
  const byKey = new Map();
  samples.forEach((s, i) => {
    for (const l of s.v) {
      if (!byKey.has(l.k)) byKey.set(l.k, []);
      byKey.get(l.k).push({ i, t: s.t, p: l.p, r: l.r });
    }
  });
  let other = 0;
  return [...byKey.entries()]
    .sort((a, b) => seriesRank(a[0]) - seriesRank(b[0]) || a[0].localeCompare(b[0]))
    .map(([name, points]) => ({ name, color: colorFor(name, seriesRank(name) === 2 ? other++ : 0), points }));
}

/** Longest step between samples still drawn as a line: three typical steps, 20 minutes at least. */
export function gapLimit(samples) {
  const steps = [];
  for (let i = 1; i < samples.length; i++) steps.push(samples[i].t - samples[i - 1].t);
  steps.sort((a, b) => a - b);
  const typical = steps[Math.floor(steps.length / 2)] ?? 0;
  return Math.max(20 * MINUTE_MS, 3 * typical);
}

/** The runs of a series: cut where its window reset (joining two windows draws a false drop) and where nothing was recorded. */
export function runs(points, limit) {
  const out = [];
  let current = [];
  for (let j = 0; j < points.length; j++) {
    const p = points[j], prev = points[j - 1];
    const reset = prev && p.r && prev.r && p.r > prev.r + MINUTE_MS && prev.r <= p.t;
    const gap = prev && p.t - prev.t > limit;
    if ((reset || gap) && current.length) { out.push(current); current = []; }
    current.push(p);
  }
  if (current.length) out.push(current);
  return out;
}

/**
 * Time ticks for the range, on round local times: every 15 min, 2 h, 6 h,
 * day or week. `xSpan` is the time span on the x axis, `{t0, t1}` in ms.
 */
export function timeTicks(range, xSpan) {
  const out = [];
  const at = new Date(xSpan.t0);
  let step, label;
  if (range === "1h") {
    at.setSeconds(0, 0);
    at.setMinutes(Math.ceil(at.getMinutes() / 15) * 15);
    step = (d) => d.setMinutes(d.getMinutes() + 15);
    label = (d) => d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  } else if (range === "6h" || range === "1d") {
    const every = range === "6h" ? 2 : 6;
    at.setMinutes(0, 0, 0);
    at.setHours(Math.ceil((at.getHours() + (xSpan.t0 > at.getTime() ? 1 : 0)) / every) * every);
    step = (d) => d.setHours(d.getHours() + every);
    label = (d) => (d.getHours() === 0
      ? d.toLocaleDateString("en-US", { weekday: "short" })
      : d.toLocaleTimeString("en-US", { hour: "numeric" }));
  } else {
    at.setHours(24, 0, 0, 0); // the next midnight
    if (range === "30d") while (at.getDay() !== 1) at.setDate(at.getDate() + 1);
    const days = range === "30d" ? 7 : 1;
    step = (d) => d.setDate(d.getDate() + days);
    label = (d) => (range === "30d"
      ? d.toLocaleDateString("en-US", { month: "short", day: "numeric" })
      : d.toLocaleDateString("en-US", { weekday: "short" }));
  }
  for (let d = at; d.getTime() <= xSpan.t1; step(d)) {
    if (d.getTime() >= xSpan.t0) out.push({ t: d.getTime(), label: label(d) });
    if (out.length > 12) break;
  }
  return out;
}

/**
 * What the chart says instead of a line when it has fewer than two samples.
 * `refreshMins` is the usage refresh interval in force (settings), or null
 * while it is not known: the text then names no interval.
 */
export function noHistoryText(sampleCount, refreshMins) {
  if (sampleCount >= 1) return "Only one sample so far. The line appears with the second.";
  const every = Number.isFinite(refreshMins) && refreshMins > 0 ? ` (every ${refreshMins} min)` : "";
  return `No history yet. One sample is stored per usage refresh${every}, so this range fills in by itself.`;
}

// Series names are the limit labels built in app/src/model.rs ("5-hour
// session", "All models · weekly", "<model> · weekly").
export function abbreviate(name) {
  if (name.startsWith("5-hour")) return "5h";
  if (name.startsWith("All models")) return "7d";
  return name.split(" ")[0];
}
