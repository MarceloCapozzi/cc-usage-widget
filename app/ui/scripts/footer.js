// Usage: footer lines.
//
// One line per plan limit, as in the mockup: label on the left, what is LEFT
// on the right ("39% left · 2h24m"): the countdown to the reset, the same bare
// format on every line (the user's choice; the tooltip spells it out). Before
// it, a ten-cell meter of what is USED, like the sessions' context meter, in
// green, amber or red (usageLevel). When a
// refresh brings no data (token
// expired, network), the last good limits stay on screen, dimmed, with a note
// saying how old they are, instead of vanishing. Between refreshes a clock
// keeps the countdowns and those ages current; past a reset the line says
// "reset" until the next refresh brings the new window. A folder override
// the backend ignored (a relative CC_USAGE_DATA_DIR or CLAUDE_CONFIG_DIR,
// `path_problems` in the snapshot) is a line in the footer's tooltip and in
// the Remote line's, under its detail, like a NATS setting that was ignored.

import { withLines } from "./connection-text.js";
import { $, create } from "./dom.js";
import { meterCells, pct, resetPassed, severity, timeAgo, timeUntil, usageLevel } from "./format.js";
import { invoke, listen } from "./tauri.js";

let lastGood = null;

function footLine(label, right, sev) {
  const line = create("div");
  if (sev && sev !== "normal") line.dataset.sev = sev;
  line.append(create("span", null, label), right);
  return line;
}

function usageMeter(l) {
  const seg = create("span", `seg use-meter ${usageLevel(l)}`);
  seg.setAttribute("aria-hidden", "true");
  const lit = meterCells(l.percent);
  for (let i = 0; i < 10; i++) seg.append(create("i", i < lit ? "on" : null));
  return seg;
}

function limitLine(l) {
  const strong = create("strong");
  strong.append(usageMeter(l), create("span", "pct", `${Math.max(0, Math.round(100 - l.percent))}% left`));
  const left = timeUntil(l.resets_at);
  // Past the reset the figures are the old window's until the next refresh:
  // say so instead of dropping the countdown without a word.
  const passed = !left && resetPassed(l.resets_at);
  // Its own fixed-width box, like the percentage's, so the meters stay in
  // one column while the countdown goes from "4d05h" to "46m". Always drawn:
  // with no reset time (no window running, e.g. no message in the last five
  // hours) it holds a dash, so the line keeps the other lines' columns.
  strong.append(document.createTextNode(" · "), create("span", "reset", left ?? (passed ? "reset" : "–")));
  const line = footLine(l.label, strong, severity(l));
  line.title =
    `${l.label}: ${pct(l.percent)} used` +
    (left
      ? `, resets in ${left}`
      : passed
        ? ", reset since; the next refresh brings the new figures"
        : ", no window running; it starts with the next message") +
    (l.is_active ? "\nThis is the limit that applies now" : "");
  return line;
}

// The last snapshot drawn, for the clock below.
let lastSnapshot = null;

// What in the footer depends on the time: the countdowns and the "… ago"
// texts. The clock redraws only when one of them changed.
function clockKey(s) {
  const shown = s.usage ? s : lastGood;
  const resets = (shown?.usage?.limits ?? []).map((l) => timeUntil(l.resets_at) ?? resetPassed(l.resets_at));
  return JSON.stringify([resets, shown ? timeAgo(shown.at) : null, timeAgo(s.at)]);
}

let drawnKey = null;

export function renderUsage(s) {
  lastSnapshot = s;
  drawnKey = clockKey(s);
  if (s.usage) lastGood = s;
  const shown = s.usage ? s : lastGood;
  const foot = $("flyout-foot");
  foot.replaceChildren();
  foot.classList.toggle("stale", !s.usage && !!shown);

  const limits = shown?.usage?.limits ?? [];
  for (const l of limits) foot.append(limitLine(l));

  const spend = shown?.usage?.spend;
  if (spend?.enabled) {
    const used = `${spend.used.toFixed(2)} ${spend.currency}`;
    const text = spend.limit ? `${used} of ${spend.limit.toFixed(2)}` : used;
    foot.append(footLine("Credits", create("strong", null, text)));
  }

  if (!limits.length && !spend?.enabled) {
    foot.append(footLine("Plan usage", create("strong", null, "no data")));
  }

  // Errors and stale data, right under the lines they affect.
  if (s.token_expired) {
    const note = create("p", "foot-note error", "The token expired. Claude Code renews it on its next request; the data comes back on its own.");
    // How the last request to Claude Code to renew it went (app/src/poller.rs).
    if (s.renew_note) note.title = s.renew_note;
    foot.append(note);
  } else if (s.usage_error) {
    foot.append(create("p", "foot-note error", `No usage data: ${s.usage_error}`));
  }
  if (!s.usage && shown) {
    foot.append(create("p", "foot-note stale", `Stale data · updated ${timeAgo(shown.at)}`));
  }

  pathProblems = s.path_problems ?? [];
  foot.append(remoteLine());

  foot.title = withLines(`Updated ${timeAgo(s.at)}`, pathProblems);
}

/**
 * Keeps the countdowns and the "… ago" texts current between refreshes,
 * which are minutes apart: every 30 s while the page is visible, and at once
 * when it shows again.
 */
export function initFooterClock() {
  const tick = () => {
    if (document.hidden || !lastSnapshot) return;
    if (clockKey(lastSnapshot) !== drawnKey) renderUsage(lastSnapshot);
  };
  setInterval(tick, 30_000);
  document.addEventListener("visibilitychange", tick);
}

// The footer's "Remote" line, from the `remote_status` command and the
// `remote-status` event (app/src/remote/hub.rs, RemoteStatus): the backend writes
// the text ("2 machines · connected", "disconnected · retrying", ...), so
// this only draws it. Connected reads "[server] 2 machines · ● connected":
// the count in the text colour, the dot and the state in green. Any other
// state is the dot and the whole text in its colour (amber connecting, red
// disconnected; "not configured" is plain muted text, no dot). Until the
// first answer it reads "…".
let remoteStatus = null;

// The ignored folder overrides of the last snapshot, under the Remote
// line's detail.
let pathProblems = [];

const REMOTE_DOT = new Set(["connected", "connecting", "disconnected"]);

// A server rack, stroked like the header's icons.
const SERVER_ICON =
  '<svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" focusable="false" fill="none" ' +
  'stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">' +
  '<rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/>' +
  '<path d="M7 7.5h.01M7 16.5h.01"/></svg>';

function remoteLine() {
  const state = remoteStatus?.state ?? "unknown";
  const text = remoteStatus?.text ?? "…";
  const strong = create("strong");
  const cut = text.lastIndexOf(" · ");
  let stateText = text;
  if (state === "connected" && cut > 0) {
    const icon = create("span", "remote-icon");
    icon.innerHTML = SERVER_ICON;
    strong.append(icon, create("span", null, text.slice(0, cut)), create("span", "remote-sep", "·"));
    stateText = text.slice(cut + 3);
  }
  const status = create("span", "remote-state");
  if (REMOTE_DOT.has(state)) {
    const dot = create("i", "remote-dot");
    dot.setAttribute("aria-hidden", "true");
    status.append(dot);
  }
  status.append(document.createTextNode(stateText));
  strong.append(status);
  const line = footLine("Remote", strong);
  line.dataset.remote = state;
  const title = withLines(remoteStatus?.detail, pathProblems);
  if (title) line.title = title;
  return line;
}

function showRemoteStatus(status) {
  if (!status || typeof status.text !== "string") return;
  remoteStatus = status;
  const old = $("flyout-foot").querySelector("[data-remote]");
  // Before the first usage snapshot the footer still shows "loading…"; the
  // line is drawn with it on the first render.
  if (old) old.replaceWith(remoteLine());
}

/** The Remote line follows the backend's `remote-status` events from the start. */
export function initRemoteStatus() {
  listen("remote-status", (e) => showRemoteStatus(e.payload));
  invoke("remote_status").then(showRemoteStatus, (err) => console.error("remote_status", err));
}
