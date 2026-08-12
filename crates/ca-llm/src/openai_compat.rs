//! One implementation covering every OpenAI-shaped endpoint.
//!
//! OpenAI, OpenRouter, NVIDIA NIM, Groq, Ollama, LM Studio and arbitrary BYOK
//! gateways all speak the same `/chat/completions` dialect. They differ only in
//! base URL, auth header, and which optional fields they honour — so they are
//! configuration, not code.

use async_trait::async_trait;
use ca_core::error::{CaError, Result};
use ca_core::provider::{Caps, ChatRequest, ChatResponse, LlmProvider, Message, Role};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// How the endpoint expects credentials to be presented.
#[derive(Debug, Clone)]
pub enum Auth {
    Bearer(String),
    Header {
        name: String,
        value: String,
    },
    /// Local servers (Ollama, LM Studio) usually need nothing.
    None,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    /// Display name used in errors and cost reporting, e.g. "nvidia-nim".
    pub provider_name: String,
    /// Base URL including `/v1`, e.g. `https://integrate.api.nvidia.com/v1`.
    pub base_url: String,
    pub model: String,
    pub auth: Auth,
    pub caps: Caps,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl OpenAiCompatConfig {
    /// NVIDIA NIM — verified working against `integrate.api.nvidia.com`.
    pub fn nvidia_nim(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_name: "nvidia-nim".into(),
            base_url: "https://integrate.api.nvidia.com/v1".into(),
            model: model.into(),
            auth: Auth::Bearer(api_key.into()),
            caps: Caps {
                json_mode: true,
                native_schema: false,
                audio_input: false,
                embeddings: true,
                context_tokens: 128_000,
                input_cost_per_mtok: None,
                output_cost_per_mtok: None,
            },
            // NIM cold-starts a model on first hit; a short timeout turns that
            // into a spurious failure at the top of a batch.
            timeout: Duration::from_secs(180),
            max_retries: 3,
        }
    }

    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_name: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: model.into(),
            auth: Auth::Bearer(api_key.into()),
            caps: Caps {
                json_mode: true,
                native_schema: true,
                embeddings: true,
                context_tokens: 128_000,
                ..Default::default()
            },
            timeout: Duration::from_secs(120),
            max_retries: 3,
        }
    }

    pub fn openrouter(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_name: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            model: model.into(),
            auth: Auth::Bearer(api_key.into()),
            caps: Caps {
                json_mode: true,
                embeddings: false,
                context_tokens: 128_000,
                ..Default::default()
            },
            timeout: Duration::from_secs(120),
            max_retries: 3,
        }
    }

    pub fn groq(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider_name: "groq".into(),
            base_url: "https://api.groq.com/openai/v1".into(),
            model: model.into(),
            auth: Auth::Bearer(api_key.into()),
            caps: Caps {
                json_mode: true,
                embeddings: false,
                context_tokens: 128_000,
                ..Default::default()
            },
            timeout: Duration::from_secs(120),
            max_retries: 3,
        }
    }

    pub fn ollama(model: impl Into<String>) -> Self {
        Self {
            provider_name: "ollama".into(),
            base_url: "http://localhost:11434/v1".into(),
            model: model.into(),
            auth: Auth::None,
            caps: Caps {
                json_mode: true,
                embeddings: true,
                context_tokens: 32_768,
                ..Default::default()
            },
            // A local model on CPU can take minutes per call; do not time it out.
            timeout: Duration::from_secs(600),
            max_retries: 1,
        }
    }

    pub fn lm_studio(model: impl Into<String>) -> Self {
        Self {
            provider_name: "lmstudio".into(),
            base_url: "http://localhost:1234/v1".into(),
            model: model.into(),
            auth: Auth::None,
            caps: Caps {
                json_mode: true,
                embeddings: true,
                context_tokens: 32_768,
                ..Default::default()
            },
            timeout: Duration::from_secs(600),
            max_retries: 1,
        }
    }

    /// Arbitrary user-supplied endpoint — the BYOK case.
    pub fn custom(
        name: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        auth: Auth,
    ) -> Self {
        Self {
            provider_name: name.into(),
            base_url: base_url.into(),
            model: model.into(),
            auth,
            caps: Caps {
                json_mode: true,
                context_tokens: 32_768,
                ..Default::default()
            },
            timeout: Duration::from_secs(180),
            max_retries: 3,
        }
    }
}

pub struct OpenAiCompatible {
    cfg: OpenAiCompatConfig,
    http: reqwest::Client,
}

impl OpenAiCompatible {
    pub fn new(cfg: OpenAiCompatConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            // Separate from the overall timeout, which is generous because a
            // cold model can legitimately take minutes to answer. Reaching the
            // host should never take minutes, so an unreachable endpoint fails
            // in seconds rather than stalling every retry for the full window.
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| CaError::Config(format!("http client: {e}")))?;
        Ok(Self { cfg, http })
    }

    pub fn config(&self) -> &OpenAiCompatConfig {
        &self.cfg
    }

    fn apply_auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.cfg.auth {
            Auth::Bearer(k) => rb.bearer_auth(k),
            Auth::Header { name, value } => rb.header(name.as_str(), value.as_str()),
            Auth::None => rb,
        }
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
    messages: Vec<WireMessage<'a>>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireChoiceMessage,
}

#[derive(Deserialize)]
struct WireChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatible {
    fn name(&self) -> &str {
        &self.cfg.provider_name
    }

    fn model(&self) -> &str {
        &self.cfg.model
    }

    fn capabilities(&self) -> Caps {
        self.cfg.caps.clone()
    }

    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse> {
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );

        // Ask for JSON mode only when the endpoint claims to support it; sending
        // `response_format` to a server that rejects unknown fields fails the call.
        let response_format = if req.json_schema.is_some() && self.cfg.caps.json_mode {
            Some(serde_json::json!({ "type": "json_object" }))
        } else {
            None
        };

        let messages: Vec<WireMessage> = req
            .messages
            .iter()
            .map(|m: &Message| WireMessage {
                role: role_str(m.role),
                content: &m.content,
            })
            .collect();

        let body = WireRequest {
            model: &self.cfg.model,
            messages,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            response_format,
        };

        let mut attempt = 0u32;
        loop {
            let started = Instant::now();
            let sent = self
                .apply_auth(self.http.post(&url))
                .json(&body)
                .send()
                .await;

            let outcome: Result<ChatResponse> = match sent {
                Err(e) => Err(CaError::provider(
                    &self.cfg.provider_name,
                    crate::describe(&e),
                )),
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        let parsed: WireResponse = r.json().await.map_err(|e| {
                            CaError::InvalidModelOutput(format!("malformed response body: {e}"))
                        })?;
                        let content = parsed
                            .choices
                            .into_iter()
                            .next()
                            .and_then(|c| c.message.content)
                            .unwrap_or_default();
                        if content.trim().is_empty() {
                            Err(CaError::InvalidModelOutput("empty completion".into()))
                        } else {
                            let u = parsed.usage.unwrap_or_default();
                            return Ok(ChatResponse {
                                content,
                                input_tokens: u.prompt_tokens,
                                output_tokens: u.completion_tokens,
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
                            provider: self.cfg.provider_name.clone(),
                            retry_after,
                        })
                    } else if status.is_server_error() {
                        let msg = r.text().await.unwrap_or_default();
                        Err(CaError::provider(
                            &self.cfg.provider_name,
                            format!("{status}: {msg}"),
                        ))
                    } else {
                        let msg = r.text().await.unwrap_or_default();
                        Err(CaError::ProviderResponse {
                            provider: self.cfg.provider_name.clone(),
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
                        // Exponential: 1s, 2s, 4s.
                        _ => Duration::from_secs(1u64 << (attempt - 1).min(6)),
                    };
                    tracing::warn!(
                        provider = %self.cfg.provider_name,
                        attempt,
                        backoff_secs = backoff.as_secs(),
                        error = %e,
                        "retrying provider call"
                    );
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if !self.cfg.caps.embeddings {
            return Err(CaError::Config(format!(
                "provider `{}` has embeddings disabled",
                self.cfg.provider_name
            )));
        }
        let url = format!("{}/embeddings", self.cfg.base_url.trim_end_matches('/'));

        #[derive(Deserialize)]
        struct EmbResp {
            data: Vec<EmbItem>,
        }
        #[derive(Deserialize)]
        struct EmbItem {
            embedding: Vec<f32>,
        }

        let r = self
            .apply_auth(self.http.post(&url))
            .json(&serde_json::json!({
                "model": self.cfg.model,
                "input": texts,
                // NIM's retriever embedders require input_type; OpenAI ignores it.
                "input_type": "passage",
            }))
            .send()
            .await
            .map_err(|e| CaError::provider(&self.cfg.provider_name, crate::describe(&e)))?;

        if !r.status().is_success() {
            let status = r.status().as_u16();
            let message = r
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            return Err(CaError::ProviderResponse {
                provider: self.cfg.provider_name.clone(),
                status,
                message,
            });
        }

        let parsed: EmbResp = r
            .json()
            .await
            .map_err(|e| CaError::InvalidModelOutput(format!("malformed embedding body: {e}")))?;
        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nim_preset_points_at_the_verified_endpoint() {
        let c = OpenAiCompatConfig::nvidia_nim("k", "m");
        assert_eq!(c.base_url, "https://integrate.api.nvidia.com/v1");
        assert!(c.caps.embeddings);
    }

    #[test]
    fn local_presets_need_no_auth_and_wait_longer() {
        for c in [
            OpenAiCompatConfig::ollama("m"),
            OpenAiCompatConfig::lm_studio("m"),
        ] {
            assert!(matches!(c.auth, Auth::None));
            // CPU inference is slow; a 2-minute default would abort valid runs.
            assert!(c.timeout >= Duration::from_secs(600));
        }
    }

    #[test]
    fn cost_is_none_when_pricing_is_unknown() {
        let caps = OpenAiCompatConfig::nvidia_nim("k", "m").caps;
        assert!(caps.cost(1000, 1000).is_none());
    }

    #[test]
    fn cost_computes_per_million_tokens() {
        let caps = Caps {
            input_cost_per_mtok: Some(1.0),
            output_cost_per_mtok: Some(3.0),
            ..Default::default()
        };
        // 1M input @ $1 + 1M output @ $3 = $4
        let c = caps.cost(1_000_000, 1_000_000).unwrap();
        assert!((c - 4.0).abs() < 1e-9, "got {c}");
    }
}
