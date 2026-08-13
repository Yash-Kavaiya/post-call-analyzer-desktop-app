//! Deterministic detectors over a transcript window.
//!
//! These are the checks that are worth catching *while the call is happening*,
//! and none of them needs a model. A card number read aloud is a card number:
//! spending a network round trip and a token budget to have an LLM notice it
//! adds latency and a chance of being missed, in exchange for nothing.
//!
//! Two design rules follow from that:
//!
//! - **Pure functions.** [`evaluate`] takes the transcript so far and returns
//!   the complete current set of findings. It holds no state, so it is testable
//!   on literals and gives the same answer twice.
//! - **The existing [`Finding`] type.** Rule hits are not a parallel concept.
//!   They carry the same severities, land in the same ledger, and roll into the
//!   same corpus risk register as anything the compliance agent files.
//!
//! Because `evaluate` re-reports every finding on every call, the caller diffs
//! against what it has already shown using [`dedupe_key`]. A rule that fired on
//! chunk three must not alert again on chunks four onward.
//!
//! **These run on raw, unredacted text and must stay local.** That is the whole
//! point of `PCI_PAN_SPOKEN`: it needs to see the digits. Nothing here may be
//! sent to a provider — the redacted window built for the live monitor is a
//! separate thing.

pub mod detectors;

use ca_core::analysis::Finding;
use ca_core::ids::CallId;
use ca_core::transcript::Segment;

/// Everything the detectors are allowed to look at.
pub struct RuleContext<'a> {
    pub call_id: &'a CallId,
    /// Stable segments so far, in time order.
    pub segments: &'a [Segment],
    /// Seconds since the call started. Larger than the last segment's end when
    /// the customer has gone quiet, which is what makes a silence rule possible.
    pub elapsed_secs: f64,
    pub banned_phrases: &'a [String],
}

/// Phrases that commit the business to something the agent cannot control.
///
/// Deliberately short and obvious. A long list produces false positives on
/// ordinary speech, and a rule that cries wolf is turned off.
pub fn default_banned_phrases() -> Vec<String> {
    [
        "i guarantee",
        "we guarantee",
        "guaranteed refund",
        "100% refund",
        "i promise you",
        "definitely will not",
        "never happen again",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Run every detector over the transcript so far.
///
/// Returns the complete current set, not the delta — see the module docs.
pub fn evaluate(ctx: &RuleContext) -> Vec<Finding> {
    let mut out = Vec::new();
    out.extend(detectors::pan_spoken(ctx));
    out.extend(detectors::banned_phrase(ctx));
    out.extend(detectors::no_disclosure(ctx));
    out.extend(detectors::hold_not_timeboxed(ctx));
    out.extend(detectors::talk_over(ctx));
    out.extend(detectors::monologue(ctx));
    // Worst first, so a caller that truncates keeps the ones that matter.
    out.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.code.cmp(&b.code))
    });
    out
}

/// Stable identity for a finding across repeated evaluation.
///
/// Not the id: [`Finding::id`] is freshly generated every call, so keying on it
/// would re-alert on every chunk. The code plus the segment it is anchored to
/// is what actually identifies "this problem, at this point in the call".
pub fn dedupe_key(f: &Finding) -> String {
    let anchor = f
        .citations
        .first()
        .map(|c| c.segment_id.as_str())
        .unwrap_or("");
    format!("{}@{}", f.code, anchor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::analysis::Severity;
    use ca_core::ids::SegmentId;
    use ca_core::transcript::SpeakerRole;

    pub(crate) fn seg(id: &str, role: SpeakerRole, start: f64, end: f64, text: &str) -> Segment {
        Segment {
            id: SegmentId::from(id),
            speaker_index: if role == SpeakerRole::Agent { 0 } else { 1 },
            role,
            start,
            end,
            text: text.into(),
            redacted_text: None,
            words: vec![],
            confidence: None,
            language: None,
        }
    }

    pub(crate) fn fire(segments: &[Segment], elapsed: f64) -> Vec<Finding> {
        let call = CallId::from("call_1");
        let phrases = default_banned_phrases();
        evaluate(&RuleContext {
            call_id: &call,
            segments,
            elapsed_secs: elapsed,
            banned_phrases: &phrases,
        })
    }

    #[test]
    fn an_empty_transcript_reports_nothing_rather_than_panicking() {
        assert!(fire(&[], 0.0).is_empty());
    }

    #[test]
    fn findings_are_ordered_worst_first() {
        let segs = [
            seg(
                "s1",
                SpeakerRole::Agent,
                0.0,
                4.0,
                "my name is Priya, I guarantee a full refund",
            ),
            seg(
                "s2",
                SpeakerRole::Customer,
                5.0,
                9.0,
                "my card is 4111 1111 1111 1111",
            ),
        ];
        let found = fire(&segs, 10.0);
        assert_eq!(found[0].severity, Severity::Critical, "got {found:#?}");
    }

    #[test]
    fn every_finding_is_stable_across_repeated_evaluation() {
        // The session diffs on dedupe_key to decide what is new. An unstable
        // key would re-alert on every chunk for the rest of the call.
        let segs = [seg(
            "s1",
            SpeakerRole::Agent,
            0.0,
            4.0,
            "I guarantee a full refund",
        )];
        let a: Vec<String> = fire(&segs, 10.0).iter().map(dedupe_key).collect();
        let b: Vec<String> = fire(&segs, 10.0).iter().map(dedupe_key).collect();
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn a_findings_key_survives_its_id_changing() {
        let segs = [seg(
            "s1",
            SpeakerRole::Agent,
            0.0,
            4.0,
            "I guarantee a full refund",
        )];
        let first = fire(&segs, 10.0);
        let second = fire(&segs, 10.0);
        assert_ne!(first[0].id.as_str(), second[0].id.as_str(), "ids are fresh");
        assert_eq!(dedupe_key(&first[0]), dedupe_key(&second[0]));
    }

    #[test]
    fn every_finding_carries_a_citation() {
        // A finding with nothing to point at cannot be checked, and the UI
        // turns citations into seek buttons.
        let segs = [
            seg("s1", SpeakerRole::Agent, 0.0, 4.0, "let me put you on hold"),
            seg(
                "s2",
                SpeakerRole::Customer,
                5.0,
                9.0,
                "card 4111 1111 1111 1111",
            ),
        ];
        for f in fire(&segs, 60.0) {
            assert!(!f.citations.is_empty(), "{} has no citation", f.code);
        }
    }
}
