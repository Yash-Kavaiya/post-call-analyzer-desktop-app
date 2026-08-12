//! Coaching notes for the agent who took the call.
//!
//! The bar here is higher than for the other agents, because the output goes to
//! a person about their own work. "Improve empathy" is not coaching — it names
//! a quality without saying what was done or what to do instead. A note that
//! survives validation has to carry an observation, a recommendation, and a
//! citation a supervisor can play back.
//!
//! Strengths are collected as well as improvements. That is not decoration:
//! feedback that is only criticism gets discounted by the person receiving it,
//! and the behaviours worth repeating are the ones most worth naming.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::{CoachingNote, CoachingReport, Severity};
use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
struct ModelOutput {
    #[serde(default)]
    strengths: Vec<ModelNote>,
    #[serde(default)]
    improvements: Vec<ModelNote>,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
struct ModelNote {
    #[serde(default)]
    behaviour: String,
    #[serde(default)]
    observation: String,
    #[serde(default)]
    recommendation: String,
    #[serde(default)]
    suggested_phrase: Option<String>,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

pub struct CoachingAgent;

impl CoachingAgent {
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
    ) -> Result<(CoachingReport, ChatResponse)> {
        if transcript.segments.is_empty() {
            return Err(CaError::Config("cannot coach on an empty transcript".into()));
        }

        let req = ChatRequest::new(vec![
            Message::system(Self::SYSTEM),
            Message::user(self.user_prompt(transcript)),
        ])
        .with_schema(serde_json::json!({ "type": "object" }));

        let (out, resp): (ModelOutput, _) = ca_llm::complete_json(provider, req).await?;
        Ok((self.validate(out, transcript), resp))
    }

    const SYSTEM: &'static str = "You coach contact-centre agents on their own calls.\n\
         \n\
         Rules you must follow:\n\
         - Coach the AGENT only. The customer's behaviour is context, never the subject.\n\
         - Every note must cite at least one segment id, copied exactly from the transcript.\n\
         - `observation` states what the agent did, quoting or paraphrasing that moment.\n\
         - `recommendation` states what to do instead, as an action, not a quality. \
           \"Let the customer finish describing the problem\" — not \"show more empathy\".\n\
         - `suggested_phrase` is optional wording the agent could say verbatim.\n\
         - severity is one of: info, low, medium, high, critical.\n\
         - Find at least one genuine strength if the call contains one. Do not invent one.\n\
         - Return only a JSON object. No prose, no markdown fence.";

    fn user_prompt(&self, transcript: &Transcript) -> String {
        format!(
            "Coach the agent on this call.\n\n\
             TRANSCRIPT (format: [mm:ss] (segment_id) Speaker: text):\n{}\n\n\
             Return JSON exactly of this shape:\n\
             {{\"strengths\":[{{\"behaviour\":\"<short handle>\",\
             \"observation\":\"<what the agent did>\",\
             \"recommendation\":\"<what to keep doing>\",\
             \"suggested_phrase\":null,\"severity\":\"info\",\
             \"citations\":[{{\"segment_id\":\"<id>\",\"quote\":\"<short quote>\"}}]}}],\
             \"improvements\":[{{\"behaviour\":\"<short handle>\",\
             \"observation\":\"<what the agent did>\",\
             \"recommendation\":\"<what to do instead>\",\
             \"suggested_phrase\":\"<optional wording>\",\"severity\":\"medium\",\
             \"citations\":[{{\"segment_id\":\"<id>\",\"quote\":\"<short quote>\"}}]}}],\
             \"summary\":\"<two sentences>\"}}",
            citations::transcript_block(transcript)
        )
    }

    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> CoachingReport {
        let strengths = self.keep_usable(out.strengths, transcript, Severity::Info);
        let improvements = self.keep_usable(out.improvements, transcript, Severity::Medium);

        CoachingReport {
            call_id: transcript.call_id.clone(),
            strengths,
            improvements,
            summary: out.summary.trim().to_string(),
        }
    }

    /// Drop notes that cannot be acted on.
    ///
    /// A note with no surviving citation cannot be checked, and a note with no
    /// recommendation is an observation. Both read as authoritative feedback
    /// about someone's work while giving them nothing to do, so neither is
    /// worth keeping.
    fn keep_usable(
        &self,
        raw: Vec<ModelNote>,
        transcript: &Transcript,
        default_severity: Severity,
    ) -> Vec<CoachingNote> {
        let mut out = Vec::new();
        for n in raw {
            let (cites, dropped) = citations::validate_citations(&n.citations, transcript);
            if dropped > 0 {
                tracing::warn!(count = dropped, behaviour = %n.behaviour, "coaching citations failed validation");
            }
            if cites.is_empty() {
                tracing::warn!(behaviour = %n.behaviour, "dropping uncitable coaching note");
                continue;
            }
            if n.recommendation.trim().is_empty() || n.observation.trim().is_empty() {
                tracing::warn!(behaviour = %n.behaviour, "dropping coaching note with nothing to act on");
                continue;
            }
            out.push(CoachingNote {
                behaviour: n.behaviour.trim().to_string(),
                observation: n.observation.trim().to_string(),
                recommendation: n.recommendation.trim().to_string(),
                suggested_phrase: n
                    .suggested_phrase
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty()),
                severity: parse_severity(&n.severity).unwrap_or(default_severity),
                citations: cites,
            });
        }
        out
    }
}

fn parse_severity(s: &str) -> Option<Severity> {
    match s.trim().to_ascii_lowercase().as_str() {
        "info" => Some(Severity::Info),
        "low" => Some(Severity::Low),
        "medium" => Some(Severity::Medium),
        "high" => Some(Severity::High),
        "critical" => Some(Severity::Critical),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::ids::{CallId, SegmentId};
    use ca_core::transcript::{Segment, SpeakerRole};

    fn seg(id: &str, start: f64, text: &str) -> Segment {
        Segment {
            id: SegmentId::from(id),
            speaker_index: 0,
            role: SpeakerRole::Agent,
            start,
            end: start + 4.0,
            text: text.into(),
            redacted_text: None,
            words: vec![],
            confidence: None,
            language: None,
        }
    }

    fn transcript() -> Transcript {
        Transcript {
            call_id: CallId::from("c1"),
            segments: vec![
                seg("seg_1", 0.0, "Thanks for calling, how can I help?"),
                seg("seg_2", 10.0, "Right, so what you need to do is—"),
            ],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "t".into(),
            model: "m".into(),
            duration_secs: 30.0,
        }
    }

    fn note(behaviour: &str, cite: Option<&str>, recommendation: &str) -> ModelNote {
        ModelNote {
            behaviour: behaviour.into(),
            observation: "the agent did a thing".into(),
            recommendation: recommendation.into(),
            suggested_phrase: Some("try saying this".into()),
            severity: "medium".into(),
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
    fn a_note_with_a_real_citation_survives_with_transcript_timings() {
        let out = ModelOutput {
            improvements: vec![note("interrupted", Some("seg_2"), "let them finish")],
            ..Default::default()
        };
        let r = CoachingAgent.validate(out, &transcript());
        assert_eq!(r.improvements.len(), 1);
        assert_eq!(r.improvements[0].citations[0].start, 10.0);
        assert_eq!(r.improvements[0].suggested_phrase.as_deref(), Some("try saying this"));
    }

    #[test]
    fn a_note_citing_nothing_is_dropped() {
        // Feedback about someone's work that cannot be played back is not
        // reviewable, and this output goes to that person.
        let out = ModelOutput {
            improvements: vec![note("vague", None, "be better")],
            ..Default::default()
        };
        assert!(CoachingAgent.validate(out, &transcript()).improvements.is_empty());
    }

    #[test]
    fn a_note_citing_an_invented_segment_is_dropped() {
        let out = ModelOutput {
            improvements: vec![note("invented", Some("seg_nope"), "do something")],
            ..Default::default()
        };
        assert!(CoachingAgent.validate(out, &transcript()).improvements.is_empty());
    }

    #[test]
    fn a_note_with_no_recommendation_is_dropped() {
        // Without one it is an observation dressed as coaching: it tells the
        // agent they did something wrong and nothing about what to do.
        let out = ModelOutput {
            improvements: vec![note("interrupted", Some("seg_2"), "   ")],
            ..Default::default()
        };
        assert!(CoachingAgent.validate(out, &transcript()).improvements.is_empty());
    }

    #[test]
    fn strengths_are_kept_separately_from_improvements() {
        let out = ModelOutput {
            strengths: vec![note("greeted well", Some("seg_1"), "keep doing this")],
            improvements: vec![note("interrupted", Some("seg_2"), "let them finish")],
            summary: "mixed".into(),
        };
        let r = CoachingAgent.validate(out, &transcript());
        assert_eq!(r.strengths.len(), 1);
        assert_eq!(r.improvements.len(), 1);
        assert_eq!(r.summary, "mixed");
    }

    #[test]
    fn an_unknown_severity_falls_back_rather_than_dropping_the_note() {
        let mut n = note("interrupted", Some("seg_2"), "let them finish");
        n.severity = "catastrophic".into();
        let out = ModelOutput { improvements: vec![n], ..Default::default() };
        let r = CoachingAgent.validate(out, &transcript());
        assert_eq!(r.improvements.len(), 1);
        assert_eq!(r.improvements[0].severity, Severity::Medium);
    }

    #[test]
    fn an_empty_suggested_phrase_becomes_none() {
        let mut n = note("interrupted", Some("seg_2"), "let them finish");
        n.suggested_phrase = Some("   ".into());
        let out = ModelOutput { improvements: vec![n], ..Default::default() };
        let r = CoachingAgent.validate(out, &transcript());
        assert!(r.improvements[0].suggested_phrase.is_none());
    }

    #[test]
    fn the_prompt_carries_segment_ids_and_never_raw_pii() {
        let mut t = transcript();
        t.segments[0].text = "call me on 9876543210".into();
        t.segments[0].redacted_text = Some("call me on [PHONE_1]".into());
        let p = CoachingAgent.user_prompt(&t);
        assert!(p.contains("seg_1"));
        assert!(p.contains("[PHONE_1]"));
        assert!(!p.contains("9876543210"), "raw PII reached the prompt");
    }
}
