//! The live monitor, wired to a real provider.
//!
//! This is where the redact-before-cloud promise is kept on the live path, and
//! it is the easiest place in the product to break it. Batch analysis redacts
//! once per call and it is obvious; live analysis redacts a small window every
//! twenty-five seconds, which is easy to treat as ephemeral. It is not — it is
//! the same customer's personal data, sent more often.
//!
//! So the guard is the same hard error the batch analyzer uses, not a warning,
//! and it fires before the request is built rather than after.

use crate::session::{guard_redaction, LiveAdvisor};
use ca_agents::LiveMonitorAgent;
use ca_core::analysis::{Finding, LiveMonitorReport};
use ca_core::error::Result;
use ca_core::provider::LlmProvider;
use ca_core::transcript::Transcript;
use ca_privacy::Redactor;
use std::sync::Arc;

pub struct ProviderAdvisor {
    provider: Arc<dyn LlmProvider>,
    agent: LiveMonitorAgent,
    /// True when the model runs on this machine. Local providers are exempt
    /// from redaction because nothing leaves the box; everything else is not.
    local: bool,
}

impl ProviderAdvisor {
    pub fn new(provider: Arc<dyn LlmProvider>, local: bool) -> Self {
        Self {
            provider,
            agent: LiveMonitorAgent,
            local,
        }
    }

    /// Replace personal data in every segment of the window.
    ///
    /// A fresh [`Redactor`] per window on purpose. Carrying one across the call
    /// would keep a growing map of real values in memory for the whole session;
    /// the cost of re-detecting a ninety-second window is microseconds, and the
    /// tokens only have to be stable within one request for the model to reason
    /// about them.
    fn redacted(window: &Transcript) -> Transcript {
        let mut redactor = Redactor::new(ca_privacy::EntityKind::all().to_vec());
        let mut out = window.clone();
        for seg in &mut out.segments {
            let result = redactor.redact(&seg.text);
            seg.redacted_text = Some(result.text.clone());
            seg.text = result.text;
        }
        out
    }
}

#[async_trait::async_trait]
impl LiveAdvisor for ProviderAdvisor {
    async fn advise(
        &self,
        window: &Transcript,
        previous_headline: Option<&str>,
        rule_hits: &[Finding],
    ) -> Result<LiveMonitorReport> {
        // Redact first, then assert it happened. Building the request before
        // checking would put the raw window one mistake away from the wire.
        let window = if self.local {
            window.clone()
        } else {
            Self::redacted(window)
        };
        guard_redaction(
            self.provider.name(),
            self.local,
            window.segments.iter().all(|s| s.redacted_text.is_some()),
        )?;

        let (report, _resp) = self
            .agent
            .run(
                self.provider.as_ref(),
                &window,
                previous_headline,
                rule_hits,
            )
            .await?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::ids::{CallId, SegmentId};
    use ca_core::provider::{Caps, ChatRequest, ChatResponse};
    use ca_core::transcript::{Segment, SpeakerRole};
    use std::sync::Mutex;

    fn window(text: &str) -> Transcript {
        Transcript {
            call_id: CallId::from("call_1"),
            segments: vec![Segment {
                id: SegmentId::from("seg_1"),
                speaker_index: 1,
                role: SpeakerRole::Customer,
                start: 0.0,
                end: 4.0,
                text: text.into(),
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
            duration_secs: 4.0,
        }
    }

    /// Captures whatever prompt it is handed, so a test can assert on what
    /// would actually have crossed the wire.
    struct Spy {
        seen: Mutex<Vec<String>>,
        local: bool,
    }

    #[async_trait::async_trait]
    impl LlmProvider for Spy {
        async fn complete(&self, req: ChatRequest) -> Result<ChatResponse> {
            let joined: String = req
                .messages
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            self.seen.lock().unwrap().push(joined);
            Ok(ChatResponse {
                content: r#"{"sentiment":-0.2,"risk":"low","headline":"ok","say_next":"","citations":[]}"#
                    .into(),
                input_tokens: 10,
                output_tokens: 5,
                model: "spy-1".into(),
                latency_ms: 1,
            })
        }
        fn capabilities(&self) -> Caps {
            Caps::default()
        }
        fn name(&self) -> &str {
            if self.local {
                "ollama"
            } else {
                "nvidia-nim"
            }
        }
        fn model(&self) -> &str {
            "spy-1"
        }
    }

    #[tokio::test]
    async fn a_remote_provider_never_sees_the_raw_window() {
        // The number below is a real-looking phone number. If it reaches the
        // prompt, live assist has quietly broken the promise the whole product
        // makes about cloud calls.
        let spy = Arc::new(Spy {
            seen: Mutex::new(Vec::new()),
            local: false,
        });
        let advisor = ProviderAdvisor::new(spy.clone(), false);
        advisor
            .advise(&window("call me back on 9876543210"), None, &[])
            .await
            .unwrap();

        let sent = spy.seen.lock().unwrap().join("\n");
        assert!(!sent.contains("9876543210"), "raw PII reached the provider");
        assert!(sent.contains("[PHONE_1]"), "nothing was tokenised: {sent}");
    }

    #[tokio::test]
    async fn a_local_provider_is_exempt_because_nothing_leaves_the_machine() {
        let spy = Arc::new(Spy {
            seen: Mutex::new(Vec::new()),
            local: true,
        });
        let advisor = ProviderAdvisor::new(spy.clone(), true);
        advisor
            .advise(&window("call me back on 9876543210"), None, &[])
            .await
            .unwrap();
        assert!(spy.seen.lock().unwrap().join("\n").contains("9876543210"));
    }

    #[tokio::test]
    async fn the_report_comes_back_normalised() {
        let spy = Arc::new(Spy {
            seen: Mutex::new(Vec::new()),
            local: true,
        });
        let report = ProviderAdvisor::new(spy, true)
            .advise(&window("hello"), None, &[])
            .await
            .unwrap();
        assert_eq!(report.headline, "ok");
        assert!(report.say_next.is_none(), "an unevidenced line survived");
    }
}
