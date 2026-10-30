// Settings → Remote connection, and the view's diagnostics.
//
// The NATS servers and bucket both roles use: the "nats" object of
// settings.json (app/src/settings.rs). The servers box takes one URL per
// line; a blank field is the default, shown as its placeholder, and "Reset
// to defaults" blanks both. Save sends them as `set_settings`'s `nats`, in
// the settings save queue (settings.js): the backend checks them with
// cc-remote's rules, and a value it refuses saves nothing, its reason shown
// under the group. A setting a CC_USAGE_NATS_* variable overrides shows the
// value in force, read-only, with a line naming the variable. The viewer
// reconnects at once; in the Publisher role the new settings apply after a
// restart, and a Restart button shows until then. Settings that could not
// be used, and folder overrides that were ignored (a relative
// CC_USAGE_DATA_DIR or CLAUDE_CONFIG_DIR), are listed in red: the first
// under the group, the second at the top of the view.

import { $, create, dataNote } from "./dom.js";
import { canReset, natsDirty, natsFields, natsInput, natsSavedNote } from "./connection-text.js";
import { saveNats } from "./settings.js";
import { state } from "./state.js";
import { invoke } from "./tauri.js";

const elServers = $("nats-servers");
const elServersEnv = $("nats-servers-env");
const elBucket = $("nats-bucket");
const elBucketEnv = $("nats-bucket-env");
const elSave = $("nats-save");
const elReset = $("nats-reset");
const elNote = $("nats-note");
const elProblems = $("nats-problems");
const elRestart = $("nats-restart");
const elRestartRun = $("nats-restart-run");
const elPathProblems = $("path-problems");

/** The fields hold an edit not saved yet: a repaint leaves them alone. */
let editing = false;

function note(text, kind) {
  dataNote(elNote, text, kind);
}

/** One red line per entry, or the box hidden when there is none. */
function paintProblems(el, lines) {
  el.replaceChildren(...(lines ?? []).map((line) => create("p", "setting-note error", line)));
  el.hidden = !el.childElementCount;
}

function paintField(input, envLine, field) {
  if (!editing) input.value = field.value;
  input.placeholder = field.placeholder;
  input.readOnly = field.readOnly;
  envLine.textContent = field.envNote ?? "";
  envLine.hidden = !field.envNote;
}

const form = () => state.savedSettings?.nats;

function updateButtons() {
  const f = form();
  elSave.disabled = !f || !natsDirty(f, elServers.value, elBucket.value);
  elReset.disabled = !f || !canReset(f);
}

/** The group and the diagnostics of a `SettingsView` (paintSettings calls it). */
export function paintConnection(view) {
  paintProblems(elPathProblems, view.pathProblems);
  const f = view.nats;
  if (!f) return;
  const fields = natsFields(f);
  paintField(elServers, elServersEnv, fields.servers);
  paintField(elBucket, elBucketEnv, fields.bucket);
  // Rows for every saved server, so none hides behind a scroll bar.
  elServers.rows = Math.max(2, elServers.value.split("\n").length);
  paintProblems(elProblems, f.problems);
  elRestart.hidden = !view.natsRestartNeeded;
  updateButtons();
}

/** Back to what is saved, the edit dropped. */
function revert() {
  editing = false;
  note(null);
  if (state.savedSettings) paintConnection(state.savedSettings);
}

async function save(input) {
  const before = form();
  elSave.disabled = true;
  elReset.disabled = true;
  note(null);
  try {
    const view = await saveNats(input);
    editing = false;
    paintConnection(view);
    note(natsSavedNote(view, before));
  } catch (err) {
    // Nothing was saved: the typed values stay, to be corrected.
    note(`Not saved: ${err}`, "error");
    updateButtons();
  }
}

function saveTyped() {
  const f = form();
  if (f && natsDirty(f, elServers.value, elBucket.value)) save(natsInput(f, elServers.value, elBucket.value));
}

/** Wires the Remote connection group. */
export function initConnection() {
  for (const el of [elServers, elBucket]) {
    el.addEventListener("input", () => {
      editing = true;
      note(null);
      updateButtons();
    });
  }

  elServers.addEventListener("keydown", (e) => {
    // Enter is a new line here; Ctrl+Enter saves.
    if (e.key === "Enter" && e.ctrlKey) {
      e.preventDefault();
      saveTyped();
    } else if (e.key === "Escape") {
      // Undo the edit instead of leaving the view.
      e.preventDefault();
      e.stopPropagation();
      revert();
      elServers.blur();
    }
  });

  elBucket.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      saveTyped();
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      revert();
      elBucket.blur();
    }
  });

  elSave.addEventListener("click", saveTyped);
  elReset.addEventListener("click", () => {
    const f = form();
    if (f) save(natsInput(f, "", ""));
  });

  elRestartRun.addEventListener("click", async () => {
    try {
      await invoke("restart_app");
    } catch (err) {
      note(String(err), "error");
    }
  });
}
