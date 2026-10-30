// node --test app/ui/tests/
import assert from "node:assert/strict";
import { test } from "node:test";

import { activityDays, activityLevel, missingPriceText, tokensOf } from "../ui/scripts/report-data.js";

const activity = {
  machines: [
    { host: "pc", days: [{ d: "2026-10-13", t: 100, h: 9 }, { d: "2026-10-14", t: 10, h: 22 }] },
    { host: "wk-01", remote: true, days: [{ d: "2026-10-14", t: 50, h: 15 }] },
  ],
};

test("activityDays adds every machine's tokens per day and keeps the largest machine's peak hour", () => {
  const days = activityDays(activity, null);
  assert.deepEqual([...days.keys()], ["2026-10-13", "2026-10-14"]);
  assert.equal(days.get("2026-10-13").t, 100);
  assert.equal(days.get("2026-10-14").t, 60);
  assert.equal(days.get("2026-10-14").h, 15);
});

test("activityDays narrows to one machine", () => {
  const days = activityDays(activity, "pc");
  assert.equal(days.get("2026-10-14").t, 10);
  assert.equal(days.get("2026-10-14").h, 22);
  assert.equal(activityDays(activity, "gone").size, 0);
});

test("activityDays takes a missing calendar as empty", () => {
  assert.equal(activityDays(null, null).size, 0);
  assert.equal(activityDays({}, null).size, 0);
});

test("activityLevel is 0 for nothing, 1 for a light day and 4 for the busiest", () => {
  assert.equal(activityLevel(0, 100), 0);
  assert.equal(activityLevel(5, 0), 0);
  assert.equal(activityLevel(1, 1_000_000), 1);
  assert.equal(activityLevel(25, 100), 2);
  assert.equal(activityLevel(50, 100), 3);
  assert.equal(activityLevel(100, 100), 4);
});

test("tokensOf counts input, output and both caches", () => {
  assert.equal(tokensOf({ input: 1, output: 2, cache_creation: 30, cache_read: 400 }), 433);
});

test("missingPriceText lists the models and shows the pricing.json format inline", () => {
  assert.equal(missingPriceText([]), null);
  assert.equal(missingPriceText(undefined), null);
  const text = missingPriceText(["claude-x-1", "other-model"]);
  assert.match(text, /^No list price for: claude-x-1, other-model\. /);
  assert.match(text, /per million tokens/);
  assert.match(text, /\{"claude-x-1": \{"input": 3, "output": 15, "cache_write": 3\.75, "cache_read": 0\.3\}\}/);
  // Neither a file that only the source tree has nor a fixed data path.
  assert.doesNotMatch(text, /tools\//);
  assert.doesNotMatch(text, /LOCALAPPDATA/);
  // The example in the notice is valid JSON in the shape the backend reads.
  const example = JSON.parse(text.match(/\{.*\}\}/)[0]);
  assert.deepEqual(Object.keys(example["claude-x-1"]), ["input", "output", "cache_write", "cache_read"]);
  // Without the backend's path it names the folder; with it, the file.
  assert.match(text, /put a pricing\.json in the cc-usage data folder, in US dollars/);
  const withPath = missingPriceText(["claude-x-1"], "D:\\data\\cc-usage\\pricing.json");
  assert.match(withPath, /To add one, put it in D:\\data\\cc-usage\\pricing\.json, in US dollars per million tokens/);
});
