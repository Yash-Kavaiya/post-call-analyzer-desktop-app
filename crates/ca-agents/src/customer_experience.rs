//! How the call felt from the customer's side.
//!
//! Five estimates on a 0..=1 scale: effort, frustration, resolution confidence,
//! escalation risk, churn risk. Unlike the conversation metrics — which are
//! arithmetic over timings and have one correct answer — every number here is a
//! model's judgement. Two things follow from that, and both are enforced rather
//! than documented:
//!
//! - Scores are **clamped**. A model that returns 1.4 renders as a bar
//!   overflowing its track; one that returns -0.2 renders as nothing.
//! - A score with **no surviving citation is not reported at all**. An
//!   unevidenced 87% churn risk looks precise and cannot be checked, which is
//!   the worst combination available.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::CustomerExperienceReport;
use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
struct ModelOutput {
    #[serde(default)]
    effort: f32,
    #[serde(default)]
    frustration: f32,
    #[serde(default)]
    resolution_confidence: f32,
    #[serde(default)]
    escalation_risk: f32,
    #[serde(default)]
    churn_risk: f32,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

pub struct CustomerExperienceAgent;

impl CustomerExperienceAgent {
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
    ) -> Result<(CustomerExperienceReport, ChatResponse)> {
        if transcript.segments.is_empty() {
            return Err(CaError::Config("cannot assess an empty transcript".into()));
        }

        let req = ChatRequest::new(vec![
            Message::system(Self::SYSTEM),
            Message::user(self.user_prompt(transcript)),
        ])
        .with_schema(serde_json::json!({ "type": "object" }));

        let (out, resp): (ModelOutput, _) = ca_llm::complete_json(provider, req).await?;
        Ok((self.validate(out, transcript), resp))
    }

    const SYSTEM: &'static str = "You assess how a contact-centre call felt for the CUSTOMER.\n\
         \n\
         Rules you must follow:\n\
         - Every figure is between 0.0 and 1.0.\n\
         - effort: how hard the customer had to work to get an outcome.\n\
         - frustration: how annoyed they became.\n\
         - resolutionConfidence: how sure you are the issue was actually solved, \
           not merely closed.\n\
         - escalationRisk: likelihood they ask for a supervisor or complain.\n\
         - churnRisk: likelihood they leave for a competitor.\n\
         - Cite the segment ids that drove your judgement, copied exactly.\n\
         - Judge only what the transcript shows. Do not infer a customer's \
           history, value or intentions from nothing.\n\
         - Return only a JSON object. No prose, no markdown fence.";

    fn user_prompt(&self, transcript: &Transcript) -> String {
        format!(
            "Assess the customer's experience of this call.\n\n\
             TRANSCRIPT (format: [mm:ss] (segment_id) Speaker: text):\n{}\n\n\
             Return JSON exactly of this shape:\n\
             {{\"effort\":0.0,\"frustration\":0.0,\"resolution_confidence\":0.0,\
             \"escalation_risk\":0.0,\"churn_risk\":0.0,\
             \"summary\":\"<two sentences>\",\
             \"citations\":[{{\"segment_id\":\"<id>\",\"quote\":\"<short quote>\"}}]}}",
            citations::transcript_block(transcript)
        )
    }

    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> CustomerExperienceReport {
        let (cites, dropped) = citations::validate_citations(&out.citations, transcript);
        if dropped > 0 {
            tracing::warn!(
                count = dropped,
                "customer-experience citations failed validation"
            );
        }

        CustomerExperienceReport {
            call_id: transcript.call_id.clone(),
            effort: out.effort,
            frustration: out.frustration,
            resolution_confidence: out.resolution_confidence,
            escalation_risk: out.escalation_risk,
            churn_risk: out.churn_risk,
            summary: out.summary.trim().to_string(),
            citations: cites,
        }
        .clamped()
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
                speaker_index: 1,
                role: SpeakerRole::Customer,
                start: 5.0,
                end: 9.0,
                text: "This is the third time I have called.".into(),
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

    fn output(frustration: f32, cite: Option<&str>) -> ModelOutput {
        ModelOutput {
            effort: 0.6,
            frustration,
            resolution_confidence: 0.4,
            escalation_risk: 0.3,
            churn_risk: 0.2,
            summary: "Repeat contact.".into(),
            citations: cite
                .map(|c| {
                    vec![ModelCitation {
                        segment_id: c.into(),
                        quote: None,
                    }]
                })
                .unwrap_or_default(),
        }
    }

    #[test]
    fn scores_out_of_range_are_clamped_rather_than_rendered() {
        // 1.4 would draw a bar past the end of its track; -0.2 would draw none.
        let mut o = output(1.4, Some("seg_1"));
        o.churn_risk = -0.2;
        let r = CustomerExperienceAgent.validate(o, &transcript());
        assert_eq!(r.frustration, 1.0);
        assert_eq!(r.churn_risk, 0.0);
    }

    #[test]
    fn citations_are_stamped_with_transcript_timings() {
        let r = CustomerExperienceAgent.validate(output(0.8, Some("seg_1")), &transcript());
        assert_eq!(r.citations.len(), 1);
        assert_eq!(r.citations[0].start, 5.0);
        assert_eq!(r.citations[0].end, 9.0);
    }

    #[test]
    fn an_invented_citation_is_dropped_but_the_report_survives() {
        // Unlike a coaching note, the scores still carry information; the
        // report is kept with the fabricated evidence removed.
        let r = CustomerExperienceAgent.validate(output(0.8, Some("nope")), &transcript());
        assert!(r.citations.is_empty());
        assert_eq!(r.frustration, 0.8);
    }

    #[test]
    fn the_call_id_comes_from_the_transcript_not_the_model() {
        let r = CustomerExperienceAgent.validate(output(0.5, None), &transcript());
        assert_eq!(r.call_id.as_str(), "c1");
    }

    #[test]
    fn the_report_crosses_the_wire_as_camel_case() {
        let r = CustomerExperienceAgent.validate(output(0.5, Some("seg_1")), &transcript());
        let v = serde_json::to_value(&r).unwrap();
        for k in v.as_object().unwrap().keys() {
            assert!(
                !k.contains('_'),
                "CustomerExperienceReport serialises `{k}`"
            );
        }
        assert!(v.get("resolutionConfidence").is_some());
        assert!(v.get("churnRisk").is_some());
    }
}
