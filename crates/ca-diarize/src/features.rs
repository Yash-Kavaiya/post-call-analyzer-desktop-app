//! Per-window voice characteristics used to tell speakers apart.
//!
//! The feature set is deliberately small and interpretable: pitch separates
//! most male/female pairs, spectral centroid and rolloff capture timbre
//! differences that pitch alone misses, and level helps when one party is on
//! a markedly worse line. Each dimension is standardised before clustering so
//! that hertz-scale pitch does not dominate the unit-scale spectral measures.

use ca_audio::features::estimate_f0;
use ca_audio::vad::VadResult;
use realfft::RealFftPlanner;

#[derive(Debug, Clone)]
pub struct SegmentFeatures {
    pub start: f64,
    pub end: f64,
    pub mean_pitch: f64,
    pub spectral_centroid: f64,
    pub spectral_rolloff: f64,
    pub mean_rms_db: f64,
}

impl SegmentFeatures {
    pub fn vector(&self) -> Vec<f64> {
        vec![
            self.mean_pitch,
            self.spectral_centroid,
            self.spectral_rolloff,
            self.mean_rms_db,
        ]
    }
}

/// Slice speech regions into fixed windows and describe each one.
pub fn extract_windows(
    samples: &[f32],
    sample_rate: u32,
    vad: &VadResult,
    window_secs: f64,
) -> Vec<SegmentFeatures> {
    let mut out = Vec::new();
    let win = (window_secs * sample_rate as f64) as usize;
    if win == 0 {
        return out;
    }

    let mut planner = RealFftPlanner::<f32>::new();
    let fft_size = 1024usize;
    let fft = planner.plan_fft_forward(fft_size);
    let mut scratch = fft.make_scratch_vec();
    let mut spectrum = fft.make_output_vec();

    for region in &vad.speech {
        let s0 = (region.start * sample_rate as f64) as usize;
        let e0 = ((region.end * sample_rate as f64) as usize).min(samples.len());

        let mut pos = s0;
        while pos + win <= e0 {
            let frame = &samples[pos..pos + win];
            let (centroid, rolloff) = spectral_shape(
                frame,
                sample_rate,
                fft_size,
                &fft,
                &mut spectrum,
                &mut scratch,
            );

            let energy: f64 = frame.iter().map(|x| (*x as f64) * (*x as f64)).sum();
            let rms = (energy / frame.len() as f64).sqrt();

            out.push(SegmentFeatures {
                start: pos as f64 / sample_rate as f64,
                end: (pos + win) as f64 / sample_rate as f64,
                mean_pitch: mean_pitch(frame, sample_rate),
                spectral_centroid: centroid,
                spectral_rolloff: rolloff,
                mean_rms_db: 20.0 * rms.max(1e-10).log10(),
            });
            pos += win;
        }
    }

    out
}

/// Spectral centroid (brightness) and 85% rolloff (where the energy runs out).
fn spectral_shape(
    frame: &[f32],
    sample_rate: u32,
    fft_size: usize,
    fft: &std::sync::Arc<dyn realfft::RealToComplex<f32>>,
    spectrum: &mut [realfft::num_complex::Complex<f32>],
    scratch: &mut [realfft::num_complex::Complex<f32>],
) -> (f64, f64) {
    let mut buf = vec![0.0f32; fft_size];
    let take = frame.len().min(fft_size);
    buf[..take].copy_from_slice(&frame[..take]);
    for (i, v) in buf.iter_mut().enumerate().take(take) {
        let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (fft_size as f32 - 1.0)).cos();
        *v *= w;
    }

    if fft
        .process_with_scratch(&mut buf, spectrum, scratch)
        .is_err()
    {
        return (0.0, 0.0);
    }

    let bin_hz = sample_rate as f64 / fft_size as f64;
    let mags: Vec<f64> = spectrum.iter().map(|c| c.norm() as f64).collect();
    let total: f64 = mags.iter().sum();
    if total < 1e-12 {
        return (0.0, 0.0);
    }

    let centroid: f64 = mags
        .iter()
        .enumerate()
        .map(|(i, m)| i as f64 * bin_hz * m)
        .sum::<f64>()
        / total;

    let mut acc = 0.0;
    let mut rolloff = 0.0;
    for (i, m) in mags.iter().enumerate() {
        acc += m;
        if acc >= total * 0.85 {
            rolloff = i as f64 * bin_hz;
            break;
        }
    }

    (centroid, rolloff)
}

/// Mean pitch over the window, delegating to `ca-audio`'s estimator so the
/// octave-error handling lives in exactly one place.
fn mean_pitch(frame: &[f32], sample_rate: u32) -> f64 {
    let sub = ((sample_rate as f64) * 0.040) as usize;
    if sub == 0 || frame.len() < sub {
        return 0.0;
    }
    let mut vals = Vec::new();
    let mut i = 0;
    // Step two sub-frames at a time: pitch is stable enough that sampling half
    // the windows halves the cost with no practical loss.
    while i + sub <= frame.len() {
        if let Some(f0) = estimate_f0(&frame[i..i + sub], sample_rate) {
            vals.push(f0);
        }
        i += sub * 2;
    }
    if vals.is_empty() {
        0.0
    } else {
        vals.iter().sum::<f64>() / vals.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn tone(freq: f64, secs: f64) -> Vec<f32> {
        let n = (SR as f64 * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / SR as f64;
                (0.4 * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn pitch_tracks_the_input_frequency() {
        let low = mean_pitch(&tone(120.0, 1.0), SR);
        let high = mean_pitch(&tone(260.0, 1.0), SR);
        assert!((low - 120.0).abs() < 15.0, "low={low}");
        assert!((high - 260.0).abs() < 20.0, "high={high}");
    }

    #[test]
    fn windows_are_produced_over_speech_regions() {
        let samples = tone(150.0, 6.0);
        let vad = ca_audio::vad::detect(&samples, SR, 0.35);
        let w = extract_windows(&samples, SR, &vad, 1.5);
        assert!(w.len() >= 3, "expected several windows, got {}", w.len());
        for f in &w {
            assert!(f.end > f.start);
        }
    }

    #[test]
    fn feature_vector_has_a_stable_width() {
        let samples = tone(150.0, 4.0);
        let vad = ca_audio::vad::detect(&samples, SR, 0.35);
        let w = extract_windows(&samples, SR, &vad, 1.5);
        assert!(w.iter().all(|f| f.vector().len() == 4));
    }

    #[test]
    fn brighter_signal_has_a_higher_centroid() {
        let samples_low = tone(200.0, 3.0);
        let samples_high = tone(3000.0, 3.0);
        let vad_l = ca_audio::vad::detect(&samples_low, SR, 0.35);
        let vad_h = ca_audio::vad::detect(&samples_high, SR, 0.35);
        let l = extract_windows(&samples_low, SR, &vad_l, 1.5);
        let h = extract_windows(&samples_high, SR, &vad_h, 1.5);
        assert!(
            h[0].spectral_centroid > l[0].spectral_centroid,
            "{} vs {}",
            h[0].spectral_centroid,
            l[0].spectral_centroid
        );
    }
}
