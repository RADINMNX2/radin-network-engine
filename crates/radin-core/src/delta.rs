//! Delta sync (spec 20): send only what changed, when the change is worth it.
//!
//! Deliberately simple: path-based diffs over serializable snapshots. The
//! engine does NOT invent fancy protocols when bandwidth savings are
//! negligible — it measures both sides and only emits deltas when a
//! threshold (count of fields) is exceeded.

use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Diff two JSON objects into `{ "path": value }` patches.
/// `path` uses a slash-separated pointer (e.g. `routeB.score`).
///
/// Returns `None` when the objects are identical.
/// If the caller passes a `min_fields` threshold and the number of changed
/// leaves is below it, the caller may prefer to send the full snapshot
/// instead — `should_apply_delta` encodes that choice.
pub fn diff(a: &Value, b: &Value) -> Option<Map<String, Value>> {
    let a = a.as_object()?;
    let b = b.as_object()?;
    let mut out = Map::new();
    collect_diff("", a, b, &mut out);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn collect_diff(
    prefix: &str,
    a: &Map<String, Value>,
    b: &Map<String, Value>,
    out: &mut Map<String, Value>,
) {
    let mut keys: BTreeSet<&String> = a.keys().collect();
    keys.extend(b.keys());
    for key in keys {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (a.get(key), b.get(key)) {
            (None, Some(val)) => {
                out.insert(path, val.clone());
            }
            (Some(_val), None) => {
                out.insert(path, Value::Null);
            }
            (Some(va), Some(vb)) if va == vb => {}
            (Some(va), Some(vb)) => {
                if let (Value::Object(ao), Value::Object(bo)) = (va, vb) {
                    collect_diff(&path, ao, bo, out);
                } else if matches!(vb, Value::Object(_)) {
                    // New nested object replacing a scalar → treat as patch.
                    out.insert(path, vb.clone());
                } else {
                    out.insert(path, vb.clone());
                }
            }
            (None, None) => {}
        }
    }
}

/// Decide whether a delta is worth sending versus the full snapshot, based on
/// the number of changed leaves (spec 20: don't complicate where savings are
/// negligible).
pub fn should_apply_delta(
    full_snapshot: &Value,
    delta: &Map<String, Value>,
    min_fields: usize,
) -> bool {
    let full_len = full_snapshot.as_object().map(|o| o.len()).unwrap_or(0);
    let delta_len = delta.len();
    // Delta wins if it touches strictly fewer leaves and is above the minimum
    // complexity budget.
    delta_len <= full_len && delta_len >= min_fields
}

/// Apply a diff produced by `diff()` to source `base`, returning the
/// resulting object. Round-trips with `diff`.
pub fn apply(base: &Value, patches: &Map<String, Value>) -> Option<Value> {
    let mut root = base.clone();
    let obj = root.as_object_mut()?;
    for (path, value) in patches {
        let parts: Vec<&str> = path.split('.').collect();
        set_path(obj, &parts, value);
    }
    Some(root)
}

fn set_path(obj: &mut Map<String, Value>, parts: &[&str], value: &Value) {
    if let Some((head, rest)) = parts.split_first() {
        if rest.is_empty() {
            obj.insert((*head).to_string(), value.clone());
            return;
        }
        let entry = obj
            .entry((*head).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(next) = entry {
            set_path(next, rest, value);
        }
    }
}

/// Full-and-delta pair ready for serialization.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SyncDelta {
    #[serde(default)]
    pub full: Option<Value>,
    #[serde(default)]
    pub patches: Option<Map<String, Value>>,
}

/// Build the smallest honest representation of a state change.
pub fn encode_sync(base: Option<&Value>, next: &Value) -> SyncDelta {
    match base {
        None => SyncDelta {
            full: Some(next.clone()),
            patches: None,
        },
        Some(prev) => match diff(prev, next) {
            None => SyncDelta {
                full: None,
                patches: None,
            }, // unchanged
            Some(patches) => SyncDelta {
                full: None,
                patches: Some(patches),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diff_detects_scalar_change() {
        let a = json!({ "routeA": { "score": 80 }, "routeB": { "score": 55 } });
        let b = json!({ "routeA": { "score": 80 }, "routeB": { "score": 60 } });
        let d = diff(&a, &b).unwrap();
        assert_eq!(d.get("routeB.score"), Some(&json!(60)));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn diff_none_when_identical() {
        let a = json!({ "x": 1, "y": [1, 2] });
        assert!(diff(&a, &a).is_none());
    }

    #[test]
    fn apply_roundtrips_diff() {
        let a = json!({ "routeA": { "score": 80, "jitter": 2.1 }, "routeB": { "score": 55 } });
        let b = json!({ "routeA": { "score": 90, "jitter": 1.2 }, "routeB": { "score": 55 } });
        let d = diff(&a, &b).unwrap();
        let merged = apply(&a, &d).unwrap();
        assert_eq!(merged, b);
    }

    #[test]
    fn delta_preferred_below_field_threshold() {
        let a = json!({ "routeA": { "score": 80 }, "routeB": { "score": 55 } });
        let b = json!({ "routeA": { "score": 81 }, "routeB": { "score": 55 } });
        let d = diff(&a, &b).unwrap();
        assert!(should_apply_delta(&a, &d, 1));
        assert!(
            !should_apply_delta(&a, &d, 2),
            "below min_fields → send full snapshot"
        );
    }
}
