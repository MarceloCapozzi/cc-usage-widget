//! Lenient field extraction from loosely typed JSON objects.
//!
//! The CLI output and the registry files are undocumented and change across
//! Claude Code versions, so every field is read on its own: a missing field or
//! one of the wrong type yields `None` instead of failing the whole record.

use cc_core::session::RegistryStatus;
use serde_json::{Map, Value};

/// A non-empty string field.
pub(crate) fn string(obj: &Map<String, Value>, key: &str) -> Option<String> {
    match obj.get(key)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// An unsigned 32-bit integer field; also accepts a numeric string.
pub(crate) fn u32_field(obj: &Map<String, Value>, key: &str) -> Option<u32> {
    match obj.get(key)? {
        Value::Number(n) => n.as_u64().and_then(|v| u32::try_from(v).ok()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A signed 64-bit integer field; accepts floats (truncated) and numeric strings.
pub(crate) fn i64_field(obj: &Map<String, Value>, key: &str) -> Option<i64> {
    match obj.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A field that is usually a string but may be an object in other versions:
/// strings are kept as they are, anything else non-null is kept as compact JSON,
/// capped at `max` characters.
pub(crate) fn loose_text(obj: &Map<String, Value>, key: &str, max: usize) -> Option<String> {
    let text = match obj.get(key)? {
        Value::Null => return None,
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    cc_core::events::truncate_text(&text, max)
}

/// `status` as a [`RegistryStatus`]; unknown values are kept verbatim.
pub(crate) fn status(obj: &Map<String, Value>, key: &str) -> Option<RegistryStatus> {
    string(obj, key).map(|s| parse_status(&s))
}

/// Maps a status string to [`RegistryStatus`], case-insensitively.
pub(crate) fn parse_status(s: &str) -> RegistryStatus {
    match s.trim().to_ascii_lowercase().as_str() {
        "busy" => RegistryStatus::Busy,
        "waiting" => RegistryStatus::Waiting,
        "idle" => RegistryStatus::Idle,
        _ => RegistryStatus::Other(s.trim().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn wrong_types_yield_none() {
        let o = obj(json!({"a": 5, "b": "x", "c": -1, "d": "", "e": 1.5e12}));
        assert_eq!(string(&o, "a"), None);
        assert_eq!(string(&o, "d"), None);
        assert_eq!(u32_field(&o, "b"), None);
        assert_eq!(u32_field(&o, "c"), None);
        assert_eq!(i64_field(&o, "e"), Some(1_500_000_000_000));
        assert_eq!(string(&o, "missing"), None);
    }

    #[test]
    fn numeric_strings_are_accepted() {
        let o = obj(json!({"pid": "21728", "t": "17"}));
        assert_eq!(u32_field(&o, "pid"), Some(21728));
        assert_eq!(i64_field(&o, "t"), Some(17));
    }

    #[test]
    fn statuses_map_case_insensitively_and_keep_unknowns() {
        assert_eq!(parse_status("Busy"), RegistryStatus::Busy);
        assert_eq!(parse_status("waiting"), RegistryStatus::Waiting);
        assert_eq!(parse_status("idle"), RegistryStatus::Idle);
        assert_eq!(
            parse_status("compacting"),
            RegistryStatus::Other("compacting".into())
        );
    }

    #[test]
    fn loose_text_keeps_objects_as_json() {
        let o = obj(json!({"w": {"type": "permission"}, "s": "approval", "n": null}));
        assert_eq!(
            loose_text(&o, "w", 100).as_deref(),
            Some(r#"{"type":"permission"}"#)
        );
        assert_eq!(loose_text(&o, "s", 100).as_deref(), Some("approval"));
        assert_eq!(loose_text(&o, "n", 100), None);
    }
}
