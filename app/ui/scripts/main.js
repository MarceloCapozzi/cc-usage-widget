// Flyout rendering: the entry module (index.html loads it as
// `<script type="module">`; theme-init.js runs before it, in <head>).
//
// No framework or bundler: plain ES modules, one per part of the panel. The
// payloads arrive from the backend already normalized (usage:
// app/src/model.rs; sessions: cc_core::state::SessionView, camelCase), so
// there is no business logic here: it only draws what came in.
//
// The markup is the ".flyout" block of the "The panel" mockup in the design
// document (docs/reference/flyout-mockup.html): the same elements and class
// names, so the mockup's CSS applies unchanged. Wording follows the mockup
// too ("5 sessions · 1 needs you", "needs you", "39% left · resets in 2h24m").
//
// Rule that runs through every module: what is not known is NOT drawn. A
// missing line says "does not apply"; a made-up 0% says "you used nothing",
// which is a different claim and often a false one.
//
// Modules:
//   main.js            startup, in the order below
//   state.js           the state several modules share
//   tauri.js, dom.js   the backend and DOM helpers
//   theme.js           the theme setting and the token colours
//   window.js          height that follows the content, moving, resizing
//   views.js           header buttons, views, the usage view's chart switch
//   footer.js          the limit lines and the Remote line
//   usage.js           the usage view's account and context; `render`
//   history-chart.js   the plan limits over time
//   activity.js        the activity calendar
//   projects.js        the usage-per-project report
//   sessions.js        the session rows and the header summary
//   keys.js            the notice of publisher keys waiting for review
//   settings.js, role.js, connection.js, backup.js   the ⚙ view
// and the pure ones the tests in app/ui-tests/ import: format.js,
// session-text.js, history-math.js, report-data.js, settings-text.js,
// key-text.js, connection-text.js.
//
// Module code only defines things and looks up elements (module scripts run
// once the page is parsed); everything that wires events or calls the
// backend is an `init…` function, called below in a fixed order.

import { initActivityResize } from "./activity.js";
import { initBackup } from "./backup.js";
import { initConnection } from "./connection.js";
import { $ } from "./dom.js";
import { initFooterClock, initRemoteStatus } from "./footer.js";
import { initKeyNotice } from "./keys.js";
import { initRanges, loadHistory } from "./history-chart.js";
import { initRole } from "./role.js";
import { initSessions } from "./sessions.js";
import { initSettings } from "./settings.js";
import { state } from "./state.js";
import { invoke, listen } from "./tauri.js";
import { applyTheme } from "./theme.js";
import { render } from "./usage.js";
import { initHeader, initSystemTheme, setView } from "./views.js";
import { fitSoon, initFit, initMoveResize } from "./window.js";

// ── Startup ─────────────────────────────────────────────────────────────────

initHeader();
initFit();
initMoveResize();
initSystemTheme();
initRanges();
initActivityResize();
initSessions();
initRemoteStatus();
initFooterClock();
initKeyNotice();
initSettings();
initRole();
initConnection();
initBackup();

// Escape in the usage or settings view goes back to the sessions; in the
// session list it hides the widget (the tray icon or the shortcut brings it
// back). Fields that use Escape themselves stop it before it gets here.
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  if (state.view !== "sessions") setView("sessions");
  else invoke("close_flyout");
});

// The browser's context menu adds nothing in a panel.
document.addEventListener("contextmenu", (e) => e.preventDefault());

listen("usage", (e) => {
  try {
    render(JSON.parse(e.payload));
    if (state.view === "usage" && state.chart === "history") loadHistory(state.currentRange);
  } catch (err) {
    console.error("unreadable payload", err);
  }
});

// When it opens, the flyout paints with the last known data instead of staying
// empty while waiting for the network; the refresh arrives later and replaces it.
listen("refresh-requested", paintLast);

async function paintLast() {
  const json = await invoke("last_snapshot");
  if (json) render(JSON.parse(json));
}

// Development flags, for reviewing and screenshotting the panel: --view
// usage or --view settings opens on that view; --backdrop paints the transparent
// cushion around the panel, so a capture shows the shadow on a known colour
// instead of black; --click clicks one element after load (devflags.rs).
// DEV (--click): an element id or a CSS selector, clicked once it exists
// (content such as the project list arrives after load), for up to 15 s.
function clickWhenThere(target, tries = 60) {
  const el = $(target) ?? document.querySelector(target);
  // A pause first, so what the page loads on its own (the 6 h chart) is
  // already drawn and does not overwrite what the click asks for.
  if (el) setTimeout(() => el.click(), 1500);
  else if (tries > 0) setTimeout(() => clickWhenThere(target, tries - 1), 250);
}

invoke("ui_flags").then(
  (flags) => {
    if (flags?.view === "usage") setView("usage");
    if (flags?.view === "settings") setView("settings");
    if (flags?.backdrop) document.body.style.background = flags.backdrop;
    if (flags?.click) clickWhenThere(flags.click);
  },
  (err) => console.error("ui_flags", err)
);

// The saved theme, before the settings view is ever opened, and the settings
// as the base the other views read (the history chart names the refresh
// interval); opening the ⚙ view loads and paints them afresh.
invoke("get_settings").then(
  (v) => {
    applyTheme(v?.theme ?? "system");
    if (v && !state.savedSettings) state.savedSettings = v;
  },
  () => {},
);

paintLast();
fitSoon();
