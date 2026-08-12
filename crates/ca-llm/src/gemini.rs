//! Google Gemini adapter (generativelanguage.googleapis.com).
//!
//! Gemini's wire format differs from OpenAI's more than Anthropic's does:
//! the system prompt is `system_instruction`, turns are `contents` with
//! `parts`, the assistant role is spelled `model` rather than `assistant`,
//! and sampling lives inside `generationConfig`. It also supports native
//! JSON output via `responseMimeType`, which we use when a schema is set.

use async_trait::async_trait;
use ca_core::error::{CaError, Result};
use ca_core::provider::{Caps, ChatRequest, ChatResponse, LlmProvider, Role};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub caps: Caps,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl GeminiConfig {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        let caps = pricing_for(&model);
        Self {
            api_key: api_key.into(),
            model,
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            caps,
            timeout: Duration::from_secs(180),
            max_retries: 3,
        }
    }
}

fn pricing_for(model: &str) -> Caps {
    // Gemini accepts audio directly, which makes it the one provider that can
    // transcribe and analyse in a single call — worth advertising in caps.
    let base = Caps {
        json_mode: true,
        native_schema: true,
        audio_input: true,
        embeddings: false,
        context_tokens: 1_000_000,
        input_cost_per_mtok: None,
        output_cost_per_mtok: None,
    };
    let (input, output) = match model {
        m if m.contains("flash-lite") => (0.10, 0.40),
        m if m.contains("flash") => (0.30, 2.50),
        m if m.contains("pro") => (1.25, 10.0),
        _ => return base,
    };
    Caps {
        input_cost_per_mtok: Some(input),
        output_cost_per_mtok: Some(output),
        ..base
    }
}

pub struct GeminiProvider {
    cfg: GeminiConfig,
    http: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(cfg: GeminiConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| CaError::Config(format!("http client: {e}")))?;
        Ok(Self { cfg, http })
    }
}

#[derive(Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct Content<'a> {
    role: &'a str,
    parts: Vec<Part<'a>>,
}

#[derive(Serialize)]
struct SystemInstruction<'a> {
    parts: Vec<Part<'a>>,
}

#[derive(Serialize)]
struct GenerationConfig {
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxOutputTokens")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "responseMimeType")]
    response_mime_type: Option<String>,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    contents: Vec<Content<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<CandidateContent>,
}

#[derive(Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<ResponsePart>,
}

#[derive(Deserialize)]
struct ResponsePart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: u32,
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    fn model(&self) -> &str {
        &self.cfg.model
    }

    fn capabilities(&self) -> Caps {
        self.cfg.caps.clone()
    }

    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse> {
        let url = format!(
            "{}/models/{}:generateContent",
            self.cfg.base_url.trim_end_matches('/'),
            self.cfg.model
        );

        let system_text: String = req
            .messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        let contents: Vec<Content> = req
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| Content {
                // Gemini calls the assistant turn `model`, not `assistant`.
                role: match m.role {
                    Role::Assistant => "model",
                    _ => "user",
                },
                parts: vec![Part { text: &m.content }],
            })
            .collect();

        if contents.is_empty() {
            return Err(CaError::Config(
                "gemini requires at least one non-system message".into(),
            ));
        }

        let body = WireRequest {
            contents,
            system_instruction: if system_text.is_empty() {
                None
            } else {
                Some(SystemInstruction {
                    parts: vec![Part { text: &system_text }],
                })
            },
            generation_config: GenerationConfig {
                temperature: req.temperature,
                max_output_tokens: req.max_tokens,
                response_mime_type: req
                    .json_schema
                    .as_ref()
                    .map(|_| "application/json".to_string()),
            },
        };

        let mut attempt = 0u32;
        loop {
            let started = Instant::now();
            let sent = self
                .http
                .post(&url)
                .header("x-goog-api-key", &self.cfg.api_key)
                .json(&body)
                .send()
                .await;

            let outcome: Result<ChatResponse> = match sent {
                Err(e) => Err(CaError::provider("gemini", crate::describe(&e))),
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        let parsed: WireResponse = r.json().await.map_err(|e| {
                            CaError::InvalidModelOutput(format!("malformed response body: {e}"))
                        })?;
                        let text: String = parsed
                            .candidates
                            .into_iter()
                            .next()
                            .and_then(|c| c.content)
                            .map(|c| {
                                c.parts
                                    .into_iter()
                                    .filter_map(|p| p.text)
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();

                        if text.trim().is_empty() {
                            // Gemini returns 200 with no candidate when a
                            // safety filter blocks the request.
                            Err(CaError::InvalidModelOutput(
                                "empty completion (possibly blocked by a safety filter)".into(),
                            ))
                        } else {
                            let u = parsed.usage_metadata.unwrap_or_default();
                            return Ok(ChatResponse {
                                content: text,
                                input_tokens: u.prompt_token_count,
                                output_tokens: u.candidates_token_count,
                                model: self.cfg.model.clone(),
                                latency_ms: started.elapsed().as_millis() as u64,
                            });
                        }
                    } else if status.as_u16() == 429 {
                        Err(CaError::RateLimited {
                            provider: "gemini".into(),
                            retry_after: None,
                        })
                    } else if status.is_server_error() {
                        let msg = r.text().await.unwrap_or_default();
                        Err(CaError::provider("gemini", format!("{status}: {msg}")))
                    } else {
                        let msg = r.text().await.unwrap_or_default();
                        Err(CaError::ProviderResponse {
                            provider: "gemini".into(),
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
                    tokio::time::sleep(Duration::from_secs(1u64 << (attempt - 1).min(6))).await;
                    tracing::warn!(provider = "gemini", attempt, error = %e, "retrying");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_advertises_audio_input() {
        // This is what lets the pipeline offer "transcribe and analyse in one call".
        assert!(pricing_for("gemini-2.5-flash").audio_input);
    }

    #[test]
    fn flash_lite_is_cheaper_than_pro() {
        let lite = pricing_for("gemini-2.5-flash-lite")
            .cost(1_000_000, 0)
            .unwrap();
        let pro = pricing_for("gemini-2.5-pro").cost(1_000_000, 0).unwrap();
        assert!(lite < pro, "lite {lite} should be cheaper than pro {pro}");
    }

    #[test]
    fn flash_lite_matches_before_flash() {
        // "flash-lite" contains "flash", so ordering of the match arms matters.
        let lite = pricing_for("gemini-2.5-flash-lite");
        assert_eq!(lite.input_cost_per_mtok, Some(0.10));
    }
}
