//! Voice activity detection.
//!
//! Drives three visible things: the pause markers on the waveform timeline,
//! the silence/cross-talk ratios on the dashboard, and the speech-rate
//! denominator (words per minute must divide by *speaking* time, not by wall
//! clock, or every call with a hold period scores as unnaturally slow).
//!
//! The detector is energy-based with an adaptive floor rather than a fixed
//! threshold, because call recordings differ by tens of dB in noise level.

use ca_core::audio::PauseRegion;

const FRAME_MS: f64 = 20.0;

/// A contiguous run of speech, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeechRegion {
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone)]
pub struct VadResult {
    pub speech: Vec<SpeechRegion>,
    pub pauses: Vec<PauseRegion>,
    /// Fraction of the call with no speech, 0..=1.
    pub silence_ratio: f32,
    pub longest_pause_secs: f32,
    /// Total speaking time in seconds.
    pub speech_secs: f64,
}

/// Detect speech regions.
///
/// `min_pause_secs` sets how long a gap must be before it counts as a pause
/// rather than the natural break between words — 0.35 s is a reasonable
/// default for conversational speech.
pub fn detect(samples: &[f32], sample_rate: u32, min_pause_secs: f64) -> VadResult {
    if samples.is_empty() || sample_rate == 0 {
        return VadResult {
            speech: vec![],
            pauses: vec![],
            silence_ratio: 0.0,
            longest_pause_secs: 0.0,
            speech_secs: 0.0,
        };
    }

    let frame_len = ((sample_rate as f64) * FRAME_MS / 1000.0).max(1.0) as usize;
    let total_secs = samples.len() as f64 / sample_rate as f64;

    // Per-frame RMS in dBFS.
    let energies: Vec<f32> = samples
        .chunks(frame_len)
        .map(|c| {
            let sum: f32 = c.iter().map(|s| s * s).sum();
            let rms = (sum / c.len() as f32).sqrt();
            20.0 * (rms.max(1e-10)).log10()
        })
        .collect();

    // Adaptive threshold from the energy distribution's two modes: p10
    // approximates the noise bed, p90 the speech level.
    let mut sorted = energies.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)];
    let p10 = pct(0.10);
    let p90 = pct(0.90);

    // When the two modes are well separated the call really does contain both
    // speech and silence, so split between them. When they are not — a clip
    // that is entirely speech, or entirely silence — there is no contrast to
    // exploit and a relative threshold degenerates (every frame lands on the
    // wrong side of it). Fall back to an absolute gate in that case.
    const ABSOLUTE_GATE_DBFS: f32 = -55.0;
    const BIMODAL_SPREAD_DB: f32 = 10.0;
    let threshold = if p90 - p10 > BIMODAL_SPREAD_DB {
        p10 + (p90 - p10) * 0.4
    } else {
        ABSOLUTE_GATE_DBFS
    };

    let voiced: Vec<bool> = energies.iter().map(|e| *e > threshold).collect();

    // Merge voiced frames into regions, bridging gaps shorter than the pause
    // threshold so that inter-word gaps do not fragment a single utterance.
    let frame_secs = frame_len as f64 / sample_rate as f64;
    let bridge_frames = (min_pause_secs / frame_secs).ceil() as usize;

    let mut speech: Vec<SpeechRegion> = Vec::new();
    let mut i = 0usize;
    while i < voiced.len() {
        if !voiced[i] {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut j = i;
        let mut gap = 0usize;
        while j < voiced.len() {
            if voiced[j] {
                end = j;
                gap = 0;
            } else {
                gap += 1;
                if gap > bridge_frames {
                    break;
                }
            }
            j += 1;
        }
        speech.push(SpeechRegion {
            start: start as f64 * frame_secs,
            end: ((end + 1) as f64 * frame_secs).min(total_secs),
        });
        i = j;
    }

    // Pauses are the complement of speech, including head and tail silence.
    let mut pauses = Vec::new();
    let mut cursor = 0.0f64;
    for r in &speech {
        if r.start - cursor >= min_pause_secs {
            pauses.push(PauseRegion {
                start: cursor,
                end: r.start,
            });
        }
        cursor = r.end;
    }
    if total_secs - cursor >= min_pause_secs {
        pauses.push(PauseRegion {
            start: cursor,
            end: total_secs,
        });
    }

    let speech_secs: f64 = speech.iter().map(|r| r.end - r.start).sum();
    let longest_pause = pauses
        .iter()
        .map(|p| p.end - p.start)
        .fold(0.0f64, f64::max);

    VadResult {
        speech,
        pauses,
        silence_ratio: if total_secs > 0.0 {
            ((total_secs - speech_secs) / total_secs).clamp(0.0, 1.0) as f32
        } else {
            0.0
        },
        longest_pause_secs: longest_pause as f32,
        speech_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn tone(secs: f64, amp: f32) -> Vec<f32> {
        let n = (SR as f64 * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / SR as f64;
                (amp as f64 * (2.0 * std::f64::consts::PI * 300.0 * t).sin()) as f32
            })
            .collect()
    }

    fn quiet(secs: f64) -> Vec<f32> {
        vec![0.0f32; (SR as f64 * secs) as usize]
    }

    #[test]
    fn empty_input_is_handled() {
        let r = detect(&[], SR, 0.35);
        assert!(r.speech.is_empty() && r.pauses.is_empty());
    }

    #[test]
    fn finds_a_single_utterance_between_silences() {
        let mut s = quiet(1.0);
        s.extend(tone(2.0, 0.4));
        s.extend(quiet(1.0));

        let r = detect(&s, SR, 0.35);
        assert_eq!(r.speech.len(), 1, "regions: {:?}", r.speech);
        let reg = r.speech[0];
        assert!((reg.start - 1.0).abs() < 0.15, "start {}", reg.start);
        assert!((reg.end - 3.0).abs() < 0.15, "end {}", reg.end);
    }

    #[test]
    fn head_and_tail_silence_become_pauses() {
        let mut s = quiet(1.0);
        s.extend(tone(1.0, 0.4));
        s.extend(quiet(1.5));
        let r = detect(&s, SR, 0.35);
        assert_eq!(r.pauses.len(), 2, "pauses: {:?}", r.pauses);
        assert!(r.longest_pause_secs > 1.0);
    }

    #[test]
    fn short_gaps_do_not_split_one_utterance() {
        // 100 ms between words must not fragment the region.
        let mut s = tone(1.0, 0.4);
        s.extend(quiet(0.1));
        s.extend(tone(1.0, 0.4));
        let r = detect(&s, SR, 0.35);
        assert_eq!(r.speech.len(), 1, "regions: {:?}", r.speech);
    }

    #[test]
    fn long_gaps_do_split_utterances() {
        let mut s = tone(1.0, 0.4);
        s.extend(quiet(1.5));
        s.extend(tone(1.0, 0.4));
        let r = detect(&s, SR, 0.35);
        assert_eq!(r.speech.len(), 2, "regions: {:?}", r.speech);
    }

    #[test]
    fn silence_ratio_is_between_zero_and_one() {
        let mut s = quiet(2.0);
        s.extend(tone(2.0, 0.4));
        let r = detect(&s, SR, 0.35);
        assert!(
            (0.0..=1.0).contains(&r.silence_ratio),
            "{}",
            r.silence_ratio
        );
        assert!(r.silence_ratio > 0.3, "expected substantial silence");
    }

    #[test]
    fn quiet_speaker_over_noise_is_still_detected() {
        // Low-level speech riding on a noise bed — the adaptive floor is what
        // makes this work where a fixed dBFS threshold would report silence.
        let noise: Vec<f32> = (0..SR as usize * 4)
            .map(|i| ((i * 2654435761usize) as f32 / u32::MAX as f32 - 0.5) * 0.004)
            .collect();
        let mut s = noise.clone();
        for (i, v) in tone(2.0, 0.05).iter().enumerate() {
            s[SR as usize + i] += v;
        }
        let r = detect(&s, SR, 0.35);
        assert!(!r.speech.is_empty(), "quiet speech was missed entirely");
    }
}
