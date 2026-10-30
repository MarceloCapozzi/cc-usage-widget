// The key notice, above the session list: publisher keys waiting for the
// viewer's user (a new machine's first key, or a known machine's new one).
//
// The list is `RemoteStatus.pending`, from the `remote-status` event, and
// `remote_pending_keys` at startup (app/src/remote/review.rs). One compact
// row per key, at most MAX_NOTICE_ROWS: the words (key-text.js), then Accept
// and Reject, which call `accept_publisher_key` / `reject_publisher_key`
// with the key's line. Nothing is accepted or rejected but by those clicks;
// the backend then sends the new list, and the notice follows it. An error
// stays on the row until the next list.

import { $, create } from "./dom.js";
import { keyNoticeParts, keyNoticeTitle, MAX_NOTICE_ROWS, moreKeysText } from "./key-text.js";
import { invoke, listen } from "./tauri.js";
import { fitSoon } from "./window.js";

const elNotice = $("key-notice");

/** What is drawn now, to skip a repaint when only counts moved. */
let shownKey = null;

function noticeRow(p) {
  const row = create("div", "key-row");
  row.title = keyNoticeTitle(p);
  const [lead, id, tail] = keyNoticeParts(p);
  const text = create("span", "key-text");
  text.append(document.createTextNode(lead), create("span", "key-id", id), document.createTextNode(tail));
  const accept = create("button", "chip key-accept", "Accept");
  const reject = create("button", "chip", "Reject");
  for (const b of [accept, reject]) b.type = "button";
  accept.setAttribute("aria-label", `Accept the key ${p.kid} of ${p.alias}`);
  reject.setAttribute("aria-label", `Reject the key ${p.kid} of ${p.alias}`);
  const error = create("span", "key-error");
  error.hidden = true;
  const decide = async (command) => {
    accept.disabled = reject.disabled = true;
    error.hidden = true;
    try {
      await invoke(command, { alias: p.alias, key: p.key });
      // The `remote-status` event that follows redraws the notice.
    } catch (err) {
      error.textContent = String(err);
      error.hidden = false;
      accept.disabled = reject.disabled = false;
      fitSoon();
    }
  };
  accept.addEventListener("click", () => decide("accept_publisher_key"));
  reject.addEventListener("click", () => decide("reject_publisher_key"));
  const actions = create("span", "key-actions");
  actions.append(accept, reject);
  row.append(text, actions, error);
  return row;
}

function paintNotice(pending) {
  if (!Array.isArray(pending)) return;
  const key = JSON.stringify(pending.map((p) => [p.alias, p.key, p.known]));
  if (key === shownKey) {
    // Same keys: only the tooltips' counts move.
    pending.slice(0, MAX_NOTICE_ROWS).forEach((p, i) => {
      const row = elNotice.children[i];
      if (row) row.title = keyNoticeTitle(p);
    });
    return;
  }
  shownKey = key;
  elNotice.replaceChildren(...pending.slice(0, MAX_NOTICE_ROWS).map(noticeRow));
  const more = moreKeysText(pending.length);
  if (more) elNotice.append(create("div", "key-more", more));
  elNotice.hidden = pending.length === 0;
  fitSoon();
}

/** The notice follows the backend's `remote-status` events from the start. */
export function initKeyNotice() {
  listen("remote-status", (e) => paintNotice(e.payload?.pending));
  invoke("remote_pending_keys").then(paintNotice, (err) => console.error("remote_pending_keys", err));
}
