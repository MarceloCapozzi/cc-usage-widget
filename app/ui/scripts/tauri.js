// The backend: commands (`invoke`), events (`listen`) and the window.
//
// `window.__TAURI__` exists because tauri.conf.json sets `withGlobalTauri: true`.
// It is mandatory in this project: without a bundler there is no way to import
// the npm package, and without that option the object is not injected and
// this module dies on its first line with no visible error.
//
// The command names, event names and argument names must match
// app/src/commands.rs (commands) and app/src/poller.rs and app/src/window.rs
// (events).

export const { invoke } = window.__TAURI__.core;
export const { listen } = window.__TAURI__.event;

/** The app version (the workspace's, from Cargo.toml). */
export const appVersion = () => window.__TAURI__.app.getVersion();

/** The widget's own window, for `startDragging`. */
export const currentWindow = () => window.__TAURI__.window.getCurrentWindow();
