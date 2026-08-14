//! What is allowed to leave the machine.
//!
//! This module is the whole privacy argument for Phase 4c, so it is worth
//! stating plainly: **metrics travel, content does not.**
//!
//! A supervisor dashboard needs to know that a call scored 62, ran nine
//! minutes, raised `PCI_PAN_SPOKEN`, and was handled by agent `person_a1b2`.
//! It does not need the transcript, the customer's name, the number they
//! called from, or the sentence the model quoted as evidence. Those are the
//! parts that make a corpus of recorded phone calls sensitive, and none of
//! them appear in any type below.
//!
//! Two things follow that are easy to get wrong:
//!
//! - **Findings carry a code and a severity, never a detail.** The detail is
//!   prose written about a specific moment in the call and routinely quotes
//!   it — `"the agent said 'give me the card number'"`. A code is a label from
//!   a fixed vocabulary and says nothing the code itself does not.
//! - **Participants are opaque ids.** `person_a1b2c3` is meaningless without
//!   the local `people` table, which never syncs. A server operator sees that
//!   two calls involved the same person without learning who.
//!
//! [`SyncRecord::assert_no_content`] exists so this is checked rather than
//! merely intended.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One finding, reduced to what is safe to send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingMetric {
    /// A label from a fixed vocabulary, such as `HOLD_NOT_TIMEBOXED`.
    pub code: String,
    /// `info`, `low`, `medium`, `high` or `critical`.
    pub severity: String,
    /// How many times this code was raised on the call.
    pub count: u32,
}

/// Everything one call contributes to a shared dashboard.
///
/// Every field is a number, a timestamp, an opaque id, or a term from a closed
/// vocabulary. There is deliberately no field that could hold a sentence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRecord {
    /// The call's local id. Opaque, and unique within a tenant.
    pub call_id: String,
    /// When the call happened, not when it was ingested.
    pub occurred_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<f64>,
    /// Pipeline stage, from a closed set.
    pub stage: String,
    /// Opaque participant ids. Never names, never numbers.
    pub agent_id: Option<String>,
    pub contact_id: Option<String>,
    /// QA total, 0..=100.
    pub qa_overall: Option<f32>,
    /// Whole-call customer sentiment, -1..=1.
    pub sentiment: Option<f32>,
    /// `None` means the agent did not run, which is not a pass.
    pub compliance_passed: Option<bool>,
    pub findings: Vec<FindingMetric>,
    /// Drives the pull cursor and makes a re-push idempotent.
    pub updated_at: DateTime<Utc>,
}

/// A vocabulary a severity is allowed to use.
const SEVERITIES: [&str; 5] = ["info", "low", "medium", "high", "critical"];

impl SyncRecord {
    /// Refuse to send anything that looks like call content.
    ///
    /// Defence in depth rather than the primary mechanism: the primary
    /// mechanism is that no field here can hold prose. This catches the case
    /// where someone later widens a field, or a code arrives carrying text it
    /// should not — and it fails the send rather than logging a warning,
    /// because a warning about data that already left is worthless.
    pub fn assert_no_content(&self) -> Result<(), String> {
        // A finding code is SCREAMING_SNAKE from a fixed set. Anything with a
        // space or lowercase prose in it is not a code.
        for f in &self.findings {
            if f.code.len() > 64
                || !f
                    .code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!(
                    "finding code `{}` is not a bare code — refusing to send",
                    f.code
                ));
            }
            if !SEVERITIES.contains(&f.severity.as_str()) {
                return Err(format!("unknown severity `{}`", f.severity));
            }
        }
        // Ids are generated locally as `<prefix>_<hex>`; anything else may be
        // a name that reached the field by mistake.
        for (label, id) in [
            ("call", Some(&self.call_id)),
            ("agent", self.agent_id.as_ref()),
            ("contact", self.contact_id.as_ref()),
        ] {
            let Some(id) = id else { continue };
            if id.len() > 80
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!("{label} id `{id}` is not an opaque id"));
            }
        }
        Ok(())
    }
}

/// A batch, with the cursor the server should report back.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushRequest {
    pub records: Vec<SyncRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResponse {
    /// How many records the server stored or refreshed.
    pub accepted: usize,
    /// Records rejected, with the reason. Reported rather than silently
    /// dropped: a client that thinks it synced and did not is worse than one
    /// that knows it failed.
    pub rejected: Vec<String>,
    /// The highest sequence number now held, for the next pull.
    pub cursor: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullResponse {
    pub records: Vec<SyncRecord>,
    /// Pass back as `since` to continue. Equal to the previous cursor when
    /// there is nothing new.
    pub cursor: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> SyncRecord {
        SyncRecord {
            call_id: "call_5cebb8162c73".into(),
            occurred_at: Some(Utc::now()),
            duration_secs: Some(64.3),
            stage: "analyzed".into(),
            agent_id: Some("person_a1b2".into()),
            contact_id: Some("person_c3d4".into()),
            qa_overall: Some(50.0),
            sentiment: Some(0.4),
            compliance_passed: Some(true),
            findings: vec![FindingMetric {
                code: "HOLD_NOT_TIMEBOXED".into(),
                severity: "high".into(),
                count: 1,
            }],
            updated_at: Utc::now(),
        }
    }

    /// The guarantee the whole phase rests on.
    #[test]
    fn a_serialised_record_contains_no_call_content() {
        let json = serde_json::to_string(&record()).unwrap();
        for leaked in [
            "Babulal",
            "Kavaiya",
            "9876543210",
            "+918485823280",
            "give me the card number",
            "Thanks for calling",
        ] {
            assert!(
                !json.contains(leaked),
                "`{leaked}` must never appear in a sync payload"
            );
        }
        // And there is no field that could hold prose in the first place.
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in [
            "detail",
            "title",
            "quote",
            "text",
            "reasoning",
            "displayName",
            "handle",
        ] {
            assert!(
                value.get(key).is_none(),
                "the record must not carry a `{key}` field"
            );
        }
    }

    #[test]
    fn a_clean_record_passes_the_guard() {
        assert!(record().assert_no_content().is_ok());
    }

    /// The failure this guards: someone widens `code` to carry the finding's
    /// prose, and a quoted sentence starts syncing.
    #[test]
    fn a_finding_code_carrying_prose_is_refused() {
        let mut r = record();
        r.findings[0].code = "The agent said give me the card number".into();
        let err = r.assert_no_content().unwrap_err();
        assert!(err.contains("not a bare code"), "got {err}");
    }

    #[test]
    fn an_unknown_severity_is_refused() {
        let mut r = record();
        r.findings[0].severity = "catastrophic".into();
        assert!(r.assert_no_content().is_err());
    }

    /// A participant id must stay opaque. A name arriving in that field is the
    /// exact leak the id exists to prevent.
    #[test]
    fn a_name_in_a_participant_field_is_refused() {
        let mut r = record();
        r.contact_id = Some("Babulal Kavaiya".into());
        let err = r.assert_no_content().unwrap_err();
        assert!(err.contains("not an opaque id"), "got {err}");
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let r = record();
        let back: SyncRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }
}
