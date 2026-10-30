//! Context window sizes per model, to estimate how full a session's context is
//! when no status line reports it.
//!
//! The status line gives the exact percentage, but only the Claude Code CLI
//! runs it; sessions in the VS Code extension's panel never do. Their
//! transcript still records how many tokens the latest reply was given, so the
//! percentage can be estimated as tokens / window.
//!
//! Sizes from Anthropic's model reference as of 2026-10-06. An unknown model
//! gets no estimate rather than a guess.

/// (model id prefix, context window in tokens). Longest prefixes first is not
/// needed: entries are exact base ids, matched on the id or the id plus a
/// suffix such as a date (`claude-haiku-4-5-20251001`) or `[1m]`.
const WINDOWS: &[(&str, u64)] = &[
    ("claude-fable-5-1", 1_000_000),
    ("claude-fable-5", 1_000_000),
    ("claude-mythos-5-1", 1_000_000),
    ("claude-mythos-5", 1_000_000),
    ("claude-opus-5-5", 1_000_000),
    ("claude-opus-5", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-sonnet-5-5", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-haiku-5-5", 1_000_000),
    ("claude-haiku-4-5", 200_000),
];

/// The context window of `model`, if it is a known model id.
pub fn context_window(model: &str) -> Option<u64> {
    let model = model.trim();
    WINDOWS
        .iter()
        .filter(|(id, _)| {
            model == *id
                || model.starts_with(&format!("{id}-"))
                || model.starts_with(&format!("{id}["))
        })
        // "claude-opus-5-5" also starts with "claude-opus-5-"; the longest id wins.
        .max_by_key(|(id, _)| id.len())
        .map(|&(_, window)| window)
}

/// `tokens` as a share of `model`'s window, rounded, capped at 100.
pub fn estimated_percent(tokens: u64, model: &str) -> Option<u8> {
    let window = context_window(model)?;
    let percent = (tokens as f64 / window as f64 * 100.0).round();
    Some(percent.clamp(0.0, 100.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_models_have_a_window_and_unknown_ones_do_not() {
        assert_eq!(context_window("claude-opus-5-5"), Some(1_000_000));
        assert_eq!(context_window("claude-haiku-4-5"), Some(200_000));
        assert_eq!(context_window("claude-haiku-4-5-20251001"), Some(200_000));
        assert_eq!(context_window("claude-sonnet-5-5[1m]"), Some(1_000_000));
        assert_eq!(context_window("gpt-5"), None);
        assert_eq!(context_window("<synthetic>"), None);
    }

    #[test]
    fn the_longest_matching_id_wins() {
        // "claude-opus-5-5" must not be read as "claude-opus-5" plus a suffix,
        // and "claude-fable-5-1" not as "claude-fable-5".
        assert_eq!(
            context_window("claude-opus-5-5"),
            context_window("claude-opus-5")
        );
        assert!(WINDOWS.iter().any(|(id, _)| *id == "claude-opus-5-5"));
    }

    #[test]
    fn estimates_round_and_cap() {
        assert_eq!(estimated_percent(577_000, "claude-opus-5-5"), Some(58));
        assert_eq!(estimated_percent(150_000, "claude-haiku-4-5"), Some(75));
        assert_eq!(estimated_percent(5_000_000, "claude-opus-5-5"), Some(100));
        assert_eq!(estimated_percent(1000, "unknown-model"), None);
    }
}
