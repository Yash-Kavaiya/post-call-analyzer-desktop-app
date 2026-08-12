use crate::audio::AudioBuffer;
use crate::error::Result;
use crate::transcript::{Segment, Transcript};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- LLM

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(c: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: c.into(),
        }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: c.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
    /// When set, the provider must return JSON matching this schema. Providers
    /// without native schema support fall back to prompt-level instruction plus
    /// local validation, so callers can always rely on parsing the result.
    pub json_schema: Option<serde_json::Value>,
}

impl ChatRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            temperature: 0.2,
            max_tokens: None,
            json_schema: None,
        }
    }

    pub fn with_schema(mut self, schema: serde_json::Value) -> Self {
        self.json_schema = Some(schema);
        self
    }
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub model: String,
    pub latency_ms: u64,
}

/// What a provider can do. The pipeline consults this instead of hard-coding
/// vendor names, so an unknown OpenAI-compatible endpoint degrades gracefully.
#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub json_mode: bool,
    pub native_schema: bool,
    pub audio_input: bool,
    pub embeddings: bool,
    pub context_tokens: u32,
    pub input_cost_per_mtok: Option<f64>,
    pub output_cost_per_mtok: Option<f64>,
}

impl Caps {
    pub fn cost(&self, input_tokens: u32, output_tokens: u32) -> Option<f64> {
        let (i, o) = (self.input_cost_per_mtok?, self.output_cost_per_mtok?);
        Some((input_tokens as f64 * i + output_tokens as f64 * o) / 1_000_000.0)
    }
}

/// One trait for every LLM vendor.
///
/// Most vendors — OpenAI, OpenRouter, NVIDIA NIM, Ollama, LM Studio, Groq and
/// any BYOK endpoint — are served by a single `OpenAiCompatible` implementation
/// parameterised by base URL and auth. Only Anthropic and Gemini need their own.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn capabilities(&self) -> Caps;

    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse>;

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(crate::error::CaError::Config(format!(
            "provider `{}` does not support embeddings",
            self.name()
        )))
    }
}

// ---------------------------------------------------------------- ASR

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    /// BCP-47 hint, or `None` to auto-detect. Auto-detect matters here: these
    /// calls code-switch between English and Indian languages mid-sentence.
    pub language: Option<String>,
    pub diarize: bool,
    pub word_timestamps: bool,
    pub num_speakers_hint: Option<u8>,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self {
            language: None,
            diarize: false,
            word_timestamps: true,
            num_speakers_hint: Some(2),
        }
    }
}

#[async_trait]
pub trait Transcriber: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    /// True when the engine returns speaker labels itself, letting the pipeline
    /// skip the separate diarization stage.
    fn provides_diarization(&self) -> bool {
        false
    }
    async fn transcribe(&self, audio: &AudioBuffer, opts: &TranscribeOptions)
        -> Result<Transcript>;
}

// ---------------------------------------------------------------- Diarization

/// A speaker-homogeneous span produced by diarization, before it is merged
/// with ASR words to form `Segment`s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub speaker_index: u8,
    pub start: f64,
    pub end: f64,
}

#[async_trait]
pub trait Diarizer: Send + Sync {
    fn name(&self) -> &str;
    async fn diarize(
        &self,
        audio: &AudioBuffer,
        num_speakers: Option<u8>,
    ) -> Result<Vec<SpeakerTurn>>;
}

// ---------------------------------------------------------------- Ingestion

/// One item discovered by a `Source`, not yet fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceItem {
    /// Stable identity for dedup across re-scans — path, S3 key, or dataset row.
    pub key: String,
    pub display_name: String,
    pub size_bytes: Option<u64>,
    /// Already-transcribed input skips ASR entirely.
    pub kind: SourceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Audio,
    Transcript,
}

/// Where calls come from: local folder, object storage, HuggingFace, or
/// pre-existing transcript files. The pipeline treats them identically.
#[async_trait]
pub trait Source: Send + Sync {
    fn name(&self) -> &str;
    async fn list(&self) -> Result<Vec<SourceItem>>;
    /// Materialise an item locally and return the path to it.
    async fn fetch(&self, item: &SourceItem) -> Result<std::path::PathBuf>;
}

// ---------------------------------------------------------------- Agents

/// Context handed to every analysis agent.
pub struct AgentContext<'a> {
    pub transcript: &'a Transcript,
    pub features: &'a crate::audio::AcousticFeatures,
    pub meta: &'a crate::audio::AudioMeta,
}

impl<'a> AgentContext<'a> {
    /// Segment ids an agent is allowed to cite. Anything outside this set is a
    /// fabrication and is rejected during validation.
    pub fn valid_segments(&self) -> impl Iterator<Item = &Segment> {
        self.transcript.segments.iter()
    }
}
