// The words and colours of a session row (sessions.js draws the row).
//
// Pure: nothing here touches the DOM or the backend, so the unit tests in
// app/ui-tests/ import it under Node. Rows are SessionView (camelCase, times
// in Unix seconds), see sessions.js.

import { elapsed, span } from "./format.js";

export const STATE_LABEL = {
  needsYou: "needs you",
  working: "working",
  done: "done",
  failed: "failed",
  idle: "idle",
  offline: "offline",
};

/** The mockup's `.state` modifier classes. */
export const STATE_CLASS = {
  needsYou: "need",
  working: "work",
  done: "done",
  failed: "fail",
  idle: "idle",
  offline: "off",
};

// Claude Code's `/color` names. Blue and green are the mockup's bar colours;
// anything else that is not a hex colour draws no bar.
export const SESSION_COLORS = {
  red: "#d9534f",
  orange: "#eb8034",
  yellow: "#e0b13a",
  green: "#3fb67a",
  cyan: "#2fb3c4",
  blue: "#4c7cf0",
  purple: "#9b6bdc",
  pink: "#e46fb0",
};

export function colorOf(name) {
  if (!name) return null;
  const key = String(name).trim().toLowerCase();
  if (SESSION_COLORS[key]) return SESSION_COLORS[key];
  return /^#[0-9a-f]{3,8}$/i.test(key) ? key : null;
}

export const nowSecs = () => Date.now() / 1000;

/** Rows from another machine carry `origin: {kind: "remote", alias}`. */
export const isRemote = (s) => s.origin?.kind === "remote";

/** The last folder of a path, for rows that have nothing better to say. */
export function folderOf(path) {
  if (!path) return "";
  const parts = String(path).split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? "";
}

/**
 * The time under the state label, as the mockup writes it: a running
 * counter while the session needs you or works ("12s"), how long ago it
 * finished ("1m ago"), how long ago a remote machine last reported ("48s
 * ago"), and for an offline row how long it has been offline ("4m").
 * Returns [unix seconds to count from, format].
 */
export function stateTime(s) {
  if (s.state === "offline") return [s.stateSince, "span"];
  if (isRemote(s)) return [s.lastActivity, "ago"];
  if (s.state === "working") return [s.turnStartedAt || s.stateSince, "elapsed"];
  if (s.state === "needsYou") return [s.stateSince, "elapsed"];
  return [s.stateSince, "ago"];
}

export function formatSince(since, fmt) {
  const secs = nowSecs() - since;
  if (fmt === "span") return span(secs);
  if (fmt === "ago") return `${span(secs)} ago`;
  return elapsed(secs);
}

/** Line 2: what the session is doing, asking or saying. */
export function subLine(s) {
  switch (s.state) {
    case "needsYou":
      return s.question || "Waiting for your answer";
    case "working":
      if (s.currentTool) {
        return s.toolSummary ? `${s.currentTool} · ${s.toolSummary}` : s.currentTool;
      }
      {
        // Background agents keep a session working after its own turn ended.
        const running = (s.subagents || []).filter((a) => a?.state === "working").length;
        if (running) return running === 1 ? "1 agent running" : `${running} agents running`;
      }
      return s.hasHooks ? "Thinking…" : folderOf(s.cwd);
    case "done":
      return s.lastMessage || "Turn finished";
    case "failed":
      return s.errorMessage || "Failed without details";
    case "offline": {
      if (!s.lastActivity) return "no heartbeat";
      const at = new Date(s.lastActivity * 1000);
      return `no heartbeat since ${at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })}`;
    }
    default:
      return (s.hasHooks && s.lastMessage) || folderOf(s.cwd);
  }
}

/**
 * The subagent tag at the end of line 3, from a row's subagents. Pure, so the
 * wording lives in one place:
 *   none finished   "5 agents"    (amber)
 *   some finished   "2/5 agents"  (amber)
 *   all finished    "5/5 agents"  (green, `allDone`)
 * One subagent reads "1 agent" / "1/1 agent". `title` is the tooltip,
 * "2 of 5 subagents finished". Null without subagents.
 */
export function agentsTag(agents) {
  const total = Array.isArray(agents) ? agents.length : 0;
  if (!total) return null;
  const finished = agents.filter((a) => a?.state === "done").length;
  const noun = total === 1 ? "agent" : "agents";
  return {
    text: finished ? `${finished}/${total} ${noun}` : `${total} ${noun}`,
    allDone: finished === total,
    title: `${finished} of ${total} ${total === 1 ? "subagent" : "subagents"} finished`,
  };
}

/**
 * One subagent's line in the row's agents block (SubagentView):
 *   "<type> · <task> · <tool> · <tool summary>"   while it runs
 *   "<type> · <task> · finished"                  once it stopped
 * Missing parts are left out, and so is a task that only repeats the type
 * (the hook falls back to the type when the call had no description).
 * "subagent" stands in when nothing else is known, e.g. a remote row's
 * placeholder.
 */
export function agentLine(a) {
  const text = (v) => (typeof v === "string" ? v.trim() : "");
  const type = text(a?.agentType);
  const task = text(a?.description);
  const parts = [type];
  if (task.toLowerCase() !== type.toLowerCase()) parts.push(task);
  const done = a?.state === "done";
  if (!done) parts.push(text(a?.currentTool), text(a?.toolSummary));
  const what = parts.filter(Boolean);
  if (!what.length) what.push("subagent");
  if (done) what.push("finished");
  return what.join(" · ");
}

/** The row tooltip's line on where a click lands, from a `focus_preview` report. */
export function focusHintText(report) {
  switch (report?.outcome) {
    case "precise":
      return "Click: brings this session's window to the front";
    case "app": {
      const exe = (report.windowExe || "").toLowerCase();
      const app = exe.includes("windowsterminal")
        ? "Windows Terminal"
        : exe.includes("code")
        ? "VS Code"
        : "the app";
      return `Click: brings ${app} to the front (not the exact tab)`;
    }
    case "notFound":
      return "This session's window was not found";
    default:
      return null;
  }
}
