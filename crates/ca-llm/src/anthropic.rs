//! Anthropic Messages API adapter.
//!
//! Anthropic differs from the OpenAI dialect in three ways that matter here:
//!
//! 1. Auth is `x-api-key` plus a required `anthropic-version` header.
//! 2. The system prompt is a **top-level field**, not a message with role
//!    `system` — sending it as a message is rejected.
//! 3. `max_tokens` is required, and on current models (Opus 5, Sonnet 5, Opus
//!    4.7/4.8, Fable 5) `temperature` was removed and returns a 400. Since our
//!    `ChatRequest` carries a temperature for every provider, this adapter
//!    drops it rather than passing it through.

use async_trait::async_trait;
use ca_core::error::{CaError, Result};
use ca_core::provider::{Caps, ChatRequest, ChatResponse, LlmProvider, Role};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8192;

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub caps: Caps,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl AnthropicConfig {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        let caps = pricing_for(&model);
        Self {
            api_key: api_key.into(),
            model,
            base_url: "https://api.anthropic.com/v1".into(),
            caps,
            timeout: Duration::from_secs(180),
            max_retries: 3,
        }
    }
}

/// Published per-MTok pricing, used for the cost column in the dashboard.
/// Unknown models get no pricing rather than a wrong guess.
fn pricing_for(model: &str) -> Caps {
    let (input, output) = match model {
        m if m.starts_with("claude-opus-5") || m.starts_with("claude-opus-4") => (5.0, 25.0),
        m if m.starts_with("claude-sonnet-5") || m.starts_with("claude-sonnet-4") => (3.0, 15.0),
        m if m.starts_with("claude-haiku-4") => (1.0, 5.0),
        m if m.starts_with("claude-fable-5") || m.starts_with("claude-mythos-5") => (10.0, 50.0),
        _ => {
            return Caps {
                json_mode: true,
                context_tokens: 200_000,
                ..Default::default()
            }
        }
    };
    Caps {
        json_mode: true,
        native_schema: false,
        audio_input: false,
        embeddings: false,
        context_tokens: 1_000_000,
        input_cost_per_mtok: Some(input),
        output_cost_per_mtok: Some(output),
    }
}

/// Models on which the `temperature` parameter was removed and now 400s.
fn rejects_temperature(model: &str) -> bool {
    const PREFIXES: [&str; 6] = [
        "claude-opus-5",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    PREFIXES.iter().any(|p| model.starts_with(p))
}

pub struct AnthropicProvider {
    cfg: AnthropicConfig,
    http: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(cfg: AnthropicConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| CaError::Config(format!("http client: {e}")))?;
        Ok(Self { cfg, http })
    }
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    content: Vec<WireBlock>,
    #[serde(default)]
    usage: Option<WireUsage>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.cfg.model
    }

    fn capabilities(&self) -> Caps {
        self.cfg.caps.clone()
    }

    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/messages", self.cfg.base_url.trim_end_matches('/'));

        // System turns are hoisted out of the message list into the top-level
        // `system` field; Anthropic rejects a message with role `system`.
        let system: Option<String> = {
            let joined = req
                .messages
                .iter()
                .filter(|m| m.role == Role::System)
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        };

        let messages: Vec<WireMessage> = req
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| WireMessage {
                role: match m.role {
                    Role::Assistant => "assistant",
                    // System is filtered out above; anything else is a user turn.
                    _ => "user",
                },
                content: &m.content,
            })
            .collect();

        if messages.is_empty() {
            return Err(CaError::Config(
                "anthropic requires at least one non-system message".into(),
            ));
        }

        let body = WireRequest {
            model: &self.cfg.model,
            max_tokens: req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            messages,
            system: system.as_deref(),
            temperature: if rejects_temperature(&self.cfg.model) {
                None
            } else {
                Some(req.temperature)
            },
        };

        let mut attempt = 0u32;
        loop {
            let started = Instant::now();
            let sent = self
                .http
                .post(&url)
                .header("x-api-key", &self.cfg.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body)
                .send()
                .await;

            let outcome: Result<ChatResponse> = match sent {
                Err(e) => Err(CaError::provider("anthropic", crate::describe(&e))),
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        let parsed: WireResponse = r.json().await.map_err(|e| {
                            CaError::InvalidModelOutput(format!("malformed response body: {e}"))
                        })?;

                        // A safety refusal returns HTTP 200 with an empty or
                        // partial body — surface it rather than treating the
                        // empty content array as a transport failure.
                        if parsed.stop_reason.as_deref() == Some("refusal") {
                            return Err(CaError::ProviderResponse {
                                provider: "anthropic".into(),
                                status: 200,
                                message: "request declined by safety classifier".into(),
                            });
                        }

                        let text: String = parsed
                            .content
                            .into_iter()
                            .filter(|b| b.kind == "text")
                            .filter_map(|b| b.text)
                            .collect::<Vec<_>>()
                            .join("");

                        if text.trim().is_empty() {
                            Err(CaError::InvalidModelOutput("empty completion".into()))
                        } else {
                            let u = parsed.usage.unwrap_or_default();
                            return Ok(ChatResponse {
                                content: text,
                                input_tokens: u.input_tokens,
                                output_tokens: u.output_tokens,
                                model: parsed.model.unwrap_or_else(|| self.cfg.model.clone()),
                                latency_ms: started.elapsed().as_millis() as u64,
                            });
                        }
                    } else if status.as_u16() == 429 {
                        let retry_after = r
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.parse::<u64>().ok());
                        Err(CaError::RateLimited {
                            provider: "anthropic".into(),
                            retry_after,
                        })
                    } else if status.is_server_error() {
                        let msg = r.text().await.unwrap_or_default();
                        Err(CaError::provider("anthropic", format!("{status}: {msg}")))
                    } else {
                        let msg = r.text().await.unwrap_or_default();
                        Err(CaError::ProviderResponse {
                            provider: "anthropic".into(),
                            status: status.as_u16(),
                            message: msg.chars().take(500).collect(),
                        })
                    }
                }
            };

            match outcome {
                Ok(v) => return Ok(v),
                Err(e) => {
                    attempt += 1;
                    if !e.is_retryable() || attempt > self.cfg.max_retries {
                        return Err(e);
                    }
                    let backoff = match &e {
                        CaError::RateLimited {
                            retry_after: Some(s),
                            ..
                        } => Duration::from_secs(*s),
                        _ => Duration::from_secs(1u64 << (attempt - 1).min(6)),
                    };
                    tracing::warn!(provider = "anthropic", attempt, error = %e, "retrying");
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_models_reject_temperature() {
        for m in [
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-fable-5",
        ] {
            assert!(rejects_temperature(m), "{m} should drop temperature");
        }
    }

    #[test]
    fn older_models_still_accept_temperature() {
        assert!(!rejects_temperature("claude-3-5-sonnet-20241022"));
        assert!(!rejects_temperature("claude-haiku-4-5"));
    }

    #[test]
    fn pricing_is_known_for_current_families() {
        let caps = pricing_for("claude-opus-5");
        // 1M in + 1M out at $5/$25.
        let c = caps.cost(1_000_000, 1_000_000).unwrap();
        assert!((c - 30.0).abs() < 1e-9, "got {c}");
    }

    #[test]
    fn unknown_model_reports_no_pricing_rather_than_guessing() {
        assert!(pricing_for("claude-future-9").cost(1000, 1000).is_none());
    }
}
