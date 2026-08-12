//! Customer / agent sentiment trajectory.
//!
//! Produces an overall score plus timed points a chart can plot. Timings come
//! from validated citations, never from model-invented clocks.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::{SentimentPoint, SentimentReport};
use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ModelOutput {
    #[serde(default)]
    overall: f32,
    #[serde(default)]
    trajectory: Vec<ModelPoint>,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
struct ModelPoint {
    #[serde(default)]
    score: f32,
    #[serde(default)]
    label: String,
    #[serde(default)]
    speaker: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

pub struct SentimentAgent;

impl SentimentAgent {
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
    ) -> Result<(SentimentReport, ChatResponse)> {
        if transcript.segments.is_empty() {
            return Err(CaError::Config("cannot analyse an empty transcript".into()));
        }

        let req = ChatRequest::new(vec![
            Message::system(self.system_prompt()),
            Message::user(self.user_prompt(transcript)),
        ])
        .with_schema(serde_json::json!({ "type": "object" }));

        let (out, resp): (ModelOutput, _) = ca_llm::complete_json(provider, req).await?;
        Ok((self.validate(out, transcript)?, resp))
    }

    fn system_prompt(&self) -> &str {
        "You analyse sentiment on contact-centre calls.\n\
         \n\
         Rules:\n\
         - Focus on the CUSTOMER's emotional state; note agent tone only when it drives the customer.\n\
         - overall and each trajectory score are floats from -1.0 (very negative) to 1.0 (very positive).\n\
         - Every trajectory point must cite at least one real segment_id from the transcript.\n\
         - Placeholders like [PHONE_1] are redacted PII — treat them as the value they stand for.\n\
         - Return only a JSON object. No prose, no markdown fence."
    }

    fn user_prompt(&self, transcript: &Transcript) -> String {
        let mut s = String::from(
            "Build a sentiment trajectory for this call (3–8 points spanning the conversation).\n\n",
        );
        s.push_str(&citations::transcript_block(transcript));
        s.push_str(
            "\n\nReturn JSON exactly of this shape:\n\
             {\"overall\":< -1..1 >,\"trajectory\":[{\"score\":<-1..1>,\"label\":\"<short>\",\
             \"speaker\":\"customer|agent|overall\",\"citations\":[{\"segment_id\":\"<id>\",\
             \"quote\":\"<short>\"}]}],\"summary\":\"<two sentences>\"}",
        );
        s
    }

    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> Result<SentimentReport> {
        let mut trajectory = Vec::new();
        for p in out.trajectory {
            let (cites, dropped) = citations::validate_citations(&p.citations, transcript);
            if dropped > 0 {
                tracing::warn!(count = dropped, "sentiment citations dropped");
            }
            if cites.is_empty() {
                continue; // unevidenced points do not enter the chart
            }
            let t = cites.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
            let speaker = match p.speaker.to_ascii_lowercase().as_str() {
                s if s.starts_with("agent") => "agent".into(),
                s if s.starts_with("cust") || s.starts_with("caller") => "customer".into(),
                _ => "overall".into(),
            };
            trajectory.push(SentimentPoint {
                t: if t.is_finite() { t } else { 0.0 },
                score: p.score.clamp(-1.0, 1.0),
                label: p.label.trim().to_string(),
                speaker,
                citations: cites,
            });
        }
        trajectory.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));

        Ok(SentimentReport {
            call_id: transcript.call_id.clone(),
            overall: out.overall.clamp(-1.0, 1.0),
            trajectory,
            summary: out.summary.trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::ids::SegmentId;
    use ca_core::transcript::{Segment, SpeakerRole};
    use ca_core::CallId;

    fn tr() -> Transcript {
        Transcript {
            call_id: CallId::from("call_1"),
            segments: vec![Segment {
                id: SegmentId::from("seg_1"),
                speaker_index: 1,
                role: SpeakerRole::Customer,
                start: 4.0,
                end: 7.0,
                text: "This is ridiculous.".into(),
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
            duration_secs: 10.0,
        }
    }

    #[test]
    fn keeps_evidenced_points_with_transcript_time() {
        let out = ModelOutput {
            overall: -0.6,
            trajectory: vec![ModelPoint {
                score: -0.8,
                label: "frustrated".into(),
                speaker: "customer".into(),
                citations: vec![ModelCitation {
                    segment_id: "seg_1".into(),
                    quote: Some("ridiculous".into()),
                }],
            }],
            summary: "Customer is upset.".into(),
        };
        let r = SentimentAgent.validate(out, &tr()).unwrap();
        assert_eq!(r.trajectory.len(), 1);
        assert!((r.trajectory[0].t - 4.0).abs() < 1e-6);
        assert_eq!(r.trajectory[0].speaker, "customer");
    }

    #[test]
    fn drops_unevidenced_points() {
        let out = ModelOutput {
            overall: 0.0,
            trajectory: vec![ModelPoint {
                score: 1.0,
                label: "happy".into(),
                speaker: "customer".into(),
                citations: vec![ModelCitation {
                    segment_id: "nope".into(),
                    quote: None,
                }],
            }],
            summary: String::new(),
        };
        let r = SentimentAgent.validate(out, &tr()).unwrap();
        assert!(r.trajectory.is_empty());
    }
}
