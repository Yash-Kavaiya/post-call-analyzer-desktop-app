//! Root-cause analysis for why the contact happened and what to fix.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::RootCauseReport;
use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ModelOutput {
    #[serde(default)]
    primary_cause: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    contributing_factors: Vec<String>,
    #[serde(default)]
    recommended_fix: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
    #[serde(default)]
    summary: String,
}

const CATEGORIES: &[&str] = &["billing", "product", "process", "agent", "other"];

pub struct RootCauseAgent;

impl RootCauseAgent {
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
    ) -> Result<(RootCauseReport, ChatResponse)> {
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
        "You are a contact-centre root-cause analyst.\n\
         \n\
         Identify the underlying reason the customer contacted support and one \
         concrete fix the business should consider.\n\
         \n\
         Rules:\n\
         - category must be one of: billing, product, process, agent, other.\n\
         - confidence is 0.0..=1.0.\n\
         - Cite real segment_ids that support the primary cause.\n\
         - Placeholders like [PHONE_1] are redacted PII.\n\
         - Return only JSON. No prose, no markdown fence."
    }

    fn user_prompt(&self, transcript: &Transcript) -> String {
        let mut s = String::from("Determine the root cause of this contact.\n\n");
        s.push_str(&citations::transcript_block(transcript));
        s.push_str(
            "\n\nReturn JSON exactly of this shape:\n\
             {\"primary_cause\":\"...\",\"category\":\"billing|product|process|agent|other\",\
             \"confidence\":0.0,\"contributing_factors\":[\"...\"],\
             \"recommended_fix\":\"...\",\
             \"citations\":[{\"segment_id\":\"<id>\",\"quote\":\"...\"}],\
             \"summary\":\"<two sentences>\"}",
        );
        s
    }

    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> Result<RootCauseReport> {
        let (cites, dropped) = citations::validate_citations(&out.citations, transcript);
        if dropped > 0 {
            tracing::warn!(count = dropped, "root-cause citations dropped");
        }

        let cat = out.category.trim().to_ascii_lowercase();
        let category = if CATEGORIES.contains(&cat.as_str()) {
            cat
        } else {
            "other".into()
        };

        Ok(RootCauseReport {
            call_id: transcript.call_id.clone(),
            primary_cause: out.primary_cause.trim().to_string(),
            category,
            confidence: out.confidence.clamp(0.0, 1.0),
            contributing_factors: out
                .contributing_factors
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            recommended_fix: out.recommended_fix.trim().to_string(),
            citations: cites,
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
                start: 1.0,
                end: 3.0,
                text: "I was double charged last month.".into(),
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
    fn normalises_unknown_category() {
        let out = ModelOutput {
            primary_cause: "Double billing".into(),
            category: "finance-ops".into(),
            confidence: 1.5,
            contributing_factors: vec!["  invoice error ".into(), "".into()],
            recommended_fix: "Reconcile invoices".into(),
            citations: vec![ModelCitation {
                segment_id: "seg_1".into(),
                quote: Some("double charged".into()),
            }],
            summary: "Billing issue.".into(),
        };
        let r = RootCauseAgent.validate(out, &tr()).unwrap();
        assert_eq!(r.category, "other");
        assert!((r.confidence - 1.0).abs() < 1e-6);
        assert_eq!(r.contributing_factors, vec!["invoice error".to_string()]);
        assert_eq!(r.citations.len(), 1);
    }
}
