// Formatting: counters, spans, dates and numbers as the panel writes them.
//
// Pure: nothing here touches the DOM or the backend, so the unit tests in
// app/ui-tests/ import it under Node.

/** A running counter: "12s", "1m02s", "1h05m", "2d03h". */
export function elapsed(secs) {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${String(s % 60).padStart(2, "0")}s`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h${String(m % 60).padStart(2, "0")}m`;
  return `${Math.floor(h / 24)}d${String(h % 24).padStart(2, "0")}h`;
}

/** A coarse span, for "… ago" and countdowns: "48s", "4m", "2h24m", "4d23h". */
export function span(secs) {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h${String(m % 60).padStart(2, "0")}m`;
  return `${Math.floor(h / 24)}d${String(h % 24).padStart(2, "0")}h`;
}

/** Time left until an ISO date, "2h24m"; null if it already passed. */
export function timeUntil(iso) {
  if (!iso) return null;
  const ms = new Date(iso).getTime() - Date.now();
  if (!Number.isFinite(ms) || ms <= 0) return null;
  // Minute granularity: a reset countdown in seconds is noise.
  return span(Math.max(60, ms / 1000));
}

/** Has the reset at this ISO date already happened? False for a missing or unreadable one. */
export function resetPassed(iso, now = Date.now()) {
  if (!iso) return false;
  const at = new Date(iso).getTime();
  return Number.isFinite(at) && at <= now;
}

export function timeAgo(iso) {
  const ms = Date.now() - new Date(iso).getTime();
  const min = Math.round(ms / 60000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const h = Math.floor(min / 60);
  return h < 24 ? `${h} h ago` : `${Math.floor(h / 24)} d ago`;
}

export function pct(v) {
  return `${Math.round(v)}%`;
}

export const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

/** Severity comes from the server; the percentage is only the fallback. */
export function severity(limit) {
  const s = limit.severity;
  if (s === "Critical" || s?.Unknown === "critical") return "critical";
  if (s === "Warning") return "warning";
  if (limit.percent >= 95) return "critical";
  if (limit.percent >= 80) return "warning";
  return "normal";
}

/**
 * The footer meter's colour: green under 50% used, amber from 50, red from
 * 80. Earlier than the server's warning, so the meter turns before the
 * limit is close; the server's severity can only raise it.
 */
export function usageLevel(limit) {
  const p = limit.percent;
  const own = p >= 80 ? "crit" : p >= 50 ? "warn" : "ok";
  const server = { critical: "crit", warning: "warn" }[severity(limit)];
  const rank = { ok: 0, warn: 1, crit: 2 };
  return server && rank[server] > rank[own] ? server : own;
}

/** Cells lit in a ten-cell meter for a percentage used, 0 to 10. */
export function meterCells(percent) {
  if (!Number.isFinite(percent)) return 0;
  return Math.min(10, Math.max(0, Math.round(percent / 10)));
}

/** "577K", "1.2M", "940": context and token counts as the status line shows them. */
export function compact(n) {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
  return String(n);
}

/** "2026-10-06" → "Oct 6, 2026". */
export function longDate(iso) {
  const d = new Date(`${iso}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric", timeZone: "UTC" });
}

/** A local day as the activity report keys it: "2026-10-06". */
export const dayKey = (d) =>
  `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;

/** An hour of the day, 0 to 23, as "12 AM", "9 AM", "12 PM", "5 PM". */
export const hourName = (h) => (h === 0 ? "12 AM" : h === 12 ? "12 PM" : h < 12 ? `${h} AM` : `${h - 12} PM`);
