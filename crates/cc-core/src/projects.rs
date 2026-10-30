//! Token usage per project: the counters the widget's usage view shows, and
//! the payload a publisher sends so the widget can show another machine's
//! projects too.
//!
//! The payload carries a project's folder name (never its path), how many
//! sessions it had, its tokens per model and when it was last active, over
//! the last [`PROJECT_WINDOW_DAYS`] days. No prompts, titles or file names.
//! The cost is not sent: the widget prices the tokens with its own table, so
//! one price list applies to every machine.
//!
//! The activity payload is the machine's tokens per day (all models and
//! projects together) and the busiest hour of each day, over the last
//! [`ACTIVITY_DAYS`] days, for the usage view's activity calendar. Days are
//! the publisher's local calendar days.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The window a project's counters cover, in days.
pub const PROJECT_WINDOW_DAYS: u32 = 30;

/// Longest project name sent, in characters; longer ones are cut.
pub const MAX_PROJECT_NAME: usize = 64;

/// Most models listed per project; the rest are summed under
/// [`OTHER_MODELS`], which keeps a value under the bucket's size limit.
pub const MAX_PROJECT_MODELS: usize = 12;

/// The model name the tokens of the models past [`MAX_PROJECT_MODELS`] are
/// summed under. It has no price, so such a project's cost reads "n/a".
pub const OTHER_MODELS: &str = "(other models)";

/// Days of activity a publisher sends: a little more than the transcripts
/// Claude Code keeps by default (30 days), so nothing on disk is left out.
pub const ACTIVITY_DAYS: usize = 35;

/// One day's activity: every token of every model and project, and the
/// busiest hour. Field names are one letter: every byte is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayActivity {
    /// The local calendar day, `YYYY-MM-DD`.
    #[serde(rename = "d")]
    pub day: String,
    /// Tokens that day.
    #[serde(rename = "t")]
    pub tokens: u64,
    /// The hour (0-23, local) with the most tokens.
    #[serde(rename = "h")]
    pub peak_hour: u8,
}

/// What a publisher sends for its activity, before encryption: the days with
/// any tokens, oldest first, at most [`ACTIVITY_DAYS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPayload {
    /// Publisher alias, same as the first token of the KV key.
    pub machine: String,
    pub days: Vec<DayActivity>,
    /// Unix seconds of this write.
    pub hb: i64,
}

impl DayActivity {
    /// A day from its tokens per hour (index = local hour).
    pub fn from_hours(day: &str, hours: &[u64; 24]) -> Self {
        let mut peak = 0usize;
        for (h, &t) in hours.iter().enumerate() {
            if t > hours[peak] {
                peak = h;
            }
        }
        Self {
            day: day.to_string(),
            tokens: hours.iter().fold(0u64, |a, &t| a.saturating_add(t)),
            peak_hour: peak as u8,
        }
    }
}

/// Tokens of one kind of use, as Claude Code's transcripts count them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

impl Tokens {
    /// Adds `other` to these counters. Saturates at `u64::MAX` instead of
    /// wrapping (release) or panicking (debug) on a corrupt transcript.
    pub fn add(&mut self, other: &Tokens) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
    }

    /// All four counters together, saturating like [`Tokens::add`].
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_creation)
            .saturating_add(self.cache_read)
    }
}

/// One model's tokens in a [`ProjectPayload`]. Field names are one letter
/// because every byte is stored in the bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTokens {
    /// The model id as the transcript has it, e.g. `claude-opus-5-5`.
    #[serde(rename = "m")]
    pub model: String,
    #[serde(rename = "i", default)]
    pub input: u64,
    #[serde(rename = "o", default)]
    pub output: u64,
    /// Cache writes.
    #[serde(rename = "w", default)]
    pub cache_creation: u64,
    /// Cache reads.
    #[serde(rename = "r", default)]
    pub cache_read: u64,
}

impl ModelTokens {
    /// The counters as [`Tokens`].
    pub fn tokens(&self) -> Tokens {
        Tokens {
            input: self.input,
            output: self.output,
            cache_creation: self.cache_creation,
            cache_read: self.cache_read,
        }
    }
}

/// What a publisher sends for one project, before encryption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPayload {
    /// Publisher alias, same as the first token of the KV key.
    pub machine: String,
    /// The project's folder name, at most [`MAX_PROJECT_NAME`] characters.
    pub name: String,
    /// Sessions (transcripts) with activity in the window.
    pub sessions: u32,
    /// Tokens per model, largest first, at most [`MAX_PROJECT_MODELS`] + 1.
    pub models: Vec<ModelTokens>,
    /// Unix seconds of the last activity, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<i64>,
    /// The window the counters cover, days ([`PROJECT_WINDOW_DAYS`]).
    pub days: u32,
    /// Unix seconds of this write.
    pub hb: i64,
}

impl ProjectPayload {
    /// The payload for a project's counters: the name cut to
    /// [`MAX_PROJECT_NAME`] characters, the models largest first and those
    /// past [`MAX_PROJECT_MODELS`] summed under [`OTHER_MODELS`].
    pub fn new(
        machine: &str,
        name: &str,
        sessions: u32,
        by_model: &HashMap<String, Tokens>,
        last: Option<i64>,
        hb: i64,
    ) -> Self {
        let mut models: Vec<(&String, &Tokens)> = by_model.iter().collect();
        // Largest first; the name breaks ties so the order is stable.
        models.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(b.0)));
        let mut listed: Vec<ModelTokens> = models
            .iter()
            .take(MAX_PROJECT_MODELS)
            .map(|(model, t)| model_tokens(model, t))
            .collect();
        if models.len() > MAX_PROJECT_MODELS {
            let mut rest = Tokens::default();
            for (_, t) in &models[MAX_PROJECT_MODELS..] {
                rest.add(t);
            }
            listed.push(model_tokens(OTHER_MODELS, &rest));
        }
        Self {
            machine: machine.to_string(),
            name: name.chars().take(MAX_PROJECT_NAME).collect(),
            sessions,
            models: listed,
            last,
            days: PROJECT_WINDOW_DAYS,
            hb,
        }
    }

    /// All the project's tokens.
    pub fn tokens(&self) -> Tokens {
        let mut total = Tokens::default();
        for m in &self.models {
            total.add(&m.tokens());
        }
        total
    }

    /// Tokens per model, for pricing.
    pub fn by_model(&self) -> HashMap<String, Tokens> {
        let mut map: HashMap<String, Tokens> = HashMap::new();
        for m in &self.models {
            map.entry(m.model.clone()).or_default().add(&m.tokens());
        }
        map
    }
}

fn model_tokens(model: &str, t: &Tokens) -> ModelTokens {
    ModelTokens {
        model: model.to_string(),
        input: t.input,
        output: t.output,
        cache_creation: t.cache_creation,
        cache_read: t.cache_read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(n: u64) -> Tokens {
        Tokens {
            input: n,
            output: n,
            cache_creation: 0,
            cache_read: 0,
        }
    }

    #[test]
    fn a_day_sums_its_hours_and_names_the_busiest() {
        let mut hours = [0u64; 24];
        hours[9] = 5;
        hours[23] = 40;
        hours[14] = 40;
        let d = DayActivity::from_hours("2026-10-13", &hours);
        assert_eq!(
            (d.tokens, d.peak_hour),
            (85, 14),
            "the first of equal peaks"
        );
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, r#"{"d":"2026-10-13","t":85,"h":14}"#);
    }

    #[test]
    fn counters_saturate_instead_of_wrapping() {
        let mut t = Tokens {
            input: u64::MAX,
            output: 1,
            cache_creation: u64::MAX - 1,
            cache_read: 0,
        };
        assert_eq!(t.total(), u64::MAX);
        t.add(&tokens(u64::MAX));
        assert_eq!((t.input, t.output), (u64::MAX, u64::MAX));
        t.add(&Tokens {
            cache_creation: 5,
            ..Tokens::default()
        });
        assert_eq!(t.cache_creation, u64::MAX);
        let mut hours = [0u64; 24];
        hours[0] = u64::MAX;
        hours[1] = 7;
        assert_eq!(
            DayActivity::from_hours("2026-10-13", &hours).tokens,
            u64::MAX
        );
    }

    #[test]
    fn models_go_largest_first_and_the_tail_is_summed() {
        let mut by_model = HashMap::new();
        for i in 0..15u64 {
            by_model.insert(format!("m{i:02}"), tokens(i + 1));
        }
        let p = ProjectPayload::new("devbox", "app", 3, &by_model, Some(10), 20);
        assert_eq!(p.models.len(), MAX_PROJECT_MODELS + 1);
        assert_eq!(p.models[0].model, "m14", "the largest first");
        let other = p.models.last().unwrap();
        assert_eq!(other.model, OTHER_MODELS);
        assert_eq!(other.input, 1 + 2 + 3, "the three smallest");
        let all: u64 = (1..=15u64).map(|n| 2 * n).sum();
        assert_eq!(p.tokens().total(), all, "nothing is lost");
    }

    #[test]
    fn the_name_is_cut_and_the_window_is_stated() {
        let p = ProjectPayload::new("devbox", &"é".repeat(100), 1, &HashMap::new(), None, 0);
        assert_eq!(p.name.chars().count(), MAX_PROJECT_NAME);
        assert_eq!(p.days, PROJECT_WINDOW_DAYS);
        assert!(p.models.is_empty());
    }

    #[test]
    fn the_payload_round_trips_with_short_model_fields() {
        let mut by_model = HashMap::new();
        by_model.insert("claude-opus-5-5".to_string(), tokens(5));
        let p = ProjectPayload::new(
            "devbox",
            "app",
            2,
            &by_model,
            Some(1_791_900_000),
            1_791_900_060,
        );
        let json = serde_json::to_string(&p).unwrap();
        assert!(
            json.contains(r#""m":"claude-opus-5-5","i":5,"o":5,"w":0,"r":0"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<ProjectPayload>(&json).unwrap(), p);
        assert_eq!(p.by_model()["claude-opus-5-5"], tokens(5));
    }
}
