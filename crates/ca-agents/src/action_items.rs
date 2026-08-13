//! Extract concrete follow-ups promised or owed after the call.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::{ActionItem, ActionItemsReport};
use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ModelOutput {
    #[serde(default)]
    items: Vec<ModelItem>,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
struct ModelItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    priority: String,
    #[serde(default)]
    due: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

const OWNERS: &[&str] = &["agent", "supervisor", "backoffice", "customer", "system"];
const PRIOS: &[&str] = &["low", "medium", "high"];

pub struct ActionItemsAgent;

impl ActionItemsAgent {
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
    ) -> Result<(ActionItemsReport, ChatResponse)> {
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
        "You extract action items from contact-centre calls.\n\
         \n\
         An action item is a concrete follow-up someone must do after the call \
         (callback, ticket, refund review, send docs, escalate, schedule, etc.).\n\
         \n\
         Rules:\n\
         - Only list items supported by the transcript; cite real segment_ids.\n\
         - owner must be one of: agent, supervisor, backoffice, customer, system.\n\
         - priority must be one of: low, medium, high.\n\
         - If nothing is owed, return an empty items array.\n\
         - Placeholders like [PHONE_1] are redacted PII.\n\
         - Return only JSON. No prose, no markdown fence."
    }

    fn user_prompt(&self, transcript: &Transcript) -> String {
        let mut s = String::from("List the action items from this call.\n\n");
        s.push_str(&citations::transcript_block(transcript));
        s.push_str(
            "\n\nReturn JSON exactly of this shape:\n\
             {\"items\":[{\"title\":\"...\",\"detail\":\"...\",\"owner\":\"agent|supervisor|backoffice|customer|system\",\
             \"priority\":\"low|medium|high\",\"due\":\"\",\"citations\":[{\"segment_id\":\"<id>\",\"quote\":\"...\"}]}],\
             \"summary\":\"<one or two sentences>\"}",
        );
        s
    }

    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> Result<ActionItemsReport> {
        let mut items = Vec::new();
        for it in out.items {
            let (cites, dropped) = citations::validate_citations(&it.citations, transcript);
            if dropped > 0 {
                tracing::warn!(count = dropped, "action-item citations dropped");
            }
            if cites.is_empty() {
                continue;
            }
            let title = it.title.trim().to_string();
            if title.is_empty() {
                continue;
            }
            let owner = normalize_choice(&it.owner, OWNERS, "agent");
            let priority = normalize_choice(&it.priority, PRIOS, "medium");
            items.push(ActionItem {
                title,
                detail: it.detail.trim().to_string(),
                owner,
                priority,
                due: it.due.trim().to_string(),
                citations: cites,
            });
        }

        // Stable order: high first, then medium, then low.
        let rank = |p: &str| match p {
            "high" => 0,
            "medium" => 1,
            _ => 2,
        };
        items.sort_by_key(|i| rank(&i.priority));

        Ok(ActionItemsReport {
            call_id: transcript.call_id.clone(),
            items,
            summary: out.summary.trim().to_string(),
        })
    }
}

fn normalize_choice(raw: &str, allowed: &[&str], default: &str) -> String {
    let v = raw.trim().to_ascii_lowercase();
    if allowed.contains(&v.as_str()) {
        v
    } else {
        default.into()
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
                speaker_index: 0,
                role: SpeakerRole::Agent,
                start: 10.0,
                end: 14.0,
                text: "I will email you the invoice today.".into(),
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
            duration_secs: 20.0,
        }
    }

    #[test]
    fn keeps_evidenced_items_and_normalises_fields() {
        let out = ModelOutput {
            items: vec![ModelItem {
                title: "Send invoice".into(),
                detail: "Email the invoice copy".into(),
                owner: "Agent-Side".into(),
                priority: "URGENT".into(),
                due: "today".into(),
                citations: vec![ModelCitation {
                    segment_id: "seg_1".into(),
                    quote: Some("email you the invoice".into()),
                }],
            }],
            summary: "One follow-up.".into(),
        };
        let r = ActionItemsAgent.validate(out, &tr()).unwrap();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].owner, "agent");
        assert_eq!(r.items[0].priority, "medium"); // URGENT not in list
        assert_eq!(r.items[0].citations.len(), 1);
    }

    #[test]
    fn drops_unevidenced_items() {
        let out = ModelOutput {
            items: vec![ModelItem {
                title: "Ghost task".into(),
                detail: String::new(),
                owner: "agent".into(),
                priority: "high".into(),
                due: String::new(),
                citations: vec![ModelCitation {
                    segment_id: "nope".into(),
                    quote: None,
                }],
            }],
            summary: String::new(),
        };
        let r = ActionItemsAgent.validate(out, &tr()).unwrap();
        assert!(r.items.is_empty());
    }
}
