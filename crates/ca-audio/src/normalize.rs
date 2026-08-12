//! Loudness measurement and normalisation.
//!
//! Phone recordings vary enormously in level — a quiet caller on a bad line
//! can sit 20 dB below a loud one. ASR accuracy and the waveform display both
//! suffer from that spread, so every call is brought to a common broadcast
//! loudness target before analysis.

use ca_core::audio::TARGET_LUFS;

/// Measured loudness plus the gain that was applied to reach the target.
#[derive(Debug, Clone, Copy)]
pub struct LoudnessResult {
    pub input_lufs: Option<f64>,
    pub gain_db: Option<f64>,
}

/// Measure integrated loudness (EBU R128) without modifying the signal.
pub fn measure_lufs(samples: &[f32], sample_rate: u32) -> Option<f64> {
    if samples.is_empty() || sample_rate == 0 {
        return None;
    }
    let mut meter = ebur128::EbuR128::new(1, sample_rate, ebur128::Mode::I).ok()?;
    meter.add_frames_f32(samples).ok()?;
    let lufs = meter.loudness_global().ok()?;
    // Digital silence measures as -inf; there is nothing to normalise against.
    if lufs.is_finite() {
        Some(lufs)
    } else {
        None
    }
}

/// Normalise in place to [`TARGET_LUFS`].
///
/// Gain is capped at +20 dB so that a near-silent recording is not amplified
/// into pure noise, and the result is peak-limited to avoid clipping — a
/// clipped waveform both looks wrong in the UI and degrades ASR.
pub fn normalize_in_place(samples: &mut [f32], sample_rate: u32) -> LoudnessResult {
    let Some(input_lufs) = measure_lufs(samples, sample_rate) else {
        return LoudnessResult {
            input_lufs: None,
            gain_db: None,
        };
    };

    let gain_db = (TARGET_LUFS - input_lufs).clamp(-20.0, 20.0);
    let mut gain = 10f64.powf(gain_db / 20.0) as f32;

    // Peak-limit: never push the loudest sample past -1 dBFS.
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.0 {
        let ceiling = 0.891_25; // -1 dBFS
        if peak * gain > ceiling {
            gain = ceiling / peak;
        }
    }

    for s in samples.iter_mut() {
        *s = (*s * gain).clamp(-1.0, 1.0);
    }

    LoudnessResult {
        input_lufs: Some(input_lufs),
        gain_db: Some(20.0 * (gain as f64).log10()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32, rate: u32, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / rate as f64;
                (amp as f64 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn silence_reports_no_loudness() {
        let s = vec![0.0f32; 16_000];
        assert!(measure_lufs(&s, 16_000).is_none());
    }

    #[test]
    fn quiet_signal_is_amplified_toward_the_target() {
        let mut s = tone(0.02, 16_000, 3.0);
        let before = measure_lufs(&s, 16_000).unwrap();
        let r = normalize_in_place(&mut s, 16_000);
        let after = measure_lufs(&s, 16_000).unwrap();
        assert!(r.gain_db.unwrap() > 0.0, "expected positive gain");
        assert!(
            after > before,
            "loudness should increase: {before} -> {after}"
        );
    }

    #[test]
    fn loud_signal_is_attenuated_toward_the_target() {
        let mut s = tone(0.9, 16_000, 3.0);
        let before = measure_lufs(&s, 16_000).unwrap();
        let r = normalize_in_place(&mut s, 16_000);
        let after = measure_lufs(&s, 16_000).unwrap();
        assert!(r.gain_db.unwrap() < 0.0, "expected negative gain");
        assert!(
            after < before,
            "loudness should decrease: {before} -> {after}"
        );
    }

    #[test]
    fn output_never_clips() {
        let mut s = tone(0.99, 16_000, 2.0);
        normalize_in_place(&mut s, 16_000);
        let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak <= 1.0, "peak {peak} exceeds full scale");
    }

    #[test]
    fn near_silent_input_is_not_amplified_without_bound() {
        // Quiet enough to want ~+33 dB, but still above EBU R128's -70 LUFS
        // absolute gate so a loudness figure exists at all. The cap is what
        // stops line noise being amplified into a roar.
        let mut s = tone(0.005, 16_000, 3.0);
        let r = normalize_in_place(&mut s, 16_000);
        assert!(r.gain_db.unwrap() <= 20.0 + 1e-6, "gain {:?}", r.gain_db);
    }

    #[test]
    fn content_below_the_ebu_gate_reports_no_loudness() {
        // Below -70 LUFS the standard gates the content out entirely; there is
        // genuinely nothing to normalise against, so we must not invent a gain.
        let mut s = tone(0.0001, 16_000, 3.0);
        let r = normalize_in_place(&mut s, 16_000);
        assert!(r.input_lufs.is_none() && r.gain_db.is_none());
    }

    #[test]
    fn empty_input_is_handled() {
        let mut s: Vec<f32> = vec![];
        let r = normalize_in_place(&mut s, 16_000);
        assert!(r.input_lufs.is_none());
    }
}
