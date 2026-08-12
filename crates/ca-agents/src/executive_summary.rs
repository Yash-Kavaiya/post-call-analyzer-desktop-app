//! The paragraph a supervisor reads instead of listening to the call.
//!
//! This is the only agent whose output is likely to be read *without* the
//! transcript beside it, which changes what validation has to do. Elsewhere a
//! dropped citation costs a link; here it means a paragraph gets quoted in a
//! report with nothing behind it. So the summary is required to cite, and a
//! summary that cites nothing is marked as unverified rather than presented as
//! though someone checked it.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::{ExecutiveSummary, Severity};
use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
struct ModelOutput {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    primary_issue: String,
    #[serde(default)]
    resolution: String,
    #[serde(default)]
    customer_outcome: String,
    #[serde(default)]
    risk: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

pub struct ExecutiveSummaryAgent;

impl ExecutiveSummaryAgent {
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
    ) -> Result<(ExecutiveSummary, ChatResponse)> {
        if transcript.segments.is_empty() {
            return Err(CaError::Config("cannot summarise an empty transcript".into()));
        }

        let req = ChatRequest::new(vec![
            Message::system(Self::SYSTEM),
            Message::user(self.user_prompt(transcript)),
        ])
        .with_schema(serde_json::json!({ "type": "object" }));

        let (out, resp): (ModelOutput, _) = ca_llm::complete_json(provider, req).await?;
        Ok((self.validate(out, transcript), resp))
    }

    const SYSTEM: &'static str =
        "You summarise contact-centre calls for a supervisor who will not listen \
         to the recording.\n\
         \n\
         Rules you must follow:\n\
         - Three or four sentences of summary. No preamble, no restating the task.\n\
         - primaryIssue: the customer's actual problem, in a short phrase.\n\
         - resolution: what was done about it. If nothing was, say so plainly.\n\
         - customerOutcome is exactly one of: positive, neutral, negative.\n\
         - risk is one of: info, low, medium, high, critical — the business risk \
           this call leaves behind, not how difficult it was.\n\
         - Cite the segment ids the summary rests on, copied exactly.\n\
         - State only what the transcript supports. Never infer an outcome that \
           was not reached during the call.\n\
         - Return only a JSON object. No prose, no markdown fence.";

    fn user_prompt(&self, transcript: &Transcript) -> String {
        format!(
            "Summarise this call.\n\n\
             TRANSCRIPT (format: [mm:ss] (segment_id) Speaker: text):\n{}\n\n\
             Return JSON exactly of this shape:\n\
             {{\"summary\":\"<three or four sentences>\",\
             \"primary_issue\":\"<short phrase>\",\
             \"resolution\":\"<what was done>\",\
             \"customer_outcome\":\"positive|neutral|negative\",\
             \"risk\":\"info|low|medium|high|critical\",\
             \"citations\":[{{\"segment_id\":\"<id>\",\"quote\":\"<short quote>\"}}]}}",
            citations::transcript_block(transcript)
        )
    }

    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> ExecutiveSummary {
        let (cites, dropped) = citations::validate_citations(&out.citations, transcript);
        if dropped > 0 {
            tracing::warn!(count = dropped, "executive-summary citations failed validation");
        }

        let mut summary = out.summary.trim().to_string();
        // This paragraph gets pasted into reports on its own. If nothing behind
        // it survived validation, it must not read as though someone checked it.
        if cites.is_empty() && !summary.is_empty() {
            summary.push_str(" (Unverified: no citation in this summary resolved to the transcript.)");
        }

        ExecutiveSummary {
            call_id: transcript.call_id.clone(),
            summary,
            primary_issue: out.primary_issue.trim().to_string(),
            resolution: out.resolution.trim().to_string(),
            customer_outcome: normalise_outcome(&out.customer_outcome),
            risk: parse_severity(&out.risk).unwrap_or(Severity::Info),
            citations: cites,
        }
    }
}

/// Anything unrecognised becomes `neutral`.
///
/// The UI colours this field. A value it does not know renders as no colour at
/// all, which reads as a styling bug rather than as missing data.
fn normalise_outcome(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "positive" | "good" | "satisfied" => "positive".into(),
        "negative" | "bad" | "dissatisfied" => "negative".into(),
        _ => "neutral".into(),
    }
}

fn parse_severity(s: &str) -> Option<Severity> {
    match s.trim().to_ascii_lowercase().as_str() {
        "info" | "none" => Some(Severity::Info),
        "low" => Some(Severity::Low),
        "medium" | "moderate" => Some(Severity::Medium),
        "high" => Some(Severity::High),
        "critical" | "severe" => Some(Severity::Critical),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::ids::{CallId, SegmentId};
    use ca_core::transcript::{Segment, SpeakerRole};

    fn transcript() -> Transcript {
        Transcript {
            call_id: CallId::from("c1"),
            segments: vec![Segment {
                id: SegmentId::from("seg_1"),
                speaker_index: 0,
                role: SpeakerRole::Agent,
                start: 2.0,
                end: 8.0,
                text: "I have raised the reversal, reference RV-88214.".into(),
                redacted_text: None,
                words: vec![],
                confidence: None,
                language: None,
            }],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "t".into(),
            model: "m".into(),
            duration_secs: 30.0,
        }
    }

    fn output(outcome: &str, risk: &str, cite: Option<&str>) -> ModelOutput {
        ModelOutput {
            summary: "The customer was charged twice and the agent reversed it.".into(),
            primary_issue: "Duplicate charge".into(),
            resolution: "Reversal raised".into(),
            customer_outcome: outcome.into(),
            risk: risk.into(),
            citations: cite
                .map(|c| vec![ModelCitation { segment_id: c.into(), quote: None }])
                .unwrap_or_default(),
        }
    }

    #[test]
    fn a_cited_summary_is_kept_verbatim() {
        let r = ExecutiveSummaryAgent.validate(output("positive", "low", Some("seg_1")), &transcript());
        assert!(!r.summary.contains("Unverified"));
        assert_eq!(r.citations[0].start, 2.0);
        assert_eq!(r.risk, Severity::Low);
        assert_eq!(r.customer_outcome, "positive");
    }

    #[test]
    fn an_uncited_summary_says_so_in_its_own_text() {
        // This paragraph is the one most likely to be read alone and pasted
        // into a report; it has to carry its own provenance.
        let r = ExecutiveSummaryAgent.validate(output("positive", "low", None), &transcript());
        assert!(r.summary.contains("Unverified"), "got {}", r.summary);
    }

    #[test]
    fn a_fabricated_citation_leaves_the_summary_marked_unverified() {
        let r = ExecutiveSummaryAgent.validate(output("positive", "low", Some("seg_nope")), &transcript());
        assert!(r.citations.is_empty());
        assert!(r.summary.contains("Unverified"));
    }

    #[test]
    fn an_unrecognised_outcome_becomes_neutral_rather_than_uncoloured() {
        for odd in ["delighted", "", "MIXED"] {
            let r = ExecutiveSummaryAgent.validate(output(odd, "low", Some("seg_1")), &transcript());
            assert_eq!(r.customer_outcome, "neutral", "input {odd:?}");
        }
    }

    #[test]
    fn common_synonyms_for_outcome_and_risk_are_accepted() {
        let r = ExecutiveSummaryAgent.validate(output("satisfied", "moderate", Some("seg_1")), &transcript());
        assert_eq!(r.customer_outcome, "positive");
        assert_eq!(r.risk, Severity::Medium);
    }

    #[test]
    fn an_unknown_risk_falls_back_to_info_not_critical() {
        // Guessing high would flood a review queue with calls nobody flagged.
        let r = ExecutiveSummaryAgent.validate(output("neutral", "spicy", Some("seg_1")), &transcript());
        assert_eq!(r.risk, Severity::Info);
    }

    #[test]
    fn an_empty_summary_is_not_annotated() {
        let mut o = output("neutral", "low", None);
        o.summary = String::new();
        let r = ExecutiveSummaryAgent.validate(o, &transcript());
        assert!(r.summary.is_empty(), "annotated an empty summary");
    }
}
