//! Acoustic feature extraction.
//!
//! These give the analysis agents non-lexical signal the transcript cannot
//! carry — a customer can say "that's fine" in a way no text model will read
//! as frustration, but pitch variance and speech rate show it.

use crate::vad::VadResult;
use ca_core::audio::AcousticFeatures;

/// Human speech fundamental frequency bounds. Anything outside is not pitch.
const MIN_F0_HZ: f64 = 60.0;
const MAX_F0_HZ: f64 = 400.0;

/// Extract features over the speech regions only.
///
/// Restricting to speech matters: averaging pitch and level across the silent
/// half of a call drags every statistic toward the noise floor.
pub fn extract(samples: &[f32], sample_rate: u32, vad: &VadResult) -> AcousticFeatures {
    if samples.is_empty() || sample_rate == 0 {
        return AcousticFeatures::default();
    }

    let mut f0s: Vec<f64> = Vec::new();
    let mut rms_acc = 0.0f64;
    let mut rms_frames = 0usize;

    // 40 ms windows: long enough to hold two periods of the lowest pitch we
    // accept, short enough that pitch is roughly stationary within a window.
    let win = ((sample_rate as f64) * 0.040) as usize;
    let base_hop = (win / 2).max(1);

    // Pitch statistics converge long before every frame has been analysed, and
    // autocorrelation is the most expensive thing in this crate. Spread a
    // bounded number of windows across the speech instead of walking all of
    // them — over 133 hours of audio the difference is hours, not seconds.
    const MAX_FRAMES: usize = 400;
    let total_speech_frames: usize = vad
        .speech
        .iter()
        .map(|r| {
            let n = ((r.end - r.start) * sample_rate as f64) as usize;
            n / base_hop.max(1)
        })
        .sum();
    let stride = (total_speech_frames / MAX_FRAMES).max(1);
    let hop = base_hop * stride;

    for region in &vad.speech {
        let s = (region.start * sample_rate as f64) as usize;
        let e = ((region.end * sample_rate as f64) as usize).min(samples.len());
        if e <= s || win == 0 {
            continue;
        }
        let mut i = s;
        while i + win <= e {
            let frame = &samples[i..i + win];

            let energy: f64 = frame.iter().map(|x| (*x as f64) * (*x as f64)).sum();
            let rms = (energy / frame.len() as f64).sqrt();
            if rms > 1e-6 {
                rms_acc += rms;
                rms_frames += 1;
                if let Some(f0) = estimate_f0(frame, sample_rate) {
                    f0s.push(f0);
                }
            }
            i += hop;
        }
    }

    let mean_pitch = mean(&f0s);
    let pitch_sd = stddev(&f0s, mean_pitch);

    // Jitter: mean absolute period-to-period variation as a percentage. A
    // standard voice-quality measure; elevated jitter tracks vocal stress.
    let jitter = if f0s.len() > 1 {
        let periods: Vec<f64> = f0s.iter().map(|f| 1.0 / f).collect();
        let diffs: f64 = periods.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        let mean_period = mean(&periods).unwrap_or(0.0);
        if mean_period > 0.0 {
            Some((diffs / (periods.len() - 1) as f64 / mean_period * 100.0) as f32)
        } else {
            None
        }
    } else {
        None
    };

    let mean_rms_db = if rms_frames > 0 {
        Some((20.0 * (rms_acc / rms_frames as f64).max(1e-10).log10()) as f32)
    } else {
        None
    };

    AcousticFeatures {
        mean_pitch_hz: mean_pitch.map(|v| v as f32),
        pitch_stddev_hz: pitch_sd.map(|v| v as f32),
        jitter_pct: jitter,
        mean_rms_db,
        // Needs the transcript's word count; filled in after ASR.
        speech_rate_wpm: None,
        silence_ratio: Some(vad.silence_ratio),
        crosstalk_ratio: None,
        longest_pause_secs: Some(vad.longest_pause_secs),
    }
}

/// Words per minute over *speaking* time.
///
/// Dividing by wall-clock duration would make any call containing a hold
/// period look pathologically slow, so the denominator is speech time.
pub fn speech_rate_wpm(word_count: usize, speech_secs: f64) -> Option<f32> {
    if speech_secs <= 0.5 || word_count == 0 {
        return None;
    }
    Some((word_count as f64 / (speech_secs / 60.0)) as f32)
}

/// Autocorrelation pitch estimate over one window.
///
/// Public so diarization can reuse it: a second copy of this drifts out of
/// sync with the octave-error handling below and silently misclassifies
/// speakers.
pub fn estimate_f0(frame: &[f32], sample_rate: u32) -> Option<f64> {
    let min_lag = (sample_rate as f64 / MAX_F0_HZ) as usize;
    let max_lag = (sample_rate as f64 / MIN_F0_HZ) as usize;
    if frame.len() <= max_lag || min_lag >= max_lag {
        return None;
    }

    let energy0: f64 = frame.iter().map(|x| (*x as f64) * (*x as f64)).sum();
    if energy0 < 1e-9 {
        return None;
    }

    // Normalised correlation coefficient in [-1, 1] for every candidate lag.
    // Dividing by the term count instead of the energy product would inflate
    // long lags and bias every estimate downward.
    let mut r = vec![0.0f64; max_lag + 1];
    for lag in min_lag..=max_lag {
        let n = frame.len() - lag;
        let (mut num, mut e1, mut e2) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            let a = frame[i] as f64;
            let b = frame[i + lag] as f64;
            num += a * b;
            e1 += a * a;
            e2 += b * b;
        }
        let denom = (e1 * e2).sqrt();
        r[lag] = if denom < 1e-12 { 0.0 } else { num / denom };
    }

    let best_corr = r[min_lag..=max_lag].iter().cloned().fold(0.0f64, f64::max);

    // Require clear periodicity; unvoiced frames (fricatives, line noise) have
    // no strong autocorrelation and would otherwise report nonsense pitch.
    const MIN_PERIODICITY: f64 = 0.5;
    if best_corr < MIN_PERIODICITY {
        return None;
    }

    // Subharmonic guard. Correlation peaks at *every* integer multiple of the
    // period, and when the true period falls between sample points a higher
    // multiple can land closer to the grid and score marginally better — a
    // 220 Hz voice then reads as 73 Hz. So take the shortest lag that is a
    // local maximum within tolerance of the global best, not the best itself.
    const SUBHARMONIC_TOLERANCE: f64 = 0.85;
    let cutoff = best_corr * SUBHARMONIC_TOLERANCE;
    let mut best_lag = 0usize;
    for lag in (min_lag + 1)..max_lag {
        if r[lag] >= cutoff && r[lag] > r[lag - 1] && r[lag] >= r[lag + 1] {
            best_lag = lag;
            break;
        }
    }
    if best_lag == 0 {
        return None;
    }

    // Parabolic interpolation across the peak recovers sub-sample precision,
    // which matters most at high pitch where lags are only a few dozen samples.
    let (y0, y1, y2) = (r[best_lag - 1], r[best_lag], r[best_lag + 1]);
    let denom = 2.0 * (2.0 * y1 - y0 - y2);
    let offset = if denom.abs() > 1e-12 {
        ((y2 - y0) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };

    Some(sample_rate as f64 / (best_lag as f64 + offset))
}

fn mean(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        None
    } else {
        Some(v.iter().sum::<f64>() / v.len() as f64)
    }
}

fn stddev(v: &[f64], mean: Option<f64>) -> Option<f64> {
    let m = mean?;
    if v.len() < 2 {
        return None;
    }
    let var = v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (v.len() - 1) as f64;
    Some(var.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad;

    const SR: u32 = 16_000;

    fn tone(freq: f64, secs: f64, amp: f32) -> Vec<f32> {
        let n = (SR as f64 * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / SR as f64;
                (amp as f64 * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn detects_pitch_of_a_known_tone() {
        // 150 Hz is a typical adult male fundamental.
        let s = tone(150.0, 2.0, 0.5);
        let v = vad::detect(&s, SR, 0.35);
        let f = extract(&s, SR, &v);
        let pitch = f.mean_pitch_hz.expect("no pitch detected");
        assert!((pitch - 150.0).abs() < 8.0, "expected ~150 Hz, got {pitch}");
    }

    #[test]
    fn detects_higher_pitch_too() {
        let s = tone(220.0, 2.0, 0.5);
        let v = vad::detect(&s, SR, 0.35);
        let pitch = extract(&s, SR, &v).mean_pitch_hz.expect("no pitch");
        assert!(
            (pitch - 220.0).abs() < 12.0,
            "expected ~220 Hz, got {pitch}"
        );
    }

    #[test]
    fn silence_yields_no_pitch() {
        let s = vec![0.0f32; SR as usize * 2];
        let v = vad::detect(&s, SR, 0.35);
        assert!(extract(&s, SR, &v).mean_pitch_hz.is_none());
    }

    #[test]
    fn steady_tone_has_low_pitch_variance() {
        let s = tone(150.0, 2.0, 0.5);
        let v = vad::detect(&s, SR, 0.35);
        let sd = extract(&s, SR, &v).pitch_stddev_hz.unwrap_or(0.0);
        assert!(sd < 15.0, "steady tone should not vary much, got sd={sd}");
    }

    #[test]
    fn speech_rate_uses_speaking_time_not_wall_clock() {
        // 150 words over 60 s of speech is 150 wpm regardless of hold time.
        assert_eq!(speech_rate_wpm(150, 60.0), Some(150.0));
    }

    #[test]
    fn speech_rate_is_none_for_degenerate_input() {
        assert!(speech_rate_wpm(0, 60.0).is_none());
        assert!(speech_rate_wpm(100, 0.1).is_none());
    }

    #[test]
    fn empty_input_yields_default_features() {
        let v = vad::detect(&[], SR, 0.35);
        let f = extract(&[], SR, &v);
        assert!(f.mean_pitch_hz.is_none() && f.mean_rms_db.is_none());
    }
}
