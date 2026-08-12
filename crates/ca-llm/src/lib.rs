//! LLM provider implementations.
//!
//! Every vendor the app supports is reachable through [`ca_core::LlmProvider`].
//! The bulk of them share a single implementation: OpenAI, OpenRouter, NVIDIA
//! NIM, Groq, Ollama, LM Studio and any BYOK gateway all speak the same
//! `/chat/completions` dialect, so they are distinguished by configuration
//! rather than by code. Vendors with their own wire format get their own module.

pub mod anthropic;
pub mod gemini;
pub mod json_repair;
pub mod openai_compat;
pub mod registry;
pub mod secrets;

pub use anthropic::{AnthropicConfig, AnthropicProvider};
pub use gemini::{GeminiConfig, GeminiProvider};
pub use json_repair::extract_json;
pub use openai_compat::{Auth, OpenAiCompatConfig, OpenAiCompatible};
pub use registry::{build_provider, ProviderKind, ProviderSpec};
pub use secrets::{key_source, resolve_key, KeySource};

use ca_core::error::{CaError, Result};
use ca_core::provider::{ChatRequest, ChatResponse, LlmProvider};
use serde::de::DeserializeOwned;

/// Flatten a `reqwest` error into something a person can act on.
///
/// `reqwest`'s own `Display` stops at "error sending request for url (...)" and
/// leaves the cause in the source chain. That single sentence is the same for a
/// timeout, a DNS failure and a refused TLS handshake — three problems with
/// three different fixes — so an overnight batch that dies produces a log the
/// next morning that says nothing. Walk the chain instead.
pub(crate) fn describe(e: &reqwest::Error) -> String {
    use std::error::Error;

    let mut parts: Vec<String> = Vec::new();
    if e.is_timeout() {
        parts.push("timed out".into());
    } else if e.is_connect() {
        parts.push("could not connect".into());
    }
    parts.push(e.to_string());

    let mut src = e.source();
    while let Some(s) = src {
        let text = s.to_string();
        // Nested transport errors repeat themselves; only add new information.
        if !parts.iter().any(|p| p == &text) {
            parts.push(text);
        }
        src = s.source();
    }
    parts.join(": ")
}

/// Run a completion and deserialize the result into `T`.
///
/// Wraps the provider call with the two failure modes that actually happen in
/// practice: the model wraps its JSON in prose or a markdown fence, and the
/// model returns JSON that does not match the expected shape. Both surface as
/// [`CaError::InvalidModelOutput`] so a batch can skip the call and continue.
pub async fn complete_json<T: DeserializeOwned>(
    provider: &dyn LlmProvider,
    req: ChatRequest,
) -> Result<(T, ChatResponse)> {
    let resp = provider.complete(req).await?;

    let json_str = extract_json(&resp.content).ok_or_else(|| {
        CaError::InvalidModelOutput(format!(
            "no JSON object found in response from `{}`: {}",
            provider.name(),
            resp.content.chars().take(300).collect::<String>()
        ))
    })?;

    let value: T = serde_json::from_str(json_str).map_err(|e| {
        CaError::InvalidModelOutput(format!(
            "response from `{}` did not match expected schema: {e}; payload: {}",
            provider.name(),
            json_str.chars().take(300).collect::<String>()
        ))
    })?;

    Ok((value, resp))
}
