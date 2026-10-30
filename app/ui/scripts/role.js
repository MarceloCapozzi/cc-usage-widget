// Settings → Role.
//
// Viewer or Publisher (settings.json `role`, app/src/publisher.rs). A switch
// is confirmed first, then `set_role` saves it and restarts the app. In the
// Publisher role the panel lists the three files publishing needs, ✓ or ✗
// with their paths, and the buttons that get them there; the backend opens
// the dialogs and reads only the files they return. "Generate key" shows
// this machine's public key line, the one the viewer's user accepts (the
// viewer's flyout asks). Once the machine has its key the button reads
// "Replace key…": the key is the machine's identity, so a small confirm
// asks for this machine's name typed back before `publisher_generate_key`
// gets it as `confirm` (the backend checks it again).
// While publishing, an import applies at once: the note says what the
// running publisher now uses, or, as an error, why it kept what it had.

import { $, create, dataNote } from "./dom.js";
import { confirmsAlias, replaceKeyWarning } from "./key-text.js";
import { paintSettings } from "./settings.js";
import { state } from "./state.js";
import { invoke } from "./tauri.js";

const elRole = $("role-select");
const elRoleConfirm = $("role-confirm");
const elRoleConfirmText = $("role-confirm-text");
const elRoleSwitch = $("role-switch");
const elRoleCancel = $("role-cancel");
const elRoleNote = $("role-note");
const elPublisherPanel = $("publisher-panel");
const elPublisherAlias = $("publisher-alias");
const elPublisherChecks = $("publisher-checks");
const elPublisherKey = $("publisher-key");
const elPublisherKeyLine = $("publisher-key-line");
const elPublisherKeyCopy = $("publisher-key-copy");
const elPublisherGenerate = $("publisher-generate");
const elPublisherImportViewer = $("publisher-import-viewer");
const elPublisherImportCreds = $("publisher-import-creds");
const elPublisherNote = $("publisher-note");
const elPublisherRestartRow = $("publisher-restart-row");
const elPublisherRestart = $("publisher-restart");
const elPublisherReplace = $("publisher-replace");
const elPublisherReplaceText = $("publisher-replace-text");
const elPublisherReplaceConfirm = $("publisher-replace-confirm");
const elPublisherReplaceRun = $("publisher-replace-run");
const elPublisherReplaceCancel = $("publisher-replace-cancel");

/** The alias in the panel, and whether this machine has its key. */
let publisherAlias = null;
let hasKey = false;

const ROLE_NAMES = { viewer: "Viewer", publisher: "Publisher" };

function roleNote(text, kind) {
  dataNote(elRoleNote, text, kind);
}

function publisherNote(text, kind) {
  dataNote(elPublisherNote, text, kind);
}

/** The Role section of a `SettingsView` (paintSettings calls it). */
export function paintRole(view) {
  const role = view.role ?? "viewer";
  // A pending switch keeps its choice on screen until confirmed or cancelled.
  if (elRoleConfirm.hidden) elRole.value = role;
  const p = view.publisher;
  elPublisherPanel.hidden = role !== "publisher" || !p;
  if (elPublisherPanel.hidden) return;
  elPublisherAlias.textContent = p.alias;
  elPublisherChecks.replaceChildren(
    ...[
      ["Credentials", p.creds],
      ["Publisher key", p.publisherKey],
      ["Viewer public key", p.viewerKey],
    ].map(([what, check]) => {
      const li = create("li", check.ok ? "ok" : null);
      li.append(
        create("span", "mark", check.ok ? "✓" : "✗"),
        create("span", "what", `${what}: ${check.detail}`),
        create("span", "path", check.path)
      );
      return li;
    })
  );
  if (p.publicKey) {
    elPublisherKeyLine.value = p.publicKey;
    elPublisherKey.hidden = false;
  } else {
    elPublisherKey.hidden = true;
  }
  publisherAlias = p.alias;
  hasKey = !!p.publicKey;
  elPublisherGenerate.textContent = hasKey ? "Replace key…" : "Generate key";
  elPublisherGenerate.title = hasKey
    ? "Make a new key for this machine; the viewer holds back its values until the new key is accepted there"
    : "";
  if (!hasKey) hideReplace();
  elPublisherRestartRow.hidden = !view.restartNeeded;
  if (view.restartNeeded) {
    publisherNote(`The new name applies after a restart: still publishing as ${view.publishingAs ?? "the old name"}.`);
  } else if (p.ready && !view.publishingAs) {
    publisherNote("Ready: publishing starts in a moment.");
  }
}

/** Runs one Publisher setup command and shows what it did. True when it did something. */
async function publisherAction(button, command, args) {
  button.disabled = true;
  publisherNote(null);
  try {
    const done = await invoke(command, args);
    // A cancelled dialog answers null.
    if (!done) return false;
    elRoleConfirm.hidden = true;
    paintSettings(done.view);
    publisherNote(done.note, done.failed ? "error" : undefined);
    if (done.publicKey) {
      elPublisherKeyLine.value = done.publicKey;
      elPublisherKey.hidden = false;
    }
    return true;
  } catch (err) {
    publisherNote(String(err), "error");
    return false;
  } finally {
    button.disabled = false;
  }
}

function hideReplace() {
  elPublisherReplace.hidden = true;
  elPublisherReplaceConfirm.value = "";
  elPublisherReplaceRun.disabled = true;
}

/** "Replace key…": the confirm, with this machine's name to type back. */
function askReplace() {
  publisherNote(null);
  elPublisherReplaceText.textContent = replaceKeyWarning(publisherAlias);
  elPublisherReplaceConfirm.value = "";
  elPublisherReplaceConfirm.placeholder = publisherAlias ?? "";
  elPublisherReplaceRun.disabled = true;
  elPublisherReplace.hidden = false;
  elPublisherReplaceConfirm.focus();
}

/** Wires the role switch and the Publisher setup buttons. */
export function initRole() {
  elRole.addEventListener("change", () => {
    const saved = state.savedSettings?.role ?? "viewer";
    roleNote(null);
    if (elRole.value === saved) {
      elRoleConfirm.hidden = true;
      return;
    }
    elRoleConfirmText.textContent =
      `cc-usage restarts to switch role to ${ROLE_NAMES[elRole.value] ?? elRole.value}.` +
      (elRole.value === "publisher"
        ? " It stops showing other machines and publishes this one's sessions."
        : " It stops publishing and shows the other machines' sessions.");
    elRoleConfirm.hidden = false;
  });

  elRoleCancel.addEventListener("click", () => {
    elRoleConfirm.hidden = true;
    elRole.value = state.savedSettings?.role ?? "viewer";
  });

  elRoleSwitch.addEventListener("click", async () => {
    elRoleSwitch.disabled = true;
    try {
      await invoke("set_role", { role: elRole.value });
      // The app restarts; nothing after this normally runs.
      roleNote("Restarting…");
    } catch (err) {
      roleNote(String(err), "error");
    } finally {
      elRoleSwitch.disabled = false;
    }
  });

  // The first key is one click; replacing it asks first.
  elPublisherGenerate.addEventListener("click", () => {
    if (hasKey) askReplace();
    else publisherAction(elPublisherGenerate, "publisher_generate_key");
  });
  elPublisherReplaceConfirm.addEventListener("input", () => {
    elPublisherReplaceRun.disabled = !confirmsAlias(elPublisherReplaceConfirm.value, publisherAlias);
  });
  elPublisherReplaceConfirm.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !elPublisherReplaceRun.disabled) elPublisherReplaceRun.click();
    if (e.key === "Escape") {
      // Closes the confirm, not the view.
      e.stopPropagation();
      hideReplace();
    }
  });
  elPublisherReplaceCancel.addEventListener("click", hideReplace);
  elPublisherReplaceRun.addEventListener("click", async () => {
    const typed = elPublisherReplaceConfirm.value;
    if (!confirmsAlias(typed, publisherAlias)) return;
    if (await publisherAction(elPublisherReplaceRun, "publisher_generate_key", { confirm: typed })) hideReplace();
    else elPublisherReplaceRun.disabled = !confirmsAlias(elPublisherReplaceConfirm.value, publisherAlias);
  });
  elPublisherImportViewer.addEventListener("click", () =>
    publisherAction(elPublisherImportViewer, "publisher_import_viewer_key")
  );
  elPublisherImportCreds.addEventListener("click", () => publisherAction(elPublisherImportCreds, "publisher_import_creds"));

  elPublisherKeyCopy.addEventListener("click", async () => {
    const line = elPublisherKeyLine.value;
    if (!line) return;
    try {
      await navigator.clipboard.writeText(line);
    } catch {
      // Older WebView2 builds without the async clipboard: select and copy.
      elPublisherKeyLine.select();
      document.execCommand("copy");
    }
    publisherNote("Copied the public key line.");
  });

  elPublisherRestart.addEventListener("click", async () => {
    try {
      await invoke("restart_app");
    } catch (err) {
      publisherNote(String(err), "error");
    }
  });
}
