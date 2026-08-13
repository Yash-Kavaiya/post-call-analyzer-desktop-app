//! Transcribing one chunk at a time against a warm whisper server.
//!
//! The model stays loaded between chunks, which is the whole reason this is a
//! server rather than repeated CLI invocations. Measured on the development
//! machine: `small.en` cold through the CLI runs at 1.3× realtime because most
//! of that is loading 487 MB; the same model warm answers a ten-second chunk in
//! 3.8 s, and `base.en` warm answers it in 1.2 s.
//!
//! `base.en` is the live model for that reason, with the authoritative pass at
//! call end still using `small.en`. Reducing the audio context instead was
//! measured and rejected: three times faster, but on dense speech it dropped a
//! clause and looped, and a live tier that feeds compliance detection cannot
//! silently lose words.

use ca_core::error::{CaError, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Transcribes one closed chunk of audio.
#[async_trait::async_trait]
pub trait ChunkTranscriber: Send + Sync {
    async fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String>;
    /// What answered, for the accounting strip.
    fn model(&self) -> String;
}

/// A `whisper-server` child process.
///
/// Killed on drop, so a crashed or closed app never leaves a server holding a
/// model resident. That is 141 MB for `base.en` and it would otherwise survive
/// until the machine restarted.
pub struct ServerHandle {
    child: Child,
    port: u16,
}

impl ServerHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Block until the server answers, or give up.
    ///
    /// Loading the model takes seconds, and a request sent before it is ready
    /// fails in a way that looks like a broken transcriber rather than a slow
    /// start.
    pub async fn wait_until_ready(&self, timeout: Duration) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/", self.port);
        let client = reqwest::Client::new();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if client
                .get(&url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        Err(CaError::Config(format!(
            "whisper-server did not become ready on port {} within {:?}",
            self.port, timeout
        )))
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Client for a running `whisper-server`.
pub struct WhisperServer {
    base_url: String,
    model_name: String,
    client: reqwest::Client,
}

impl WhisperServer {
    /// Start a server and return a handle that owns the process.
    pub fn spawn(binary: &Path, model: &Path, port: u16, threads: usize) -> Result<ServerHandle> {
        if !binary.exists() {
            return Err(CaError::Config(format!(
                "whisper-server not found at {}",
                binary.display()
            )));
        }
        if !model.exists() {
            return Err(CaError::Config(format!(
                "live model not found at {} — fetch it with `pwsh scripts/fetch-models.ps1 -Model base.en`",
                model.display()
            )));
        }
        let child = Command::new(binary)
            .arg("-m")
            .arg(model)
            .arg("-t")
            .arg(threads.max(1).to_string())
            .arg("--port")
            .arg(port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| CaError::Config(format!("spawn whisper-server: {e}")))?;
        Ok(ServerHandle { child, port })
    }

    pub fn client(port: u16, model_name: impl Into<String>) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            model_name: model_name.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Where the live model is expected to live, beside the authoritative one.
    pub fn default_model_path(models_dir: &Path) -> PathBuf {
        models_dir.join("ggml-base.en.bin")
    }
}

#[async_trait::async_trait]
impl ChunkTranscriber for WhisperServer {
    async fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        let part =
            reqwest::multipart::Part::bytes(crate::wav::mono_wav_bytes(samples, sample_rate))
                .file_name("chunk.wav")
                .mime_str("audio/wav")
                .map_err(|e| CaError::Config(format!("build chunk part: {e}")))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "json")
            // Greedy. A live chunk is re-read seconds later by the
            // authoritative pass, so sampling variety buys nothing here.
            .text("temperature", "0.0");

        let resp = self
            .client
            .post(format!("{}/inference", self.base_url))
            // Generous against a measured ~1.2 s: a chunk that takes longer
            // than this means the server is wedged, not slow.
            .timeout(Duration::from_secs(30))
            .multipart(form)
            .send()
            .await
            .map_err(|e| CaError::Config(format!("whisper-server request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(CaError::Config(format!(
                "whisper-server returned {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CaError::Config(format!("whisper-server sent invalid JSON: {e}")))?;
        Ok(body
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .trim()
            .to_string())
    }

    fn model(&self) -> String {
        self.model_name.clone()
    }
}

/// A transcriber that returns queued text, for tests and the mock backend.
///
/// Public rather than `cfg(test)` so the session's own tests — and any future
/// crate's — can drive the live loop without a model on disk.
pub struct StubTranscriber {
    queued: std::sync::Mutex<std::collections::VecDeque<String>>,
    fallback: String,
}

impl StubTranscriber {
    pub fn new(lines: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            queued: std::sync::Mutex::new(lines.into_iter().map(Into::into).collect()),
            fallback: String::new(),
        }
    }

    /// Text returned once the queue is empty, instead of silence.
    pub fn with_fallback(mut self, text: impl Into<String>) -> Self {
        self.fallback = text.into();
        self
    }
}

#[async_trait::async_trait]
impl ChunkTranscriber for StubTranscriber {
    async fn transcribe(&self, _samples: &[f32], _sample_rate: u32) -> Result<String> {
        let mut q = self
            .queued
            .lock()
            .map_err(|_| CaError::Config("stub transcriber poisoned".into()))?;
        Ok(q.pop_front().unwrap_or_else(|| self.fallback.clone()))
    }

    fn model(&self) -> String {
        "stub".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_stub_returns_queued_text_in_order() {
        let t = StubTranscriber::new(["first", "second"]);
        assert_eq!(t.transcribe(&[0.1], 16_000).await.unwrap(), "first");
        assert_eq!(t.transcribe(&[0.1], 16_000).await.unwrap(), "second");
        assert_eq!(t.transcribe(&[0.1], 16_000).await.unwrap(), "");
    }

    #[tokio::test]
    async fn the_stub_can_keep_answering_after_its_queue_empties() {
        let t = StubTranscriber::new(["only"]).with_fallback("more talking");
        assert_eq!(t.transcribe(&[0.1], 16_000).await.unwrap(), "only");
        assert_eq!(t.transcribe(&[0.1], 16_000).await.unwrap(), "more talking");
    }

    #[tokio::test]
    async fn an_empty_chunk_is_not_sent_anywhere() {
        // Guards the real client too: posting a zero-length wav wastes a round
        // trip and some servers answer with an error rather than empty text.
        let s = WhisperServer::client(59_999, "base.en");
        assert_eq!(s.transcribe(&[], 16_000).await.unwrap(), "");
    }

    #[tokio::test]
    async fn a_server_that_is_not_running_is_an_error_rather_than_a_hang() {
        let s = WhisperServer::client(59_999, "base.en");
        let err = s.transcribe(&[0.1; 1600], 16_000).await.unwrap_err();
        assert!(format!("{err}").contains("whisper-server"), "got {err}");
    }

    #[test]
    fn a_missing_binary_is_reported_before_anything_is_spawned() {
        let Err(err) = WhisperServer::spawn(
            Path::new("does/not/exist.exe"),
            Path::new("does/not/exist.bin"),
            59_998,
            4,
        ) else {
            panic!("spawned a server from a binary that does not exist");
        };
        assert!(format!("{err}").contains("whisper-server not found"));
    }

    #[test]
    fn a_missing_model_names_the_script_that_fetches_it() {
        // The model is a 141 MB download that is deliberately not in the repo,
        // so the error has to say how to get it.
        let dir = std::env::temp_dir();
        let binary = dir.join("fake-whisper-server.exe");
        std::fs::write(&binary, b"not really").unwrap();
        let Err(err) = WhisperServer::spawn(&binary, Path::new("does/not/exist.bin"), 59_997, 4)
        else {
            panic!("spawned a server with no model");
        };
        assert!(format!("{err}").contains("fetch-models"), "got {err}");
    }
}
