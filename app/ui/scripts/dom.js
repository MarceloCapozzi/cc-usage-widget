// DOM construction.
//
// Everything with createElement/textContent and never innerHTML with data: an
// email, a model name or a session title comes from outside, and there is no
// reason to give them the chance to inject markup.

export const $ = (id) => document.getElementById(id);

export function create(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text !== undefined && text !== null) el.textContent = text;
  return el;
}

export const NS = "http://www.w3.org/2000/svg";

export const svgEl = (tag, attrs = {}) => {
  const n = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
  return n;
};

/** A note under a control: shown with `text`, hidden without; red for "error". */
export function dataNote(el, text, kind) {
  el.hidden = !text;
  el.textContent = text ?? "";
  el.classList.toggle("error", kind === "error");
}
