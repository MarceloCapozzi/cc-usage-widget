// The saved theme, before the first paint.
//
// A classic script in <head>, not part of the modules: module scripts are
// deferred and run only after the whole page is parsed, by which time the
// panel may already have painted in the system colours and would then flash
// into the saved ones. This runs before <body> exists. It only applies the
// choice; theme.js (THEME_KEY, applyTheme) owns it and writes it back.
try {
  const theme = localStorage.getItem("cc-usage.theme") || "system";
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
} catch { /* no storage */ }
