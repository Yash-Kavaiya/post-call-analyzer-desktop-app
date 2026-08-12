//! Local transcription via the whisper.cpp CLI.
//!
//! whisper.cpp is driven as a subprocess rather than linked in. That avoids a
//! libclang/bindgen build dependency, keeps the binary MIT-licensed and
//! separately updatable, and costs nothing measurable: process startup is
//! milliseconds against a transcription that runs for seconds to minutes.

use crate::wav;
use async_trait::async_trait;
use ca_core::audio::AudioBuffer;
use ca_core::error::{CaError, Result};
use ca_core::ids::SegmentId;
use ca_core::provider::{TranscribeOptions, Transcriber};
use ca_core::transcript::{Segment, Speaker, SpeakerRole, Transcript, Word};
use ca_core::CallId;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct WhisperCliConfig {
    pub binary: PathBuf,
    pub model: PathBuf,
    pub threads: usize,
    /// `None` means auto-detect. Only meaningful on a multilingual model —
    /// an English-only model ignores it and transcribes everything as English.
    pub language: Option<String>,
    pub beam_size: u32,
}

impl WhisperCliConfig {
    pub fn new(binary: impl Into<PathBuf>, model: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            model: model.into(),
            // whisper.cpp scales poorly past ~8 threads; leaving headroom also
            // lets the batch runner keep several files in flight at once,
            // which beats one file using every core.
            threads: 8,
            language: None,
            beam_size: 5,
        }
    }

    pub fn with_threads(mut self, n: usize) -> Self {
        self.threads = n.max(1);
        self
    }

    pub fn with_language(mut self, lang: Option<String>) -> Self {
        self.language = lang;
        self
    }

    /// True when the model file is one of the English-only variants, which
    /// cannot transcribe the Hindi and code-switched calls in scope.
    pub fn is_english_only(&self) -> bool {
        self.model
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains(".en"))
            .unwrap_or(false)
    }
}

pub struct WhisperCliTranscriber {
    cfg: WhisperCliConfig,
}

impl WhisperCliTranscriber {
    pub fn new(cfg: WhisperCliConfig) -> Result<Self> {
        if !cfg.binary.exists() {
            return Err(CaError::Config(format!(
                "whisper binary not found at {}",
                cfg.binary.display()
            )));
        }
        if !cfg.model.exists() {
            return Err(CaError::Config(format!(
                "whisper model not found at {}",
                cfg.model.display()
            )));
        }
        if cfg.is_english_only() {
            tracing::warn!(
                model = %cfg.model.display(),
                "English-only model selected; non-English calls will transcribe poorly"
            );
        }
        Ok(Self { cfg })
    }

    pub fn config(&self) -> &WhisperCliConfig {
        &self.cfg
    }
}

// ---- whisper.cpp `-ojf` JSON ----

#[derive(Deserialize)]
struct WhisperJson {
    #[serde(default)]
    transcription: Vec<JsonSegment>,
    #[serde(default)]
    result: Option<JsonResult>,
}

#[derive(Deserialize)]
struct JsonResult {
    #[serde(default)]
    language: Option<String>,
}

#[derive(Deserialize)]
struct JsonSegment {
    offsets: Offsets,
    #[serde(default)]
    text: String,
    #[serde(default)]
    tokens: Vec<JsonToken>,
}

#[derive(Deserialize)]
struct JsonToken {
    #[serde(default)]
    text: String,
    offsets: Offsets,
    #[serde(default)]
    p: f32,
}

#[derive(Deserialize)]
struct Offsets {
    from: i64,
    to: i64,
}

/// Merge whisper's sub-word tokens into words.
///
/// whisper marks a word boundary with a leading space, so a token without one
/// is a continuation and punctuation attaches to the word it follows. Emitting
/// raw tokens instead would give the agents citation anchors that land
/// mid-word and read as nonsense when quoted back to a user.
fn tokens_to_words(tokens: &[JsonToken]) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();

    for t in tokens {
        // Special markers such as [_BEG_] and [_TT_xx] carry no text.
        if t.text.starts_with("[_") {
            continue;
        }
        let raw = &t.text;
        if raw.trim().is_empty() {
            continue;
        }

        let starts_word = raw.starts_with(' ');
        let piece = raw.trim();

        match words.last_mut() {
            Some(w) if !starts_word => {
                w.text.push_str(piece);
                w.end = t.offsets.to as f64 / 1000.0;
                // Keep the weakest token's confidence: a word is only as
                // trustworthy as its least certain piece.
                w.confidence = Some(w.confidence.unwrap_or(1.0).min(t.p));
            }
            _ => words.push(Word {
                text: piece.to_string(),
                start: t.offsets.from as f64 / 1000.0,
                end: t.offsets.to as f64 / 1000.0,
                confidence: Some(t.p),
            }),
        }
    }

    words
}

#[async_trait]
impl Transcriber for WhisperCliTranscriber {
    fn name(&self) -> &str {
        "whisper-cli"
    }

    fn model(&self) -> &str {
        self.cfg
            .model
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("whisper")
    }

    fn provides_diarization(&self) -> bool {
        false
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer,
        opts: &TranscribeOptions,
    ) -> Result<Transcript> {
        if audio.is_empty() {
            return Err(CaError::Config("cannot transcribe empty audio".into()));
        }

        // Unique stem so concurrent files in a batch cannot collide.
        let stem = format!("ca-asr-{}", uuid_like());
        let dir = std::env::temp_dir();
        let wav_path = dir.join(format!("{stem}.wav"));
        let out_stem = dir.join(&stem);

        wav::write_mono_16bit(&wav_path, &audio.samples, audio.sample_rate)?;

        let result = self.run(&wav_path, &out_stem, opts).await;

        // Clean up regardless of outcome; a failed batch must not leave
        // gigabytes of WAV files behind.
        let _ = std::fs::remove_file(&wav_path);
        let json_path = dir.join(format!("{stem}.json"));
        let parsed = result.and_then(|_| self.parse(&json_path, audio));
        let _ = std::fs::remove_file(&json_path);

        parsed
    }
}

impl WhisperCliTranscriber {
    async fn run(&self, wav_path: &Path, out_stem: &Path, opts: &TranscribeOptions) -> Result<()> {
        // Never ask an English-only model to detect language: the detection
        // pass runs anyway, costs real time, and can only ever answer "en".
        let lang = if self.cfg.is_english_only() {
            "en".to_string()
        } else {
            opts.language
                .clone()
                .or_else(|| self.cfg.language.clone())
                .unwrap_or_else(|| "auto".to_string())
        };

        let mut cmd = tokio::process::Command::new(&self.cfg.binary);
        cmd.arg("-m")
            .arg(&self.cfg.model)
            .arg("-f")
            .arg(wav_path)
            .arg("-of")
            .arg(out_stem)
            .arg("-ojf") // token-level JSON: needed for word timestamps
            .arg("-t")
            .arg(self.cfg.threads.to_string())
            .arg("-bs")
            .arg(self.cfg.beam_size.to_string())
            .arg("-l")
            .arg(lang)
            // Suppress non-speech tokens; they otherwise appear as bracketed
            // sound effects like "(clears throat)" inside the transcript.
            .arg("-sns")
            // Do not carry decoded text between windows. Carrying it is what
            // drives whisper's repetition loops — one bad window seeds the
            // next and the model emits the same phrase for the rest of the
            // call. Call audio (hold music, crosstalk, silence) triggers this
            // often enough that the small accuracy cost is worth paying.
            .arg("-mc")
            .arg("0");

        let output = cmd
            .output()
            .await
            .map_err(|e| CaError::provider("whisper-cli", format!("failed to spawn: {e}")))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(CaError::provider(
                "whisper-cli",
                format!(
                    "exited with {}: {}",
                    output.status,
                    err.chars().take(400).collect::<String>()
                ),
            ));
        }
        Ok(())
    }

    fn parse(&self, json_path: &Path, audio: &AudioBuffer) -> Result<Transcript> {
        let raw = std::fs::read_to_string(json_path).map_err(|e| {
            CaError::provider("whisper-cli", format!("no JSON output produced: {e}"))
        })?;
        let parsed: WhisperJson = serde_json::from_str(&raw)
            .map_err(|e| CaError::InvalidModelOutput(format!("whisper JSON: {e}")))?;

        let language = parsed.result.and_then(|r| r.language);

        let segments: Vec<Segment> = parsed
            .transcription
            .into_iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| {
                let words = tokens_to_words(&s.tokens);
                let confidence = if words.is_empty() {
                    None
                } else {
                    Some(
                        words.iter().filter_map(|w| w.confidence).sum::<f32>() / words.len() as f32,
                    )
                };
                Segment {
                    id: SegmentId::new(),
                    // Diarization runs separately and rewrites these.
                    speaker_index: 0,
                    role: SpeakerRole::Unknown,
                    start: s.offsets.from as f64 / 1000.0,
                    end: s.offsets.to as f64 / 1000.0,
                    text: s.text.trim().to_string(),
                    redacted_text: None,
                    words,
                    confidence,
                    language: language.clone(),
                }
            })
            .collect();

        let speakers = if segments.is_empty() {
            vec![]
        } else {
            vec![Speaker {
                index: 0,
                role: SpeakerRole::Unknown,
                role_confidence: None,
                total_speech_secs: segments.iter().map(|s| s.duration()).sum(),
                turn_count: segments.len() as u32,
            }]
        };

        Ok(Transcript {
            call_id: CallId::new(),
            segments,
            speakers,
            primary_language: language.clone(),
            languages: language.into_iter().collect(),
            engine: "whisper-cli".to_string(),
            model: self.model().to_string(),
            duration_secs: audio.duration_secs(),
        })
    }
}

/// Small unique-ish suffix; avoids pulling `uuid` into this crate just to name
/// a temp file.
fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str, from: i64, to: i64, p: f32) -> JsonToken {
        JsonToken {
            text: text.to_string(),
            offsets: Offsets { from, to },
            p,
        }
    }

    #[test]
    fn special_markers_are_dropped() {
        let words = tokens_to_words(&[tok("[_BEG_]", 0, 0, 1.0), tok(" Hi", 100, 200, 0.9)]);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "Hi");
    }

    #[test]
    fn leading_space_starts_a_new_word() {
        let words = tokens_to_words(&[tok(" Hello", 0, 300, 0.99), tok(" there", 300, 600, 0.95)]);
        assert_eq!(
            words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            vec!["Hello", "there"]
        );
    }

    #[test]
    fn continuation_tokens_merge_into_one_word() {
        // whisper splits rare words across tokens; they must rejoin or the
        // agents get citation anchors that land mid-word.
        let words = tokens_to_words(&[tok(" refund", 0, 200, 0.9), tok("able", 200, 350, 0.8)]);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "refundable");
        assert_eq!(words[0].start, 0.0);
        assert!((words[0].end - 0.35).abs() < 1e-9);
    }

    #[test]
    fn punctuation_attaches_to_the_preceding_word() {
        let words = tokens_to_words(&[tok(" Hello", 0, 300, 0.99), tok("?", 300, 400, 0.5)]);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "Hello?");
    }

    #[test]
    fn merged_word_keeps_the_weakest_confidence() {
        let words = tokens_to_words(&[tok(" Hello", 0, 300, 0.99), tok("?", 300, 400, 0.5)]);
        assert!((words[0].confidence.unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn offsets_convert_from_milliseconds_to_seconds() {
        let words = tokens_to_words(&[tok(" word", 1500, 2250, 1.0)]);
        assert!((words[0].start - 1.5).abs() < 1e-9);
        assert!((words[0].end - 2.25).abs() < 1e-9);
    }

    #[test]
    fn english_only_models_are_recognised_by_filename() {
        assert!(WhisperCliConfig::new("w.exe", "ggml-small.en.bin").is_english_only());
        assert!(!WhisperCliConfig::new("w.exe", "ggml-large-v3-turbo.bin").is_english_only());
    }

    #[test]
    fn missing_binary_is_reported_at_construction() {
        let cfg = WhisperCliConfig::new("definitely-not-here.exe", "nope.bin");
        assert!(WhisperCliTranscriber::new(cfg).is_err());
    }

    #[test]
    fn temp_stems_are_unique_across_calls() {
        assert_ne!(uuid_like(), uuid_like());
    }
}
