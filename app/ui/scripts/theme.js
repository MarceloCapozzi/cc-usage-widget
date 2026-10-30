// Theme.
//
// The Theme setting puts data-theme="light" or "dark" on <html>, or nothing
// for System (tokens.css follows prefers-color-scheme then). The last choice
// is also kept in localStorage, so the panel paints in the right colours
// before the backend answers (theme-init.js, a classic script in <head>,
// applies it before the first paint); that copy is a convenience,
// settings.json is the truth. Charts take their colours from the tokens when
// they draw, so a change redraws them (redrawCharts in views.js).

// theme-init.js reads the same key.
export const THEME_KEY = "cc-usage.theme";

export function applyTheme(theme) {
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
  try { localStorage.setItem(THEME_KEY, theme || "system"); } catch { /* private mode */ }
}

/** A token's current value, e.g. cssVar("--accent"). */
export const cssVar = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
