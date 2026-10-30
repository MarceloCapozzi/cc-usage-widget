// Activity calendar.
//
// report.activity (app/src/activity.rs): each machine's tokens per local day
// and its busiest hour. One cell per day, weeks as columns from Sunday,
// six months, GitHub's greens; the level is the day's share of the busiest
// day shown, on a square root so a light day still shows. The machine
// filter of the project list applies here too. The sums are in
// report-data.js.

import { $, create, svgEl } from "./dom.js";
import { compact, dayKey, hourName, plural } from "./format.js";
import { activityDays, activityLevel } from "./report-data.js";
import { state } from "./state.js";
import { cssVar } from "./theme.js";

export const elActivity = $("activity");
const ACTIVITY_WEEKS = 26;
// GitHub's contribution greens, from the tokens (light or dark).
const activityLevels = () => [0, 1, 2, 3, 4].map((i) => cssVar(`--act-${i}`));

// The grid follows the view's width (a resized window).
let activityWidth = 0;

export function drawActivity(activity) {
  state.lastActivity = activity;
  elActivity.replaceChildren();
  const width = elActivity.clientWidth;
  if (!width) return; // hidden: drawn again when the view opens
  activityWidth = width;
  const days = activityDays(activity, state.projectHost);

  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const firstSunday = new Date(today);
  firstSunday.setDate(today.getDate() - today.getDay() - (ACTIVITY_WEEKS - 1) * 7);
  let total = 0, active = 0, max = 0;
  for (let i = 0; i < ACTIVITY_WEEKS * 7; i++) {
    const d = new Date(firstSunday);
    d.setDate(firstSunday.getDate() + i);
    if (d > today) break;
    const v = days.get(dayKey(d))?.t ?? 0;
    total += v;
    if (v) active++;
    max = Math.max(max, v);
  }

  const head = create("div", "activity-head");
  head.append(
    create("span", "label", `${compact(total)} tokens in the last 6 months`),
    create("span", "activity-days", plural(active, "active day", "active days"))
  );
  elActivity.append(head);

  // The grid spans the width exactly: n cells and n - 1 gaps, after the
  // weekday labels, so its right edge is the view's.
  const LBL = 28, TOP = 12, GAP = 2.5;
  const step = (width - LBL + GAP) / ACTIVITY_WEEKS, cell = step - GAP;
  const height = TOP + 7 * step - GAP;
  const grid = svgEl("svg", { class: "activity-grid", viewBox: `0 0 ${width} ${height}`, width, height });
  for (const [row, name] of [[1, "Mon"], [3, "Wed"], [5, "Fri"]]) {
    const t = svgEl("text", { class: "axis-text", x: 0, y: (TOP + row * step + cell - 2).toFixed(1) });
    t.textContent = name;
    grid.append(t);
  }
  const ACTIVITY_LEVELS = activityLevels();
  let lastMonth = -1;
  for (let w = 0; w < ACTIVITY_WEEKS; w++) {
    const col = new Date(firstSunday);
    col.setDate(firstSunday.getDate() + w * 7);
    // A month's name over the first week that ends in it.
    const end = new Date(col);
    end.setDate(col.getDate() + 6);
    if (end.getMonth() !== lastMonth && end.getDate() <= 7) {
      lastMonth = end.getMonth();
      const t = svgEl("text", { class: "axis-text", x: (LBL + w * step).toFixed(1), y: 8 });
      t.textContent = end.toLocaleDateString("en-US", { month: "short" });
      grid.append(t);
    }
    for (let r = 0; r < 7; r++) {
      const d = new Date(col);
      d.setDate(col.getDate() + r);
      if (d > today) break;
      const key = dayKey(d);
      const v = days.get(key)?.t ?? 0;
      grid.append(svgEl("rect", {
        class: "activity-cell", "data-day": key,
        x: (LBL + w * step).toFixed(2), y: (TOP + r * step).toFixed(2),
        width: cell.toFixed(2), height: cell.toFixed(2), rx: 2,
        fill: ACTIVITY_LEVELS[activityLevel(v, max)],
      }));
    }
  }
  const wrap = create("div", "activity-wrap");
  const tip = create("div", "activity-tip");
  tip.hidden = true;
  wrap.append(grid, tip);
  grid.addEventListener("pointermove", (ev) => {
    const key = ev.target?.dataset?.day;
    if (!key) { tip.hidden = true; return; }
    const [y, m, dd] = key.split("-").map(Number);
    const date = new Date(y, m - 1, dd);
    const d = days.get(key);
    tip.textContent = `${date.toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric" })} · ` +
      (d ? `${compact(d.t)} tokens · peak ${hourName(d.h)}` : "no activity");
    tip.hidden = false;
    const box = wrap.getBoundingClientRect(), r = ev.target.getBoundingClientRect();
    const cx = r.left - box.left + r.width / 2;
    tip.style.left = tip.style.right = "";
    if (cx > box.width / 2) tip.style.right = `${Math.max(0, box.width - cx - 12)}px`;
    else tip.style.left = `${Math.max(0, cx - 12)}px`;
    tip.style.top = `${r.top - box.top - tip.offsetHeight - 4}px`;
  });
  grid.addEventListener("pointerleave", () => { tip.hidden = true; });
  elActivity.append(wrap);

  const foot = create("div", "activity-foot");
  const who = create("span");
  if (state.projectHost) who.append(create("span", "tag remote machine", state.projectHost));
  else who.textContent = "All machines";
  const scale = create("span", "activity-scale");
  scale.append(document.createTextNode("Less"));
  for (const c of ACTIVITY_LEVELS) {
    const i = create("i");
    i.style.background = c;
    scale.append(i);
  }
  scale.append(document.createTextNode("More"));
  foot.append(who, scale);
  elActivity.append(foot);
}

/** Redraws the grid when the view's width changes. */
export function initActivityResize() {
  new ResizeObserver(() => {
    const w = elActivity.clientWidth;
    if (w && w !== activityWidth && state.lastActivity) {
      activityWidth = w;
      drawActivity(state.lastActivity);
    }
  }).observe(elActivity);
}
