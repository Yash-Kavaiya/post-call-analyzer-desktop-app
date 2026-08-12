//! Sample-rate conversion.
//!
//! Written by hand rather than pulled from a crate because this sits directly
//! on the ASR quality path: a resampler that aliases produces transcription
//! errors that are very hard to trace back to their cause. A windowed-sinc
//! kernel is well understood, deterministic, and — importantly — testable
//! against a known signal, which is what the tests at the bottom do.

/// Half-width of the sinc kernel in output samples. 16 taps per side is well
/// past the point of audible difference for speech while staying cheap enough
/// to run over 133 hours of audio.
const HALF_TAPS: i64 = 16;

/// Resample `input` from `from_rate` to `to_rate`.
///
/// Handles both directions. When downsampling, the kernel is widened by the
/// rate ratio so it doubles as the anti-aliasing filter — skipping that step
/// is the classic way to fold high-frequency energy back into the speech band.
pub fn resample(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }

    let ratio = to_rate as f64 / from_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);

    // Downsampling: the filter cutoff must follow the *output* Nyquist, so the
    // kernel stretches in input-sample terms. Upsampling needs no such change.
    let filter_scale = if ratio < 1.0 { ratio } else { 1.0 };
    let half_width = (HALF_TAPS as f64 / filter_scale).ceil() as i64;

    // The kernel is precomputed once and sampled by table lookup. Evaluating
    // sinc and the window per tap instead costs three transcendental calls for
    // every one of ~100 taps on every output sample — around a billion sin/cos
    // calls per four-minute recording, which made a batch run untenable.
    let table = KernelTable::build(half_width, filter_scale);

    for n in 0..out_len {
        // Position in input-sample space that this output sample reads from.
        let center = n as f64 / ratio;
        let center_floor = center.floor() as i64;

        let k_start = center_floor - half_width;
        let k_end = center_floor + half_width;

        // Distance decreases by exactly 1.0 per tap, so the table index steps
        // by a constant — no per-tap arithmetic beyond an add.
        let mut acc = 0.0f64;
        let mut norm = 0.0f64;

        for k in k_start..=k_end {
            if k < 0 || k as usize >= input.len() {
                continue;
            }
            let w = table.weight(center - k as f64);
            acc += input[k as usize] as f64 * w;
            norm += w;
        }

        // Normalising by the realised window weight keeps gain flat at the
        // edges, where the kernel is truncated by the buffer bounds.
        out.push(if norm.abs() > 1e-12 {
            (acc / norm) as f32
        } else {
            0.0
        });
    }

    out
}

/// Windowed-sinc kernel sampled on a fine grid, read back with linear
/// interpolation. `OVERSAMPLE` sub-steps per input sample keeps interpolation
/// error far below the quantisation floor of 16-bit audio.
struct KernelTable {
    values: Vec<f64>,
    half_width: f64,
    oversample: f64,
}

const OVERSAMPLE: usize = 512;

impl KernelTable {
    fn build(half_width: i64, filter_scale: f64) -> Self {
        let hw = half_width as f64;
        let n = (half_width as usize) * 2 * OVERSAMPLE + 1;
        let mut values = Vec::with_capacity(n);
        for i in 0..n {
            let dist = i as f64 / OVERSAMPLE as f64 - hw;
            values.push(sinc(dist * filter_scale) * blackman(dist, hw));
        }
        Self {
            values,
            half_width: hw,
            oversample: OVERSAMPLE as f64,
        }
    }

    #[inline]
    fn weight(&self, dist: f64) -> f64 {
        let pos = (dist + self.half_width) * self.oversample;
        if pos < 0.0 {
            return 0.0;
        }
        let idx = pos as usize;
        if idx + 1 >= self.values.len() {
            return 0.0;
        }
        let frac = pos - idx as f64;
        self.values[idx] * (1.0 - frac) + self.values[idx + 1] * frac
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Blackman window over `[-half_width, half_width]`, zero outside.
fn blackman(dist: f64, half_width: f64) -> f64 {
    if dist.abs() > half_width {
        return 0.0;
    }
    let t = (dist + half_width) / (2.0 * half_width); // 0..=1
    let two_pi_t = 2.0 * std::f64::consts::PI * t;
    0.42 - 0.5 * two_pi_t.cos() + 0.08 * (2.0 * two_pi_t).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / rate as f64;
                (2.0 * std::f64::consts::PI * freq * t).sin() as f32
            })
            .collect()
    }

    /// Dominant frequency by zero-crossing rate — crude but dependency-free
    /// and plenty accurate for a pure tone.
    fn dominant_freq(signal: &[f32], rate: u32) -> f64 {
        let crossings = signal
            .windows(2)
            .filter(|w| (w[0] <= 0.0 && w[1] > 0.0))
            .count();
        crossings as f64 / (signal.len() as f64 / rate as f64)
    }

    #[test]
    fn identity_when_rates_match() {
        let s = sine(440.0, 16_000, 0.1);
        assert_eq!(resample(&s, 16_000, 16_000), s);
    }

    #[test]
    fn output_length_follows_the_rate_ratio() {
        let s = sine(440.0, 48_000, 1.0);
        let out = resample(&s, 48_000, 16_000);
        // 1 second in, 1 second out at the new rate (±1 sample of rounding).
        assert!(
            (out.len() as i64 - 16_000).abs() <= 1,
            "expected ~16000, got {}",
            out.len()
        );
    }

    #[test]
    fn downsampling_48k_to_16k_preserves_speech_band_tone() {
        // 1 kHz sits squarely in the speech band and well under the 8 kHz
        // output Nyquist, so it must survive the conversion intact.
        let s = sine(1000.0, 48_000, 0.5);
        let out = resample(&s, 48_000, 16_000);
        let f = dominant_freq(&out, 16_000);
        assert!((f - 1000.0).abs() < 20.0, "expected ~1000 Hz, got {f}");
    }

    #[test]
    fn upsampling_preserves_tone() {
        let s = sine(1000.0, 8_000, 0.5);
        let out = resample(&s, 8_000, 16_000);
        let f = dominant_freq(&out, 16_000);
        assert!((f - 1000.0).abs() < 20.0, "expected ~1000 Hz, got {f}");
    }

    #[test]
    fn above_nyquist_content_is_attenuated_not_aliased() {
        // 12 kHz cannot be represented at 16 kHz. Without an anti-aliasing
        // filter it would fold down to 4 kHz and land in the speech band —
        // exactly the failure this kernel exists to prevent.
        let s = sine(12_000.0, 48_000, 0.5);
        let out = resample(&s, 48_000, 16_000);
        let rms: f32 = (out.iter().map(|x| x * x).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms < 0.10, "aliased energy leaked through: rms={rms}");
    }

    #[test]
    fn empty_input_is_handled() {
        assert!(resample(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn gain_is_flat_across_the_buffer_including_edges() {
        // Constant signal must stay constant — catches edge-normalisation bugs.
        let s = vec![0.5f32; 4800];
        let out = resample(&s, 48_000, 16_000);
        for (i, v) in out.iter().enumerate() {
            assert!((v - 0.5).abs() < 0.02, "sample {i} drifted to {v}");
        }
    }
}
