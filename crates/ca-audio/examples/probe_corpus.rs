//! Decode real corpus files end to end and report what came out.
//! Usage: cargo run -p ca-audio --release --example probe_corpus -- <dir> [count]

use ca_audio::{process_file, ProcessOptions};
use std::path::PathBuf;
use std::io::Write;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".".to_string());
    let count: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    matches!(
                        e.to_ascii_lowercase().as_str(),
                        "m4a" | "mp3" | "wav" | "flac" | "ogg" | "opus" | "mp4" | "webm"
                    )
                })
                .unwrap_or(false)
        })
        .collect();
    files.sort();

    // Spread the sample across the corpus rather than taking the first N,
    // which would all be from one caller and one recording setup.
    let stride = (files.len() / count.max(1)).max(1);
    let picked: Vec<_> = files.iter().step_by(stride).take(count).collect();

    println!("corpus: {} files, probing {}\n", files.len(), picked.len());

    let opts = ProcessOptions::default();
    let mut ok = 0usize;
    let mut total_audio = 0.0f64;
    let started = Instant::now();

    for path in picked {
        let name: String = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .chars()
            .take(46)
            .collect();
        let t = Instant::now();
        match process_file(path, &opts) {
            Ok(p) => {
                ok += 1;
                total_audio += p.meta.duration_secs;
                println!("OK  {name}");
                println!(
                    "    {:.1}s  {} -> {} Hz  {}ch  {}",
                    p.meta.duration_secs,
                    p.meta.source_sample_rate,
                    p.audio.sample_rate,
                    p.meta.source_channels,
                    p.meta.codec
                );
                println!(
                    "    loudness {:.1} LUFS  gain {:+.1} dB  speech {:.0}%  pauses {}",
                    p.meta.input_lufs.unwrap_or(f64::NAN),
                    p.meta.gain_db.unwrap_or(0.0),
                    (1.0 - p.features.silence_ratio.unwrap_or(0.0)) * 100.0,
                    p.pauses.len()
                );
                println!(
                    "    pitch {:.0} Hz (sd {:.0})  rms {:.1} dB  longest pause {:.1}s",
                    p.features.mean_pitch_hz.unwrap_or(f32::NAN),
                    p.features.pitch_stddev_hz.unwrap_or(f32::NAN),
                    p.features.mean_rms_db.unwrap_or(f32::NAN),
                    p.features.longest_pause_secs.unwrap_or(0.0),
                );
                println!(
                    "    waveform levels {} | decoded in {:.2}s ({:.0}x realtime)\n",
                    p.peaks.levels.len(),
                    t.elapsed().as_secs_f64(),
                    p.meta.duration_secs / t.elapsed().as_secs_f64().max(1e-9),
                );
            }
            Err(e) => println!("ERR {name}\n    {e}\n"),
        }
    }

    let wall = started.elapsed().as_secs_f64();
    println!("--- {ok} ok / {} audio-seconds in {wall:.1}s ({:.0}x realtime) ---",
        total_audio as u64, total_audio / wall.max(1e-9));
    Ok(())
}
