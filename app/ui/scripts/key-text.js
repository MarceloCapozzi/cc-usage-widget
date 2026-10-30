// The words of the key notice (keys.js) and of "Replace key…" (role.js):
// publisher keys waiting for the viewer's user, as the backend sends them
// (app/src/remote/review.rs, PendingKey: camelCase, times in Unix seconds).
//
// Pure: nothing here touches the DOM or the backend, so the unit tests in
// app/ui-tests/ import it under Node.

import { plural, span } from "./format.js";

/** Notice rows drawn at most; the rest are summed up in one line. */
export const MAX_NOTICE_ROWS = 3;

/** The short fingerprint: the first 8 characters of the key's base64. */
export function keyFingerprint(p) {
  if (p?.fingerprint) return p.fingerprint;
  const line = p?.key ?? "";
  const cut = line.indexOf(":");
  return (cut >= 0 ? line.slice(cut + 1) : line).slice(0, 8);
}

/**
 * The notice line of one waiting key, in parts so the key id and the
 * fingerprint can be set in the mono face: `[lead, kidAndFingerprint, tail]`.
 * A new machine: "New machine devbox wants to publish · key p20261014-3f9a
 * AbCdEfGh". A known one: "devbox presents a new key (p20261014-3f9a AbCdEfGh)".
 */
export function keyNoticeParts(p) {
  const id = `${p.kid} ${keyFingerprint(p)}`;
  if (p.known) return [`${p.alias} presents a new key (`, id, ")"];
  return [`New machine ${p.alias} wants to publish · key `, id, ""];
}

/** The notice line as plain text (the parts joined). */
export const keyNoticeText = (p) => keyNoticeParts(p).join("");

/** The row's tooltip: what is held back and how to check the key. */
export function keyNoticeTitle(p, now = Date.now() / 1000) {
  const seen =
    p.count > 1
      ? `First seen ${span(now - p.firstSeen)} ago, last ${span(now - p.lastSeen)} ago`
      : `Seen ${span(now - p.firstSeen)} ago`;
  return (
    `${seen}; ${plural(p.count, "value", "values")} held back.\n` +
    `Accept only if ${p.alias} shows this key (Settings → Role, or cc-publish keygen --if-missing): ${p.key}\n` +
    "Accept: its values show from its next write. Reject: they are dropped."
  );
}

/** The line under the rows when more keys wait than are drawn; null otherwise. */
export function moreKeysText(total) {
  const more = total - MAX_NOTICE_ROWS;
  return more > 0 ? `+ ${plural(more, "more key", "more keys")} to review` : null;
}

/** "Replace key…": what replacing this machine's key means, before the alias is typed. */
export function replaceKeyWarning(alias) {
  return (
    `This replaces ${alias}'s identity: the viewer holds back its values until you accept ` +
    `the new key there. Type ${alias} to confirm.`
  );
}

/** Whether the typed confirmation matches the alias (spaces around it ignored, as the backend does). */
export const confirmsAlias = (typed, alias) => !!alias && (typed ?? "").trim() === alias;
