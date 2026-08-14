//! Compliance / policy-script checks as citable findings.

use crate::citations::{self, ModelCitation};
use ca_core::analysis::{ComplianceReport, Finding, Severity};
use ca_core::error::{CaError, Result};
use ca_core::ids::FindingId;
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use ca_core::AgentKind;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ModelOutput {
    /// Model's own pass/fail — ignored; we recompute from surviving findings.
    #[serde(default)]
    #[allow(dead_code)]
    passed: bool,
    #[serde(default)]
    findings: Vec<ModelFinding>,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
struct ModelFinding {
    #[serde(default)]
    code: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    citations: Vec<ModelCitation>,
}

pub struct ComplianceAgent;

impl ComplianceAgent {
    pub async fn run(
        &self,
        provider: &dyn LlmProvider,
        transcript: &Transcript,
    ) -> Result<(ComplianceReport, ChatResponse)> {
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
        "You are a contact-centre compliance reviewer.\n\
         \n\
         Check the AGENT against common regulated-contact rules:\n\
         - identity / company disclosure at open\n\
         - permission before recording or collecting sensitive data (when relevant)\n\
         - no guaranteed outcomes the agent cannot control\n\
         - no collection of full payment card / password / OTP in clear speech\n\
         - mini-Miranda / disclosure scripts when debt or collections language appears\n\
         - professional language (no harassment, discrimination, threats)\n\
         \n\
         Rules:\n\
         - Only report issues you can cite with a real segment_id.\n\
         - If nothing is wrong, return passed=true and an empty findings array.\n\
         - severity is one of: info, low, medium, high, critical.\n\
         - Placeholders like [PHONE_1] are redacted PII.\n\
         - Return only JSON. No prose, no markdown fence."
    }

    fn user_prompt(&self, transcript: &Transcript) -> String {
        let mut s = String::from("Review this call for compliance issues.\n\n");
        s.push_str(&citations::transcript_block(transcript));
        s.push_str(
            "\n\nReturn JSON exactly of this shape:\n\
             {\"passed\":<bool>,\"findings\":[{\"code\":\"<SHORT_CODE>\",\"title\":\"<short>\",\
             \"detail\":\"<one or two sentences>\",\"severity\":\"medium\",\
             \"citations\":[{\"segment_id\":\"<id>\",\"quote\":\"<short>\"}]}],\
             \"summary\":\"<two sentences>\"}",
        );
        s
    }

    fn validate(&self, out: ModelOutput, transcript: &Transcript) -> Result<ComplianceReport> {
        let mut findings = Vec::new();
        for f in out.findings {
            let (cites, dropped) = citations::validate_citations(&f.citations, transcript);
            if dropped > 0 {
                tracing::warn!(count = dropped, code = %f.code, "compliance citations dropped");
            }
            if cites.is_empty() {
                // Unevidenced compliance hits must not enter the ledger.
                continue;
            }
            // An unrecognised severity is treated as medium rather than
            // dropped: the finding is evidenced, only its label is unusable.
            let severity = Severity::parse(&f.severity).unwrap_or(Severity::Medium);
            findings.push(Finding {
                id: FindingId::new(),
                plugin: None,
                call_id: transcript.call_id.clone(),
                agent: AgentKind::Compliance,
                code: f.code.trim().to_ascii_uppercase(),
                title: f.title.trim().to_string(),
                detail: f.detail.trim().to_string(),
                severity,
                citations: cites,
            });
        }

        // passed is recomputed from surviving findings: high/critical always
        // fail, per the one definition in `Severity::is_blocking`.
        let passed = !findings.iter().any(|f| f.severity.is_blocking());

        Ok(ComplianceReport {
            call_id: transcript.call_id.clone(),
            passed,
            findings,
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
                speaker_index: 0,
                role: SpeakerRole::Agent,
                start: 0.0,
                end: 2.0,
                text: "Give me your full card number now.".into(),
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
            duration_secs: 5.0,
        }
    }

    #[test]
    fn drops_findings_without_real_citations() {
        let out = ModelOutput {
            passed: false,
            findings: vec![ModelFinding {
                code: "PCI".into(),
                title: "Card data".into(),
                detail: "asked for PAN".into(),
                severity: "critical".into(),
                citations: vec![ModelCitation {
                    segment_id: "missing".into(),
                    quote: None,
                }],
            }],
            summary: "bad".into(),
        };
        let r = ComplianceAgent.validate(out, &tr()).unwrap();
        assert!(r.findings.is_empty());
        assert!(r.passed); // no surviving high/critical findings
    }

    #[test]
    fn keeps_evidenced_critical_and_fails() {
        let out = ModelOutput {
            passed: true, // model lies; local logic overrides
            findings: vec![ModelFinding {
                code: "PCI_PAN".into(),
                title: "Card PAN requested".into(),
                detail: "Agent asked for full card number.".into(),
                severity: "critical".into(),
                citations: vec![ModelCitation {
                    segment_id: "seg_1".into(),
                    quote: Some("card number".into()),
                }],
            }],
            summary: "PCI risk.".into(),
        };
        let r = ComplianceAgent.validate(out, &tr()).unwrap();
        assert_eq!(r.findings.len(), 1);
        assert!(!r.passed);
        assert_eq!(r.findings[0].severity, Severity::Critical);
    }
}
