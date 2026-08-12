//! Background noise suppression (RNNoise).
//!
//! Off by default. On the call recordings in scope it helps a hissy line
//! noticeably, but on already-clean audio it can shave off consonant detail
//! that ASR relies on — so it stays a per-batch opt-in rather than something
//! applied blindly to 133 hours of audio.

/// RNNoise is defined for 48 kHz mono in 480-sample (10 ms) frames.
const FRAME: usize = nnnoiseless::DenoiseState::FRAME_SIZE;

/// Denoise a 48 kHz mono signal.
///
/// Input that is not a whole number of frames is zero-padded, then the output
/// is truncated back to the original length so callers see no length change.
pub fn denoise_48k(samples: &[f32]) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    let mut state = nnnoiseless::DenoiseState::new();
    let mut out = Vec::with_capacity(samples.len() + FRAME);
    let mut in_frame = [0.0f32; FRAME];
    let mut out_frame = [0.0f32; FRAME];

    for chunk in samples.chunks(FRAME) {
        in_frame[..chunk.len()].copy_from_slice(chunk);
        if chunk.len() < FRAME {
            in_frame[chunk.len()..].fill(0.0);
        }
        state.process_frame(&mut out_frame, &in_frame);
        out.extend_from_slice(&out_frame);
    }

    out.truncate(samples.len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_handled() {
        assert!(denoise_48k(&[]).is_empty());
    }

    #[test]
    fn output_length_matches_input_length() {
        // Deliberately not a frame multiple, to exercise the padding path.
        let s = vec![0.01f32; FRAME * 3 + 17];
        assert_eq!(denoise_48k(&s).len(), s.len());
    }

    #[test]
    fn input_shorter_than_one_frame_still_works() {
        let s = vec![0.01f32; 100];
        assert_eq!(denoise_48k(&s).len(), 100);
    }

    #[test]
    fn silence_stays_quiet() {
        let s = vec![0.0f32; FRAME * 4];
        let out = denoise_48k(&s);
        let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(
            peak < 1e-3,
            "denoiser introduced energy into silence: {peak}"
        );
    }
}
