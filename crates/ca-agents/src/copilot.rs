//! Ask questions about a call, grounded in its transcript.
//!
//! The difference between this and a chat box wired to a model is that every
//! claim has to point at a segment. An ungrounded answer about a call is worse
//! than no answer: it is fluent, specific, and there is nothing on screen to
//! tell you it was invented.
//!
//! So the answer is validated the same way an agent's findings are — citations
//! must resolve to real segment ids — and an answer that cites nothing is
//! labelled rather than shown as though it were checked. The UI turns each
//! surviving citation into a button that seeks the audio, which is what makes
//! "did the agent actually say that" a two-second question.
//!
//! Conversation history is carried so follow-ups work ("what about after
//! that?"), but the transcript is re-sent every turn rather than relying on the
//! model to remember it. A model that has forgotten half the call still answers
//! confidently about the half it kept.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::Citation;
use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message, Role};
use ca_core::transcript::Transcript;
use serde::{Deserialize, Serialize};

/// How much conversation history is replayed. Beyond this the context is mostly
/// old questions rather than the call being asked about.
const MAX_HISTORY_TURNS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotTurn {
    /// "user" or "assistant".
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotAnswer {
    pub answer: String,
    pub citations: Vec<Citation>,
    /// True when nothing the model cited resolved to the transcript.
    ///
    /// Surfaced rather than hidden: the answer may still be correct, but no
    /// part of it has been checked against the call.
    pub grounded: bool,
}

#[derive(Debug, Deserialize, Default)]
struct ModelOutput {
    #[serde(default)]
    answer: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

pub struct CallCopilot;

impl CallCopilot {
    /// Answer a question about one call.
    pub async fn ask(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
        question: &str,
        history: &[CopilotTurn],
    ) -> Result<(CopilotAnswer, ChatResponse)> {
        if transcript.segments.is_empty() {
            return Err(CaError::Config(
                "this call has no transcript to answer from".into(),
            ));
        }
        if question.trim().is_empty() {
            return Err(CaError::Config("ask a question".into()));
        }

        let mut messages = vec![Message::system(Self::SYSTEM)];

        // The transcript goes in every turn. Relying on the model to carry it
        // across a conversation means later answers are drawn from whatever it
        // happened to retain, with no way to tell from the outside.
        messages.push(Message::user(format!(
            "TRANSCRIPT (format: [mm:ss] (segment_id) Speaker: text):\n{}",
            citations::transcript_block(transcript)
        )));

        for turn in history.iter().rev().take(MAX_HISTORY_TURNS).rev() {
            messages.push(Message {
                role: if turn.role == "assistant" {
                    Role::Assistant
                } else {
                    Role::User
                },
                content: turn.content.clone(),
            });
        }

        messages.push(Message::user(format!(
            "{}\n\nReturn JSON exactly of this shape:\n\
             {{\"answer\":\"<your answer>\",\
             \"citations\":[{{\"segment_id\":\"<id>\",\"quote\":\"<short quote>\"}}]}}",
            question.trim()
        )));

        let req = ChatRequest::new(messages).with_schema(serde_json::json!({ "type": "object" }));
        let (out, resp): (ModelOutput, _) = ca_llm::complete_json(provider, req).await?;
        Ok((self.validate(out, transcript), resp))
    }

    const SYSTEM: &'static str =
        "You answer questions about a contact-centre call, using only its transcript.\n\
         \n\
         Rules you must follow:\n\
         - Answer only from the transcript. If it does not contain the answer, \
           say so plainly rather than inferring.\n\
         - Cite the segment ids your answer rests on, copied exactly.\n\
         - Placeholders like [PHONE_1] are redacted personal data. Treat them as \
           the value they stand for; never comment on the redaction itself.\n\
         - Be brief. Two or three sentences unless asked for more.\n\
         - Return only a JSON object. No prose, no markdown fence.";

    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> CopilotAnswer {
        let (cites, dropped) = citations::validate_citations(&out.citations, transcript);
        if dropped > 0 {
            tracing::warn!(count = dropped, "copilot citations failed validation");
        }

        let answer = out.answer.trim().to_string();
        let grounded = !cites.is_empty();

        CopilotAnswer {
            answer,
            citations: cites,
            grounded,
        }
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
            segments: vec![
                Segment {
                    id: SegmentId::from("seg_1"),
                    speaker_index: 0,
                    role: SpeakerRole::Agent,
                    start: 0.0,
                    end: 6.0,
                    text: "Thanks for calling, my name is Priya.".into(),
                    redacted_text: None,
                    words: vec![],
                    confidence: None,
                    language: None,
                },
                Segment {
                    id: SegmentId::from("seg_2"),
                    speaker_index: 1,
                    role: SpeakerRole::Customer,
                    start: 7.0,
                    end: 12.0,
                    text: "My card was charged twice.".into(),
                    redacted_text: None,
                    words: vec![],
                    confidence: None,
                    language: None,
                },
            ],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "t".into(),
            model: "m".into(),
            duration_secs: 30.0,
        }
    }

    fn output(answer: &str, cite: Option<&str>) -> ModelOutput {
        ModelOutput {
            answer: answer.into(),
            citations: cite
                .map(|c| vec![ModelCitation { segment_id: c.into(), quote: Some("q".into()) }])
                .unwrap_or_default(),
        }
    }

    #[test]
    fn a_cited_answer_is_grounded_and_carries_transcript_timings() {
        let a = CallCopilot.validate(output("They were charged twice.", Some("seg_2")), &transcript());
        assert!(a.grounded);
        assert_eq!(a.citations.len(), 1);
        assert_eq!(a.citations[0].start, 7.0);
        assert_eq!(a.citations[0].end, 12.0);
    }

    #[test]
    fn an_uncited_answer_is_marked_ungrounded_rather_than_hidden() {
        // The answer may still be right; the point is that nothing on screen
        // would otherwise distinguish it from one that was checked.
        let a = CallCopilot.validate(output("The customer seemed happy.", None), &transcript());
        assert!(!a.grounded);
        assert!(!a.answer.is_empty(), "the answer itself is still shown");
    }

    #[test]
    fn a_fabricated_citation_does_not_make_an_answer_look_grounded() {
        let a = CallCopilot.validate(output("They said so at the end.", Some("seg_99")), &transcript());
        assert!(a.citations.is_empty());
        assert!(!a.grounded, "an invented citation passed as evidence");
    }

    #[tokio::test]
    async fn an_empty_question_is_refused_before_any_model_call() {
        struct NeverCalled;
        #[async_trait::async_trait]
        impl LlmProvider for NeverCalled {
            fn name(&self) -> &str { "never" }
            fn model(&self) -> &str { "never" }
            fn capabilities(&self) -> ca_core::provider::Caps { Default::default() }
            async fn complete(&self, _: ChatRequest) -> Result<ChatResponse> {
                panic!("asked the model an empty question");
            }
        }
        let err = CallCopilot
            .ask(&NeverCalled, &transcript(), "   ", &[])
            .await
            .err()
            .unwrap();
        assert!(format!("{err}").contains("ask a question"));
    }

    #[tokio::test]
    async fn a_call_with_no_transcript_is_refused_with_a_usable_message() {
        struct NeverCalled;
        #[async_trait::async_trait]
        impl LlmProvider for NeverCalled {
            fn name(&self) -> &str { "never" }
            fn model(&self) -> &str { "never" }
            fn capabilities(&self) -> ca_core::provider::Caps { Default::default() }
            async fn complete(&self, _: ChatRequest) -> Result<ChatResponse> {
                panic!("asked about a call with no transcript");
            }
        }
        let mut t = transcript();
        t.segments.clear();
        let err = CallCopilot
            .ask(&NeverCalled, &t, "what happened?", &[])
            .await
            .err()
            .unwrap();
        assert!(format!("{err}").contains("no transcript"));
    }

    #[tokio::test]
    async fn the_transcript_is_resent_every_turn_and_history_is_bounded() {
        use std::sync::Mutex;

        struct Capture(Mutex<Vec<Message>>);
        #[async_trait::async_trait]
        impl LlmProvider for Capture {
            fn name(&self) -> &str { "capture" }
            fn model(&self) -> &str { "capture-1" }
            fn capabilities(&self) -> ca_core::provider::Caps { Default::default() }
            async fn complete(&self, req: ChatRequest) -> Result<ChatResponse> {
                *self.0.lock().unwrap() = req.messages.clone();
                Ok(ChatResponse {
                    content: r#"{"answer":"ok","citations":[]}"#.into(),
                    input_tokens: 1,
                    output_tokens: 1,
                    model: "capture-1".into(),
                    latency_ms: 1,
                })
            }
        }

        // More history than the cap, so the trim is exercised.
        let history: Vec<CopilotTurn> = (0..20)
            .map(|i| CopilotTurn {
                role: if i % 2 == 0 { "user".into() } else { "assistant".into() },
                content: format!("turn {i}"),
            })
            .collect();

        let p = Capture(Mutex::new(vec![]));
        CallCopilot
            .ask(&p, &transcript(), "and after that?", &history)
            .await
            .unwrap();

        let sent = p.0.lock().unwrap().clone();
        let joined: String = sent.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n");

        assert!(joined.contains("seg_1"), "the transcript was not re-sent");
        assert!(joined.contains("turn 19"), "the most recent history was dropped");
        assert!(!joined.contains("turn 0"), "history was not bounded");
    }

    #[tokio::test]
    async fn raw_pii_never_reaches_the_model() {
        use std::sync::Mutex;

        struct Capture(Mutex<String>);
        #[async_trait::async_trait]
        impl LlmProvider for Capture {
            fn name(&self) -> &str { "capture" }
            fn model(&self) -> &str { "capture-1" }
            fn capabilities(&self) -> ca_core::provider::Caps { Default::default() }
            async fn complete(&self, req: ChatRequest) -> Result<ChatResponse> {
                *self.0.lock().unwrap() =
                    req.messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n");
                Ok(ChatResponse {
                    content: r#"{"answer":"ok","citations":[]}"#.into(),
                    input_tokens: 1,
                    output_tokens: 1,
                    model: "capture-1".into(),
                    latency_ms: 1,
                })
            }
        }

        let mut t = transcript();
        t.segments[1].text = "my number is 9876543210".into();
        t.segments[1].redacted_text = Some("my number is [PHONE_1]".into());

        let p = Capture(Mutex::new(String::new()));
        CallCopilot.ask(&p, &t, "what is their number?", &[]).await.unwrap();

        let sent = p.0.lock().unwrap().clone();
        assert!(sent.contains("[PHONE_1]"));
        assert!(!sent.contains("9876543210"), "raw PII reached the copilot prompt");
    }
}
