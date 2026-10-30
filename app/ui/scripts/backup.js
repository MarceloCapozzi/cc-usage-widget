// Settings → Data: backup and restore.
//
// The backend opens the native Save and Open dialogs and does the work
// (app/src/backup.rs); this only collects the choices and says what
// happened. A restore takes two steps: Restore… opens a backup and shows
// what it holds, Restore merges it. The backend restores only the file it
// last showed.

import { $, dataNote } from "./dom.js";
import { plural } from "./format.js";
import { restoreReport } from "./settings-text.js";
import { invoke } from "./tauri.js";

const elBackupSecrets = $("backup-secrets");
const elBackupPasswords = $("backup-passwords");
const elBackupPassword = $("backup-password");
const elBackupConfirm = $("backup-confirm");
const elBackupRun = $("backup-run");
const elBackupNote = $("backup-note");
const elRestorePick = $("restore-pick");
const elRestorePanel = $("restore-panel");
const elRestoreSummary = $("restore-summary");
const elRestorePassword = $("restore-password");
const elRestoreRun = $("restore-run");
const elRestoreCancel = $("restore-cancel");
const elRestoreNote = $("restore-note");

let inspected = null;

function closeRestore() {
  inspected = null;
  elRestorePanel.hidden = true;
  elRestorePassword.value = "";
}

/** Wires Back up…, Restore… and Restore. */
export function initBackup() {
  elBackupSecrets.addEventListener("change", () => {
    elBackupPasswords.hidden = !elBackupSecrets.checked;
    if (!elBackupSecrets.checked) {
      elBackupPassword.value = "";
      elBackupConfirm.value = "";
    }
    dataNote(elBackupNote, null);
  });

  elBackupRun.addEventListener("click", async () => {
    const includeSecrets = elBackupSecrets.checked;
    const password = elBackupPassword.value;
    if (includeSecrets) {
      if (password.length < 8) {
        dataNote(elBackupNote, "Use a password of at least 8 characters.", "error");
        return;
      }
      if (password !== elBackupConfirm.value) {
        dataNote(elBackupNote, "The two passwords do not match.", "error");
        return;
      }
    }
    dataNote(elBackupNote, null);
    elBackupRun.disabled = true;
    try {
      const saved = await invoke("backup_data", {
        includeSecrets,
        password: includeSecrets ? password : null,
      });
      if (!saved) return;
      const left = saved.skipped?.length ? ` Left out, unreadable: ${saved.skipped.join(", ")}.` : "";
      const keys = includeSecrets ? " Keys and credentials included: keep the file and the password apart." : "";
      dataNote(elBackupNote, `Saved ${saved.path}.${keys}${left}`, left ? "error" : null);
      elBackupPassword.value = "";
      elBackupConfirm.value = "";
    } catch (err) {
      dataNote(elBackupNote, String(err), "error");
    } finally {
      elBackupRun.disabled = false;
    }
  });

  elRestorePick.addEventListener("click", async () => {
    dataNote(elRestoreNote, null);
    elRestorePick.disabled = true;
    try {
      const summary = await invoke("inspect_backup");
      if (!summary) return;
      inspected = summary;
      const c = summary.counts;
      const created = new Date(summary.created).toLocaleString([], {
        year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
      });
      const from = summary.sameMachine ? `this machine (${summary.machine})` : summary.machine;
      elRestoreSummary.textContent =
        `From ${from}, ${created}: ${plural(c.historySamples, "sample", "samples")}, ` +
        `${plural(c.activityDays, "day", "days")} of activity, ${plural(c.remoteProjects, "project", "projects")}` +
        `${c.hasPricing ? ", prices" : ""}; keys and credentials: ${summary.hasSecrets ? "yes" : "no"}.`;
      elRestorePassword.hidden = !summary.hasSecrets;
      elRestorePassword.value = "";
      elRestorePanel.hidden = false;
    } catch (err) {
      closeRestore();
      dataNote(elRestoreNote, String(err), "error");
    } finally {
      elRestorePick.disabled = false;
    }
  });

  elRestoreCancel.addEventListener("click", () => {
    closeRestore();
    dataNote(elRestoreNote, null);
  });

  elRestoreRun.addEventListener("click", async () => {
    if (!inspected) return;
    elRestoreRun.disabled = true;
    dataNote(elRestoreNote, null);
    try {
      const report = await invoke("restore_backup", {
        path: inspected.path,
        password: inspected.hasSecrets ? elRestorePassword.value : null,
      });
      closeRestore();
      const [text, kind] = restoreReport(report);
      dataNote(elRestoreNote, text, kind);
    } catch (err) {
      // A wrong password changes nothing: the summary stays for another try.
      dataNote(elRestoreNote, String(err), "error");
    } finally {
      elRestoreRun.disabled = false;
    }
  });
}
