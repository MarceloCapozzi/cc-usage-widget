// Usage-per-project report.
//
// This is what a browser extension cannot show: it comes from the transcripts
// on disk, not from the API. It lives in the usage view, under the chart.
// Each project carries the machine it ran on (this machine's name, or the
// alias of the publisher that sent it), in the same blue tag as the rows.

import { drawActivity, elActivity } from "./activity.js";
import { $, create } from "./dom.js";
import { compact, longDate, plural, timeAgo } from "./format.js";
import { missingPriceText, tokensOf } from "./report-data.js";
import { state } from "./state.js";
import { invoke } from "./tauri.js";

const elProjects = $("projects");

// Each load of the usage view is numbered. A scan can take seconds, and
// opening the view twice runs two: whichever ends last would paint, so a
// slow earlier one could cover the newer result. Only the latest one draws.
let projectsRequest = 0;

export async function loadProjects() {
  const request = ++projectsRequest;
  elProjects.replaceChildren(create("p", "empty", "Reading transcripts…"));
  if (!state.lastActivity) elActivity.replaceChildren(create("p", "empty", "Reading transcripts…"));
  try {
    // 30 days: a window wide enough to see a trend without walking the whole
    // history, which on old accounts is hundreds of MB.
    const report = JSON.parse(await invoke("usage_by_project", { days: 30 }));
    if (request === projectsRequest) drawProjects(report);
  } catch (err) {
    if (request !== projectsRequest) return;
    elProjects.replaceChildren(create("p", "notice error", `Could not read the transcripts: ${err}`));
    // A calendar already drawn stays; a first load must not keep saying it
    // is reading.
    if (!state.lastActivity) elActivity.replaceChildren(create("p", "empty", "Activity unavailable"));
  }
}

/** Narrows the list and the calendar to one machine (`state.projectHost`), or null for all. */
function setProjectHost(host) {
  state.projectHost = host;
  if (state.lastReport) drawProjects(state.lastReport);
}

/** A project's machine tag: a toggle when there is more than one machine. */
function hostTag(p, filterable) {
  const tag = create(filterable ? "button" : "span", "tag remote machine", p.host);
  const where = p.remote
    ? `Remote machine: ${p.host}${p.last_seen ? `, last active ${timeAgo(p.last_seen)}` : ""}`
    : `This machine: ${p.host}`;
  if (!filterable) {
    tag.title = where;
    return tag;
  }
  const on = state.projectHost === p.host;
  tag.type = "button";
  tag.classList.add("host-filter");
  tag.dataset.host = p.host;
  tag.setAttribute("aria-pressed", String(on));
  tag.title = `${where}\n${on ? "Click: show every machine" : "Click: show only this machine's projects"}`;
  tag.addEventListener("click", () => setProjectHost(on ? null : p.host));
  return tag;
}

function drawProjects(report) {
  state.lastReport = report;
  elProjects.replaceChildren();
  drawActivity(report.activity);

  if (!report.projects.length) {
    elProjects.append(create("p", "empty", "No activity in the last 30 days"));
    return;
  }

  const hosts = new Set(report.projects.map((p) => p.host).filter(Boolean));
  const filterable = hosts.size > 1;
  // A machine that is no longer listed cannot stay selected.
  if (!filterable || !hosts.has(state.projectHost)) state.projectHost = null;
  const shown = state.projectHost ? report.projects.filter((p) => p.host === state.projectHost) : report.projects;
  const total = shown.reduce((sum, p) => sum + tokensOf(p.tokens), 0);

  const head = create("div", "row");
  const label = create("span", "projects-label");
  label.append(create("span", "label", "Last 30 days"));
  if (state.projectHost) {
    // What the list is narrowed to, and the way back.
    const chip = create("button", "tag remote machine filter-chip");
    chip.type = "button";
    chip.title = "Show every machine";
    chip.append(create("span", null, state.projectHost), create("span", "chip-x", "✕"));
    chip.addEventListener("click", () => setProjectHost(null));
    label.append(chip);
  }
  head.append(label, create("span", "pct", `${compact(total)} tokens`));
  elProjects.append(head);

  for (const p of shown) {
    const t = tokensOf(p.tokens);
    const item = create("div", "project");
    const top = create("div", "row");
    top.append(
      create("span", "project-name", p.name),
      // An unpriced cost is "n/a", never 0: we do not know, it is not free.
      create("span", "cost", p.cost_usd == null ? "n/a" : `$${p.cost_usd.toFixed(2)}`)
    );
    const bar = create("div", "bar-track");
    const fill = create("div", "fill");
    fill.style.width = `${total ? (t / total) * 100 : 0}%`;
    bar.append(fill);
    const meta = create("div", "meta");
    // The machine on the left; "1 session · 1.4B tokens" on the right.
    const where = create("span", "project-host");
    if (p.host) where.append(hostTag(p, filterable));
    meta.append(
      where,
      create("span", "project-used", `${plural(p.sessions, "session", "sessions")} · ${compact(t)} tokens`)
    );
    item.append(top, bar, meta);
    elProjects.append(item);
  }

  // Costs come from built-in API list prices: say what they are, and that a
  // subscription does not pay them per token.
  // One sentence per line: wrapped mid-sentence it read as one block.
  const note = create("p", "cost-note");
  note.append(
    create("span", null, `Costs are estimates at API list prices (${longDate(report.prices_as_of)}).`),
    create("span", null, "Your plan is not billed per token.")
  );
  elProjects.append(note);
  // Explain a missing cost instead of leaving "n/a" unexplained.
  const missing = missingPriceText(report.models_without_pricing, report.pricing_path);
  if (missing) elProjects.append(create("p", "notice", missing));
}
