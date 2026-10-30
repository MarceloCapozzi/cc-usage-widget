// Header actions: 🗑 clear, 📊 usage, ⚙ settings, ↻ refresh, ⏻ hide; the
// three views (`state.view`) and the usage view's chart (`state.chart`).

import { drawActivity } from "./activity.js";
import { $ } from "./dom.js";
import { loadHistory } from "./history-chart.js";
import { loadProjects } from "./projects.js";
import { loadSettings, stopCapture } from "./settings.js";
import { state, VIEW_ID } from "./state.js";
import { invoke } from "./tauri.js";
import { fitSoon } from "./window.js";

let refreshing = false;

async function refreshNow() {
  if (refreshing) return;
  refreshing = true;
  const btn = $("btn-refresh");
  btn.classList.add("spinning");
  try {
    await invoke("refresh_now");
  } finally {
    refreshing = false;
    btn.classList.remove("spinning");
  }
}

/** Opens "sessions", "usage" or "settings", loading what that view shows. */
export function setView(next) {
  state.view = next;
  for (const [name, id] of Object.entries(VIEW_ID)) $(id).hidden = name !== next;
  $("btn-usage").setAttribute("aria-pressed", String(next === "usage"));
  $("btn-settings").setAttribute("aria-pressed", String(next === "settings"));
  $("btn-back").hidden = next === "sessions";
  if (next === "usage") {
    if (state.chart === "history") loadHistory(state.currentRange);
    loadProjects();
  }
  if (next === "settings") loadSettings();
  else stopCapture();
  fitSoon();
}

function setChart(next) {
  state.chart = next;
  $("activity").hidden = next !== "activity";
  $("history").hidden = next !== "history";
  for (const b of document.querySelectorAll(".chart-switch button")) {
    b.setAttribute("aria-selected", String(b.dataset.chart === next));
  }
  if (next === "history") loadHistory(state.currentRange);
  else if (state.lastActivity) drawActivity(state.lastActivity);
  fitSoon();
}

/** Redraws what holds token colours, after the theme changed. */
export function redrawCharts() {
  if (state.view === "usage") {
    if (state.chart === "history") loadHistory(state.currentRange);
    else if (state.lastActivity) drawActivity(state.lastActivity);
  }
}

/** The header's buttons, the back button and the chart switch. */
export function initHeader() {
  $("btn-refresh").addEventListener("click", refreshNow);
  // Clears every row (clear_sessions in commands.rs): local sessions that are
  // still alive come back on the next pass, about 2 s, and remote ones that
  // are still publishing with their next heartbeat; ended ones stay gone.
  $("btn-clear").addEventListener("click", () =>
    invoke("clear_sessions").catch((err) => console.error("clear_sessions", err))
  );
  // Hides, like Escape; quitting is in the tray menu, so the header button can
  // not end the app by accident.
  $("btn-hide").addEventListener("click", () => invoke("close_flyout"));
  $("btn-web").addEventListener("click", () => invoke("open_usage"));

  $("btn-usage").addEventListener("click", () => setView(state.view === "usage" ? "sessions" : "usage"));
  $("btn-back").addEventListener("click", () => setView("sessions"));

  for (const b of document.querySelectorAll(".chart-switch button")) {
    b.addEventListener("click", () => setChart(b.dataset.chart));
  }
  $("btn-settings").addEventListener("click", () => setView(state.view === "settings" ? "sessions" : "settings"));
}

/** System follows Windows live: the CSS does it alone, the charts need a redraw. */
export function initSystemTheme() {
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", redrawCharts);
}
