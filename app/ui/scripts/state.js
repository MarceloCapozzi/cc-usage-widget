// The state more than one module reads or changes, in one object.
//
// Modules import `state` and read or assign its fields (`state.view =
// "usage"`); an exported `let` could not be assigned from outside its own
// module. State only one module uses stays a `let` in that module.

export const state = {
  // "sessions", "usage" (account, chart, projects) or "settings". The usage
  // and ⚙ buttons each open their view and, pressed again, go back to the
  // list (views.js).
  view: "sessions",
  // The usage view's one chart: "activity" (the default each time the app
  // starts) or "history" (the plan limits over time).
  chart: "activity",
  // The history chart's range, a key of RANGE_HOURS (history-math.js).
  currentRange: "6h",
  // The machine the project list is narrowed to (a click on its tag), or
  // null for every machine. Kept while the app runs, not saved.
  projectHost: null,
  // The last usage-per-project report and its activity calendar, redrawn
  // when the machine filter, the width or the theme changes.
  lastReport: null,
  lastActivity: null,
  // What `get_settings` / `set_settings` last answered (settings.js), the
  // base every save starts from.
  savedSettings: null,
};

/** Each view's element. */
export const VIEW_ID = { sessions: "view-sessions", usage: "view-usage", settings: "view-settings" };
