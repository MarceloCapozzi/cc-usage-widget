// Sums over the usage-per-project report (`usage_by_project`): its projects'
// tokens and its activity calendar's days, and the notice for models without
// a price (projects.js and activity.js draw them).
//
// Pure: nothing here touches the DOM or the backend, so the unit tests in
// app/ui-tests/ import it under Node.

/** Every token a project used: input, output and both kinds of cache. */
export const tokensOf = (t) => t.input + t.output + t.cache_creation + t.cache_read;

/**
 * Tokens per day of the machines shown, and each day's busiest hour (that of
 * its largest machine). `host` narrows it to one machine; null is every one.
 */
export function activityDays(activity, host) {
  const days = new Map();
  for (const m of activity?.machines ?? []) {
    if (host && m.host !== host) continue;
    for (const d of m.days) {
      const have = days.get(d.d) ?? { t: 0, h: d.h, top: 0 };
      have.t += d.t;
      if (d.t > have.top) { have.top = d.t; have.h = d.h; }
      days.set(d.d, have);
    }
  }
  return days;
}

/**
 * The notice under the project list when some models have no price: which
 * ones, and how to add a price, with the format inline (the example file
 * lives only in the source tree). `path` is the report's `pricing_path`,
 * the file in the data folder in force (it can be moved).
 */
export function missingPriceText(models, path) {
  if (!models?.length) return null;
  const where = path ? `put it in ${path}` : "put a pricing.json in the cc-usage data folder";
  return (
    `No list price for: ${models.join(", ")}. ` +
    `To add one, ${where}, in US dollars per million tokens, for example ` +
    `{"${models[0]}": {"input": 3, "output": 15, "cache_write": 3.75, "cache_read": 0.3}}. ` +
    "The cache prices are optional."
  );
}

/**
 * A calendar cell's level, 0 to 4: the day's share of the busiest day shown,
 * on a square root so a light day still shows.
 */
export const activityLevel = (v, max) => (!v || !max ? 0 : Math.max(1, Math.ceil(4 * Math.sqrt(v / max))));
