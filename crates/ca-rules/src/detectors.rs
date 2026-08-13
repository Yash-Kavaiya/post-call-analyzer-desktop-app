//! One function per rule.
//!
//! Each takes the whole transcript so far and returns every hit it can see, so
//! they compose by concatenation and can be read in isolation. All of them
//! anchor their finding to a real segment: a finding with nothing to point at
//! cannot be checked, and the UI turns each citation into a seek button.

use crate::RuleContext;
use ca_core::analysis::{Citation, Finding, Severity};
use ca_core::ids::FindingId;
use ca_core::transcript::{Segment, SpeakerRole};
use ca_core::AgentKind;
use ca_privacy::patterns::{self, EntityKind};
use regex::Regex;
use std::sync::LazyLock;

/// How long the agent has to identify themselves before it counts as missing.
const DISCLOSURE_WINDOW_SECS: f64 = 30.0;

/// Overlap between two speakers before it stops being a back-channel ("mhm")
/// and starts being one talking over the other.
const TALK_OVER_SECS: f64 = 1.5;

/// One speaker holding the floor this long without a turn is a lecture.
const MONOLOGUE_SECS: f64 = 90.0;

static HOLD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(put you on hold|on hold|hold on|bear with me|hold the line|give me a (?:moment|second|minute))\b").unwrap()
});

/// A stated duration — "two minutes", "30 seconds", "a couple of minutes".
static DURATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:\d+|a|one|two|three|four|five|ten|fifteen|twenty|thirty|couple(?: of)?|few)\s*(?:-|\s)?\s*(?:sec|second|seconds|min|minute|minutes)\b",
    )
    .unwrap()
});

/// The agent naming themselves or the business.
static DISCLOSURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(my name is|this is \w+|you'?re speaking (?:with|to)|calling from|i'?m calling on behalf of|thank you for calling|welcome to)\b",
    )
    .unwrap()
});

fn cite(s: &Segment, quote: Option<String>) -> Citation {
    Citation {
        segment_id: s.id.clone(),
        start: s.start,
        end: s.end,
        quote,
    }
}

fn finding(
    ctx: &RuleContext,
    code: &str,
    title: &str,
    detail: String,
    severity: Severity,
    citations: Vec<Citation>,
) -> Finding {
    Finding {
        id: FindingId::new(),
        call_id: ctx.call_id.clone(),
        agent: AgentKind::Rules,
        code: code.into(),
        title: title.into(),
        detail,
        severity,
        citations,
    }
}

/// A payment card or Aadhaar number spoken aloud.
///
/// The reason the rules tier exists. Detection is checksum-validated by
/// `ca-privacy` rather than pattern-matched, so an order reference that happens
/// to be sixteen digits does not raise a critical alert — a rule that cries
/// wolf on every long number is a rule someone turns off.
pub fn pan_spoken(ctx: &RuleContext) -> Vec<Finding> {
    let mut out = Vec::new();
    for s in ctx.segments {
        // Raw text on purpose: this rule exists to see the digits, and it never
        // leaves the machine.
        let hits = patterns::detect_all(&s.text, &[EntityKind::Card, EntityKind::Aadhaar]);
        // One finding per segment is enough to act on. Three card numbers in
        // one sentence is one problem, not three alerts.
        let Some((_, _, kind, _)) = hits.into_iter().next() else {
            continue;
        };
        let (code, what) = match kind {
            EntityKind::Card => ("PCI_PAN_SPOKEN", "A payment card number"),
            _ => ("AADHAAR_SPOKEN", "An Aadhaar number"),
        };
        out.push(finding(
            ctx,
            code,
            "Sensitive number spoken aloud",
            format!(
                "{what} was said on the call by the {}. It is now in the recording.",
                role_word(s.role)
            ),
            Severity::Critical,
            // The number itself is deliberately not quoted: the quote is shown
            // in the UI and stored in the ledger.
            vec![cite(s, None)],
        ));
    }
    out
}

/// A commitment the agent cannot keep.
pub fn banned_phrase(ctx: &RuleContext) -> Vec<Finding> {
    let mut out = Vec::new();
    for s in agent_segments(ctx) {
        let lower = s.text.to_lowercase();
        for phrase in ctx.banned_phrases {
            if lower.contains(&phrase.to_lowercase()) {
                out.push(finding(
                    ctx,
                    "BANNED_PHRASE",
                    "Unsupportable commitment",
                    format!("The agent said \"{phrase}\", which commits to an outcome they do not control."),
                    Severity::Medium,
                    vec![cite(s, Some(phrase.clone()))],
                ));
                break; // the first match per segment is the finding
            }
        }
    }
    out
}

/// No identification in the opening.
///
/// Only fires once the window has actually elapsed. Reporting it at four
/// seconds would be alerting on a call that is still being answered.
pub fn no_disclosure(ctx: &RuleContext) -> Vec<Finding> {
    if ctx.elapsed_secs < DISCLOSURE_WINDOW_SECS {
        return vec![];
    }
    let opening: Vec<&Segment> = agent_segments(ctx)
        .into_iter()
        .filter(|s| s.start < DISCLOSURE_WINDOW_SECS)
        .collect();
    if opening.is_empty() {
        // The agent has not spoken at all yet. That is a different problem and
        // not one this rule can evidence.
        return vec![];
    }
    if opening.iter().any(|s| DISCLOSURE.is_match(&s.text)) {
        return vec![];
    }
    vec![finding(
        ctx,
        "NO_DISCLOSURE",
        "No identification at open",
        format!(
            "The agent did not name themselves or the business in the first {:.0} seconds.",
            DISCLOSURE_WINDOW_SECS
        ),
        Severity::Medium,
        vec![cite(opening[0], None)],
    )]
}

/// A hold with no expectation set.
///
/// Silence with no stated length is the most common avoidable complaint on a
/// call, and it is trivially detectable.
pub fn hold_not_timeboxed(ctx: &RuleContext) -> Vec<Finding> {
    let mut out = Vec::new();
    for s in agent_segments(ctx) {
        if !HOLD.is_match(&s.text) || DURATION.is_match(&s.text) {
            continue;
        }
        out.push(finding(
            ctx,
            "HOLD_NOT_TIMEBOXED",
            "Hold placed without a stated duration",
            "The agent went to hold without saying how long it would take.".into(),
            Severity::High,
            vec![cite(s, None)],
        ));
    }
    out
}

/// Two people talking at once for long enough that one of them lost the thread.
pub fn talk_over(ctx: &RuleContext) -> Vec<Finding> {
    let mut out = Vec::new();
    for pair in ctx.segments.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if a.speaker_index == b.speaker_index {
            continue;
        }
        let overlap = (a.end.min(b.end) - b.start.max(a.start)).max(0.0);
        if overlap < TALK_OVER_SECS {
            continue;
        }
        out.push(finding(
            ctx,
            "TALK_OVER",
            "Speakers talked over each other",
            format!("{overlap:.1}s of overlapping speech."),
            Severity::Low,
            vec![cite(b, None)],
        ));
    }
    out
}

/// One speaker holding the floor without a turn.
pub fn monologue(ctx: &RuleContext) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut run_start: Option<&Segment> = None;
    let mut previous: Option<&Segment> = None;

    for s in ctx.segments {
        let same_speaker = previous.is_some_and(|p| p.speaker_index == s.speaker_index);
        if !same_speaker {
            if let (Some(start), Some(end)) = (run_start, previous) {
                if let Some(f) = long_run(ctx, start, end) {
                    out.push(f);
                }
            }
            run_start = Some(s);
        }
        previous = Some(s);
    }
    // The run in progress counts too: a monologue still happening is exactly
    // the one worth interrupting.
    if let (Some(start), Some(end)) = (run_start, previous) {
        if let Some(f) = long_run(ctx, start, end) {
            out.push(f);
        }
    }
    out
}

fn long_run(ctx: &RuleContext, start: &Segment, end: &Segment) -> Option<Finding> {
    let secs = end.end - start.start;
    if secs < MONOLOGUE_SECS {
        return None;
    }
    Some(finding(
        ctx,
        "MONOLOGUE",
        "One speaker held the floor",
        format!(
            "The {} spoke for {secs:.0}s without a turn.",
            role_word(start.role)
        ),
        Severity::Low,
        vec![cite(start, None)],
    ))
}

fn agent_segments<'a>(ctx: &'a RuleContext) -> Vec<&'a Segment> {
    ctx.segments
        .iter()
        .filter(|s| s.role == SpeakerRole::Agent)
        .collect()
}

fn role_word(role: SpeakerRole) -> &'static str {
    match role {
        SpeakerRole::Agent => "agent",
        SpeakerRole::Customer => "customer",
        SpeakerRole::Unknown => "speaker",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{fire, seg};

    fn codes(segments: &[Segment], elapsed: f64) -> Vec<String> {
        fire(segments, elapsed)
            .into_iter()
            .map(|f| f.code)
            .collect()
    }

    // ------------------------------------------------------------------ PAN

    #[test]
    fn a_spoken_card_number_is_critical() {
        let segs = [seg(
            "s1",
            SpeakerRole::Customer,
            0.0,
            10.0,
            "my card is 4111 1111 1111 1111",
        )];
        let f = fire(&segs, 12.0);
        let pan = f.iter().find(|f| f.code == "PCI_PAN_SPOKEN").unwrap();
        assert_eq!(pan.severity, Severity::Critical);
        assert_eq!(pan.citations[0].segment_id.as_str(), "s1");
    }

    #[test]
    fn a_number_that_fails_luhn_is_not_a_card() {
        // An order reference that happens to be sixteen digits must not raise
        // a critical alert; a rule that cries wolf gets turned off.
        let segs = [seg(
            "s1",
            SpeakerRole::Customer,
            0.0,
            10.0,
            "my reference is 4111 1111 1111 1112",
        )];
        assert!(!codes(&segs, 12.0).contains(&"PCI_PAN_SPOKEN".to_string()));
    }

    #[test]
    fn the_card_number_is_never_quoted_into_the_finding() {
        // The quote is rendered in the UI and written to the findings ledger.
        // Copying the PAN there would leak it into exactly the places this
        // rule exists to protect.
        let segs = [seg(
            "s1",
            SpeakerRole::Customer,
            0.0,
            10.0,
            "my card is 4111 1111 1111 1111",
        )];
        for f in fire(&segs, 12.0) {
            let blob = format!("{}{}{:?}", f.title, f.detail, f.citations);
            assert!(!blob.contains("4111"), "the PAN leaked into {}", f.code);
        }
    }

    // --------------------------------------------------------------- phrases

    #[test]
    fn an_unsupportable_commitment_is_reported() {
        let segs = [seg(
            "s1",
            SpeakerRole::Agent,
            0.0,
            5.0,
            "I guarantee this will be refunded today",
        )];
        assert!(codes(&segs, 6.0).contains(&"BANNED_PHRASE".to_string()));
    }

    #[test]
    fn the_customer_saying_it_is_not_the_agents_problem() {
        let segs = [seg(
            "s1",
            SpeakerRole::Customer,
            0.0,
            5.0,
            "you said I guarantee it would be refunded",
        )];
        assert!(!codes(&segs, 6.0).contains(&"BANNED_PHRASE".to_string()));
    }

    // ------------------------------------------------------------ disclosure

    #[test]
    fn a_missing_disclosure_fires_only_after_the_opening_window() {
        let segs = [seg(
            "s1",
            SpeakerRole::Agent,
            0.0,
            5.0,
            "yeah what do you want",
        )];
        assert!(
            !codes(&segs, 10.0).contains(&"NO_DISCLOSURE".to_string()),
            "alerted while the call was still being answered"
        );
        assert!(codes(&segs, 40.0).contains(&"NO_DISCLOSURE".to_string()));
    }

    #[test]
    fn naming_yourself_satisfies_disclosure() {
        let segs = [seg(
            "s1",
            SpeakerRole::Agent,
            0.0,
            5.0,
            "Thank you for calling Northwind, my name is Priya",
        )];
        assert!(!codes(&segs, 40.0).contains(&"NO_DISCLOSURE".to_string()));
    }

    #[test]
    fn an_agent_who_has_not_spoken_yet_is_not_reported() {
        // Nothing to cite, and nothing they have done wrong.
        let segs = [seg("s1", SpeakerRole::Customer, 0.0, 5.0, "hello?")];
        assert!(!codes(&segs, 40.0).contains(&"NO_DISCLOSURE".to_string()));
    }

    // ------------------------------------------------------------------ hold

    #[test]
    fn a_hold_without_a_duration_is_blocking() {
        let segs = [seg(
            "s1",
            SpeakerRole::Agent,
            0.0,
            5.0,
            "let me put you on hold while I check",
        )];
        let f = fire(&segs, 6.0);
        let hold = f.iter().find(|f| f.code == "HOLD_NOT_TIMEBOXED").unwrap();
        assert!(hold.severity.is_blocking());
    }

    #[test]
    fn a_hold_with_a_duration_is_not_reported() {
        for text in [
            "let me put you on hold for two minutes",
            "hold on for 30 seconds while I check",
            "bear with me a couple of minutes",
        ] {
            let segs = [seg("s1", SpeakerRole::Agent, 0.0, 5.0, text)];
            assert!(
                !codes(&segs, 6.0).contains(&"HOLD_NOT_TIMEBOXED".to_string()),
                "reported a time-boxed hold: {text}"
            );
        }
    }

    // ------------------------------------------------------------- talk-over

    #[test]
    fn sustained_overlap_is_reported() {
        let segs = [
            seg("s1", SpeakerRole::Agent, 0.0, 8.0, "so what I can do is"),
            seg("s2", SpeakerRole::Customer, 5.0, 9.0, "no listen to me"),
        ];
        assert!(codes(&segs, 10.0).contains(&"TALK_OVER".to_string()));
    }

    #[test]
    fn a_back_channel_is_not_talking_over() {
        // "mhm" landing on the tail of a sentence is normal conversation.
        let segs = [
            seg("s1", SpeakerRole::Agent, 0.0, 8.0, "so what I can do is"),
            seg("s2", SpeakerRole::Customer, 7.6, 8.4, "mhm"),
        ];
        assert!(!codes(&segs, 10.0).contains(&"TALK_OVER".to_string()));
    }

    // ------------------------------------------------------------- monologue

    #[test]
    fn a_long_uninterrupted_run_is_reported() {
        let segs = [
            seg("s1", SpeakerRole::Agent, 0.0, 40.0, "first"),
            seg("s2", SpeakerRole::Agent, 40.0, 95.0, "still going"),
        ];
        assert!(codes(&segs, 96.0).contains(&"MONOLOGUE".to_string()));
    }

    #[test]
    fn a_run_broken_by_the_other_speaker_is_not_a_monologue() {
        let segs = [
            seg("s1", SpeakerRole::Agent, 0.0, 40.0, "first"),
            seg("s2", SpeakerRole::Customer, 41.0, 44.0, "right"),
            seg("s3", SpeakerRole::Agent, 45.0, 95.0, "carrying on"),
        ];
        assert!(!codes(&segs, 96.0).contains(&"MONOLOGUE".to_string()));
    }
}
