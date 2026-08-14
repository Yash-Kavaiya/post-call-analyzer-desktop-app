//! The judgement tier of live assist.
//!
//! One agent instead of eight. A ten-minute call analysed every twenty-five
//! seconds cannot afford the whole battery — that would be some 200 provider
//! calls per call — and a person on a live call does not want eight panels
//! anyway. They want to know whether this is going badly and what to say next.
//!
//! Everything the battery does properly still happens, just at hang-up, on the
//! complete transcript. This is explicitly the fast, approximate read.
//!
//! Two things are refused rather than guessed:
//!
//! - **`say_next` without evidence.** The suggestion is read aloud to a
//!   customer. An invented one is worse than none, so a suggestion whose
//!   citations do not resolve is dropped and the field left empty.
//! - **An unparseable risk.** Falls back to medium rather than to the extremes:
//!   inventing `critical` cries wolf, inventing `info` hides a real problem.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::{Finding, LiveMonitorReport, Severity};
use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
struct ModelOutput {
    #[serde(default)]
    sentiment: f32,
    #[serde(default)]
    risk: String,
    #[serde(default)]
    headline: String,
    #[serde(default)]
    say_next: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

pub struct LiveMonitorAgent;

impl LiveMonitorAgent {
    /// Read the last stretch of a call in progress.
    ///
    /// `previous_headline` carries the last pass's conclusion forward, which is
    /// how the model keeps continuity without being re-sent the whole call
    /// every twenty-five seconds.
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        window: &Transcript,
        previous_headline: Option<&str>,
        rule_hits: &[Finding],
    ) -> Result<(LiveMonitorReport, ChatResponse)> {
        if window.segments.is_empty() {
            return Err(CaError::Config(
                "cannot monitor a call with no transcript yet".into(),
            ));
        }

        let req = ChatRequest::new(vec![
            Message::system(self.system_prompt()),
            Message::user(self.user_prompt(window, previous_headline, rule_hits)),
        ])
        .with_schema(serde_json::json!({ "type": "object" }));

        let (out, resp): (ModelOutput, _) = ca_llm::complete_json(provider, req).await?;
        Ok((self.validate(out, window)?, resp))
    }

    fn system_prompt(&self) -> &str {
        "You advise a contact-centre agent DURING a live call.\n\
         \n\
         You are read in real time by someone who is still talking, so be brief \
         and concrete.\n\
         \n\
         Rules:\n\
         - sentiment is the CUSTOMER's state right now, -1.0 (very negative) to 1.0 (very positive).\n\
         - risk is one of: info, low, medium, high, critical.\n\
         - headline is ONE short sentence on where the call stands.\n\
         - say_next is a single sentence the agent could say verbatim. Leave it \
           empty unless the transcript supports it — it will be read aloud to a customer.\n\
         - Cite real segment_ids for anything you claim.\n\
         - Deterministic checks have already run; do not repeat them, react to them.\n\
         - Placeholders like [PHONE_1] are redacted PII.\n\
         - Return only a JSON object. No prose, no markdown fence."
    }

    fn user_prompt(
        &self,
        window: &Transcript,
        previous_headline: Option<&str>,
        rule_hits: &[Finding],
    ) -> String {
        let mut s = String::from("This call is still in progress.\n\n");
        if let Some(prev) = previous_headline {
            if !prev.trim().is_empty() {
                s.push_str(&format!("Where you left it: {prev}\n\n"));
            }
        }
        if !rule_hits.is_empty() {
            s.push_str("Automatic checks have already flagged:\n");
            for f in rule_hits.iter().take(8) {
                s.push_str(&format!("- [{}] {}\n", f.severity.as_str(), f.title));
            }
            s.push('\n');
        }
        s.push_str("Most recent conversation:\n\n");
        s.push_str(&citations::transcript_block(window));
        s.push_str(
            "\n\nReturn JSON exactly of this shape:\n\
             {\"sentiment\":<-1..1>,\"risk\":\"info|low|medium|high|critical\",\
             \"headline\":\"<one sentence>\",\"say_next\":\"<one sentence or empty>\",\
             \"citations\":[{\"segment_id\":\"<id>\",\"quote\":\"<short>\"}]}",
        );
        s
    }

    fn validate(&self, out: ModelOutput, window: &Transcript) -> Result<LiveMonitorReport> {
        let (cites, dropped) = citations::validate_citations(&out.citations, window);
        if dropped > 0 {
            tracing::warn!(count = dropped, "live monitor citations dropped");
        }

        let say_next = out.say_next.trim().to_string();
        // A suggestion nothing supports is dropped rather than shown. It is
        // read aloud to a customer, so an invented one is worse than silence.
        let say_next = if say_next.is_empty() || cites.is_empty() {
            None
        } else {
            Some(say_next)
        };

        Ok(LiveMonitorReport {
            call_id: window.call_id.clone(),
            sentiment: out.sentiment.clamp(-1.0, 1.0),
            risk: Severity::parse(&out.risk).unwrap_or(Severity::Medium),
            headline: out.headline.trim().to_string(),
            say_next,
            citations: cites,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::ids::SegmentId;
    use ca_core::transcript::{Segment, SpeakerRole};
    use ca_core::CallId;

    fn window() -> Transcript {
        Transcript {
            call_id: CallId::from("call_1"),
            segments: vec![Segment {
                id: SegmentId::from("seg_1"),
                speaker_index: 1,
                role: SpeakerRole::Customer,
                start: 4.0,
                end: 7.0,
                text: "I have been charged twice and nobody will help me".into(),
                redacted_text: None,
                words: vec![],
                confidence: None,
                language: None,
            }],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "live".into(),
            model: "live".into(),
            duration_secs: 10.0,
        }
    }

    fn output(say_next: &str, segment: &str) -> ModelOutput {
        ModelOutput {
            sentiment: -0.7,
            risk: "high".into(),
            headline: "Customer is escalating over a duplicate charge".into(),
            say_next: say_next.into(),
            citations: vec![ModelCitation {
                segment_id: segment.into(),
                quote: Some("charged twice".into()),
            }],
        }
    }

    #[test]
    fn an_evidenced_suggestion_survives() {
        let r = LiveMonitorAgent
            .validate(
                output("I can see the duplicate — I'll reverse it now.", "seg_1"),
                &window(),
            )
            .unwrap();
        assert_eq!(r.risk, Severity::High);
        assert!((r.sentiment - -0.7).abs() < 1e-6);
        assert!(r.say_next.is_some());
        assert_eq!(r.citations.len(), 1);
    }

    #[test]
    fn an_unevidenced_suggestion_is_dropped_rather_than_shown() {
        // This line is read aloud to a customer. Fluent and invented is worse
        // than nothing at all.
        let r = LiveMonitorAgent
            .validate(
                output("Tell them their refund arrived yesterday.", "nope"),
                &window(),
            )
            .unwrap();
        assert!(r.say_next.is_none(), "an ungrounded line reached the agent");
        assert!(r.citations.is_empty());
    }

    #[test]
    fn an_empty_suggestion_stays_empty_rather_than_becoming_a_blank_line() {
        let r = LiveMonitorAgent
            .validate(output("   ", "seg_1"), &window())
            .unwrap();
        assert!(r.say_next.is_none());
    }

    #[test]
    fn an_unparseable_risk_falls_back_to_medium() {
        // Not to critical, which would cry wolf, and not to info, which would
        // hide a real problem.
        let mut out = output("do the thing", "seg_1");
        out.risk = "catastrophic".into();
        let r = LiveMonitorAgent.validate(out, &window()).unwrap();
        assert_eq!(r.risk, Severity::Medium);
    }

    #[test]
    fn sentiment_is_clamped_to_the_scale_it_claims_to_use() {
        let mut out = output("do the thing", "seg_1");
        out.sentiment = -4.2;
        let r = LiveMonitorAgent.validate(out, &window()).unwrap();
        assert!((r.sentiment - -1.0).abs() < 1e-6);
    }

    #[test]
    fn the_prompt_carries_the_previous_headline_and_the_rule_hits() {
        use ca_core::analysis::Finding;
        use ca_core::ids::FindingId;
        use ca_core::AgentKind;

        let hits = vec![Finding {
            id: FindingId::new(),
            plugin: None,
            call_id: CallId::from("call_1"),
            agent: AgentKind::Rules,
            code: "PCI_PAN_SPOKEN".into(),
            title: "Sensitive number spoken aloud".into(),
            detail: "d".into(),
            severity: Severity::Critical,
            citations: vec![],
        }];
        let prompt = LiveMonitorAgent.user_prompt(&window(), Some("Was calm a minute ago"), &hits);
        assert!(prompt.contains("Was calm a minute ago"), "no continuity");
        assert!(
            prompt.contains("Sensitive number spoken aloud"),
            "no rule hits"
        );
        assert!(prompt.contains("seg_1"), "no transcript");
    }

    #[tokio::test]
    async fn an_empty_window_is_refused_before_a_provider_is_called() {
        struct NeverCalled;

        #[async_trait::async_trait]
        impl LlmProvider for NeverCalled {
            async fn complete(
                &self,
                _req: ca_core::provider::ChatRequest,
            ) -> Result<ca_core::provider::ChatResponse> {
                panic!("a provider was called with nothing to look at");
            }
            fn capabilities(&self) -> ca_core::provider::Caps {
                ca_core::provider::Caps::default()
            }
            fn name(&self) -> &str {
                "never"
            }
            fn model(&self) -> &str {
                "none"
            }
        }

        let mut empty = window();
        empty.segments.clear();
        assert!(LiveMonitorAgent
            .run(&NeverCalled, &empty, None, &[])
            .await
            .is_err());
    }
}
