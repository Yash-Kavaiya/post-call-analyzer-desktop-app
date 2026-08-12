use serde::{Deserialize, Serialize};

/// The canonical internal audio format. Every decoder normalises to this so
/// downstream stages (ASR, diarization, features) never branch on source format.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Broadcast loudness target. -16 LUFS is the common spoken-word/podcast target
/// and keeps quiet callers audible without clipping loud ones.
pub const TARGET_LUFS: f64 = -16.0;

/// Decoded, normalised, mono audio ready for analysis.
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl AudioBuffer {
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / self.sample_rate as f64
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Samples covering `[start, end)` in seconds, clamped to the buffer.
    pub fn slice_secs(&self, start: f64, end: f64) -> &[f32] {
        let sr = self.sample_rate as f64;
        let s = ((start.max(0.0)) * sr) as usize;
        let e = ((end.max(0.0)) * sr) as usize;
        let s = s.min(self.samples.len());
        let e = e.clamp(s, self.samples.len());
        &self.samples[s..e]
    }
}

/// What we learned about the source file, persisted alongside the call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioMeta {
    pub duration_secs: f64,
    pub source_sample_rate: u32,
    pub source_channels: u16,
    pub codec: String,
    pub bytes: u64,
    /// Integrated loudness measured *before* normalisation.
    pub input_lufs: Option<f64>,
    /// Gain applied to reach `TARGET_LUFS`.
    pub gain_db: Option<f64>,
    pub denoised: bool,
}

/// Acoustic features extracted per call. These feed the dashboard and give the
/// analysis agents non-lexical signal the transcript alone cannot carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcousticFeatures {
    #[serde(alias = "mean_pitch_hz")]
    pub mean_pitch_hz: Option<f32>,
    #[serde(alias = "pitch_stddev_hz")]
    pub pitch_stddev_hz: Option<f32>,
    /// Cycle-to-cycle pitch perturbation; elevated jitter correlates with stress.
    #[serde(alias = "jitter_pct")]
    pub jitter_pct: Option<f32>,
    #[serde(alias = "mean_rms_db")]
    pub mean_rms_db: Option<f32>,
    /// Estimated words per minute across the whole call.
    #[serde(alias = "speech_rate_wpm")]
    pub speech_rate_wpm: Option<f32>,
    /// Fraction of the call with no speech from anyone.
    #[serde(alias = "silence_ratio")]
    pub silence_ratio: Option<f32>,
    /// Fraction of the call where two or more speakers overlap.
    #[serde(alias = "crosstalk_ratio")]
    pub crosstalk_ratio: Option<f32>,
    #[serde(alias = "longest_pause_secs")]
    pub longest_pause_secs: Option<f32>,
}

/// Precomputed render data for the waveform timeline.
///
/// The UI never decodes audio itself: Rust produces this once and the frontend
/// draws straight from it, which is what keeps scrubbing smooth on long calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformPeaks {
    /// Buckets per zoom level, coarsest first.
    pub levels: Vec<PeakLevel>,
    #[serde(alias = "duration_secs")]
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeakLevel {
    #[serde(alias = "bucket_secs")]
    pub bucket_secs: f64,
    /// Interleaved (min, max) pairs in -1.0..=1.0.
    #[serde(alias = "min_max")]
    pub min_max: Vec<f32>,
    /// Normalised 0..=1 spectral energy per bucket, for the heat overlay.
    pub energy: Vec<f32>,
}

/// A stretch of the call with no speech, surfaced on the timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseRegion {
    pub start: f64,
    pub end: f64,
}

/// A stretch where speakers talk over each other.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrosstalkRegion {
    pub start: f64,
    pub end: f64,
    pub speakers: Vec<String>,
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    /// Every key the UI reads must be camelCase.
    ///
    /// This exists because it was not, and the failure was invisible: the
    /// waveform renderer read `minMax`, the backend sent `min_max`, and the
    /// component drew nothing at all rather than erroring. Nothing in the type
    /// system connects a Rust field name to the TypeScript that consumes it, so
    /// the connection is asserted here instead.
    fn keys(v: &serde_json::Value) -> Vec<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn assert_camel(v: &serde_json::Value, what: &str) {
        for k in keys(v) {
            assert!(
                !k.contains('_'),
                "{what} serialises `{k}`, which no camelCase consumer will read"
            );
        }
    }

    #[test]
    fn waveform_peaks_cross_the_wire_as_camel_case() {
        let peaks = WaveformPeaks {
            levels: vec![PeakLevel {
                bucket_secs: 1.0,
                min_max: vec![-0.5, 0.5],
                energy: vec![0.4],
            }],
            duration_secs: 12.0,
        };
        let v = serde_json::to_value(&peaks).unwrap();
        assert_camel(&v, "WaveformPeaks");
        assert!(v.get("durationSecs").is_some());
        assert_camel(&v["levels"][0], "PeakLevel");
        assert!(v["levels"][0].get("minMax").is_some(), "the waveform bug");
        assert!(v["levels"][0].get("bucketSecs").is_some());
    }

    #[test]
    fn acoustic_features_cross_the_wire_as_camel_case() {
        let v = serde_json::to_value(AcousticFeatures::default()).unwrap();
        assert_camel(&v, "AcousticFeatures");
        assert!(v.get("meanPitchHz").is_some());
        assert!(v.get("speechRateWpm").is_some());
    }

    #[test]
    fn peaks_already_written_in_snake_case_still_load() {
        // Existing databases hold the old spelling. Rejecting it would erase
        // every waveform already computed, which is a worse bug than the one
        // being fixed.
        let old = r#"{"levels":[{"bucket_secs":0.5,"min_max":[-1.0,1.0],"energy":[0.2]}],
                      "duration_secs":30.0}"#;
        let peaks: WaveformPeaks = serde_json::from_str(old).unwrap();
        assert_eq!(peaks.duration_secs, 30.0);
        assert_eq!(peaks.levels[0].bucket_secs, 0.5);
        assert_eq!(peaks.levels[0].min_max.len(), 2);
    }

    #[test]
    fn features_already_written_in_snake_case_still_load() {
        let old = r#"{"mean_pitch_hz":178.4,"speech_rate_wpm":147.0}"#;
        let f: AcousticFeatures = serde_json::from_str(old).unwrap();
        assert_eq!(f.mean_pitch_hz, Some(178.4));
        assert_eq!(f.speech_rate_wpm, Some(147.0));
    }
}
