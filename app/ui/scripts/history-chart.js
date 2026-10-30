// History.
//
// One point per stored sample. Gaps are neither interpolated nor filled: a gap
// means "that refresh failed", and covering it would invent data.
//
// Series colours: the mockup's accent blue and work amber. Each line also
// carries its name at the tip, so the legend is not the only way to tell them
// apart. The arithmetic (series, gaps, runs, ticks) is in history-math.js.

import { $, create, svgEl } from "./dom.js";
import { abbreviate, buildSeries, gapLimit, HOUR_MS, noHistoryText, RANGE_HOURS, runs, timeTicks } from "./history-math.js";
import { state } from "./state.js";
import { invoke } from "./tauri.js";
import { cssVar } from "./theme.js";

// Series colours by limit: the 5-hour session blue, the overall weekly
// amber, the per-model weeklies green and on (tokens, so they follow the theme).
const seriesColors = () => [cssVar("--accent"), cssVar("--work"), cssVar("--done"), cssVar("--series-4"), cssVar("--need")];

const svg = $("svg");
const W = 324, H = 130;
// Percentage axis on the left, direct labels with the current value
// ("5h 42%") on the right. With both on the same side they overlap.
const M = { top: 8, right: 44, bottom: 16, left: 26 };
const areaX = W - M.left - M.right;
const areaY = H - M.top - M.bottom;

let currentSeries = null;
// The time span on the x axis: the range, ending now.
let xSpan = { t0: 0, t1: 1 };

const gx = (t) => M.left + ((t - xSpan.t0) / (xSpan.t1 - xSpan.t0)) * areaX;
const gy = (v) => M.top + areaY - (Math.max(0, Math.min(100, v)) / 100) * areaY;

function seriesColor(name, k) {
  const colors = seriesColors();
  if (name.startsWith("5-hour")) return colors[0];
  if (name.startsWith("All models")) return colors[1];
  return colors[Math.min(colors.length - 1, 2 + k)];
}

const linePath = (run) => run.map((p, j) => `${j ? "L" : "M"}${gx(p.t).toFixed(1)},${gy(p.p).toFixed(1)}`).join(" ");
const areaPath = (run) =>
  `${linePath(run)} L${gx(run[run.length - 1].t).toFixed(1)},${gy(0).toFixed(1)} L${gx(run[0].t).toFixed(1)},${gy(0).toFixed(1)} Z`;

// Each request is numbered, as in projects.js: range clicks and every usage
// event while the chart shows can overlap, and an older answer arriving last
// would draw its samples on the newer range's axis. Only the latest draws.
let historyRequest = 0;

export async function loadHistory(range) {
  state.currentRange = range;
  const request = ++historyRequest;
  let samples = [];
  try {
    samples = JSON.parse(await invoke("history", { range }));
  } catch (err) {
    console.error("history", err);
  }
  if (request === historyRequest) drawHistory(samples, range);
}

function drawHistory(samples, range) {
  svg.replaceChildren();
  $("legend").replaceChildren();
  currentSeries = null;

  // With fewer than two points there is no line to draw. Say why, instead of
  // showing an empty box that looks like an error.
  if (samples.length < 2) {
    const notice = create("div", "no-history", noHistoryText(samples.length, state.savedSettings?.refreshMins ?? null));
    $("legend").append(notice);
    return;
  }

  xSpan = { t1: Date.now(), t0: Date.now() - (RANGE_HOURS[range] ?? 6) * HOUR_MS };
  const series = buildSeries(samples, seriesColor);
  const limit = gapLimit(samples);
  currentSeries = { series, samples, limit };

  for (const v of [0, 50, 100]) {
    const y = gy(v);
    svg.append(svgEl("line", {
      class: "grid-line" + (v === 50 ? " dashed" : ""),
      x1: M.left, x2: M.left + areaX, y1: y.toFixed(1), y2: y.toFixed(1),
    }));
    const t = svgEl("text", { class: "axis-text", x: M.left - 5, y: (y + 3).toFixed(1), "text-anchor": "end" });
    t.textContent = v + "%";
    svg.append(t);
  }

  // Round local times along the bottom, with a faint guide line each.
  for (const tick of timeTicks(range, xSpan)) {
    const x = gx(tick.t);
    svg.append(svgEl("line", { class: "grid-line tick", x1: x.toFixed(1), x2: x.toFixed(1), y1: M.top, y2: M.top + areaY }));
    const t = svgEl("text", { class: "axis-text", x: x.toFixed(1), y: H - 3, "text-anchor": "middle" });
    t.textContent = tick.label;
    svg.append(t);
  }

  for (const s of series) {
    for (const run of runs(s.points, limit)) {
      // The 5-hour session as a light area: its windows read as blocks.
      if (s.name.startsWith("5-hour")) svg.append(svgEl("path", { class: "series-area", d: areaPath(run), fill: s.color }));
      svg.append(svgEl("path", { class: "series", d: linePath(run), stroke: s.color }));
    }
  }

  // Direct labels with the current value, spread apart if they overlap.
  const ends = series
    .map((s) => ({ s, p: s.points[s.points.length - 1].p }))
    .map((e) => ({ ...e, y: gy(e.p) }))
    .sort((a, b) => a.y - b.y);
  for (let k = 1; k < ends.length; k++) {
    if (ends[k].y - ends[k - 1].y < 10) ends[k].y = ends[k - 1].y + 10;
  }
  for (const e of ends) {
    const t = svgEl("text", {
      class: "direct-label", x: M.left + areaX + 4,
      y: (e.y + 3).toFixed(1), fill: e.s.color,
    });
    t.textContent = `${abbreviate(e.s.name)} ${Math.round(e.p)}%`;
    svg.append(t);
  }

  svg.append(svgEl("line", { class: "cursor", id: "cursor", y1: M.top, y2: M.top + areaY }));
  for (const s of series) {
    svg.append(svgEl("circle", {
      class: "hover-dot", r: 3.5, fill: s.color, stroke: "var(--panel)", "stroke-width": 2,
    }));
  }
  const capture = svgEl("rect", {
    x: M.left, y: M.top, width: areaX, height: areaY, fill: "transparent",
  });
  capture.addEventListener("pointermove", moveCursor);
  capture.addEventListener("pointerleave", hideCursor);
  svg.append(capture);

  for (const s of series) {
    const item = create("span");
    const dot = create("i", "legend-dot");
    dot.style.background = s.color;
    item.append(dot, document.createTextNode(s.name));
    $("legend").append(item);
  }
}

function moveCursor(ev) {
  if (!currentSeries) return;
  const { series, samples, limit } = currentSeries;
  const box = svg.getBoundingClientRect();
  const x = (ev.clientX - box.left) * (W / box.width);
  const frac = Math.max(0, Math.min(1, (x - M.left) / areaX));
  const t = xSpan.t0 + frac * (xSpan.t1 - xSpan.t0);
  // The sample nearest in time; none where nothing was recorded.
  let i = 0;
  for (let j = 1; j < samples.length; j++) {
    if (Math.abs(samples[j].t - t) < Math.abs(samples[i].t - t)) i = j;
  }
  if (Math.abs(samples[i].t - t) > limit) { hideCursor(); return; }
  const cx = gx(samples[i].t);

  const cursor = svg.querySelector("#cursor");
  cursor.setAttribute("x1", cx.toFixed(1));
  cursor.setAttribute("x2", cx.toFixed(1));
  cursor.style.opacity = 1;

  const dots = svg.querySelectorAll(".hover-dot");
  const tt = $("tooltip");
  tt.replaceChildren(
    create("div", null, new Date(samples[i].t).toLocaleString([], {
      weekday: "short", hour: "numeric", minute: "2-digit",
    }))
  );
  series.forEach((s, k) => {
    const p = s.points.find((q) => q.i === i);
    if (!p) { dots[k].style.opacity = 0; return; }
    dots[k].setAttribute("cx", cx.toFixed(1));
    dots[k].setAttribute("cy", gy(p.p).toFixed(1));
    dots[k].style.opacity = 1;
    const row = create("div", "tt-row");
    const dot = create("i", "legend-dot");
    dot.style.background = s.color;
    row.append(dot, document.createTextNode(`${s.name} `), create("b", null, `${Math.round(p.p)}%`));
    tt.append(row);
  });

  tt.style.left = `${(cx / W) * 100}%`;
  tt.style.top = `${(gy(series[0].points[0]?.p ?? 50) / H) * 100}%`;
  tt.style.opacity = 1;
}

function hideCursor() {
  $("tooltip").style.opacity = 0;
  const c = svg.querySelector("#cursor");
  if (c) c.style.opacity = 0;
  svg.querySelectorAll(".hover-dot").forEach((p) => (p.style.opacity = 0));
}

/** The range buttons over the chart (1h … 30d). */
export function initRanges() {
  document.querySelectorAll(".ranges button").forEach((b) => {
    b.addEventListener("click", () => {
      document.querySelectorAll(".ranges button").forEach((o) => o.removeAttribute("aria-pressed"));
      b.setAttribute("aria-pressed", "true");
      loadHistory(b.dataset.range);
    });
  });
}
