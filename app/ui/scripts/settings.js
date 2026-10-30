// Widget settings (⚙ view): shortcut, machine name, full-screen hiding.
//
// `get_settings` / `set_settings` in app/src/commands.rs; the file is
// settings.json in the app config dir. The shortcut is captured from a key
// press, in the spelling the backend understands ("Alt+Shift+U",
// "Ctrl+Win+F5"). A combination that will not register is not saved: the
// previous one stays in force and the reason is shown under the field. The
// machine name is typed; one that breaks the alias rules is not saved
// either, and the backend's reason is shown under its field. Any other save
// that fails shows its reason under the list of settings. The Role, Remote
// connection and Data sections of the same view are role.js, connection.js
// and backup.js; the Remote connection group saves through the same queue
// (saveNats).

import { paintConnection } from "./connection.js";
import { $ } from "./dom.js";
import { paintRole } from "./role.js";
import { showSessions } from "./sessions.js";
import { isFunctionKey, keyName } from "./settings-text.js";
import { state } from "./state.js";
import { appVersion, invoke } from "./tauri.js";
import { applyTheme } from "./theme.js";
import { redrawCharts } from "./views.js";

const elShortcut = $("shortcut-field");
const elShortcutSave = $("shortcut-save");
const elShortcutClear = $("shortcut-clear");
const elShortcutNote = $("shortcut-note");
const elMachine = $("machine-field");
const elMachineSave = $("machine-save");
const elMachineReset = $("machine-reset");
const elMachineNote = $("machine-note");
const elFullscreen = [...document.querySelectorAll('input[name="fullscreen"]')];
const elStartWithWindows = $("start-with-windows");
const elAutostartNote = $("autostart-note");
const elRefreshEvery = $("refresh-every");
const elTheme = $("theme-select");
const elHideIdle = $("hide-idle");
const elRemoveOffline = $("remove-offline");
const elSettingsNote = $("settings-note");

let capturedShortcut = null;
let capturing = false;

function shortcutNote(text, kind) {
  elShortcutNote.hidden = !text;
  elShortcutNote.textContent = text ?? "";
  elShortcutNote.classList.toggle("error", kind === "error");
}

function machineNote(text, kind) {
  elMachineNote.hidden = !text;
  elMachineNote.textContent = text ?? "";
  elMachineNote.classList.toggle("error", kind === "error");
}

/** Shows a `SettingsView` from the backend and keeps it as `state.savedSettings`. */
export function paintSettings(view) {
  state.savedSettings = view;
  capturedShortcut = null;
  elShortcut.value = view.shortcut;
  elShortcut.placeholder = view.shortcut ? "" : "None · click, then press keys";
  elShortcutSave.disabled = true;
  elShortcutClear.disabled = !view.shortcut;
  elMachine.value = view.effectiveMachineName ?? "";
  elMachine.placeholder = view.defaultMachineName ?? "";
  elMachineSave.disabled = true;
  // Only a name of one's own can be reset; the default is already in force.
  elMachineReset.disabled = !view.machineName;
  elMachineReset.title = `Back to ${view.defaultMachineName}, from the computer name`;
  for (const r of elFullscreen) r.checked = r.value === (view.hideOnFullscreen ?? "never");
  elStartWithWindows.checked = !!view.startWithWindows;
  elRefreshEvery.value = String(view.refreshMins ?? 5);
  elTheme.value = view.theme ?? "system";
  applyTheme(elTheme.value);
  elHideIdle.value = String(view.hideIdleAfterMins ?? 0);
  elRemoveOffline.value = String(view.removeOfflineAfterMins ?? 0);
  if (view.shortcutError) shortcutNote(view.shortcutError, "error");
  else shortcutNote(null);
  paintRole(view);
  paintConnection(view);
}

export async function loadSettings() {
  try {
    paintSettings(await invoke("get_settings"));
  } catch (err) {
    console.error("get_settings", err);
  }
}

// Saves run one at a time, each built on what the one before it saved. In
// flight together, a request built on settings a moment old could undo
// another field's change (arrow keys on a closed list fire `change` on
// every press), and the backend could keep a mix of the two. `pending`
// counts the saves queued or running: a reply with more to come only
// updates `state.savedSettings`, and the last one paints, so the fields do
// not flick back through the values in between.
let saveChain = Promise.resolve();
let pending = 0;

function queued(task) {
  pending++;
  const run = saveChain.then(task).finally(() => pending--);
  saveChain = run.catch(() => {});
  return run;
}

function settingsNote(text, kind) {
  elSettingsNote.hidden = !text;
  elSettingsNote.textContent = text ?? "";
  elSettingsNote.classList.toggle("error", kind === "error");
}

/** Saves `changes` over the settings in force; resolves to the new view, or null if not saved. */
function saveSettings(changes) {
  return queued(() => saveNow(changes));
}

/** The settings in force with `changes` over them. Every field, always: the backend fills a missing one with its default. */
function settingsWith(savedSettings, changes) {
  return {
    shortcut: savedSettings.shortcut,
    hideOnFullscreen: savedSettings.hideOnFullscreen,
    refreshMins: savedSettings.refreshMins,
    theme: savedSettings.theme,
    hideIdleAfterMins: savedSettings.hideIdleAfterMins,
    removeOfflineAfterMins: savedSettings.removeOfflineAfterMins,
    machineName: savedSettings.machineName ?? null,
    ...changes,
  };
}

/**
 * Saves the Remote connection group (`nats`: `{ servers, bucket }`) in the
 * same queue, the other settings as they are. Resolves to the new view;
 * rejects with the backend's reason when nothing was saved (connection.js
 * shows it under the group).
 */
export function saveNats(nats) {
  return queued(async () => {
    const savedSettings = state.savedSettings;
    if (!savedSettings) throw new Error("the settings are not loaded yet");
    const view = await invoke("set_settings", { settings: settingsWith(savedSettings, {}), nats });
    if (pending > 1) state.savedSettings = view;
    else paintSettings(view);
    return view;
  });
}

async function saveNow(changes) {
  const savedSettings = state.savedSettings;
  if (!savedSettings) return null;
  const settings = settingsWith(savedSettings, changes);
  const general = !("machineName" in changes) && !("shortcut" in changes);
  try {
    const view = await invoke("set_settings", { settings });
    if (pending > 1) state.savedSettings = view;
    else paintSettings(view);
    if (general) settingsNote(null);
    return view;
  } catch (err) {
    // Not saved: show what is in force again, with the reason, under the
    // field that was being changed (the shortcut or the machine name), or
    // under the list of settings for any other.
    await loadSettings();
    if ("machineName" in changes) {
      elMachine.value = changes.machineName ?? "";
      elMachineSave.disabled = false;
      machineNote(String(err), "error");
    } else if ("shortcut" in changes) {
      elShortcut.value = settings.shortcut;
      shortcutNote(String(err), "error");
    } else {
      settingsNote(`Not saved: ${err}`, "error");
    }
    return null;
  }
}

// The machine name tags the local rows, which the backend builds: ask for
// them again once it changed (in demo mode nothing else would resend them).
async function saveMachineName(typed) {
  const view = await saveSettings({ machineName: typed });
  if (!view) return;
  machineNote(
    view.machineName
      ? `Saved: this machine's sessions show as ${view.effectiveMachineName}.`
      : `Back to the default, ${view.effectiveMachineName}.`
  );
  invoke("sessions").then(showSessions, (err) => console.error("sessions", err));
}

function startCapture() {
  if (capturing) return;
  capturing = true;
  // Let go of the global shortcut, so pressing the current one here is
  // captured instead of hiding the widget.
  invoke("pause_shortcut", { paused: true }).catch((err) => console.error("pause_shortcut", err));
}

/** Ends a shortcut capture, registering the saved shortcut again. */
export function stopCapture() {
  if (!capturing) return;
  capturing = false;
  invoke("pause_shortcut", { paused: false }).catch((err) => shortcutNote(String(err), "error"));
}

/** Wires the fields of the widget settings. */
export function initSettings() {
  appVersion().then(
    (v) => { $("app-version").textContent = `cc-usage ${v}`; },
    (err) => console.error("getVersion", err),
  );

  elMachine.addEventListener("input", () => {
    elMachineSave.disabled = elMachine.value.trim() === (state.savedSettings?.effectiveMachineName ?? "");
    machineNote(null);
  });

  elMachine.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (!elMachineSave.disabled) saveMachineName(elMachine.value);
    } else if (e.key === "Escape") {
      // Undo the edit instead of hiding the widget.
      e.preventDefault();
      e.stopPropagation();
      elMachine.value = state.savedSettings?.effectiveMachineName ?? "";
      elMachineSave.disabled = true;
      machineNote(null);
      elMachine.blur();
    }
  });

  elMachineSave.addEventListener("click", () => saveMachineName(elMachine.value));
  elMachineReset.addEventListener("click", () => saveMachineName(null));

  elShortcut.addEventListener("focus", startCapture);
  elShortcut.addEventListener("blur", () => {
    // Leaving the field without saving shows what is in force.
    if (state.savedSettings && capturedShortcut === null) elShortcut.value = state.savedSettings.shortcut;
    stopCapture();
  });

  elShortcut.addEventListener("keydown", (e) => {
    const mods = [];
    if (e.ctrlKey) mods.push("Ctrl");
    if (e.altKey) mods.push("Alt");
    if (e.shiftKey) mods.push("Shift");
    if (e.metaKey) mods.push("Win");
    // Tab alone still moves the focus.
    if (e.key === "Tab" && !mods.length) return;
    e.preventDefault();
    // Never reaches the document's Escape handler, which hides the widget.
    e.stopPropagation();

    if (e.key === "Escape" && !mods.length) {
      capturedShortcut = null;
      elShortcut.value = state.savedSettings?.shortcut ?? "";
      elShortcutSave.disabled = true;
      elShortcut.blur();
      return;
    }
    const key = keyName(e.code);
    if (!key) {
      // Only modifiers so far: show them while the rest is pressed.
      elShortcut.value = mods.length ? `${mods.join("+")}+…` : "";
      return;
    }
    if (!mods.length && !isFunctionKey(key)) {
      shortcutNote("Add Ctrl, Alt, Shift or Win to the key: a single key would be taken from every application.", "error");
      return;
    }
    capturedShortcut = [...mods, key].join("+");
    elShortcut.value = capturedShortcut;
    elShortcutSave.disabled = capturedShortcut === state.savedSettings?.shortcut;
    shortcutNote(null);
  });

  // Releasing a lone modifier leaves the field showing what is in force.
  elShortcut.addEventListener("keyup", () => {
    if (capturedShortcut === null && elShortcut.value.endsWith("+…")) {
      elShortcut.value = state.savedSettings?.shortcut ?? "";
    }
  });

  // `mousedown` so the field keeps the focus (and the capture its pause) until
  // the click has saved.
  elShortcutSave.addEventListener("mousedown", (e) => e.preventDefault());
  elShortcutSave.addEventListener("click", async () => {
    if (capturedShortcut === null) return;
    const view = await saveSettings({ shortcut: capturedShortcut });
    elShortcut.blur();
    if (view) shortcutNote(view.shortcut ? `Saved: ${view.shortcut} shows or hides the widget.` : "Saved.");
  });

  elShortcutClear.addEventListener("click", async () => {
    const view = await saveSettings({ shortcut: "" });
    if (view) shortcutNote("No shortcut: the tray icon shows and hides the widget.");
  });

  for (const r of elFullscreen) {
    r.addEventListener("change", () => {
      if (r.checked) saveSettings({ hideOnFullscreen: r.value });
    });
  }

  elTheme.addEventListener("change", () => {
    applyTheme(elTheme.value);
    redrawCharts();
    saveSettings({ theme: elTheme.value });
  });

  elRefreshEvery.addEventListener("change", () => {
    saveSettings({ refreshMins: Number(elRefreshEvery.value) });
  });

  // Not in settings.json: the system holds it (the tray menu has the same
  // switch). What comes back is the state really in force.
  // In the same queue as the saves, so its answer cannot paint over theirs.
  elStartWithWindows.addEventListener("change", () => {
    const enabled = elStartWithWindows.checked;
    elAutostartNote.hidden = true;
    queued(async () => {
      try {
        const view = await invoke("set_start_with_windows", { enabled });
        if (pending > 1) state.savedSettings = view;
        else paintSettings(view);
      } catch (err) {
        await loadSettings();
        elAutostartNote.textContent = String(err);
        elAutostartNote.classList.add("error");
        elAutostartNote.hidden = false;
      }
    });
  });

  elHideIdle.addEventListener("change", () => {
    saveSettings({ hideIdleAfterMins: Number(elHideIdle.value) });
  });

  elRemoveOffline.addEventListener("change", () => {
    saveSettings({ removeOfflineAfterMins: Number(elRemoveOffline.value) });
  });
}
