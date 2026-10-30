// node --test app/ui-tests/
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  confirmsAlias,
  keyFingerprint,
  keyNoticeParts,
  keyNoticeText,
  keyNoticeTitle,
  MAX_NOTICE_ROWS,
  moreKeysText,
  replaceKeyWarning,
} from "../ui/scripts/key-text.js";

const T0 = 1_800_000_000;
const LINE = "p20261014-3f9a:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

const pending = (over = {}) => ({
  alias: "devbox",
  kid: "p20261014-3f9a",
  key: LINE,
  fingerprint: "AAECAwQF",
  known: false,
  firstSeen: T0,
  lastSeen: T0,
  count: 1,
  ...over,
});

test("the fingerprint is the first 8 base64 characters of the key", () => {
  assert.equal(keyFingerprint(pending()), "AAECAwQF");
  assert.equal(keyFingerprint(pending({ fingerprint: undefined })), "AAECAwQF");
  assert.equal(keyFingerprint({ key: "k:Hx4dHBsaGRgX" }), "Hx4dHBsa");
  assert.equal(keyFingerprint({}), "");
});

test("a new machine asks to publish, with its key id and fingerprint", () => {
  assert.equal(keyNoticeText(pending()), "New machine devbox wants to publish · key p20261014-3f9a AAECAwQF");
  const [, id] = keyNoticeParts(pending());
  assert.equal(id, "p20261014-3f9a AAECAwQF");
});

test("a known machine presents a new key", () => {
  assert.equal(keyNoticeText(pending({ known: true })), "devbox presents a new key (p20261014-3f9a AAECAwQF)");
  assert.deepEqual(keyNoticeParts(pending({ known: true })), [
    "devbox presents a new key (",
    "p20261014-3f9a AAECAwQF",
    ")",
  ]);
});

test("the tooltip says what is held back and shows the whole key line", () => {
  const once = keyNoticeTitle(pending(), T0 + 90);
  assert.match(once, /^Seen 1m ago; 1 value held back\./);
  assert.ok(once.includes(LINE));
  const many = keyNoticeTitle(pending({ count: 7, lastSeen: T0 + 600 }), T0 + 660);
  assert.match(many, /^First seen 11m ago, last 1m ago; 7 values held back\./);
  assert.match(many, /Accept only if devbox shows this key/);
});

test("keys past the drawn rows are summed up", () => {
  assert.equal(moreKeysText(MAX_NOTICE_ROWS), null);
  assert.equal(moreKeysText(MAX_NOTICE_ROWS + 1), "+ 1 more key to review");
  assert.equal(moreKeysText(MAX_NOTICE_ROWS + 4), "+ 4 more keys to review");
});

test("replacing a key asks for the alias typed back", () => {
  assert.equal(
    replaceKeyWarning("devbox"),
    "This replaces devbox's identity: the viewer holds back its values until you accept the new key there. Type devbox to confirm."
  );
  assert.equal(confirmsAlias("devbox", "devbox"), true);
  assert.equal(confirmsAlias("  devbox ", "devbox"), true);
  for (const typed of ["", "Devbox", "kan", "devbox2", null, undefined]) {
    assert.equal(confirmsAlias(typed, "devbox"), false, String(typed));
  }
  assert.equal(confirmsAlias("", ""), false, "no alias, no confirmation");
});
