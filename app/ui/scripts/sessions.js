// Sessions.
//
// One row per Claude Code session (SessionView: camelCase, times in Unix
// seconds): local ones as app/src/sessions.rs and the cc-core state machine
// build them (tagged with this machine's name), remote ones from the NATS
// bucket (app/src/remote/, tagged with the publisher's alias; the first
// few subagents travel as short lines, the rest as placeholders made from
// their counts), both as `origin.alias`, merged by the backend in panel
// order (needs you, failed, working, done, idle, offline). The backend
// emits `sessions` with the full list whenever it changes; elapsed times
// are counted here, once a second. The row's words (state labels, line 2,
// the agents tag and lines, times) are in session-text.js.
//
// Row anatomy, from the mockup (the machine tag has its own line, under the
// meter, so a long name never squeezes the title):
//   ┃ name                                      needs you
//   ┃ what it is doing now                            12s
//   ┃ ▪▪▪▫▫▫▫▫ 42% · model [1/2 agents]
//   ┃ [machine]
//     ┆ ● explore · Find the row code · Grep · Capa…    3s
//     ┆ ● plan · finished                           1m02s

import { $, create } from "./dom.js";
import { compact, elapsed, plural } from "./format.js";
import {
  agentLine,
  agentsTag,
  colorOf,
  focusHintText,
  formatSince,
  isRemote,
  STATE_CLASS,
  STATE_LABEL,
  stateTime,
  subLine,
} from "./session-text.js";
import { invoke, listen } from "./tauri.js";
import { fitSoon } from "./window.js";

const elSessionList = $("session-list");

const METER_CELLS = 8;
const MAX_AGENT_LINES = 4;

let currentSessions = null;
let sessionsHovered = false;
let pendingSessions = null;
let focusNote = null;
// Focus previews per session id, so hovering a row twice does not probe twice.
const focusHints = new Map();

/** The mockup's eight-cell context meter; empty when the percentage is unknown. */
function segMeter(percent) {
  const seg = create("span", "seg");
  seg.setAttribute("aria-hidden", "true");
  const known = typeof percent === "number";
  const on = known ? Math.round((Math.max(0, Math.min(100, percent)) / 100) * METER_CELLS) : 0;
  for (let i = 0; i < METER_CELLS; i++) seg.append(create("i", i < on ? "on" : null));
  return seg;
}

/** Line 3: context meter and model, then the subagent tag; or the no-hooks hint. */
function meterLine(s) {
  const parts = [];
  const hasPct = typeof s.contextPercent === "number";
  if (hasPct) parts.push(`${s.contextPercent}%`);
  // The transcript gives the context size but not the window, so without the
  // percentage the row shows the size and leaves the cells empty instead of
  // inventing a fill.
  else if (typeof s.contextTokens === "number") parts.push(compact(s.contextTokens));
  if (s.model) parts.push(s.model);
  const agents = agentsTag(s.subagents);

  if (!parts.length && !agents) {
    // Without hooks the row only knows busy/idle. Say why, briefly; the
    // tooltip has the full explanation.
    return s.hasHooks || isRemote(s) ? null : create("div", "meter hint", "No detail yet: restart the session");
  }
  const line = create("div", "meter");
  if (hasPct) line.setAttribute("aria-label", `Context ${s.contextPercent}%`);
  line.append(segMeter(s.contextPercent));
  if (parts.length) line.append(create("span", "text", parts.join(" · ")));
  if (agents) {
    const tag = create("span", agents.allDone ? "tag agents-tag all-done" : "tag agents-tag", agents.text);
    tag.title = agents.title;
    line.append(tag);
  }
  return line;
}

/** Line 4: the machine tag on a line of its own, or null without a name. */
function hostLine(s) {
  const tag = machineTag(s);
  if (!tag) return null;
  const line = create("div", "host");
  line.append(tag);
  return line;
}

/**
 * The machine tag, under the meter line: this machine's name on local rows (from
 * the settings, `origin.alias`, see remote::name_local_rows), the publisher's
 * alias on remote ones, both in the same blue tag. Long names are cut with
 * an ellipsis (CSS); the tag's tooltip has the whole name.
 */
function machineTag(s) {
  const name = s.origin?.alias;
  if (!name) return null;
  const tag = create("span", "tag remote machine", name);
  tag.title = isRemote(s) ? `Remote machine: ${name}` : `This machine: ${name}`;
  return tag;
}

function rowTooltip(s) {
  // The title is cut with an ellipsis in the row, so the tooltip starts with it.
  const lines = [s.displayName];
  if (isRemote(s)) {
    lines.push(`Remote session on ${s.origin.alias}`);
    if (s.cwd) lines.push(s.cwd);
    lines.push("", "Remote rows cannot be focused from here");
    return lines.join("\n");
  }
  if (s.cwd) lines.push(s.cwd);
  if (s.entrypoint) {
    lines.push(s.entrypoint === "claude-vscode" ? "In VS Code" : `Entry point: ${s.entrypoint}`);
  }
  lines.push(
    s.hasHooks
      ? "Hooks: reporting"
      : "Hooks: no events yet. Only busy/idle is known; a session opened before the hooks were installed shows its detail once restarted."
  );
  if (s.stalled) lines.push("No hook event for over 10 minutes: it may be stuck");
  if (s.contextEstimated) {
    lines.push("Context estimated from the transcript and the model's window (no status line in this session)");
  }
  const sub = subLine(s);
  if (sub && sub.length > 40) lines.push("", sub);
  const hint = focusHints.get(s.id);
  if (hint) lines.push("", hint);
  else if (s.pid) lines.push("", "Click: bring its window to the front");
  return lines.join("\n");
}

function agentsBlock(agents) {
  const box = create("div", "agents");
  // Running ones first, then finished, each in start order; at most a few
  // lines so one busy session does not push the others out of view.
  const ordered = [...agents].sort((a, b) =>
    a.state === b.state ? a.startedAt - b.startedAt : a.state === "working" ? -1 : 1
  );
  for (const a of ordered.slice(0, MAX_AGENT_LINES)) {
    const done = a.state === "done";
    const time = create("span");
    if (!done) {
      time.dataset.since = String(a.startedAt);
      time.dataset.fmt = "elapsed";
    } else if (a.finishedAt) {
      time.textContent = elapsed(a.finishedAt - a.startedAt);
    }
    const line = create("div");
    // One line, cut with an ellipsis (widget.css). A cut one gets its own
    // tooltip with all of it; otherwise the row's tooltip shows.
    const what = create("span", done ? "dot ok" : "dot", agentLine(a));
    what.addEventListener("mouseenter", () => {
      if (what.scrollWidth > what.clientWidth) what.title = what.textContent;
      else what.removeAttribute("title");
    });
    line.append(what, time);
    box.append(line);
  }
  if (ordered.length > MAX_AGENT_LINES) {
    const more = create("div", "more");
    more.append(create("span", null, `+${ordered.length - MAX_AGENT_LINES} more`));
    box.append(more);
  }
  return box;
}

function sessionRow(s) {
  const row = create("div", `sess state-${s.state}`);
  if (s.stalled) row.classList.add("stalled");

  // The left bar shows the session's state at a glance (the user asked for
  // it, reading the mockup's bars as state). Idle and offline rows have none;
  // for them, a colour set with /color still shows.
  const bar = create("div", "bar");
  const STATE_BAR = { needsYou: "var(--need)", failed: "var(--fail)", working: "var(--work)", done: "var(--done)" };
  const color = STATE_BAR[s.state] ?? colorOf(s.color);
  if (color) bar.style.background = color;

  const main = create("div", "main");
  const name = create("div", "name");
  name.append(create("span", "title", s.displayName));
  main.append(name, create("div", "sub", subLine(s)));
  const meter = meterLine(s);
  if (meter) main.append(meter);
  const host = hostLine(s);
  if (host) main.append(host);

  const state = create("div", `state ${STATE_CLASS[s.state] ?? "idle"}`);
  const [since, fmt] = stateTime(s);
  const time = create("span");
  time.dataset.since = String(since);
  time.dataset.fmt = fmt;
  state.append(create("b", null, STATE_LABEL[s.state] ?? s.state), time);

  row.append(bar, main, state);
  if (s.subagents?.length) row.append(agentsBlock(s.subagents));
  row.title = rowTooltip(s);

  if (s.pid && !isRemote(s)) {
    row.classList.add("clickable");
    row.tabIndex = 0;
    row.setAttribute("role", "button");
    row.addEventListener("click", () => focusSession(s));
    row.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        focusSession(s);
      }
    });
    row.addEventListener("mouseenter", () => loadFocusHint(s, row));
  }
  return row;
}

function renderSessions(list) {
  if (sessionsHovered) {
    // The list freezes under the pointer, so a row does not jump away right
    // before a click. The newest list is drawn when the pointer leaves.
    pendingSessions = list;
    return;
  }
  pendingSessions = null;
  const ids = new Set(list.map((s) => s.id));
  for (const id of [...focusHints.keys()]) if (!ids.has(id)) focusHints.delete(id);

  // Rows the "Hide idle sessions after" setting hides arrive marked
  // `hidden`; they are only counted, in the header.
  const shown = list.filter((s) => !s.hidden);
  elSessionList.replaceChildren();
  if (!shown.length) {
    const text = list.length
      ? "Every session is idle and hidden (⚙ → Hide idle sessions after)"
      : "No active Claude Code sessions";
    elSessionList.append(create("div", "sessions-empty", text));
  } else {
    for (const s of shown) elSessionList.append(sessionRow(s));
  }
  renderHeader(list);
  tickElapsed();
  fitSoon();
}

/**
 * "5 sessions · 1 needs you · 2 hidden", counting what is shown, or a short
 * note after a failed click.
 */
function renderHeader(list) {
  const summary = $("head-summary");
  summary.classList.toggle("note", focusNote !== null);
  if (focusNote) {
    summary.textContent = focusNote;
    return;
  }
  if (!Array.isArray(list)) {
    summary.textContent = "Sessions";
    return;
  }
  const shown = list.filter((s) => !s.hidden);
  const hidden = list.length - shown.length;
  const n = shown.length;
  const waiting = shown.filter((s) => s.state === "needsYou").length;
  summary.textContent =
    (n === 0 ? "No sessions" : plural(n, "session", "sessions")) +
    (waiting ? ` · ${waiting} ${waiting === 1 ? "needs" : "need"} you` : "");
  if (hidden) {
    const more = create("span", "hidden-count", ` · ${hidden} hidden`);
    more.title =
      `${plural(hidden, "idle session is", "idle sessions are")} not shown: idle for longer ` +
      "than the delay in ⚙ → Hide idle sessions after. Each one shows again as soon as it changes state.";
    summary.append(more);
  }
}

function tickElapsed() {
  for (const el of elSessionList.querySelectorAll("[data-since]")) {
    el.textContent = formatSince(Number(el.dataset.since), el.dataset.fmt);
  }
}

async function loadFocusHint(s, row) {
  if (focusHints.has(s.id)) return;
  focusHints.set(s.id, null);
  try {
    const hint = focusHintText(await invoke("focus_preview", { sessionId: s.id }));
    if (hint) {
      focusHints.set(s.id, hint);
      row.title = rowTooltip(s);
    }
  } catch (err) {
    console.error("focus_preview", err);
  }
}

async function focusSession(s) {
  try {
    const report = await invoke("focus_session", { sessionId: s.id });
    if (!report?.outcome || report.outcome === "notFound" || report.outcome === "unsupported") {
      flashSessionNote("That session's window was not found");
    }
  } catch (err) {
    console.error("focus_session", err);
    flashSessionNote("Could not focus the session");
  }
}

/** A short message in the header, for three seconds. */
function flashSessionNote(text) {
  focusNote = text;
  renderHeader(currentSessions);
  setTimeout(() => {
    focusNote = null;
    renderHeader(currentSessions);
  }, 3000);
}

/** Draws a `sessions` answer or event. */
export function showSessions(list) {
  // null: the backend has not finished its first pass yet; keep "Looking…".
  if (!Array.isArray(list)) return;
  currentSessions = list;
  renderSessions(currentSessions);
}

function releaseSessionsFreeze() {
  sessionsHovered = false;
  if (pendingSessions) renderSessions(pendingSessions);
}

/** The list's hover freeze, the `sessions` event, the one-second clock and the first load. */
export function initSessions() {
  elSessionList.addEventListener("mouseenter", () => (sessionsHovered = true));
  elSessionList.addEventListener("mouseleave", releaseSessionsFreeze);
  // The flyout can hide with the pointer still over the list (a row click hands
  // focus to another window), and then `mouseleave` never fires. Losing focus
  // releases the freeze so the list is current when the flyout opens again.
  window.addEventListener("blur", releaseSessionsFreeze);

  listen("sessions", (e) => showSessions(e.payload));
  setInterval(() => {
    if (!document.hidden) tickElapsed();
  }, 1000);
  invoke("sessions").then(showSessions, (err) => console.error("sessions", err));
}
