// node --test app/ui-tests/
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  canReset,
  envNote,
  natsDirty,
  natsFields,
  natsInput,
  natsSavedNote,
  parseServers,
  serversText,
  withLines,
} from "../ui/scripts/connection-text.js";

const DEFAULTS = ["wss://connect.ngs.global", "tls://connect.ngs.global"];

/** A `NatsForm` as the backend sends it: nothing saved, no variable. */
const form = (over = {}) => ({
  servers: [],
  bucket: "",
  defaultServers: DEFAULTS,
  defaultBucket: "cc-sessions",
  effectiveServers: DEFAULTS,
  effectiveBucket: "cc-sessions",
  serversEnv: null,
  bucketEnv: null,
  problems: [],
  ...over,
});

test("parseServers takes one URL per line or comma, trimmed, blanks dropped", () => {
  assert.deepEqual(parseServers(" tls://a.example:4222 \n\n wss://b.example\r\n"), [
    "tls://a.example:4222",
    "wss://b.example",
  ]);
  assert.deepEqual(parseServers("tls://a.example, wss://b.example"), ["tls://a.example", "wss://b.example"]);
  assert.deepEqual(parseServers(""), []);
  assert.deepEqual(parseServers(" \n , "), []);
  assert.deepEqual(parseServers(undefined), []);
  assert.equal(serversText(["tls://a", "wss://b"]), "tls://a\nwss://b");
  assert.equal(serversText(null), "");
});

test("an unset field shows what is saved, with the default as placeholder", () => {
  const f = natsFields(form({ servers: ["tls://nats.example:4222"], bucket: "team" }));
  assert.deepEqual(f.servers, {
    value: "tls://nats.example:4222",
    placeholder: DEFAULTS.join("\n"),
    readOnly: false,
    envNote: null,
  });
  assert.deepEqual(f.bucket, { value: "team", placeholder: "cc-sessions", readOnly: false, envNote: null });
  // Nothing saved: blank, the default in the placeholder.
  assert.equal(natsFields(form()).servers.value, "");
});

test("a field a variable sets shows the value in force, read-only, and names it", () => {
  const f = natsFields(
    form({
      servers: ["tls://saved.example"],
      effectiveServers: ["wss://env.example"],
      serversEnv: "CC_USAGE_NATS_SERVERS",
      effectiveBucket: "envbucket",
      bucketEnv: "CC_USAGE_NATS_BUCKET",
    })
  );
  assert.equal(f.servers.value, "wss://env.example");
  assert.equal(f.servers.readOnly, true);
  assert.equal(f.servers.placeholder, "");
  assert.equal(f.servers.envNote, envNote("CC_USAGE_NATS_SERVERS"));
  assert.match(f.servers.envNote, /^Set by CC_USAGE_NATS_SERVERS, which wins over this setting; unset/);
  assert.equal(f.bucket.value, "envbucket");
  assert.equal(f.bucket.readOnly, true);
});

test("the saved input sends what was typed, or the saved value of an overridden setting", () => {
  assert.deepEqual(natsInput(form(), "tls://a\n wss://b ", "  team "), {
    servers: ["tls://a", "wss://b"],
    bucket: "team",
  });
  const locked = form({ servers: ["tls://saved"], bucket: "kept", serversEnv: "CC_USAGE_NATS_SERVERS" });
  assert.deepEqual(natsInput(locked, "wss://env.example", "new"), { servers: ["tls://saved"], bucket: "new" });
  // Reset: blank fields, which are the defaults.
  assert.deepEqual(natsInput(form({ servers: ["tls://x"], bucket: "b" }), "", ""), { servers: [], bucket: "" });
});

test("Save has something to do only when the typed values differ from the saved ones", () => {
  const saved = form({ servers: ["tls://a", "wss://b"], bucket: "team" });
  assert.equal(natsDirty(saved, "tls://a\nwss://b", "team"), false);
  assert.equal(natsDirty(saved, " tls://a \n\nwss://b\n", " team "), false, "spacing is not a change");
  assert.equal(natsDirty(saved, "wss://b\ntls://a", "team"), true, "order counts");
  assert.equal(natsDirty(saved, "tls://a", "team"), true);
  assert.equal(natsDirty(saved, "tls://a\nwss://b", ""), true);
  assert.equal(natsDirty(form(), "", ""), false);
  // An overridden field's text never counts as an edit.
  const locked = form({ serversEnv: "CC_USAGE_NATS_SERVERS", effectiveServers: ["wss://env"] });
  assert.equal(natsDirty(locked, "wss://env", ""), false);
});

test("Reset to defaults has something to do only with a saved value no variable overrides", () => {
  assert.equal(canReset(form()), false);
  assert.equal(canReset(form({ bucket: "team" })), true);
  assert.equal(canReset(form({ servers: ["tls://a"] })), true);
  assert.equal(canReset(form({ servers: ["tls://a"], serversEnv: "CC_USAGE_NATS_SERVERS" })), false);
  assert.equal(
    canReset(form({ servers: ["tls://a"], serversEnv: "CC_USAGE_NATS_SERVERS", bucket: "b" })),
    true
  );
});

test("the note after a save says what changed and when it applies", () => {
  const before = form();
  const moved = form({ servers: ["tls://nats.example"], effectiveServers: ["tls://nats.example"] });
  assert.equal(
    natsSavedNote({ role: "viewer", nats: moved }, before),
    "Saved: the viewer reconnects to cc-sessions now."
  );
  assert.equal(
    natsSavedNote({ role: "viewer", nats: form() }, moved),
    "Back to the defaults: the viewer reconnects to cc-sessions now."
  );
  assert.equal(
    natsSavedNote({ role: "publisher", natsRestartNeeded: true, nats: moved }, before),
    "Saved: publishing uses them after a restart."
  );
  assert.equal(natsSavedNote({ role: "publisher", natsRestartNeeded: false, nats: moved }, before), "Saved.");
  // Saved, but what is in force did not move (a variable wins, or the same values).
  const overridden = form({ bucket: "team", bucketEnv: "CC_USAGE_NATS_BUCKET", effectiveBucket: "envb" });
  assert.equal(
    natsSavedNote({ role: "viewer", nats: overridden }, { ...overridden, bucket: "" }),
    "Saved; the connection is unchanged."
  );
});

test("withLines puts the extra lines under the tooltip's own text", () => {
  const line = "CC_USAGE_DATA_DIR is relative (data) and is ignored; set an absolute path.";
  assert.equal(withLines("Updated 2 min ago", [line]), `Updated 2 min ago\n${line}`);
  assert.equal(withLines("Updated 2 min ago", []), "Updated 2 min ago");
  assert.equal(withLines(null, [line]), line);
  assert.equal(withLines(undefined, undefined), null);
});
