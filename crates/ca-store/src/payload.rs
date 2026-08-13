//! Normalising stored agent payloads on read.
//!
//! Analyses are persisted as whatever JSON the agent's report type serialised to
//! at the time. Those types carry `#[serde(rename_all = "camelCase")]` now, but
//! rows written before that rename are on disk in snake_case — a real corpus
//! holds both. Nothing re-writes them: the `analyses` table is keyed
//! `(call_id, agent)`, so an old row only changes when that agent is re-run,
//! which for a paid batch is not something to require.
//!
//! The symptom is quiet rather than loud. `RootCauseReport.primary_cause`
//! serialises as `primaryCause`, so a reader looking for that key finds nothing
//! in an older row and renders an empty field — a panel that says the model
//! found no cause, when what happened is that the key was spelled differently.
//!
//! Converting keys on read fixes every reader at once and touches no stored
//! data. It is safe because these payloads are serialised structs: every key is
//! a field name, so there is no free-form map whose keys carry meaning.

use serde_json::{Map, Value};

/// Rewrite every object key in a payload from `snake_case` to `camelCase`.
///
/// Keys already in camelCase are left exactly as they are, so this is a no-op
/// for anything written by a current build.
pub fn normalise(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (camel(&k), normalise(v)))
                .collect::<Map<String, Value>>(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(normalise).collect()),
        other => other,
    }
}

/// `primary_cause` → `primaryCause`. Anything without an underscore is returned
/// untouched rather than rebuilt.
fn camel(key: &str) -> String {
    if !key.contains('_') {
        return key.to_string();
    }
    let mut out = String::with_capacity(key.len());
    let mut upper_next = false;
    for ch in key.chars() {
        if ch == '_' {
            // Only the first underscore of a run capitalises, and a leading or
            // trailing one is simply dropped.
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snake_case_keys_become_camel_case() {
        let v = normalise(json!({"primary_cause": "Double billing"}));
        assert_eq!(v, json!({"primaryCause": "Double billing"}));
    }

    #[test]
    fn keys_already_in_camel_case_are_untouched() {
        let v = json!({"primaryCause": "x", "category": "billing"});
        assert_eq!(normalise(v.clone()), v);
    }

    #[test]
    fn nested_objects_and_arrays_are_converted_too() {
        // Citations are the case that matters: they arrive one level down
        // inside an array, and `segment_id` is what the UI keys them on.
        let v = normalise(json!({
            "contributing_factors": ["a", "b"],
            "citations": [{"segment_id": "seg_1", "start": 1.0}],
        }));
        assert_eq!(
            v,
            json!({
                "contributingFactors": ["a", "b"],
                "citations": [{"segmentId": "seg_1", "start": 1.0}],
            })
        );
    }

    #[test]
    fn values_are_never_touched_only_keys() {
        // A quote from the transcript may contain anything at all.
        let v = normalise(json!({"quote": "call_me_back on the_number"}));
        assert_eq!(v, json!({"quote": "call_me_back on the_number"}));
    }

    #[test]
    fn a_run_of_underscores_does_not_produce_a_stray_capital() {
        assert_eq!(camel("a__b"), "aB");
        assert_eq!(camel("_leading"), "Leading");
        assert_eq!(camel("trailing_"), "trailing");
    }

    #[test]
    fn a_non_object_payload_survives_unchanged() {
        assert_eq!(normalise(json!(null)), json!(null));
        assert_eq!(normalise(json!([1, 2])), json!([1, 2]));
    }

    #[test]
    fn both_shapes_of_a_stored_report_read_the_same_way() {
        // The whole point: an older row and a newer one must be
        // indistinguishable to everything downstream.
        let old = normalise(json!({
            "call_id": "call_1",
            "primary_cause": "Renewal charged twice",
            "contributing_factors": ["retry"],
            "recommended_fix": "Add an idempotency key",
        }));
        let new = normalise(json!({
            "callId": "call_1",
            "primaryCause": "Renewal charged twice",
            "contributingFactors": ["retry"],
            "recommendedFix": "Add an idempotency key",
        }));
        assert_eq!(old, new);
    }
}
