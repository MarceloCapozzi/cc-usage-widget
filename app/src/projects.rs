//! Usage per project: this machine's, read from Claude Code's transcripts
//! ([`cc_local::projects`]), and other machines', from the project payloads
//! their publishers send ([`cc_core::projects::ProjectPayload`]), priced
//! with one table. Something a browser extension cannot see, because this
//! data only exists on disk.
//!
//! # About cost
//!
//! Prices are **not** hardcoded. A made-up number in a cost table propagates
//! into real decisions, and prices change. The widget reads `pricing.json` (see
//! `tools/pricing.example.json`); if a model is not in that table, it reports
//! tokens and leaves the cost as `None`, which is displayed as "n/a" instead of
//! as zero.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

pub use cc_core::projects::Tokens;
pub use cc_local::projects::projects_dir;

/// Price per million tokens, per model.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
}

pub type PricingTable = HashMap<String, ModelPricing>;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectUsage {
    /// The machine the project ran on: this machine's name, or the alias of
    /// the publisher that sent it. Filled in by the caller for local ones.
    pub host: Option<String>,
    /// True when it came from another machine's publisher.
    pub remote: bool,
    /// Readable name, derived from the `cwd`.
    pub name: String,
    pub path: String,
    pub tokens: Tokens,
    pub by_model: HashMap<String, Tokens>,
    /// `None` when the price of some model used is missing: it means "unknown",
    /// not zero.
    pub cost_usd: Option<f64>,
    pub sessions: usize,
    pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub projects: Vec<ProjectUsage>,
    pub totals: Tokens,
    pub cost_usd: Option<f64>,
    /// Models seen without a price in the table. Reported so the user knows why
    /// a cost appears incomplete, instead of suspecting a bug.
    pub models_without_pricing: Vec<String>,
    /// Date of the built-in list prices, so the UI can say how current they are.
    pub prices_as_of: &'static str,
}

/// When the built-in prices were taken from Anthropic's published list prices.
pub const BUILTIN_PRICES_AS_OF: &str = "2026-10-06";

/// Anthropic's first-party API list prices, dollars per million tokens:
/// (model, input, output, 5-minute cache write, cache read).
///
/// What a subscription (Pro, Max, Team) pays is not per token, so the report
/// calls the result an API-equivalent cost. Cache writes use the 5-minute
/// rate; where Anthropic publishes no model-specific cache rate the standard
/// multipliers apply (writes 1.25x input, reads 0.1x input). Claude Haiku 5.5
/// uses its rate card for prompts of 100K tokens or fewer. A pricing.json in
/// the data folder overrides any entry, for newer prices or other providers.
const BUILTIN_PRICES: &[(&str, f64, f64, f64, f64)] = &[
    ("claude-fable-5-1", 10.0, 50.0, 12.5, 0.25),
    ("claude-fable-5", 10.0, 50.0, 12.5, 1.0),
    ("claude-opus-5-5", 4.0, 20.0, 5.0, 0.20),
    ("claude-opus-5", 5.0, 25.0, 6.25, 0.50),
    ("claude-opus-4-8", 5.0, 25.0, 6.25, 0.50),
    ("claude-opus-4-7", 5.0, 25.0, 6.25, 0.50),
    ("claude-opus-4-6", 5.0, 25.0, 6.25, 0.50),
    ("claude-sonnet-5-5", 2.0, 10.0, 2.5, 0.20),
    ("claude-sonnet-5", 2.0, 10.0, 2.5, 0.20),
    ("claude-sonnet-4-6", 3.0, 15.0, 3.75, 0.30),
    ("claude-haiku-5-5", 0.10, 0.50, 0.125, 0.01),
    ("claude-haiku-4-5", 1.0, 5.0, 1.25, 0.10),
];

/// The built-in table, see [`BUILTIN_PRICES`].
pub fn builtin_pricing() -> PricingTable {
    BUILTIN_PRICES
        .iter()
        .map(|&(model, input, output, cache_write, cache_read)| {
            (
                model.to_string(),
                ModelPricing {
                    input,
                    output,
                    cache_write: Some(cache_write),
                    cache_read: Some(cache_read),
                },
            )
        })
        .collect()
}

/// The built-in prices with the user's `pricing.json` laid on top: an entry in
/// the file replaces the built-in one for that model.
pub fn load_pricing() -> PricingTable {
    let path = crate::paths::file(crate::paths::PRICING);
    let mut table = builtin_pricing();
    if let Some(user) = std::fs::read_to_string(path)
        .ok()
        .map(|raw| parse_pricing(&raw))
    {
        table.extend(user);
    }
    table
}

/// Pricing for a transcript's model id, also when it carries a date suffix
/// (`claude-haiku-4-5-20251001` is priced as `claude-haiku-4-5`).
fn price_for<'a>(pricing: &'a PricingTable, model: &str) -> Option<&'a ModelPricing> {
    pricing.get(model).or_else(|| {
        let (base, suffix) = model.rsplit_once('-')?;
        (suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()))
            .then(|| pricing.get(base))
            .flatten()
    })
}

/// Lenient parsing: keys that are not a rate are skipped instead of
/// invalidating the whole file.
///
/// JSON has no comments, so the natural way to document the table is an extra
/// key (`_README`, loose notes). With a direct `from_str` into
/// `HashMap<String, ModelPricing>` that fails the whole file and the table ends
/// up empty **silently**: the user sees "n/a" on every project with no hint
/// that the problem is their comment.
pub fn parse_pricing(raw: &str) -> PricingTable {
    let Ok(map) = serde_json::from_str::<HashMap<String, serde_json::Value>>(raw) else {
        return PricingTable::new();
    };
    map.into_iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .filter_map(|(k, v)| {
            serde_json::from_value::<ModelPricing>(v)
                .ok()
                .map(|p| (k, p))
        })
        .collect()
}

/// This machine's projects, priced. `since_days` bounds the window; `None`
/// reads everything.
pub fn scan(since_days: Option<i64>, pricing: &PricingTable) -> Report {
    scan_dir(&projects_dir(), since_days, pricing)
}

/// [`scan`] over the transcripts under `dir`.
pub fn scan_dir(dir: &Path, since_days: Option<i64>, pricing: &PricingTable) -> Report {
    report_from(local_usage(dir, since_days), pricing)
}

/// This machine's projects under `dir`, not priced yet, without a host.
pub fn local_usage(dir: &Path, since_days: Option<i64>) -> Vec<ProjectUsage> {
    local_scan(dir, since_days).0
}

/// [`local_usage`], and from the same pass the tokens per local day and
/// hour for the activity calendar.
pub fn local_scan(
    dir: &Path,
    since_days: Option<i64>,
) -> (
    Vec<ProjectUsage>,
    std::collections::BTreeMap<String, [u64; 24]>,
) {
    let scan = cc_local::projects::scan_all(dir, since_days);
    let projects = scan
        .projects
        .into_iter()
        .map(|p| ProjectUsage {
            host: None,
            remote: false,
            name: p.name,
            path: p.path,
            tokens: p.tokens,
            by_model: p.by_model,
            cost_usd: None,
            sessions: p.sessions as usize,
            last_seen: p
                .last_seen
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
        })
        .collect();
    (projects, scan.days)
}

/// Another machine's project, as its publisher sent it. Its path is not
/// sent, so it stays empty.
pub fn remote_usage(alias: &str, p: &cc_core::projects::ProjectPayload) -> ProjectUsage {
    ProjectUsage {
        host: Some(alias.to_string()),
        remote: true,
        name: p.name.clone(),
        path: String::new(),
        tokens: p.tokens(),
        by_model: p.by_model(),
        cost_usd: None,
        sessions: p.sessions as usize,
        last_seen: p.last.and_then(|t| chrono::DateTime::from_timestamp(t, 0)),
    }
}

/// Prices every project, adds up the totals and sorts by usage.
pub fn report_from(mut projects: Vec<ProjectUsage>, pricing: &PricingTable) -> Report {
    let mut unpriced: Vec<String> = Vec::new();
    let mut totals = Tokens::default();
    let mut total_cost = Some(0.0);
    for p in &mut projects {
        totals.add(&p.tokens);
        p.cost_usd = cost_of(&p.by_model, pricing, &mut unpriced);
        match (total_cost, p.cost_usd) {
            (Some(acc), Some(c)) => total_cost = Some(acc + c),
            // A single unpriced project leaves the total as "unknown": adding up
            // what is known and presenting it as the total would silently
            // underestimate.
            _ => total_cost = None,
        }
    }
    // Highest usage first: `Reverse` on the key instead of inverting the
    // comparator by hand.
    projects.sort_by_key(|p| std::cmp::Reverse(p.tokens.total()));

    unpriced.sort();
    unpriced.dedup();

    Report {
        projects,
        totals,
        cost_usd: total_cost,
        models_without_pricing: unpriced,
        prices_as_of: BUILTIN_PRICES_AS_OF,
    }
}

fn cost_of(
    by_model: &HashMap<String, Tokens>,
    pricing: &PricingTable,
    missing: &mut Vec<String>,
) -> Option<f64> {
    let mut total = 0.0;
    for (model, t) in by_model {
        let Some(p) = price_for(pricing, model) else {
            missing.push(model.clone());
            return None;
        };
        // Prices are expressed per million tokens.
        let m = 1_000_000.0;
        total += t.input as f64 / m * p.input;
        total += t.output as f64 / m * p.output;
        // If no cache price is declared, the input price is used as an
        // approximation: it is the closest one and never ends up as zero.
        total += t.cache_creation as f64 / m * p.cache_write.unwrap_or(p.input);
        total += t.cache_read as f64 / m * p.cache_read.unwrap_or(p.input);
    }
    Some(total)
}

/// How long the usage view's report is reused: opening the view again
/// within this shows the report already made instead of scanning every
/// transcript again.
pub const REPORT_REUSE: Duration = Duration::from_secs(30);

/// The usage view's last report (its JSON) and what it was made for. The
/// app holds it behind an async mutex for the whole scan, so a request
/// that arrives while one runs waits for it and reuses its report instead
/// of starting a second scan.
#[derive(Debug, Default)]
pub struct RecentReport {
    made: Option<(Instant, String, String)>,
}

impl RecentReport {
    /// The report made for `key` less than [`REPORT_REUSE`] before `now`.
    pub fn get(&self, key: &str, now: Instant) -> Option<String> {
        let (at, made_for, json) = self.made.as_ref()?;
        (made_for == key && now.saturating_duration_since(*at) < REPORT_REUSE).then(|| json.clone())
    }

    /// Keeps `json`, made for `key` at `now`.
    pub fn put(&mut self, key: String, json: String, now: Instant) {
        self.made = Some((now, key, json));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    #[test]
    fn a_recent_report_is_reused_for_the_same_request_only() {
        let t0 = Instant::now();
        let mut recent = RecentReport::default();
        assert_eq!(recent.get("30|wk-01", t0), None);
        recent.put("30|wk-01".into(), "{}".into(), t0);
        assert_eq!(
            recent.get("30|wk-01", t0 + Duration::from_secs(5)),
            Some("{}".into())
        );
        // Another window or machine name is another report.
        assert_eq!(recent.get("7|wk-01", t0), None);
        assert_eq!(recent.get("30|wk-02", t0), None);
        // And it ages out.
        assert_eq!(recent.get("30|wk-01", t0 + REPORT_REUSE), None);
    }

    fn dir_with(name: &str, lines: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cuw-{}-{name}", std::process::id()));
        let sub = dir.join("proj");
        std::fs::create_dir_all(&sub).unwrap();
        let mut f = std::fs::File::create(sub.join("s1.jsonl")).unwrap();
        f.write_all(lines.join("\n").as_bytes()).unwrap();
        dir
    }

    fn line(cwd: &str, model: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"timestamp":"{}","cwd":"{}","message":{{"model":"{}","usage":{{"input_tokens":{},"output_tokens":{},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            chrono::Utc::now().to_rfc3339(),
            cwd.replace('\\', "\\\\"),
            model,
            input,
            output
        )
    }

    /// Two projects = two transcript folders, as Claude Code stores them.
    fn dir_with_projects(name: &str, projects: &[(&str, &[&str])]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cuw-{}-{name}", std::process::id()));
        for (folder, lines) in projects {
            let sub = dir.join(folder);
            std::fs::create_dir_all(&sub).unwrap();
            let mut f = std::fs::File::create(sub.join("s1.jsonl")).unwrap();
            f.write_all(lines.join("\n").as_bytes()).unwrap();
        }
        dir
    }

    #[test]
    fn aggregates_by_project_and_sorts_by_usage() {
        let dir = dir_with_projects(
            "aggregate",
            &[
                ("p-small", &[&line(r"C:\p\small", "claude-opus-5", 10, 20)]),
                (
                    "p-large",
                    &[
                        &line(r"C:\p\large", "claude-opus-5", 1000, 2000),
                        &line(r"C:\p\large", "claude-opus-5", 500, 100),
                    ],
                ),
            ],
        );
        let r = scan_dir(&dir, None, &PricingTable::new());
        assert_eq!(r.projects.len(), 2);
        assert_eq!(r.projects[0].name, "large", "the highest usage goes first");
        assert_eq!(r.projects[0].tokens.total(), 3600);
        assert_eq!(r.totals.total(), 3630);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Subagent and workflow transcripts sit below the session's folder; they
    /// add tokens to their project, not projects or sessions of their own.
    #[test]
    fn subagent_transcripts_belong_to_their_project() {
        let dir = dir_with_projects(
            "subagents",
            &[("c--p-app", &[&line(r"C:\p\app", "m", 100, 0)])],
        );
        let session = dir.join("c--p-app").join("0badf00d");
        for sub in ["subagents", "subagents/workflows/wf_1"] {
            let d = session.join(sub);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("agent-a.jsonl"), line(r"C:\p\app", "m", 10, 0)).unwrap();
        }
        let r = scan_dir(&dir, None, &PricingTable::new());
        assert_eq!(r.projects.len(), 1, "no project per subagent folder");
        assert_eq!(r.projects[0].name, "app");
        assert_eq!(r.projects[0].tokens.total(), 120);
        assert_eq!(r.projects[0].sessions, 1, "a subagent is not a session");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The bug seen with real data: within a session the `cwd` changes when it
    /// enters a subdirectory. Grouping by `cwd` invented a "src-tauri" project
    /// next to "web-dashboard" and left the count at 0.
    #[test]
    fn a_subdirectory_is_not_another_project() {
        let dir = dir_with_projects(
            "subdir",
            &[(
                "c--Users-x-Projects-web-dashboard",
                &[
                    &line(r"C:\Users\x\Projects\web-dashboard", "m", 100, 100),
                    &line(r"C:\Users\x\Projects\web-dashboard\src-tauri", "m", 50, 50),
                    &line(
                        r"C:\Users\x\Projects\web-dashboard\src\scripts",
                        "m",
                        25,
                        25,
                    ),
                ],
            )],
        );
        let r = scan_dir(&dir, None, &PricingTable::new());
        assert_eq!(r.projects.len(), 1, "subdirectories are the same project");
        assert_eq!(r.projects[0].name, "web-dashboard", "the shortest cwd wins");
        assert_eq!(r.projects[0].tokens.total(), 350);
        assert_eq!(r.projects[0].sessions, 1, "one file is one session");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn counts_one_session_per_transcript() {
        let dir = std::env::temp_dir().join(format!("cuw-{}-sessions", std::process::id()));
        let sub = dir.join("proj");
        std::fs::create_dir_all(&sub).unwrap();
        for n in 0..3 {
            let mut f = std::fs::File::create(sub.join(format!("s{n}.jsonl"))).unwrap();
            f.write_all(line(r"C:\p\a", "m", 10, 10).as_bytes())
                .unwrap();
        }
        let r = scan_dir(&dir, None, &PricingTable::new());
        assert_eq!(r.projects[0].sessions, 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The module's central behaviour: without a price, no cost is made up.
    #[test]
    fn without_a_price_the_cost_is_unknown_not_zero() {
        let dir = dir_with(
            "noprice",
            &[&line(r"C:\p\a", "model-without-rate", 1000, 1000)],
        );
        let r = scan_dir(&dir, None, &PricingTable::new());
        assert_eq!(r.projects[0].cost_usd, None);
        assert_eq!(r.cost_usd, None);
        assert_eq!(r.models_without_pricing, vec!["model-without-rate"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn computes_the_cost_with_the_given_table() {
        let dir = dir_with("withprice", &[&line(r"C:\p\a", "m1", 1_000_000, 1_000_000)]);
        let mut prices = PricingTable::new();
        prices.insert(
            "m1".into(),
            ModelPricing {
                input: 3.0,
                output: 15.0,
                cache_write: None,
                cache_read: None,
            },
        );
        let r = scan_dir(&dir, None, &prices);
        assert_eq!(r.projects[0].cost_usd, Some(18.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An unpriced project cannot silently vanish from the total: the total
    /// becomes "unknown" instead of reporting only what is known.
    #[test]
    fn an_unpriced_model_invalidates_the_total() {
        // Two folders: with grouping by transcript, a single file would be a
        // single project and the effect on the total would not be tested.
        let dir = dir_with_projects(
            "mixed",
            &[
                ("p-a", &[&line(r"C:\p\a", "m1", 1_000_000, 0)]),
                ("p-b", &[&line(r"C:\p\b", "unknown", 1_000_000, 0)]),
            ],
        );
        let mut prices = PricingTable::new();
        prices.insert(
            "m1".into(),
            ModelPricing {
                input: 3.0,
                output: 15.0,
                cache_write: None,
                cache_read: None,
            },
        );
        let r = scan_dir(&dir, None, &prices);
        assert_eq!(r.cost_usd, None);
        assert!(r.projects.iter().any(|p| p.cost_usd == Some(3.0)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn builtin_prices_cover_current_models_and_dated_ids() {
        let table = builtin_pricing();
        // The four models this machine's transcripts use.
        for model in [
            "claude-fable-5-1",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
        ] {
            assert!(price_for(&table, model).is_some(), "{model}");
        }
        let opus = price_for(&table, "claude-opus-5-5").unwrap();
        assert_eq!(
            (opus.input, opus.output, opus.cache_read),
            (4.0, 20.0, Some(0.20))
        );
        // A dated id falls back to its base model; anything else does not.
        assert!(price_for(&table, "claude-haiku-4-5-20251001").is_some());
        assert!(price_for(&table, "claude-haiku-4-5-latest").is_none());
        assert!(price_for(&table, "claude-unknown-9").is_none());
    }

    #[test]
    fn a_pricing_file_overrides_the_builtin_entry() {
        let mut table = builtin_pricing();
        table.extend(parse_pricing(
            r#"{"_README": ["notes"], "claude-opus-5-5": {"input": 1.0, "output": 2.0}}"#,
        ));
        let opus = &table["claude-opus-5-5"];
        assert_eq!((opus.input, opus.output, opus.cache_read), (1.0, 2.0, None));
        assert!(
            table.contains_key("claude-sonnet-5-5"),
            "other built-in entries stay"
        );
    }

    #[test]
    fn corrupt_lines_do_not_abort_the_scan() {
        let dir = dir_with(
            "corrupt",
            &[
                "{this is not json",
                "",
                r#"{"no":"usage"}"#,
                &line(r"C:\p\a", "m", 5, 5),
            ],
        );
        let r = scan_dir(&dir, None, &PricingTable::new());
        assert_eq!(r.projects.len(), 1);
        assert_eq!(r.totals.total(), 10);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The example file carries a `_README` to document itself. If that broke
    /// parsing, the user would see "n/a" everywhere without knowing why.
    #[test]
    fn the_pricing_table_tolerates_comments() {
        let raw = r#"{
            "_README": ["this is a comment", "over several lines"],
            "_note": "something else",
            "m1": { "input": 3.0, "output": 15.0 },
            "broken": { "input": "not a number" }
        }"#;
        let t = parse_pricing(raw);
        assert_eq!(t.len(), 1, "only m1 is a valid rate");
        assert_eq!(t["m1"].output, 15.0);
    }

    #[test]
    fn invalid_json_gives_an_empty_table_without_panicking() {
        assert!(parse_pricing("{this is not json").is_empty());
    }

    /// The shipped example, `tools/pricing.example.json`, parses as is.
    #[test]
    fn the_example_pricing_file_parses() {
        let t = parse_pricing(include_str!("../../tools/pricing.example.json"));
        assert_eq!(t.len(), 3);
        assert_eq!(t["claude-opus-5"].cache_write, Some(0.0));
        assert_eq!(t["claude-haiku-4-5-20251001"].cache_read, Some(0.0));
    }

    /// The cache prices are optional, each on its own.
    #[test]
    fn partial_cache_prices_parse() {
        let t = parse_pricing(
            r#"{
              "claude-sonnet-5": { "input": 3.0, "output": 15.0 },
              "claude-haiku-4-5-20251001": { "input": 1.0, "output": 5.0, "cache_read": 0.1 }
            }"#,
        );
        assert_eq!(t.len(), 2);
        assert_eq!(t["claude-sonnet-5"].cache_write, None);
        assert_eq!(t["claude-sonnet-5"].cache_read, None);
        assert_eq!(t["claude-haiku-4-5-20251001"].cache_write, None);
        assert_eq!(t["claude-haiku-4-5-20251001"].cache_read, Some(0.1));
    }

    #[test]
    fn a_missing_directory_gives_an_empty_report() {
        let r = scan_dir(Path::new(r"Z:\does\not\exist"), None, &PricingTable::new());
        assert!(r.projects.is_empty());
        assert_eq!(r.totals.total(), 0);
    }
}
