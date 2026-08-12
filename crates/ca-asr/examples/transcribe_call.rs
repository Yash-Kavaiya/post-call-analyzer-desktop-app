//! Full local path on a real recording: decode -> condition -> transcribe.
//! Usage: cargo run -p ca-asr --release --example transcribe_call -- <file.m4a>

use ca_asr::{WhisperCliConfig, WhisperCliTranscriber};
use ca_audio::{process_file, ProcessOptions};
use ca_core::provider::{TranscribeOptions, Transcriber};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: transcribe_call <file>")?;
    let path = PathBuf::from(path);

    // Repo-relative so the example works from a clean checkout.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .ok_or("cannot locate workspace root")?;
    let binary = root.join("vendor/whisper/Release/whisper-cli.exe");
    let model = root.join("models/ggml-small.en.bin");

    println!("decoding {}", path.display());
    let t0 = Instant::now();
    let processed = process_file(&path, &ProcessOptions::default())?;
    println!(
        "  {:.1}s audio, {} Hz, {} -> normalised {:+.1} dB in {:.2}s",
        processed.meta.duration_secs,
        processed.audio.sample_rate,
        processed.meta.codec,
        processed.meta.gain_db.unwrap_or(0.0),
        t0.elapsed().as_secs_f64()
    );

    let cfg = WhisperCliConfig::new(binary, model).with_threads(16);
    if cfg.is_english_only() {
        println!("  NOTE: English-only model — non-English speech will be unreliable");
    }
    let asr = WhisperCliTranscriber::new(cfg)?;

    println!("transcribing with {} / {}", asr.name(), asr.model());
    let t1 = Instant::now();
    let transcript = asr
        .transcribe(&processed.audio, &TranscribeOptions::default())
        .await?;
    let elapsed = t1.elapsed().as_secs_f64();

    println!(
        "  {} segments, {} words in {:.1}s ({:.1}x realtime)\n",
        transcript.segments.len(),
        transcript.word_count(),
        elapsed,
        processed.meta.duration_secs / elapsed.max(1e-9)
    );

    for s in transcript.segments.iter().take(12) {
        println!(
            "  [{} -> {}] {}",
            ca_core::transcript::fmt_ts(s.start),
            ca_core::transcript::fmt_ts(s.end),
            s.text
        );
    }
    if transcript.segments.len() > 12 {
        println!("  ... {} more", transcript.segments.len() - 12);
    }

    // Word timings are what the analysis agents cite, so show a few.
    if let Some(first) = transcript.segments.first() {
        println!("\n  first words with timings:");
        for w in first.words.iter().take(8) {
            println!(
                "    {:>7.2}-{:<7.2} p={:.2}  {}",
                w.start,
                w.end,
                w.confidence.unwrap_or(0.0),
                w.text
            );
        }
    }

    Ok(())
}
