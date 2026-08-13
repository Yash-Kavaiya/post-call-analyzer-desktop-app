//! What the live screen is told, as it happens.
//!
//! A tagged enum rather than a bag of optional fields, so the UI matches on one
//! discriminant and every variant carries exactly what that moment needs.
//!
//! There is deliberately no partial-hypothesis variant. Chunks are cut at
//! pauses and transcribed whole, so text arrives already final — there is never
//! an interim guess that later changes. [`LiveEvent::Speaking`] drives the
//! "listening" marker instead: it says a channel has voice activity right now,
//! not what that voice might be saying. Supporting revisable partials would
//! mean re-transcribing an overlapping window, which the latency measurements
//! ruled out.

use crate::Channel;
use ca_core::analysis::{Finding, Severity};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LiveEvent {
    /// The session is capturing. Carries the call id so the UI can navigate to
    /// the finished call later.
    #[serde(rename_all = "camelCase")]
    Started {
        call_id: String,
        agent_device: String,
        customer_device: String,
    },

    /// Per-channel level, for a meter. The one signal that distinguishes "this
    /// call is quiet" from "this device is not actually capturing".
    #[serde(rename_all = "camelCase")]
    Level {
        agent_dbfs: f32,
        customer_dbfs: f32,
        elapsed_secs: f64,
    },

    /// A channel started or stopped having voice activity.
    #[serde(rename_all = "camelCase")]
    Speaking { channel: Channel, speaking: bool },

    /// A finalised piece of transcript. Never revised.
    #[serde(rename_all = "camelCase")]
    Stable {
        segment_id: String,
        channel: Channel,
        start_secs: f64,
        end_secs: f64,
        text: String,
    },

    /// A deterministic detector fired. Already deduplicated — each one is
    /// reported once per call.
    #[serde(rename_all = "camelCase")]
    Rule { finding: Finding },

    /// The throttled monitor answered.
    #[serde(rename_all = "camelCase")]
    Monitor {
        sentiment: f32,
        risk: Severity,
        headline: String,
        say_next: Option<String>,
        elapsed_secs: f64,
    },

    /// Something degraded but the session continues. Capture is the only thing
    /// that cannot be redone, so nothing else is allowed to end the call.
    #[serde(rename_all = "camelCase")]
    Warning { message: String },

    /// Capture stopped. The call is now ordinary work for the batch pipeline.
    #[serde(rename_all = "camelCase")]
    Ended {
        call_id: String,
        duration_secs: f64,
        media_path: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_carry_a_kind_the_ui_can_switch_on() {
        let v = serde_json::to_value(LiveEvent::Speaking {
            channel: Channel::Agent,
            speaking: true,
        })
        .unwrap();
        assert_eq!(v["kind"], "speaking");
        assert_eq!(v["channel"], "agent");
    }

    #[test]
    fn event_fields_cross_the_wire_as_camel_case() {
        let v = serde_json::to_value(LiveEvent::Stable {
            segment_id: "seg_1".into(),
            channel: Channel::Customer,
            start_secs: 1.0,
            end_secs: 2.0,
            text: "hello".into(),
        })
        .unwrap();
        assert!(v.get("segmentId").is_some(), "got {v}");
        assert!(v.get("startSecs").is_some());
    }

    #[test]
    fn the_monitor_event_can_say_it_has_no_suggestion() {
        // An invented next line is read aloud to a customer, so "nothing to
        // suggest" has to be representable rather than an empty string.
        let v = serde_json::to_value(LiveEvent::Monitor {
            sentiment: -0.4,
            risk: Severity::High,
            headline: "Customer is escalating".into(),
            say_next: None,
            elapsed_secs: 42.0,
        })
        .unwrap();
        assert!(v["sayNext"].is_null());
        assert_eq!(v["risk"], "high");
    }
}
