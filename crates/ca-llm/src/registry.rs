//! Turning a saved provider configuration into a live [`LlmProvider`].
//!
//! This is the seam the settings UI writes to and the pipeline reads from.
//! Adding a vendor that speaks the OpenAI dialect means adding one enum
//! variant and one match arm — no new HTTP code.

use crate::anthropic::{AnthropicConfig, AnthropicProvider};
use crate::gemini::{GeminiConfig, GeminiProvider};
use crate::openai_compat::{Auth, OpenAiCompatConfig, OpenAiCompatible};
use ca_core::error::{CaError, Result};
use ca_core::provider::LlmProvider;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    NvidiaNim,
    OpenAi,
    OpenRouter,
    Groq,
    Ollama,
    LmStudio,
    /// Any user-supplied OpenAI-compatible endpoint (the BYOK case).
    OpenAiCompatible,
    Anthropic,
    Gemini,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::NvidiaNim => "nvidia_nim",
            ProviderKind::OpenAi => "openai",
            ProviderKind::OpenRouter => "openrouter",
            ProviderKind::Groq => "groq",
            ProviderKind::Ollama => "ollama",
            ProviderKind::LmStudio => "lmstudio",
            ProviderKind::OpenAiCompatible => "openai_compatible",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
        }
    }

    /// Display name for the settings UI.
    pub fn label(&self) -> &'static str {
        match self {
            ProviderKind::NvidiaNim => "NVIDIA NIM",
            ProviderKind::OpenAi => "OpenAI",
            ProviderKind::OpenRouter => "OpenRouter",
            ProviderKind::Groq => "Groq",
            ProviderKind::Ollama => "Ollama (local)",
            ProviderKind::LmStudio => "LM Studio (local)",
            ProviderKind::OpenAiCompatible => "Custom (OpenAI-compatible)",
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::Gemini => "Google Gemini",
        }
    }

    /// Whether the provider runs on this machine. Local providers need no key
    /// and are exempt from the redact-before-send requirement, since nothing
    /// leaves the box.
    pub fn is_local(&self) -> bool {
        matches!(self, ProviderKind::Ollama | ProviderKind::LmStudio)
    }

    pub fn requires_api_key(&self) -> bool {
        !self.is_local()
    }

    /// The environment variable this vendor's own tooling uses.
    ///
    /// Reading the conventional name means a user who already has the vendor's
    /// CLI working needs no extra setup, and — more to the point — no key ever
    /// has to be typed into a config file that might get committed.
    pub fn env_var(&self) -> Option<&'static str> {
        match self {
            ProviderKind::NvidiaNim => Some("NVIDIA_API_KEY"),
            ProviderKind::OpenAi => Some("OPENAI_API_KEY"),
            ProviderKind::OpenRouter => Some("OPENROUTER_API_KEY"),
            ProviderKind::Groq => Some("GROQ_API_KEY"),
            ProviderKind::Anthropic => Some("ANTHROPIC_API_KEY"),
            ProviderKind::Gemini => Some("GEMINI_API_KEY"),
            ProviderKind::OpenAiCompatible => Some("CA_LLM_API_KEY"),
            ProviderKind::Ollama | ProviderKind::LmStudio => None,
        }
    }

    /// Look the key up in the environment. Blank is treated as absent so that
    /// an exported-but-empty variable fails at build time with a clear message
    /// rather than as a 401 partway through a batch.
    pub fn api_key_from_env(&self) -> Option<String> {
        let name = self.env_var()?;
        std::env::var(name)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().to_string())
    }

    /// Parse the `as_str` form, plus a few spellings a person would type.
    pub fn parse(s: &str) -> Option<ProviderKind> {
        let k = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        ProviderKind::all()
            .iter()
            .copied()
            .find(|p| p.as_str() == k)
            .or(match k.as_str() {
                "nim" | "nvidia" => Some(ProviderKind::NvidiaNim),
                "lm_studio" => Some(ProviderKind::LmStudio),
                "claude" => Some(ProviderKind::Anthropic),
                "google" => Some(ProviderKind::Gemini),
                "custom" | "byok" => Some(ProviderKind::OpenAiCompatible),
                _ => None,
            })
    }

    pub fn all() -> &'static [ProviderKind] {
        &[
            ProviderKind::NvidiaNim,
            ProviderKind::OpenAi,
            ProviderKind::OpenRouter,
            ProviderKind::Groq,
            ProviderKind::Ollama,
            ProviderKind::LmStudio,
            ProviderKind::OpenAiCompatible,
            ProviderKind::Anthropic,
            ProviderKind::Gemini,
        ]
    }
}

/// A persisted provider configuration. The API key is *not* stored here — it
/// lives in the OS credential store and is looked up by `kind` at build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSpec {
    pub kind: ProviderKind,
    pub model: String,
    /// Required for `OpenAiCompatible`, ignored elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional display label so a user can keep two configs of one vendor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}

impl ProviderSpec {
    pub fn new(kind: ProviderKind, model: impl Into<String>) -> Self {
        Self {
            kind,
            model: model.into(),
            base_url: None,
            nickname: None,
        }
    }
}

/// Build a live provider from a spec plus a secret.
///
/// `api_key` is `None` for local providers. Passing `None` for a provider that
/// needs one is a configuration error rather than a runtime auth failure, so it
/// surfaces before a batch starts instead of on the first call.
pub fn build_provider(spec: &ProviderSpec, api_key: Option<&str>) -> Result<Arc<dyn LlmProvider>> {
    if spec.kind.requires_api_key() && api_key.map(|k| k.trim().is_empty()).unwrap_or(true) {
        return Err(CaError::Config(format!(
            "provider `{}` requires an API key",
            spec.kind.label()
        )));
    }
    let key = api_key.unwrap_or_default().to_string();
    let model = spec.model.clone();

    let provider: Arc<dyn LlmProvider> = match spec.kind {
        ProviderKind::NvidiaNim => Arc::new(OpenAiCompatible::new(
            OpenAiCompatConfig::nvidia_nim(key, model),
        )?),
        ProviderKind::OpenAi => Arc::new(OpenAiCompatible::new(OpenAiCompatConfig::openai(
            key, model,
        ))?),
        ProviderKind::OpenRouter => Arc::new(OpenAiCompatible::new(
            OpenAiCompatConfig::openrouter(key, model),
        )?),
        ProviderKind::Groq => {
            Arc::new(OpenAiCompatible::new(OpenAiCompatConfig::groq(key, model))?)
        }
        ProviderKind::Ollama => Arc::new(OpenAiCompatible::new(OpenAiCompatConfig::ollama(model))?),
        ProviderKind::LmStudio => {
            Arc::new(OpenAiCompatible::new(OpenAiCompatConfig::lm_studio(model))?)
        }
        ProviderKind::OpenAiCompatible => {
            let base = spec.base_url.as_deref().ok_or_else(|| {
                CaError::Config("custom OpenAI-compatible provider needs a base_url".into())
            })?;
            Arc::new(OpenAiCompatible::new(OpenAiCompatConfig::custom(
                spec.nickname.clone().unwrap_or_else(|| "custom".into()),
                base,
                model,
                Auth::Bearer(key),
            ))?)
        }
        ProviderKind::Anthropic => {
            Arc::new(AnthropicProvider::new(AnthropicConfig::new(key, model))?)
        }
        ProviderKind::Gemini => Arc::new(GeminiProvider::new(GeminiConfig::new(key, model))?),
    };

    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_providers_need_no_key() {
        for kind in [ProviderKind::Ollama, ProviderKind::LmStudio] {
            let spec = ProviderSpec::new(kind, "llama3");
            assert!(build_provider(&spec, None).is_ok(), "{kind:?} should build");
        }
    }

    #[test]
    fn cloud_providers_fail_fast_without_a_key() {
        // Better to fail here than three hours into a 1,968-call batch.
        let spec = ProviderSpec::new(ProviderKind::Anthropic, "claude-opus-5");
        let err = build_provider(&spec, None).err().unwrap();
        assert!(matches!(err, CaError::Config(_)), "got {err:?}");
    }

    #[test]
    fn blank_key_is_treated_as_missing() {
        let spec = ProviderSpec::new(ProviderKind::OpenAi, "gpt-4o");
        assert!(build_provider(&spec, Some("   ")).is_err());
    }

    #[test]
    fn custom_endpoint_requires_a_base_url() {
        let spec = ProviderSpec::new(ProviderKind::OpenAiCompatible, "some-model");
        let err = build_provider(&spec, Some("k")).err().unwrap();
        assert!(format!("{err}").contains("base_url"), "got {err}");
    }

    #[test]
    fn every_kind_builds_when_given_what_it_needs() {
        for kind in ProviderKind::all() {
            let mut spec = ProviderSpec::new(*kind, "m");
            if *kind == ProviderKind::OpenAiCompatible {
                spec.base_url = Some("https://example.test/v1".into());
            }
            let key = if kind.requires_api_key() {
                Some("k")
            } else {
                None
            };
            assert!(
                build_provider(&spec, key).is_ok(),
                "{kind:?} failed to build"
            );
        }
    }

    #[test]
    fn every_cloud_provider_names_an_env_var() {
        // A vendor with no env var has no way to receive a key without one
        // being written to disk, which is the thing this avoids.
        for kind in ProviderKind::all() {
            assert_eq!(
                kind.env_var().is_some(),
                kind.requires_api_key(),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn parse_accepts_canonical_and_colloquial_names() {
        for kind in ProviderKind::all() {
            assert_eq!(ProviderKind::parse(kind.as_str()), Some(*kind));
        }
        assert_eq!(ProviderKind::parse("NIM"), Some(ProviderKind::NvidiaNim));
        assert_eq!(
            ProviderKind::parse("lm-studio"),
            Some(ProviderKind::LmStudio)
        );
        assert_eq!(ProviderKind::parse("nonsense"), None);
    }

    #[test]
    fn kind_round_trips_through_serde() {
        for kind in ProviderKind::all() {
            let json = serde_json::to_string(kind).unwrap();
            let back: ProviderKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back);
        }
    }
}
