// The pure parts of Settings → Remote connection (connection.js) and the
// tooltip lines the footer adds (footer.js). The group edits the "nats"
// object of settings.json; the backend sends it as `SettingsView.nats`
// (app/src/settings.rs, NatsForm): the servers and bucket saved, those in
// force, the defaults, and the variable that overrides each, if one does.
//
// Pure: nothing here touches the DOM or the backend, so the unit tests in
// app/ui-tests/ import it under Node.

/**
 * The servers typed in the box: one per line (a comma splits too, as in
 * CC_USAGE_NATS_SERVERS), trimmed, blank ones left out.
 */
export function parseServers(text) {
  return String(text ?? "")
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** The box's text for a server list, one per line. */
export const serversText = (list) => (list ?? []).join("\n");

/** The line under a field a variable sets. */
export const envNote = (variable) =>
  `Set by ${variable}, which wins over this setting; unset the variable to change it here.`;

/**
 * How the two fields are drawn: `{ servers, bucket }`, each
 * `{ value, placeholder, readOnly, envNote }`. A setting a variable
 * overrides shows the value in force, read-only, and names the variable;
 * any other shows what is saved, with the default as the placeholder (blank
 * is the default).
 */
export function natsFields(form) {
  const field = (variable, inForce, saved, fallback) =>
    variable
      ? { value: inForce, placeholder: "", readOnly: true, envNote: envNote(variable) }
      : { value: saved, placeholder: fallback, readOnly: false, envNote: null };
  return {
    servers: field(
      form.serversEnv,
      serversText(form.effectiveServers),
      serversText(form.servers),
      serversText(form.defaultServers)
    ),
    bucket: field(form.bucketEnv, form.effectiveBucket ?? "", form.bucket ?? "", form.defaultBucket ?? ""),
  };
}

/**
 * The `nats` argument of `set_settings`: the typed values, except for a
 * setting a variable overrides, which sends what is saved (the backend
 * keeps it anyway).
 */
export function natsInput(form, serversTyped, bucketTyped) {
  return {
    servers: form.serversEnv ? [...(form.servers ?? [])] : parseServers(serversTyped),
    bucket: form.bucketEnv ? form.bucket ?? "" : String(bucketTyped ?? "").trim(),
  };
}

const sameList = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);

/** Whether the typed values differ from what is saved: Save has something to do. */
export function natsDirty(form, serversTyped, bucketTyped) {
  const input = natsInput(form, serversTyped, bucketTyped);
  return input.bucket !== (form.bucket ?? "") || !sameList(input.servers, form.servers ?? []);
}

/** Whether "Reset to defaults" has something to do: a saved value no variable overrides. */
export function canReset(form) {
  return (!form.serversEnv && (form.servers ?? []).length > 0) || (!form.bucketEnv && !!form.bucket);
}

const sameConnection = (a, b) =>
  !!a && !!b && a.effectiveBucket === b.effectiveBucket && sameList(a.effectiveServers, b.effectiveServers);

/**
 * The note after a save, from the view it answered and the group's form
 * before it: what changed and when it applies. The viewer reconnects at
 * once; a running Publisher uses new settings after a restart.
 */
export function natsSavedNote(view, before) {
  const form = view.nats;
  const saved = (form.servers ?? []).length > 0 || !!form.bucket;
  const what = saved ? "Saved" : "Back to the defaults";
  if (sameConnection(before, form)) return `${what}; the connection is unchanged.`;
  if (view.role === "publisher") {
    return view.natsRestartNeeded ? `${what}: publishing uses them after a restart.` : `${what}.`;
  }
  return `${what}: the viewer reconnects to ${form.effectiveBucket} now.`;
}

/**
 * A tooltip with more lines under it (the ignored folder overrides); null
 * when there is nothing to show.
 */
export function withLines(text, lines) {
  const all = [text, ...(lines ?? [])].filter(Boolean);
  return all.length ? all.join("\n") : null;
}
