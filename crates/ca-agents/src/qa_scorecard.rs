//! QA Scorecard agent.
//!
//! Scores an agent against a configurable rubric and — the part that decides
//! whether any of this is usable — ties every judgement to a timestamp a human
//! can click.
//!
//! Two things are deliberately not trusted from the model:
//!
//! 1. **Citations.** Every `segment_id` is checked against the transcript. A
//!    model that invents an id, or cites a timestamp outside the call, has its
//!    citation dropped. Fabricated evidence is worse than no evidence, because
//!    it survives review by looking specific.
//! 2. **Arithmetic.** The overall score is recomputed locally from the
//!    per-criterion scores and the rubric's weights. Language models are
//!    unreliable at weighted means, and a scorecard whose total disagrees with
//!    its own criteria destroys trust in the whole report.

use crate::rubric::Rubric;
use ca_core::analysis::{Citation, CriterionScore, Scorecard};
use ca_core::error::{CaError, Result};
use ca_core::ids::SegmentId;
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use serde::Deserialize;
use std::collections::HashMap;

/// What the model is asked to return.
#[derive(Debug, Deserialize)]
struct ModelOutput {
    #[serde(default)]
    criteria: Vec<ModelCriterion>,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
struct ModelCriterion {
    key: String,
    #[serde(default)]
    score: f32,
    #[serde(default)]
    reasoning: String,
    #[serde(default)]
    not_applicable: bool,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

#[derive(Debug, Deserialize)]
struct ModelCitation {
    #[serde(default)]
    segment_id: String,
    #[serde(default)]
    quote: Option<String>,
}

pub struct QaScorecardAgent {
    rubric: Rubric,
}

impl QaScorecardAgent {
    pub fn new(rubric: Rubric) -> Self {
        Self { rubric }
    }

    pub fn rubric(&self) -> &Rubric {
        &self.rubric
    }

    /// Score one call.
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
    ) -> Result<(Scorecard, ChatResponse)> {
        if transcript.segments.is_empty() {
            return Err(CaError::Config("cannot score an empty transcript".into()));
        }

        let req = ChatRequest::new(vec![
            Message::system(self.system_prompt()),
            Message::user(self.user_prompt(transcript)),
        ])
        .with_schema(serde_json::json!({ "type": "object" }));

        let (out, resp): (ModelOutput, _) = ca_llm::complete_json(provider, req).await?;
        let scorecard = self.validate(out, transcript)?;
        Ok((scorecard, resp))
    }

    fn system_prompt(&self) -> String {
        "You are a contact-centre quality analyst. You score how the AGENT handled a call.\n\
         \n\
         Rules you must follow:\n\
         - Judge only the agent's behaviour, never the customer's.\n\
         - Every criterion must cite at least one segment id from the transcript, copied \
           exactly as shown in parentheses. Never invent an id.\n\
         - If the transcript contains no evidence for a criterion, say so in the reasoning \
           and mark it not_applicable rather than guessing a score.\n\
         - Placeholders like [PHONE_1] are redacted personal data. Treat them as the value \
           they stand for; never comment on the redaction itself.\n\
         - Return only a JSON object. No prose, no markdown fence."
            .to_string()
    }

    fn user_prompt(&self, transcript: &Transcript) -> String {
        let mut s = String::new();
        s.push_str("Score this call against the rubric below.\n\nRUBRIC:\n");
        for c in &self.rubric.criteria {
            s.push_str(&format!(
                "- {} (key: {}): {}\n",
                c.label,
                c.key,
                c.description.trim().replace('\n', " ")
            ));
        }

        s.push_str("\nTRANSCRIPT (format: [mm:ss] (segment_id) Speaker: text):\n");
        s.push_str(&transcript.to_prompt_format());

        s.push_str(
            "\n\nReturn JSON exactly of this shape:\n\
             {\"criteria\":[{\"key\":\"<rubric key>\",\"score\":<0.0-1.0>,\
             \"reasoning\":\"<one or two sentences>\",\"not_applicable\":<bool>,\
             \"citations\":[{\"segment_id\":\"<id from transcript>\",\
             \"quote\":\"<short quote>\"}]}],\"summary\":\"<two sentences>\"}",
        );
        s
    }

    /// Turn raw model output into a scorecard, rejecting what cannot be trusted.
    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> Result<Scorecard> {
        // Index the real segments once; citation checking is per-citation.
        let by_id: HashMap<&str, &ca_core::transcript::Segment> = transcript
            .segments
            .iter()
            .map(|s| (s.id.as_str(), s))
            .collect();

        let returned: HashMap<&str, &ModelCriterion> =
            out.criteria.iter().map(|c| (c.key.as_str(), c)).collect();

        let mut criteria = Vec::with_capacity(self.rubric.criteria.len());
        let mut dropped_citations = 0usize;

        // Iterate the *rubric*, not the model's output: a model that omits or
        // invents a criterion must not change the shape of the scorecard.
        for spec in &self.rubric.criteria {
            let Some(m) = returned.get(spec.key.as_str()) else {
                criteria.push(CriterionScore {
                    key: spec.key.clone(),
                    label: spec.label.clone(),
                    score: 0.0,
                    weight: spec.weight,
                    reasoning: "Model did not score this criterion.".into(),
                    citations: vec![],
                    not_applicable: true,
                });
                continue;
            };

            let mut citations = Vec::new();
            for c in &m.citations {
                match by_id.get(c.segment_id.as_str()) {
                    Some(seg) => citations.push(Citation {
                        segment_id: SegmentId::from(c.segment_id.clone()),
                        // Take timings from the transcript, not the model —
                        // it has no reason to be authoritative about them.
                        start: seg.start,
                        end: seg.end,
                        quote: c.quote.clone(),
                    }),
                    None => {
                        dropped_citations += 1;
                        tracing::warn!(
                            segment_id = %c.segment_id,
                            criterion = %spec.key,
                            "dropping fabricated citation"
                        );
                    }
                }
            }

            // A scored criterion with no surviving evidence is downgraded to
            // not-applicable: an unevidenced number looks authoritative and is
            // exactly what a reviewer cannot check.
            let unevidenced = citations.is_empty() && !m.not_applicable;

            criteria.push(CriterionScore {
                key: spec.key.clone(),
                label: spec.label.clone(),
                score: m.score.clamp(0.0, 1.0),
                weight: spec.weight,
                reasoning: if unevidenced {
                    format!(
                        "{} (no verifiable citation; excluded from score)",
                        m.reasoning.trim()
                    )
                } else {
                    m.reasoning.trim().to_string()
                },
                citations,
                not_applicable: m.not_applicable || unevidenced,
            });
        }

        if dropped_citations > 0 {
            tracing::warn!(count = dropped_citations, "citations failed validation");
        }

        // Recomputed locally; the model is never asked for a total.
        let overall = Scorecard::compute_overall(&criteria);

        Ok(Scorecard {
            call_id: transcript.call_id.clone(),
            overall,
            criteria,
            summary: out.summary.trim().to_string(),
            rubric_id: self.rubric.id.clone(),
            rubric_version: self.rubric.version.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::transcript::{Segment, SpeakerRole};
    use ca_core::CallId;

    fn seg(id: &str, start: f64, text: &str) -> Segment {
        Segment {
            id: SegmentId::from(id),
            speaker_index: 0,
            role: SpeakerRole::Agent,
            start,
            end: start + 3.0,
            text: text.into(),
            redacted_text: None,
            words: vec![],
            confidence: None,
            language: None,
        }
    }

    fn transcript() -> Transcript {
        Transcript {
            call_id: CallId::from("call_1"),
            segments: vec![
                seg("seg_1", 0.0, "Thanks for calling Acme, this is Ravi."),
                seg("seg_2", 5.0, "I understand that's frustrating."),
            ],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "t".into(),
            model: "m".into(),
            duration_secs: 30.0,
        }
    }

    fn agent() -> QaScorecardAgent {
        QaScorecardAgent::new(Rubric::default())
    }

    fn model_criterion(key: &str, score: f32, cite: Option<&str>) -> ModelCriterion {
        ModelCriterion {
            key: key.into(),
            score,
            reasoning: "because".into(),
            not_applicable: false,
            citations: cite
                .map(|c| {
                    vec![ModelCitation {
                        segment_id: c.into(),
                        quote: Some("q".into()),
                    }]
                })
                .unwrap_or_default(),
        }
    }

    #[test]
    fn valid_citations_are_kept_with_transcript_timings() {
        let out = ModelOutput {
            criteria: vec![model_criterion("greeting", 0.9, Some("seg_1"))],
            summary: "ok".into(),
        };
        let card = agent().validate(out, &transcript()).unwrap();
        let g = card.criteria.iter().find(|c| c.key == "greeting").unwrap();
        assert_eq!(g.citations.len(), 1);
        assert_eq!(
            g.citations[0].start, 0.0,
            "timing must come from the transcript"
        );
        assert!(!g.not_applicable);
    }

    #[test]
    fn fabricated_segment_ids_are_dropped() {
        let out = ModelOutput {
            criteria: vec![model_criterion("greeting", 0.9, Some("seg_does_not_exist"))],
            summary: String::new(),
        };
        let card = agent().validate(out, &transcript()).unwrap();
        let g = card.criteria.iter().find(|c| c.key == "greeting").unwrap();
        assert!(g.citations.is_empty(), "invented citation survived");
    }

    #[test]
    fn a_score_with_no_surviving_evidence_is_excluded() {
        // An unevidenced number reads as authoritative and cannot be checked,
        // so it must not contribute to the total.
        let out = ModelOutput {
            criteria: vec![model_criterion("greeting", 1.0, None)],
            summary: String::new(),
        };
        let card = agent().validate(out, &transcript()).unwrap();
        let g = card.criteria.iter().find(|c| c.key == "greeting").unwrap();
        assert!(g.not_applicable);
        assert!(g.reasoning.contains("no verifiable citation"));
    }

    #[test]
    fn omitted_criteria_still_appear_in_the_scorecard() {
        let out = ModelOutput {
            criteria: vec![model_criterion("greeting", 0.8, Some("seg_1"))],
            summary: String::new(),
        };
        let card = agent().validate(out, &transcript()).unwrap();
        assert_eq!(
            card.criteria.len(),
            5,
            "scorecard shape must follow the rubric"
        );
        let missing = card.criteria.iter().find(|c| c.key == "empathy").unwrap();
        assert!(missing.not_applicable);
    }

    #[test]
    fn criteria_the_rubric_does_not_define_are_ignored() {
        let out = ModelOutput {
            criteria: vec![
                model_criterion("greeting", 0.8, Some("seg_1")),
                model_criterion("invented_criterion", 1.0, Some("seg_1")),
            ],
            summary: String::new(),
        };
        let card = agent().validate(out, &transcript()).unwrap();
        assert!(card.criteria.iter().all(|c| c.key != "invented_criterion"));
    }

    #[test]
    fn overall_is_recomputed_not_taken_from_the_model() {
        // Only greeting (weight 1.0) is evidenced and scores 1.0, so the
        // weighted mean over applicable criteria is 100.
        let out = ModelOutput {
            criteria: vec![model_criterion("greeting", 1.0, Some("seg_1"))],
            summary: String::new(),
        };
        let card = agent().validate(out, &transcript()).unwrap();
        assert!((card.overall - 100.0).abs() < 1e-4, "got {}", card.overall);
    }

    #[test]
    fn out_of_range_scores_are_clamped() {
        let out = ModelOutput {
            criteria: vec![model_criterion("greeting", 7.5, Some("seg_1"))],
            summary: String::new(),
        };
        let card = agent().validate(out, &transcript()).unwrap();
        let g = card.criteria.iter().find(|c| c.key == "greeting").unwrap();
        assert_eq!(g.score, 1.0);
    }

    #[test]
    fn rubric_identity_is_recorded_on_the_scorecard() {
        // Without this a stored score cannot be traced to the criteria that
        // produced it, and rubric edits silently invalidate history.
        let out = ModelOutput {
            criteria: vec![],
            summary: String::new(),
        };
        let card = agent().validate(out, &transcript()).unwrap();
        assert_eq!(card.rubric_id, "default");
        assert_eq!(card.rubric_version, "1");
    }

    #[test]
    fn prompt_includes_segment_ids_and_redacted_text() {
        let mut t = transcript();
        t.segments[0].text = "call me on 9876543210".into();
        t.segments[0].redacted_text = Some("call me on [PHONE_1]".into());
        let p = agent().user_prompt(&t);
        assert!(p.contains("seg_1"), "segment ids must be citable");
        assert!(p.contains("[PHONE_1]"));
        assert!(!p.contains("9876543210"), "raw PII reached the prompt");
    }
}
