//! User-facing configuration: which model, on what terms, with whose key.
//!
//! A library crate rather than part of the desktop shell, for the reason the
//! rest of this workspace is arranged that way — the CLI and any future server
//! need the same defaults and the same validation, and a rule that lives in a
//! Tauri command is a rule the other two frontends will get subtly wrong.
//!
//! One screen decides which model the analysis agents talk to, on what terms,
//! and with whose key. Three properties matter more than the plumbing:
//!
//! - **The key is never round-tripped.** It goes in once, straight to the OS
//!   credential store; every reply afterwards says only whether one is present
//!   and where it came from. A settings form that reads a secret back is a
//!   settings form that leaks it into logs, screenshots and error messages.
//! - **Nothing is guessed.** The model list is per vendor with a working
//!   default, and the endpoint is shown, so what is about to be called is
//!   visible before it is called.
//! - **It can be tested.** A key that is wrong should be discovered by pressing
//!   a button, not three hours into a metered batch.

use ca_core::call::CallStage;
use ca_llm::secrets::{self, KeySource};
use ca_llm::{ProviderKind, ProviderSpec};
use serde::{Deserialize, Serialize};

/// The settings row key. One blob rather than a row per field, so a save is
/// atomic and a partially-written configuration cannot exist.
pub const SETTINGS_KEY: &str = "llm";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub provider: ProviderKind,
    pub model: String,
    /// Only meaningful for a custom OpenAI-compatible endpoint.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Model calls in flight during a scoring run.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Redact before anything leaves the machine.
    #[serde(default = "default_true")]
    pub redact_before_send: bool,
}

fn default_concurrency() -> usize {
    4
}
fn default_true() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            // NIM is the default because it is the one vendor this machine has
            // ever had a credential for; it is not an endorsement.
            provider: ProviderKind::NvidiaNim,
            model: default_model_for(ProviderKind::NvidiaNim).to_string(),
            base_url: None,
            concurrency: default_concurrency(),
            redact_before_send: true,
        }
    }
}

impl AppSettings {
    pub fn spec(&self) -> ProviderSpec {
        let mut spec = ProviderSpec::new(self.provider, &self.model);
        spec.base_url = self.base_url.clone().filter(|s| !s.trim().is_empty());
        spec
    }

    /// Reject configurations that cannot work, before they are saved.
    pub fn validate(&self) -> Result<(), String> {
        if self.model.trim().is_empty() {
            return Err("Choose a model.".into());
        }
        if self.provider == ProviderKind::OpenAiCompatible
            && self
                .base_url
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            return Err("A custom endpoint needs a base URL, e.g. https://host/v1".into());
        }
        if self.concurrency == 0 || self.concurrency > 32 {
            return Err("Concurrency must be between 1 and 32.".into());
        }
        // Sending unredacted transcripts to someone else's server is a decision
        // with consequences that outlive the run, so it cannot be made by
        // accident on a vendor endpoint.
        if !self.redact_before_send && !self.provider.is_local() {
            return Err(
                "Redaction can only be turned off for a provider running on this machine.".into(),
            );
        }
        Ok(())
    }
}

/// A model that works today, per vendor, so the field is never blank.
pub fn default_model_for(kind: ProviderKind) -> &'static str {
    match kind {
        // Fast free-tier smoke default. Prefer nemotron-3-nano/super in Settings for agents.
        ProviderKind::NvidiaNim => "meta/llama-3.1-8b-instruct",
        ProviderKind::OpenAi => "gpt-4o",
        ProviderKind::OpenRouter => "anthropic/claude-sonnet-4.5",
        ProviderKind::Groq => "llama-3.3-70b-versatile",
        ProviderKind::Ollama => "llama3.1",
        ProviderKind::LmStudio => "local-model",
        ProviderKind::OpenAiCompatible => "",
        ProviderKind::Anthropic => "claude-sonnet-5",
        ProviderKind::Gemini => "gemini-2.5-pro",
    }
}

/// Models worth offering without typing. Not exhaustive — the field stays free
/// text, because a vendor adds models faster than this list can be updated.
pub fn suggested_models(kind: ProviderKind) -> &'static [&'static str] {
    match kind {
        ProviderKind::NvidiaNim => &[
            "meta/llama-3.1-8b-instruct",
            "nvidia/nemotron-3-nano-30b-a3b",
            "nvidia/nemotron-3-super-120b-a12b",
            "nvidia/llama-3.3-nemotron-super-49b-v1.5",
            "meta/llama-3.3-70b-instruct",
        ],
        ProviderKind::OpenAi => &["gpt-4o", "gpt-4o-mini", "gpt-4.1", "o4-mini"],
        ProviderKind::OpenRouter => &[
            "anthropic/claude-sonnet-4.5",
            "openai/gpt-4o",
            "meta-llama/llama-3.3-70b-instruct",
        ],
        ProviderKind::Groq => &["llama-3.3-70b-versatile", "llama-3.1-8b-instant"],
        ProviderKind::Ollama => &["llama3.1", "qwen2.5", "mistral", "phi4"],
        ProviderKind::LmStudio => &["local-model"],
        ProviderKind::OpenAiCompatible => &[],
        ProviderKind::Anthropic => &[
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-haiku-4-5-20251001",
        ],
        ProviderKind::Gemini => &["gemini-2.5-pro", "gemini-2.5-flash"],
    }
}

/// The endpoint a vendor will be called on, shown so it is never a surprise.
pub fn endpoint_for(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::NvidiaNim => "https://integrate.api.nvidia.com/v1",
        ProviderKind::OpenAi => "https://api.openai.com/v1",
        ProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
        ProviderKind::Groq => "https://api.groq.com/openai/v1",
        ProviderKind::Ollama => "http://localhost:11434/v1",
        ProviderKind::LmStudio => "http://localhost:1234/v1",
        ProviderKind::OpenAiCompatible => "",
        ProviderKind::Anthropic => "https://api.anthropic.com/v1",
        ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta",
    }
}

/// One vendor, as the settings screen shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub kind: ProviderKind,
    pub id: String,
    pub label: String,
    pub is_local: bool,
    pub requires_api_key: bool,
    /// The vendor's conventional variable, offered as an alternative to typing
    /// the key in at all.
    pub env_var: Option<String>,
    pub endpoint: String,
    pub default_model: String,
    pub suggested_models: Vec<String>,
    /// Whether a usable key is present, and from where. Never the key itself.
    pub key_source: KeySource,
}

pub fn provider_catalogue() -> Vec<ProviderInfo> {
    ProviderKind::all()
        .iter()
        .map(|&kind| ProviderInfo {
            kind,
            id: kind.as_str().to_string(),
            label: kind.label().to_string(),
            is_local: kind.is_local(),
            requires_api_key: kind.requires_api_key(),
            env_var: kind.env_var().map(str::to_string),
            endpoint: endpoint_for(kind).to_string(),
            default_model: default_model_for(kind).to_string(),
            suggested_models: suggested_models(kind)
                .iter()
                .map(|s| s.to_string())
                .collect(),
            key_source: secrets::key_source(kind),
        })
        .collect()
}

/// The outcome of pressing "Test connection".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTest {
    pub ok: bool,
    /// What the endpoint actually answered with, which is not always what was
    /// asked for — routers substitute models.
    pub model: Option<String>,
    pub latency_ms: Option<u64>,
    pub message: String,
}

/// Which stage a call must already have reached before it can be scored.
///
/// Redacted when the model is someone else's; merely transcribed is enough for
/// one running on this machine, where there is nothing to leak.
pub fn required_stage(settings: &AppSettings) -> CallStage {
    if settings.provider.is_local() && !settings.redact_before_send {
        CallStage::Diarized
    } else {
        CallStage::Redacted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cloud_vendor_has_a_working_default_model() {
        // A blank model field is a failed first run for someone who just wants
        // to press the button.
        for kind in ProviderKind::all() {
            if *kind == ProviderKind::OpenAiCompatible {
                continue; // No endpoint is known, so no model can be.
            }
            assert!(!default_model_for(*kind).is_empty(), "{kind:?}");
            assert!(!endpoint_for(*kind).is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn the_default_model_is_among_the_suggestions() {
        for kind in ProviderKind::all() {
            let suggestions = suggested_models(*kind);
            if suggestions.is_empty() {
                continue;
            }
            assert!(
                suggestions.contains(&default_model_for(*kind)),
                "{kind:?} defaults to a model it does not offer"
            );
        }
    }

    #[test]
    fn a_custom_endpoint_without_a_url_is_rejected() {
        let s = AppSettings {
            provider: ProviderKind::OpenAiCompatible,
            model: "m".into(),
            base_url: Some("   ".into()),
            ..AppSettings::default()
        };
        assert!(s.validate().unwrap_err().contains("base URL"));
    }

    #[test]
    fn redaction_cannot_be_disabled_for_a_remote_provider() {
        // The whole redact-before-cloud promise reduces to this check.
        let s = AppSettings {
            provider: ProviderKind::OpenAi,
            model: "gpt-4o".into(),
            redact_before_send: false,
            ..AppSettings::default()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn redaction_may_be_disabled_for_a_local_provider() {
        let s = AppSettings {
            provider: ProviderKind::Ollama,
            model: "llama3.1".into(),
            redact_before_send: false,
            ..AppSettings::default()
        };
        assert!(s.validate().is_ok());
        assert_eq!(required_stage(&s), CallStage::Diarized);
    }

    #[test]
    fn a_remote_provider_may_only_score_redacted_calls() {
        let s = AppSettings::default();
        assert_eq!(required_stage(&s), CallStage::Redacted);
    }

    #[test]
    fn absurd_concurrency_is_rejected() {
        for n in [0usize, 33] {
            let s = AppSettings {
                concurrency: n,
                ..AppSettings::default()
            };
            assert!(s.validate().is_err(), "{n} was accepted");
        }
    }

    #[test]
    fn the_catalogue_covers_every_vendor_and_carries_no_secrets() {
        let cat = provider_catalogue();
        assert_eq!(cat.len(), ProviderKind::all().len());
        // Serialised and inspected because this payload crosses to the UI.
        let json = serde_json::to_string(&cat).unwrap();
        assert!(!json.contains("password"));
        assert!(!json.contains("apiKey"));
    }

    #[test]
    fn defaults_round_trip_through_serde() {
        let json = serde_json::to_string(&AppSettings::default()).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provider, ProviderKind::NvidiaNim);
        assert!(back.redact_before_send);
    }
}
