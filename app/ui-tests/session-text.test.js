// node --test app/ui/tests/
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  agentLine,
  agentsTag,
  colorOf,
  focusHintText,
  folderOf,
  formatSince,
  isRemote,
  stateTime,
  subLine,
} from "../ui/scripts/session-text.js";

const now = () => Date.now() / 1000;

test("agentsTag counts the finished subagents", () => {
  assert.equal(agentsTag(undefined), null);
  assert.equal(agentsTag([]), null);
  const working = { state: "working" }, done = { state: "done" };
  assert.deepEqual(agentsTag([working, working, working, working, working]), {
    text: "5 agents",
    allDone: false,
    title: "0 of 5 subagents finished",
  });
  assert.deepEqual(agentsTag([done, done, working, working, working]), {
    text: "2/5 agents",
    allDone: false,
    title: "2 of 5 subagents finished",
  });
  assert.equal(agentsTag([done, done]).text, "2/2 agents");
  assert.equal(agentsTag([done, done]).allDone, true);
  assert.deepEqual(agentsTag([working]), { text: "1 agent", allDone: false, title: "0 of 1 subagent finished" });
  assert.equal(agentsTag([done]).text, "1/1 agent");
});

test("agentLine names a subagent's type, task, tool and summary, leaving out what is missing", () => {
  const running = (a) => ({ state: "working", ...a });
  assert.equal(
    agentLine(running({ agentType: "Explore", description: "Find usages", currentTool: "Grep", toolSummary: "remote_text" })),
    "Explore · Find usages · Grep · remote_text"
  );
  assert.equal(agentLine(running({ description: "Find usages", currentTool: "Grep" })), "Find usages · Grep");
  assert.equal(agentLine(running({ agentType: "Plan", toolSummary: "lib.rs" })), "Plan · lib.rs");
  assert.equal(agentLine(running({ agentType: "Plan", description: null, currentTool: "  " })), "Plan");
  // A task that only repeats the type (no description in the call).
  assert.equal(agentLine(running({ agentType: "Explore", description: "explore" })), "Explore");
  // Remote placeholders and agents nothing is known about.
  assert.equal(agentLine(running({ agentType: null })), "subagent");
  assert.equal(agentLine(undefined), "subagent");
  // Finished: the type and task stay, the tool goes.
  assert.equal(
    agentLine({ state: "done", agentType: "Plan", description: "Plan the fix", currentTool: "Bash" }),
    "Plan · Plan the fix · finished"
  );
  assert.equal(agentLine({ state: "done" }), "subagent · finished");
});

test("colorOf knows Claude Code's /color names and hex colours, nothing else", () => {
  assert.equal(colorOf("blue"), "#4c7cf0");
  assert.equal(colorOf("  Green "), "#3fb67a");
  assert.equal(colorOf("#ABC"), "#abc");
  assert.equal(colorOf("#12345678"), "#12345678");
  assert.equal(colorOf("#12"), null);
  assert.equal(colorOf("teal"), null);
  assert.equal(colorOf("red; background: url(x)"), null);
  assert.equal(colorOf(null), null);
});

test("folderOf is the last folder of a Windows or POSIX path", () => {
  assert.equal(folderOf("C:\\repo\\cc-usage\\"), "cc-usage");
  assert.equal(folderOf("/home/me/proj"), "proj");
  assert.equal(folderOf(""), "");
  assert.equal(folderOf(undefined), "");
});

test("isRemote looks at the origin", () => {
  assert.equal(isRemote({ origin: { kind: "remote", alias: "wk-01" } }), true);
  assert.equal(isRemote({ origin: { kind: "local", alias: "pc" } }), false);
  assert.equal(isRemote({}), false);
});

test("stateTime counts from the right moment in the right format", () => {
  assert.deepEqual(stateTime({ state: "offline", stateSince: 5, lastActivity: 3 }), [5, "span"]);
  assert.deepEqual(stateTime({ state: "working", stateSince: 5, lastActivity: 3, origin: { kind: "remote" } }), [3, "ago"]);
  assert.deepEqual(stateTime({ state: "working", stateSince: 5, turnStartedAt: 4 }), [4, "elapsed"]);
  assert.deepEqual(stateTime({ state: "working", stateSince: 5, turnStartedAt: 0 }), [5, "elapsed"]);
  assert.deepEqual(stateTime({ state: "needsYou", stateSince: 5 }), [5, "elapsed"]);
  assert.deepEqual(stateTime({ state: "done", stateSince: 5 }), [5, "ago"]);
});

test("formatSince writes the three formats", () => {
  const since = now() - 62;
  assert.equal(formatSince(since, "elapsed"), "1m02s");
  assert.equal(formatSince(since, "span"), "1m");
  assert.equal(formatSince(since, "ago"), "1m ago");
});

test("subLine says what the session is doing, asking or saying", () => {
  assert.equal(subLine({ state: "needsYou", question: "Allow Bash?" }), "Allow Bash?");
  assert.equal(subLine({ state: "needsYou" }), "Waiting for your answer");
  assert.equal(subLine({ state: "working", currentTool: "Grep", toolSummary: "TODO" }), "Grep · TODO");
  assert.equal(subLine({ state: "working", currentTool: "Grep" }), "Grep");
  assert.equal(subLine({ state: "working", hasHooks: true }), "Thinking…");
  const agents = [{ state: "working" }, { state: "working" }, { state: "done" }];
  assert.equal(subLine({ state: "working", hasHooks: true, subagents: agents }), "2 agents running");
  assert.equal(subLine({ state: "working", subagents: [{ state: "working" }] }), "1 agent running");
  assert.equal(subLine({ state: "working", currentTool: "Bash", subagents: agents }), "Bash");
  assert.equal(subLine({ state: "working", hasHooks: false, cwd: "C:\\src\\app" }), "app");
  assert.equal(subLine({ state: "done" }), "Turn finished");
  assert.equal(subLine({ state: "failed", errorMessage: "rate limited" }), "rate limited");
  assert.equal(subLine({ state: "offline" }), "no heartbeat");
  assert.match(subLine({ state: "offline", lastActivity: now() }), /^no heartbeat since \d\d:\d\d$/);
  assert.equal(subLine({ state: "idle", hasHooks: true, lastMessage: "Done." }), "Done.");
  assert.equal(subLine({ state: "idle", hasHooks: false, lastMessage: "Done.", cwd: "/x/y" }), "y");
});

test("focusHintText says where a click lands", () => {
  assert.equal(focusHintText({ outcome: "precise" }), "Click: brings this session's window to the front");
  assert.equal(
    focusHintText({ outcome: "app", windowExe: "C:\\...\\WindowsTerminal.exe" }),
    "Click: brings Windows Terminal to the front (not the exact tab)"
  );
  assert.equal(focusHintText({ outcome: "app", windowExe: "Code.exe" }), "Click: brings VS Code to the front (not the exact tab)");
  assert.equal(focusHintText({ outcome: "app" }), "Click: brings the app to the front (not the exact tab)");
  assert.equal(focusHintText({ outcome: "notFound" }), "This session's window was not found");
  assert.equal(focusHintText({ outcome: "unsupported" }), null);
  assert.equal(focusHintText(null), null);
});
