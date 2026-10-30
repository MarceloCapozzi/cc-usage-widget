// Usage view: the account line and the current session's context, and
// `render`, which paints a usage snapshot (footer and this view).

import { $, create } from "./dom.js";
import { renderUsage } from "./footer.js";
import { compact, pct } from "./format.js";
import { fitSoon } from "./window.js";

/** A usage block with a progress bar (usage view only). */
function block({ label, percent, left, right, markAt }) {
  const root = create("div", "block");
  const row = create("div", "row");
  row.append(create("span", "label", label), create("span", "pct", pct(percent)));
  root.append(row);

  const bar = create("div", "bar-track");
  const fill = create("div", "fill");
  fill.style.width = `${Math.max(0, Math.min(100, percent))}%`;
  bar.append(fill);
  if (typeof markAt === "number") {
    const mark = create("div", "compact-mark");
    mark.style.left = `${Math.max(0, Math.min(100, markAt))}%`;
    bar.append(mark);
  }
  root.append(bar);

  if (left || right) {
    const meta = create("div", "meta");
    meta.append(create("span", null, left ?? ""), create("span", null, right ?? ""));
    root.append(meta);
  }
  return root;
}

function renderSecondary(s) {
  // The email stands out (text colour, bold); the plan follows it, muted.
  const account = $("account-text");
  account.replaceChildren();
  if (s.email) account.append(create("span", "account-email", s.email));
  if (s.plan_label) {
    account.append(create("span", "account-plan", s.email ? ` · ${s.plan_label}` : s.plan_label));
  }
  if (!s.email && !s.plan_label) account.textContent = "Account unknown";
  account.title = [s.email, s.plan_label].filter(Boolean).join(" · ");

  // Context window from the status line files, only while fresh (the
  // backend resolves freshness so the rule lives in one place).
  const live = $("live-blocks");
  live.replaceChildren();
  const ctx = s.live_fresh ? s.live?.context : null;
  if (ctx) {
    live.append(
      block({
        label: "Current session",
        percent: ctx.percent,
        left: tokensLabel(ctx),
        right: ctx.model ?? "",
      })
    );
  }

  const notices = $("notices");
  notices.replaceChildren();
  // Every session row already shows its context (exact from the status line,
  // or estimated from the transcript in VS Code panel sessions, which never
  // run one). This section only adds the status line's detail when there is
  // a fresh one, so without it the section is simply not shown.
  $("live-section").hidden = !ctx;
}

function tokensLabel(ctx) {
  if (ctx.used_tokens == null || ctx.total_tokens == null) return "";
  return `${compact(ctx.used_tokens)} / ${compact(ctx.total_tokens)}`;
}

/** Paints a usage snapshot (`usage` event, `last_snapshot`): the footer and the usage view. */
export function render(s) {
  renderUsage(s);
  renderSecondary(s);
  fitSoon();
}
