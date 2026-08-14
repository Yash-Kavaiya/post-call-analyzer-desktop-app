//! Running a declared agent against a transcript.
//!
//! Everything a built-in agent gets, a plugin gets: the redacted transcript,
//! one JSON-mode model call, and citation validation against real segment ids.
//! Nothing here is a shortcut around the guards the battery already has.

use ca_core::analysis::{Finding, Severity};
use ca_core::error::{CaError, Result};
use ca_core::ids::FindingId;
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider, Message};
use ca_core::transcript::Transcript;
use ca_core::AgentKind;
use serde::Deserialize;

use crate::spec::AgentSpec;

/// What a plugin produced.
#[derive(Debug, Clone)]
pub struct PluginResult {
    /// The validated payload, stored as the analysis for this agent.
    pub payload: serde_json::Value,
    /// Findings, already citation-checked. Empty when the plugin does not file
    /// them.
    pub findings: Vec<Finding>,
    /// Citations the model invented, dropped rather than stored.
    pub dropped_citations: usize,
}

#[derive(Deserialize)]
struct RawFinding {
    #[serde(default)]
    code: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    citations: Vec<RawCitation>,
}

#[derive(Deserialize)]
struct RawCitation {
    #[serde(default)]
    segment_id: String,
    #[serde(default)]
    quote: Option<String>,
}

/// Ask the model the plugin's question.
pub async fn run_plugin(
    spec: &AgentSpec,
    provider: &dyn LlmProvider,
    transcript: &Transcript,
) -> Result<(PluginResult, ChatResponse)> {
    if transcript.segments.is_empty() {
        return Err(CaError::Config("cannot analyse an empty transcript".into()));
    }

    let req = ChatRequest::new(vec![
        Message::system(system_prompt(spec)),
        Message::user(user_prompt(spec, transcript)),
    ])
    .with_schema(serde_json::json!({ "type": "object" }));

    let (value, resp): (serde_json::Value, _) = ca_llm::complete_json(provider, req).await?;
    Ok((validate(spec, value, transcript)?, resp))
}

fn system_prompt(spec: &AgentSpec) -> String {
    format!(
        "You are a contact-centre analyst running one specific check.\n\
         \n\
         {}\n\
         \n\
         Rules:\n\
         - Answer only about what the transcript shows. Do not infer beyond it.\n\
         - Placeholders like [PHONE_1] are redacted personal data. Do not try to \
           guess what they stood for.\n\
         - Only report a finding you can cite with a real segment_id.\n\
         - severity is one of: info, low, medium, high, critical.\n\
         - Return only JSON. No prose, no markdown fence.",
        spec.instructions.trim()
    )
}

fn user_prompt(spec: &AgentSpec, transcript: &Transcript) -> String {
    let mut s = format!("{}\n\n", spec.title);
    s.push_str(&ca_agents::citations::transcript_block(transcript));
    s.push_str("\n\nReturn JSON exactly of this shape:\n");
    s.push_str(&spec.response_shape());
    s
}

fn validate(
    spec: &AgentSpec,
    value: serde_json::Value,
    transcript: &Transcript,
) -> Result<PluginResult> {
    let payload = spec.validate_payload(&value)?;

    let mut findings = Vec::new();
    let mut dropped_citations = 0usize;

    if spec.findings {
        let raw: Vec<RawFinding> = value
            .get("findings")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| CaError::InvalidModelOutput(format!("findings: {e}")))?
            .unwrap_or_default();

        for f in raw {
            if f.code.trim().is_empty() {
                continue;
            }
            // The built-in validator owns the shape, so a plugin's citation
            // is checked by exactly the same code a built-in agent's is.
            let raw_cites: Vec<ca_agents::citations::ModelCitation> = f
                .citations
                .iter()
                .map(|c| ca_agents::citations::ModelCitation {
                    segment_id: c.segment_id.clone(),
                    quote: c.quote.clone(),
                })
                .collect();
            let (cites, dropped) = ca_agents::citations::validate_citations(&raw_cites, transcript);
            dropped_citations += dropped;

            // A plugin's findings go into the same register a supervisor acts
            // on, so the evidence rule is the built-in one and is not the
            // plugin author's to relax below the default.
            if spec.require_citations && cites.is_empty() {
                continue;
            }

            findings.push(Finding {
                id: FindingId::new(),
                call_id: transcript.call_id.clone(),
                // Plugins share one kind so the battery's exhaustive matches
                // stay exhaustive; the code carries which plugin it was.
                agent: AgentKind::Rules,
                code: format!(
                    "{}_{}",
                    spec.name.to_ascii_uppercase(),
                    f.code.trim().to_ascii_uppercase()
                ),
                title: if f.title.trim().is_empty() {
                    spec.title.clone()
                } else {
                    f.title.trim().to_string()
                },
                detail: f.detail.trim().to_string(),
                // An unrecognised severity becomes medium rather than dropping
                // an evidenced finding over a label.
                severity: Severity::parse(&f.severity).unwrap_or(Severity::Medium),
                citations: cites,
            });
        }
    }

    Ok(PluginResult {
        payload,
        findings,
        dropped_citations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::ids::{CallId, SegmentId};
    use ca_core::transcript::{Segment, SpeakerRole};

    fn transcript() -> Transcript {
        Transcript {
            call_id: CallId::from("call_1"),
            segments: vec![Segment {
                id: SegmentId::from("seg_a"),
                speaker_index: 0,
                role: SpeakerRole::Agent,
                start: 1.0,
                end: 3.0,
                text: "I will call you back tomorrow".into(),
                redacted_text: None,
                words: vec![],
                confidence: None,
                language: None,
            }],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            duration_secs: 3.0,
            engine: "test".into(),
            model: "test".into(),
        }
    }

    fn spec(findings: bool, require_citations: bool) -> AgentSpec {
        AgentSpec {
            name: "callback_sla".into(),
            title: "Callback within SLA".into(),
            description: String::new(),
            instructions: "Decide whether the agent promised a callback and when.".into(),
            fields: vec![crate::spec::FieldSpec {
                name: "promised".into(),
                kind: crate::spec::FieldKind::Boolean,
                description: String::new(),
                required: true,
            }],
            findings,
            require_citations,
        }
    }

    #[test]
    fn a_valid_response_becomes_a_payload() {
        let r = validate(
            &spec(false, true),
            serde_json::json!({ "promised": true }),
            &transcript(),
        )
        .unwrap();
        assert_eq!(r.payload["promised"], serde_json::json!(true));
        assert!(r.findings.is_empty(), "this plugin does not file findings");
    }

    /// A plugin's findings reach the same register a supervisor acts on, so an
    /// invented citation must not survive.
    #[test]
    fn a_finding_citing_a_segment_that_does_not_exist_is_dropped() {
        let r = validate(
            &spec(true, true),
            serde_json::json!({
                "promised": true,
                "findings": [{
                    "code": "LATE", "title": "Too late", "detail": "…",
                    "severity": "high",
                    "citations": [{ "segment_id": "seg_invented" }]
                }]
            }),
            &transcript(),
        )
        .unwrap();

        assert!(
            r.findings.is_empty(),
            "unevidenced finding must not survive"
        );
        assert_eq!(r.dropped_citations, 1);
    }

    #[test]
    fn a_finding_citing_a_real_segment_is_kept_and_namespaced() {
        let r = validate(
            &spec(true, true),
            serde_json::json!({
                "promised": true,
                "findings": [{
                    "code": "LATE", "title": "Too late", "detail": "Promised in four days.",
                    "severity": "high",
                    "citations": [{ "segment_id": "seg_a", "quote": "call you back" }]
                }]
            }),
            &transcript(),
        )
        .unwrap();

        assert_eq!(r.findings.len(), 1);
        assert_eq!(
            r.findings[0].code, "CALLBACK_SLA_LATE",
            "the plugin name namespaces the code so two plugins cannot collide"
        );
        assert_eq!(r.findings[0].severity, Severity::High);
    }

    #[test]
    fn an_unrecognised_severity_becomes_medium_rather_than_dropping_the_finding() {
        let r = validate(
            &spec(true, true),
            serde_json::json!({
                "promised": true,
                "findings": [{
                    "code": "LATE", "title": "t", "detail": "d", "severity": "spicy",
                    "citations": [{ "segment_id": "seg_a" }]
                }]
            }),
            &transcript(),
        )
        .unwrap();
        assert_eq!(r.findings[0].severity, Severity::Medium);
    }

    #[test]
    fn a_response_missing_a_required_field_is_refused() {
        let err = validate(&spec(false, true), serde_json::json!({}), &transcript()).unwrap_err();
        assert!(format!("{err}").contains("promised"), "got {err}");
    }

    /// Redaction is upstream, but the prompt must not invite the model to
    /// reverse it.
    #[test]
    fn the_prompt_tells_the_model_not_to_guess_at_redacted_data() {
        let p = system_prompt(&spec(false, true));
        assert!(p.contains("[PHONE_1]"));
        assert!(p.contains("Do not try to"), "got {p}");
    }
}
