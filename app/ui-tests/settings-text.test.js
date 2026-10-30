// node --test app/ui/tests/
import assert from "node:assert/strict";
import { test } from "node:test";

import { isFunctionKey, keyName, restoreReport } from "../ui/scripts/settings-text.js";

test("keyName spells the key as the backend reads it and skips modifiers", () => {
  assert.equal(keyName("KeyU"), "U");
  assert.equal(keyName("Digit7"), "7");
  assert.equal(keyName("F5"), "F5");
  assert.equal(keyName("Space"), "Space");
  for (const code of ["ControlLeft", "ShiftRight", "AltLeft", "MetaLeft", "OSRight", "Control"]) {
    assert.equal(keyName(code), null, code);
  }
  assert.equal(keyName(""), null);
});

test("only F1 to F24 may be a shortcut alone", () => {
  for (const key of ["F1", "F9", "F12", "F24"]) assert.equal(isFunctionKey(key), true, key);
  for (const key of ["F0", "F25", "U", "Space", "F"]) assert.equal(isFunctionKey(key), false, key);
});

const base = {
  historyAdded: 3,
  activity: [],
  projectsAdded: 1,
  projectsUpdated: 0,
  pricing: "kept",
  secrets: null,
  secretsWithheld: false,
};

test("restoreReport sums what was added", () => {
  const [text, kind] = restoreReport({
    ...base,
    activity: [{ machine: "pc", added: 2, raised: 1 }, { machine: "wk-01", added: 3, raised: 0 }],
    pricing: "restored",
  });
  assert.equal(
    text,
    "Restored: 3 samples added; 5 days of activity added, 1 raised (pc, wk-01); 1 project added, 0 updated; prices restored."
  );
  assert.equal(kind, null);
});

test("restoreReport says why keys were left out", () => {
  const [text] = restoreReport({ ...base, historyAdded: 1, secretsWithheld: true });
  assert.equal(
    text,
    "Restored: 1 sample added; 0 days of activity added, 0 raised; 1 project added, 0 updated." +
      " Keys and credentials not restored: no password given."
  );
});

test("restoreReport lists the keys and flags a conflict as an error", () => {
  const secrets = { written: ["viewer.key"], merged: [], skipped: ["known.json"], conflicts: [] };
  assert.match(restoreReport({ ...base, secrets })[0], / Keys and credentials: written viewer.key; already here known.json\.$/);
  assert.equal(restoreReport({ ...base, secrets })[1], null);
  const empty = { written: [], merged: [], skipped: [], conflicts: [] };
  assert.match(restoreReport({ ...base, secrets: empty })[0], /Keys and credentials: nothing to do\.$/);
  const [text, kind] = restoreReport({ ...base, secrets: { ...empty, conflicts: ["a.creds"] } });
  assert.match(text, /Kept this machine's version of a\.creds: it differs from the backup's\.$/);
  assert.equal(kind, "error");
});
