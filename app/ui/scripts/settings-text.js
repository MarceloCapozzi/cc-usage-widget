// The ⚙ view's pure parts: the key name of a captured shortcut and the line
// that reports a restore (settings.js and backup.js use them).
//
// Pure: nothing here touches the DOM or the backend, so the unit tests in
// app/ui-tests/ import it under Node.

import { plural } from "./format.js";

/** The key part of a combination, from `KeyboardEvent.code`; null for a modifier. */
export function keyName(code) {
  if (/^(Control|Shift|Alt|Meta|OS)(Left|Right)?$/.test(code)) return null;
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  return code || null;
}

/** A function key, F1 to F24: the only key that may be a shortcut alone. */
export const isFunctionKey = (key) => /^F([1-9]|1[0-9]|2[0-4])$/.test(key);

/** One line out of the backend's report (backup.rs `Report`). */
export function restoreReport(r) {
  const parts = [`${plural(r.historyAdded, "sample", "samples")} added`];
  const added = r.activity.reduce((n, m) => n + m.added, 0);
  const raised = r.activity.reduce((n, m) => n + m.raised, 0);
  parts.push(`${plural(added, "day", "days")} of activity added, ${raised} raised` +
    (r.activity.length ? ` (${r.activity.map((m) => m.machine).join(", ")})` : ""));
  parts.push(`${plural(r.projectsAdded, "project", "projects")} added, ${r.projectsUpdated} updated`);
  if (r.pricing === "restored") parts.push("prices restored");
  let text = `Restored: ${parts.join("; ")}.`;
  let kind = null;
  const s = r.secrets;
  if (s) {
    const done = [];
    if (s.written.length) done.push(`written ${s.written.join(", ")}`);
    if (s.merged.length) done.push(`added keys to ${s.merged.join(", ")}`);
    if (s.skipped.length) done.push(`already here ${s.skipped.join(", ")}`);
    text += ` Keys and credentials: ${done.join("; ") || "nothing to do"}.`;
    if (s.conflicts.length) {
      text += ` Kept this machine's version of ${s.conflicts.join(", ")}: it differs from the backup's.`;
      kind = "error";
    }
  } else if (r.secretsWithheld) {
    text += " Keys and credentials not restored: no password given.";
  }
  return [text, kind];
}
