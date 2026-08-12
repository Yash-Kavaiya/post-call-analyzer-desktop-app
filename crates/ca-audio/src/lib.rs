//! Audio ingest and pre-processing.
//!
//! One entry point, [`process_file`], takes any supported media file and
//! produces everything the rest of the pipeline needs: normalised mono audio
//! at the ASR sample rate, acoustic features, waveform render data, and the
//! metadata to persist alongside the call.
//!
//! Decoding is pure Rust (symphonia, MPL-2.0) rather than an ffmpeg
//! subprocess — a GPL ffmpeg build would force GPL on the whole product, and
//! shelling out per file adds process overhead across a 1,968-call batch.

pub mod decode;
pub mod denoise;
pub mod features;
pub mod normalize;
pub mod peaks;
pub mod resample;
pub mod vad;

use ca_core::audio::{AcousticFeatures, AudioBuffer, AudioMeta, PauseRegion, WaveformPeaks};
use ca_core::audio::TARGET_SAMPLE_RATE;
use ca_core::error::Result;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ProcessOptions {
    /// Run RNNoise suppression. Off by default: it costs a 48 kHz round trip,
    /// and on clean recordings it can remove speech detail that helps ASR.
    pub denoise: bool,
    pub normalize: bool,
    /// Gap length that counts as a pause rather than an inter-word break.
    pub min_pause_secs: f64,
    pub target_sample_rate: u32,
}

impl Default for ProcessOptions {
    fn default() -> Self {
        Self {
            denoise: false,
            normalize: true,
            min_pause_secs: 0.35,
            target_sample_rate: TARGET_SAMPLE_RATE,
        }
    }
}

pub struct ProcessedAudio {
    pub audio: AudioBuffer,
    pub meta: AudioMeta,
    pub features: AcousticFeatures,
    pub peaks: WaveformPeaks,
    pub pauses: Vec<PauseRegion>,
    /// Total speaking time; the denominator for words per minute.
    pub speech_secs: f64,
}

/// Decode, condition, and analyse one media file.
pub fn process_file(path: &Path, opts: &ProcessOptions) -> Result<ProcessedAudio> {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let decoded = decode::decode_file(path)?;
    let source_rate = decoded.sample_rate;
    let source_channels = decoded.channels;
    let codec = decoded.codec.clone();

    let mut samples = decoded.samples;

    // RNNoise is defined at 48 kHz, so denoising happens there and the result
    // is then brought down to the ASR rate in a single conversion.
    if opts.denoise {
        let at48 = resample::resample(&samples, source_rate, 48_000);
        let cleaned = denoise::denoise_48k(&at48);
        samples = resample::resample(&cleaned, 48_000, opts.target_sample_rate);
    } else {
        samples = resample::resample(&samples, source_rate, opts.target_sample_rate);
    }

    let loudness = if opts.normalize {
        normalize::normalize_in_place(&mut samples, opts.target_sample_rate)
    } else {
        normalize::LoudnessResult {
            input_lufs: normalize::measure_lufs(&samples, opts.target_sample_rate),
            gain_db: None,
        }
    };

    let duration_secs = samples.len() as f64 / opts.target_sample_rate as f64;

    let vad_result = vad::detect(&samples, opts.target_sample_rate, opts.min_pause_secs);
    let features = features::extract(&samples, opts.target_sample_rate, &vad_result);
    let peaks = peaks::build(&samples, opts.target_sample_rate);

    Ok(ProcessedAudio {
        audio: AudioBuffer {
            samples,
            sample_rate: opts.target_sample_rate,
        },
        meta: AudioMeta {
            duration_secs,
            source_sample_rate: source_rate,
            source_channels,
            codec,
            bytes,
            input_lufs: loudness.input_lufs,
            gain_db: loudness.gain_db,
            denoised: opts.denoise,
        },
        features,
        peaks,
        pauses: vad_result.pauses.clone(),
        speech_secs: vad_result.speech_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_normalise_but_do_not_denoise() {
        let o = ProcessOptions::default();
        assert!(o.normalize);
        assert!(!o.denoise);
        assert_eq!(o.target_sample_rate, TARGET_SAMPLE_RATE);
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        let r = process_file(Path::new("does-not-exist.m4a"), &ProcessOptions::default());
        assert!(r.is_err());
    }
}
