//! Minimal WAV writer.
//!
//! whisper.cpp accepts flac/mp3/ogg/wav but not m4a, so the conditioned audio
//! from `ca-audio` is handed over as 16-bit PCM. Writing it here rather than
//! re-decoding means whisper sees exactly the normalised signal the rest of
//! the pipeline analysed — same samples, same loudness, no second decode.

use ca_core::error::{CaError, Result};
use std::io::Write;
use std::path::Path;

/// Write mono f32 samples as a 16-bit PCM WAV file.
pub fn write_mono_16bit(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut buf: Vec<u8> = Vec::with_capacity(44 + data_len as usize);

    let byte_rate = sample_rate * 2; // mono * 16-bit
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // format: PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels: mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        // Clamp before scaling: a sample above full scale would otherwise wrap
        // to the opposite polarity and read as a loud click.
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }

    let mut f = std::fs::File::create(path).map_err(|e| CaError::AudioDecode {
        path: path.display().to_string(),
        source: Box::new(e),
    })?;
    f.write_all(&buf).map_err(|e| CaError::AudioDecode {
        path: path.display().to_string(),
        source: Box::new(e),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn writes_a_well_formed_header() {
        let p = tmp("ca_asr_wav_header.wav");
        write_mono_16bit(&p, &[0.0, 0.5, -0.5], 16_000).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(&b[36..40], b"data");
        // 44-byte header + 3 samples * 2 bytes
        assert_eq!(b.len(), 44 + 6);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn sample_rate_round_trips_into_the_header() {
        let p = tmp("ca_asr_wav_rate.wav");
        write_mono_16bit(&p, &[0.0; 8], 16_000).unwrap();
        let b = std::fs::read(&p).unwrap();
        let rate = u32::from_le_bytes([b[24], b[25], b[26], b[27]]);
        assert_eq!(rate, 16_000);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn out_of_range_samples_clamp_instead_of_wrapping() {
        // Without the clamp, +1.5 would wrap to a large negative value and
        // produce an audible click in the middle of speech.
        let p = tmp("ca_asr_wav_clamp.wav");
        write_mono_16bit(&p, &[1.5, -1.5], 16_000).unwrap();
        let b = std::fs::read(&p).unwrap();
        let a = i16::from_le_bytes([b[44], b[45]]);
        let c = i16::from_le_bytes([b[46], b[47]]);
        assert!(a > 32_000, "positive clamp failed: {a}");
        assert!(c < -32_000, "negative clamp failed: {c}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn empty_input_produces_a_header_only_file() {
        let p = tmp("ca_asr_wav_empty.wav");
        write_mono_16bit(&p, &[], 16_000).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 44);
        let _ = std::fs::remove_file(&p);
    }
}
