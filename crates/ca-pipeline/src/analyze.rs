//! The analysis stage: transcript in, persisted [`Analysis`] out.
//!
//! This is where the pipeline first talks to a model, so it is also where the
//! redact-before-cloud promise is *enforced* rather than documented. A remote
//! provider is never handed a transcript that has not been through redaction,
//! and the check is a hard error rather than a warning — a batch that quietly
//! leaked 1,968 calls' worth of phone numbers cannot be un-leaked.

use ca_agents::{
    ActionItemsAgent, ComplianceAgent, QaScorecardAgent, RootCauseAgent, SentimentAgent,
};
use ca_core::analysis::{AgentKind, Analysis, Finding, Usage};
use ca_core::error::{CaError, Result};
use ca_core::ids::AnalysisId;
use ca_core::provider::{ChatResponse, LlmProvider};
use ca_core::transcript::Transcript;
use std::sync::Arc;

/// Everything one multi-agent pass produces for a single call.
#[derive(Debug, Default)]
pub struct BatteryResult {
    pub analyses: Vec<Analysis>,
    pub findings: Vec<Finding>,
}

/// The LLM half of the pipeline: a provider plus the agents that use it.
///
/// Held separately from [`crate::Pipeline`] so that a transcription-only run
/// needs no credentials at all — the common case when filling a corpus
/// overnight before deciding which model to spend money on.
pub struct Analyzer {
    provider: Arc<dyn LlmProvider>,
    qa: QaScorecardAgent,
    sentiment: SentimentAgent,
    compliance: ComplianceAgent,
    root_cause: RootCauseAgent,
    action_items: ActionItemsAgent,
    coaching: ca_agents::CoachingAgent,
    customer_experience: ca_agents::CustomerExperienceAgent,
    executive_summary: ca_agents::ExecutiveSummaryAgent,
    /// True when the model runs on this machine (Ollama, LM Studio). Local
    /// providers are exempt from the redaction requirement because nothing
    /// leaves the box; everything else is not.
    local: bool,
}

impl Analyzer {
    pub fn new(provider: Arc<dyn LlmProvider>, qa: QaScorecardAgent, local: bool) -> Self {
        Self {
            provider,
            qa,
            sentiment: SentimentAgent,
            compliance: ComplianceAgent,
            root_cause: RootCauseAgent,
            action_items: ActionItemsAgent,
            coaching: ca_agents::CoachingAgent,
            customer_experience: ca_agents::CustomerExperienceAgent,
            executive_summary: ca_agents::ExecutiveSummaryAgent,
            local,
        }
    }

    pub fn is_local(&self) -> bool {
        self.local
    }

    pub fn provider_name(&self) -> String {
        self.provider.name().to_string()
    }

    fn guard_redaction(&self, redacted: bool) -> Result<()> {
        if !self.local && !redacted {
            return Err(CaError::Config(format!(
                "refusing to send an unredacted transcript to remote provider `{}`",
                self.provider.name()
            )));
        }
        Ok(())
    }

    /// Score one call (QA scorecard only) — kept for callers that only need QA.
    pub async fn score(&self, transcript: &Transcript, redacted: bool) -> Result<Analysis> {
        self.guard_redaction(redacted)?;
        let (card, resp) = self.qa.run(self.provider.as_ref(), transcript).await?;
        let payload = serde_json::to_value(&card).map_err(|e| CaError::Other(Box::new(e)))?;
        Ok(self.envelope(transcript, AgentKind::QaScorecard, payload, &resp))
    }

    /// Run the specialised agent battery: QA, sentiment, compliance, root cause,
    /// action items.
    ///
    /// Individual agent failures are captured as error envelopes when possible
    /// so one flaky agent does not discard the rest of a paid batch.
    pub async fn run_battery(
        &self,
        transcript: &Transcript,
        redacted: bool,
    ) -> Result<BatteryResult> {
        self.guard_redaction(redacted)?;
        let mut out = BatteryResult::default();
        let provider = self.provider.as_ref();

        // QA
        match self.qa.run(provider, transcript).await {
            Ok((card, resp)) => {
                let payload =
                    serde_json::to_value(&card).map_err(|e| CaError::Other(Box::new(e)))?;
                out.analyses
                    .push(self.envelope(transcript, AgentKind::QaScorecard, payload, &resp));
            }
            Err(e) => {
                tracing::error!(error = %e, "qa agent failed");
                out.analyses
                    .push(self.error_envelope(transcript, AgentKind::QaScorecard, e.to_string()));
            }
        }

        // Sentiment
        match self.sentiment.run(provider, transcript).await {
            Ok((report, resp)) => {
                let payload =
                    serde_json::to_value(&report).map_err(|e| CaError::Other(Box::new(e)))?;
                out.analyses
                    .push(self.envelope(transcript, AgentKind::Sentiment, payload, &resp));
            }
            Err(e) => {
                tracing::error!(error = %e, "sentiment agent failed");
                out.analyses
                    .push(self.error_envelope(transcript, AgentKind::Sentiment, e.to_string()));
            }
        }

        // Compliance
        match self.compliance.run(provider, transcript).await {
            Ok((report, resp)) => {
                out.findings.extend(report.findings.clone());
                let payload =
                    serde_json::to_value(&report).map_err(|e| CaError::Other(Box::new(e)))?;
                out.analyses
                    .push(self.envelope(transcript, AgentKind::Compliance, payload, &resp));
            }
            Err(e) => {
                tracing::error!(error = %e, "compliance agent failed");
                out.analyses
                    .push(self.error_envelope(transcript, AgentKind::Compliance, e.to_string()));
            }
        }

        // Root cause
        match self.root_cause.run(provider, transcript).await {
            Ok((report, resp)) => {
                let payload =
                    serde_json::to_value(&report).map_err(|e| CaError::Other(Box::new(e)))?;
                out.analyses
                    .push(self.envelope(transcript, AgentKind::RootCause, payload, &resp));
            }
            Err(e) => {
                tracing::error!(error = %e, "root-cause agent failed");
                out.analyses
                    .push(self.error_envelope(transcript, AgentKind::RootCause, e.to_string()));
            }
        }

        // Action items
        match self.action_items.run(provider, transcript).await {
            Ok((report, resp)) => {
                let payload =
                    serde_json::to_value(&report).map_err(|e| CaError::Other(Box::new(e)))?;
                out.analyses
                    .push(self.envelope(transcript, AgentKind::ActionItems, payload, &resp));
            }
            Err(e) => {
                tracing::error!(error = %e, "action-items agent failed");
                out.analyses
                    .push(self.error_envelope(transcript, AgentKind::ActionItems, e.to_string()));
            }
        }

        // Coaching
        match self.coaching.run(provider, transcript).await {
            Ok((report, resp)) => {
                let payload =
                    serde_json::to_value(&report).map_err(|e| CaError::Other(Box::new(e)))?;
                out.analyses
                    .push(self.envelope(transcript, AgentKind::Coaching, payload, &resp));
            }
            Err(e) => {
                tracing::error!(error = %e, "coaching agent failed");
                out.analyses
                    .push(self.error_envelope(transcript, AgentKind::Coaching, e.to_string()));
            }
        }

        // Customer experience
        match self.customer_experience.run(provider, transcript).await {
            Ok((report, resp)) => {
                let payload =
                    serde_json::to_value(&report).map_err(|e| CaError::Other(Box::new(e)))?;
                out.analyses.push(self.envelope(
                    transcript,
                    AgentKind::CustomerExperience,
                    payload,
                    &resp,
                ));
            }
            Err(e) => {
                tracing::error!(error = %e, "customer-experience agent failed");
                out.analyses.push(self.error_envelope(
                    transcript,
                    AgentKind::CustomerExperience,
                    e.to_string(),
                ));
            }
        }

        // Executive summary last: it is the one a supervisor reads on its own,
        // so it benefits from being the freshest pass over the transcript.
        match self.executive_summary.run(provider, transcript).await {
            Ok((report, resp)) => {
                let payload =
                    serde_json::to_value(&report).map_err(|e| CaError::Other(Box::new(e)))?;
                out.analyses.push(self.envelope(
                    transcript,
                    AgentKind::ExecutiveSummary,
                    payload,
                    &resp,
                ));
            }
            Err(e) => {
                tracing::error!(error = %e, "executive-summary agent failed");
                out.analyses.push(self.error_envelope(
                    transcript,
                    AgentKind::ExecutiveSummary,
                    e.to_string(),
                ));
            }
        }

        // Hard-fail only if every agent failed with no usable payload.
        let any_ok = out.analyses.iter().any(|a| a.error.is_none());
        if !any_ok {
            return Err(CaError::Config(
                "all analysis agents failed; see per-agent error envelopes".into(),
            ));
        }
        Ok(out)
    }

    /// Wrap an agent result with the accounting the dashboard reports on.
    fn envelope(
        &self,
        transcript: &Transcript,
        agent: AgentKind,
        payload: serde_json::Value,
        resp: &ChatResponse,
    ) -> Analysis {
        Analysis {
            id: AnalysisId::new(),
            call_id: transcript.call_id.clone(),
            agent,
            payload,
            provider: self.provider.name().to_string(),
            // The model the provider actually answered with, which is not
            // always the one that was asked for: routers substitute.
            model: resp.model.clone(),
            usage: Usage {
                input_tokens: resp.input_tokens,
                output_tokens: resp.output_tokens,
                cost_usd: self.cost_of(resp),
                latency_ms: resp.latency_ms,
            },
            created_at: chrono::Utc::now(),
            error: None,
        }
    }

    fn error_envelope(
        &self,
        transcript: &Transcript,
        agent: AgentKind,
        error: String,
    ) -> Analysis {
        Analysis {
            id: AnalysisId::new(),
            call_id: transcript.call_id.clone(),
            agent,
            payload: serde_json::json!({ "error": error }),
            provider: self.provider.name().to_string(),
            model: self.provider.model().to_string(),
            usage: Usage::default(),
            created_at: chrono::Utc::now(),
            error: Some(error),
        }
    }

    /// Priced from the provider's declared rates. `None` when the provider
    /// does not publish them - an absent cost is honest, a zero would
    /// understate the bill and be summed into the dashboard as if it were free.
    fn cost_of(&self, resp: &ChatResponse) -> Option<f64> {
        self.provider
            .capabilities()
            .cost(resp.input_tokens, resp.output_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_agents::Rubric;
    use ca_core::provider::{Caps, ChatRequest};
    use ca_core::transcript::{Segment, SpeakerRole};
    use ca_core::ids::{CallId, SegmentId};

    /// A provider that fails the test if it is ever called. The redaction guard
    /// must reject before any network work happens, not after.
    struct NeverCalled;

    #[async_trait::async_trait]
    impl LlmProvider for NeverCalled {
        async fn complete(&self, _req: ChatRequest) -> Result<ChatResponse> {
            panic!("provider was called despite an unredacted transcript");
        }
        fn capabilities(&self) -> Caps {
            Caps::default()
        }
        fn name(&self) -> &str {
            "never-called"
        }
        fn model(&self) -> &str {
            "none"
        }
    }

    fn transcript() -> Transcript {
        Transcript {
            call_id: CallId::from("call_1"),
            segments: vec![Segment {
                id: SegmentId::from("seg_1"),
                speaker_index: 0,
                role: SpeakerRole::Agent,
                start: 0.0,
                end: 2.0,
                text: "call me on 9876543210".into(),
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
            duration_secs: 2.0,
        }
    }

    fn analyzer(local: bool) -> Analyzer {
        Analyzer::new(
            Arc::new(NeverCalled),
            QaScorecardAgent::new(Rubric::default()),
            local,
        )
    }

    #[tokio::test]
    async fn remote_providers_reject_unredacted_transcripts() {
        let err = analyzer(false).score(&transcript(), false).await.err().unwrap();
        assert!(matches!(err, CaError::Config(_)), "got {err:?}");
        assert!(format!("{err}").contains("unredacted"));
    }

    #[tokio::test]
    async fn battery_also_refuses_unredacted_remote() {
        let err = analyzer(false)
            .run_battery(&transcript(), false)
            .await
            .err()
            .unwrap();
        assert!(format!("{err}").contains("unredacted"));
    }

    #[test]
    fn cost_is_none_when_the_provider_does_not_publish_rates() {
        // Summing an assumed zero into the spend dashboard would report a
        // 1,968-call cloud batch as free.
        let resp = ChatResponse {
            content: String::new(),
            input_tokens: 1000,
            output_tokens: 1000,
            model: "m".into(),
            latency_ms: 0,
        };
        assert!(analyzer(true).cost_of(&resp).is_none());
    }
}
