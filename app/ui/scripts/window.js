// The widget window: its height, moving it and resizing it.

import { $ } from "./dom.js";
import { state, VIEW_ID } from "./state.js";
import { currentWindow, invoke, listen } from "./tauri.js";

// ── Window height ───────────────────────────────────────────────────────────
//
// Until someone resizes the widget by hand, its height follows the content:
// this measures what the panel would need and asks
// `fit_height` for it (clamped to the window limits in window_state.rs, past
// which the view scrolls between a fixed header and footer). Once the size
// was chosen by hand the backend ignores these requests; a reset turns them
// back on and sends `fit-request`. The view never stretches (flex: 0 1 auto),
// so its scrollHeight is always its content.

let lastFit = 0;
let fitQueued = false;

// Fractional heights (getBoundingClientRect), rounded up once at the end:
// rows are 88.6 px tall, and summing rounded offsetHeights came out a pixel
// short, which clipped the last row's rule.
const heightOf = (id) => $(id).getBoundingClientRect().height;

function desiredHeight() {
  let content;
  if (state.view !== "sessions") {
    const v = $(VIEW_ID[state.view]);
    content = v.scrollHeight + (v.offsetHeight - v.clientHeight);
  } else {
    // The list is never squeezed (the view around it is), so it measures
    // what the view would need, plus the notice of publisher keys to review
    // above it (0 while hidden).
    content = heightOf("key-notice") + heightOf("session-list");
  }
  const panelBorder = 2;
  const bodyPadding = 16;
  return heightOf("flyout-head") + content + heightOf("flyout-foot") + panelBorder + bodyPadding;
}

export function fitSoon() {
  if (fitQueued) return;
  fitQueued = true;
  // setTimeout, not requestAnimationFrame: frames do not run while the
  // window is hidden, and it should already have the right size when shown.
  setTimeout(() => {
    fitQueued = false;
    const h = Math.ceil(desiredHeight());
    if (h === lastFit) return;
    lastFit = h;
    invoke("fit_height", { height: h }).catch((err) => console.error("fit_height", err));
  }, 0);
}

/** Refits whenever what is measured changes size, and on the backend's request. */
export function initFit() {
  new ResizeObserver(fitSoon).observe($("session-list"));
  new ResizeObserver(fitSoon).observe($("key-notice"));
  new ResizeObserver(fitSoon).observe($("flyout-foot"));
  for (const id of ["history", "activity", "live-section", "projects", "view-settings"]) {
    new ResizeObserver(fitSoon).observe($(id));
  }

  // After a reset the backend follows the content again: measure afresh even
  // if the content did not change since the last request.
  listen("fit-request", () => {
    lastFit = 0;
    fitSoon();
  });
}

// ── Moving and resizing ─────────────────────────────────────────────────────
//
// The window rules. The widget moves when the
// header or any empty part of the panel is dragged; rows (clickable to focus
// a session), buttons, fields, the chart and the scrollbars keep their own
// use. A Tauri drag region is not used because over one a double click
// maximizes the window, and a deep region would also swallow the
// scrollbars.
//
// A double click on those same drag areas hides the widget, like ⏻ and
// Escape. So that the first press of a double click does not already move
// the window, a press only arms the drag; the window starts moving once the
// pointer has travelled a few pixels with the button held, as the grips do
// for resizing. The second press (`detail === 2`) disarms it and hides.

const NO_DRAG = "button, a, input, select, textarea, label, .sess, .grip, .ranges, .chart";

/** Is the press on the element's own scrollbar rather than its content? */
function onScrollbar(event, el) {
  return (
    (el.clientWidth > 0 && event.offsetX > el.clientWidth) ||
    (el.clientHeight > 0 && event.offsetY > el.clientHeight)
  );
}

/** Would a press here move the window (the header or empty panel space)? */
function isDragArea(event) {
  const target = event.target;
  if (!(target instanceof Element) || !target.closest(".flyout")) return false;
  return !target.closest(NO_DRAG) && !onScrollbar(event, target);
}

let pendingMove = null;

/* Resizing from the grips along the edges.
 *
 * The resize does not start on `pointerdown` but once the pointer has moved a
 * few pixels, so a plain click is not taken for a drag and a double click
 * stays a double click. */
const DRAG_THRESHOLD = 4;
let grab = null;

export function initMoveResize() {
  document.addEventListener("mousedown", (event) => {
    if (event.button !== 0 || !isDragArea(event)) return;
    event.preventDefault();
    if (event.detail === 2) {
      pendingMove = null;
      invoke("close_flyout").catch((err) => console.error("close_flyout", err));
      return;
    }
    pendingMove = event.detail === 1 ? { x: event.clientX, y: event.clientY } : null;
  });

  document.addEventListener("mousemove", (event) => {
    if (!pendingMove) return;
    if (!(event.buttons & 1)) {
      pendingMove = null;
      return;
    }
    const travelled = Math.hypot(event.clientX - pendingMove.x, event.clientY - pendingMove.y);
    if (travelled < DRAG_THRESHOLD) return;
    pendingMove = null;
    currentWindow()
      .startDragging()
      .catch((err) => console.error("startDragging", err));
  });

  document.addEventListener("mouseup", () => (pendingMove = null));

  for (const grip of document.querySelectorAll(".grip")) {
    grip.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      grab = { dir: grip.dataset.dir, x: event.clientX, y: event.clientY };
    });

    /* Double click on an edge: back to the original size, where the widget
     * already is. `mousedown` with `detail === 2` rather than `dblclick`, as in
     * the calendar: the first press may already be on its way to a drag. */
    grip.addEventListener("mousedown", (event) => {
      if (event.detail !== 2 || event.button !== 0) return;
      // `pointerdown` has just armed a resize for this second press. Left
      // armed, the slightest tremor after the double click would start
      // dragging the edge that was meant to reset it.
      grab = null;
      event.preventDefault();
      invoke("reset_window").catch((err) => console.error("reset_window", err));
    });
  }

  document.addEventListener("pointermove", (event) => {
    if (!grab) return;
    const travelled = Math.hypot(event.clientX - grab.x, event.clientY - grab.y);
    if (travelled < DRAG_THRESHOLD) return;
    const direction = grab.dir;
    grab = null;
    invoke("start_resize", { direction }).catch((err) => console.error("start_resize", err));
  });

  document.addEventListener("pointerup", () => (grab = null));
}
