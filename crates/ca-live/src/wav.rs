//! Incremental stereo WAV, and a reader for playing one back.
//!
//! A live call cannot be held in memory. At 16 kHz `f32` that is ~230 MB per
//! hour per channel, so audio is written as it arrives and the process keeps
//! only what it is currently transcribing.
//!
//! One stereo file rather than two mono ones: the rest of the product assumes
//! one media file per call, and left/right preserves the channel split that
//! makes speaker labels exact. Left is the microphone (the agent), right is the
//! system loopback (the customer).
//!
//! The two channels arrive independently — separate devices, separate callbacks
//! — so the writer buffers each and emits interleaved frames only up to the
//! shorter side, padding the remainder with silence when the call ends. Writing
//! whatever arrived first would drift the two sides apart by however much one
//! device happened to be ahead.

use ca_core::error::{CaError, Result};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::Channel;

const HEADER_BYTES: u32 = 44;
const CHANNELS: u16 = 2;
const BITS: u16 = 16;

pub struct StereoWavWriter {
    out: BufWriter<File>,
    sample_rate: u32,
    /// Interleaved sample frames written so far, for the header.
    frames_written: u32,
    agent: Vec<f32>,
    customer: Vec<f32>,
}

impl StereoWavWriter {
    pub fn create(path: &Path, sample_rate: u32) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| CaError::Storage(format!("create {}: {e}", dir.display())))?;
        }
        let file = File::create(path)
            .map_err(|e| CaError::Storage(format!("create {}: {e}", path.display())))?;
        let mut w = Self {
            out: BufWriter::new(file),
            sample_rate,
            frames_written: 0,
            agent: Vec::new(),
            customer: Vec::new(),
        };
        // Sizes are patched on finish; a crash mid-call leaves a header
        // claiming zero length, which every decoder treats as an empty file
        // rather than as corruption.
        w.write_header(0)?;
        Ok(w)
    }

    pub fn push(&mut self, channel: Channel, samples: &[f32]) -> Result<()> {
        match channel {
            Channel::Agent => self.agent.extend_from_slice(samples),
            Channel::Customer => self.customer.extend_from_slice(samples),
        }
        self.drain()
    }

    /// Interleave and write everything both channels can cover.
    fn drain(&mut self) -> Result<()> {
        let n = self.agent.len().min(self.customer.len());
        if n == 0 {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(n * 4);
        for i in 0..n {
            bytes.extend_from_slice(&to_i16(self.agent[i]).to_le_bytes());
            bytes.extend_from_slice(&to_i16(self.customer[i]).to_le_bytes());
        }
        self.out
            .write_all(&bytes)
            .map_err(|e| CaError::Storage(format!("write wav: {e}")))?;
        self.agent.drain(..n);
        self.customer.drain(..n);
        self.frames_written += n as u32;
        Ok(())
    }

    /// Flush the tail, pad the shorter channel, and patch the header.
    pub fn finish(mut self) -> Result<()> {
        let pad = self.agent.len().max(self.customer.len());
        self.agent.resize(pad, 0.0);
        self.customer.resize(pad, 0.0);
        self.drain()?;
        self.out
            .flush()
            .map_err(|e| CaError::Storage(format!("flush wav: {e}")))?;
        let frames = self.frames_written;
        let file = self
            .out
            .into_inner()
            .map_err(|e| CaError::Storage(format!("finish wav: {e}")))?;
        let mut file = file;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| CaError::Storage(format!("seek wav: {e}")))?;
        write_header_to(&mut file, self.sample_rate, frames)?;
        file.flush()
            .map_err(|e| CaError::Storage(format!("flush wav header: {e}")))?;
        Ok(())
    }

    fn write_header(&mut self, frames: u32) -> Result<()> {
        let sr = self.sample_rate;
        write_header_to(&mut self.out, sr, frames)
    }
}

fn write_header_to<W: Write>(w: &mut W, sample_rate: u32, frames: u32) -> Result<()> {
    let block_align = CHANNELS * BITS / 8;
    let data_len = frames * block_align as u32;
    let byte_rate = sample_rate * block_align as u32;

    let mut h = Vec::with_capacity(HEADER_BYTES as usize);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&CHANNELS.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&BITS.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());

    w.write_all(&h)
        .map_err(|e| CaError::Storage(format!("write wav header: {e}")))
}

fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// A mono WAV of the samples, in memory — what the transcriber posts.
pub fn mono_wav_bytes(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let block_align: u16 = 2;
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&to_i16(*s).to_le_bytes());
    }
    out
}

/// A decoded 16-bit PCM WAV, split by channel.
pub struct WavData {
    pub sample_rate: u32,
    /// One vector per channel, each already `f32` in −1..1.
    pub channels: Vec<Vec<f32>>,
}

/// Read a 16-bit PCM WAV, keeping the channels apart.
///
/// Deliberately not `ca_audio::decode_file`: that downmixes to mono, which is
/// exactly the information the replay source needs to keep. The format is
/// narrow on purpose — this only ever reads files this module wrote.
pub fn read_pcm16(path: &Path) -> Result<WavData> {
    let mut buf = Vec::new();
    File::open(path)
        .and_then(|mut f| f.read_to_end(&mut buf))
        .map_err(|e| CaError::Storage(format!("read {}: {e}", path.display())))?;

    if buf.len() < 44 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return Err(CaError::Storage(format!(
            "{} is not a RIFF/WAVE file",
            path.display()
        )));
    }

    let u16at = |o: usize| u16::from_le_bytes([buf[o], buf[o + 1]]);
    let u32at = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);

    // Walk the chunk list rather than assuming a 44-byte header: some writers
    // insert LIST or fact chunks before the data.
    let (mut channels, mut sample_rate, mut bits) = (0u16, 0u32, 0u16);
    let (mut data_at, mut data_len) = (0usize, 0usize);
    let mut pos = 12usize;
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let len = u32at(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= buf.len() {
            channels = u16at(body + 2);
            sample_rate = u32at(body + 4);
            bits = u16at(body + 14);
        } else if id == b"data" {
            data_at = body;
            data_len = len.min(buf.len().saturating_sub(body));
            break;
        }
        pos = body + len + (len % 2);
    }

    if bits != 16 || channels == 0 || sample_rate == 0 {
        return Err(CaError::Storage(format!(
            "{} is not 16-bit PCM ({bits} bits, {channels} channels)",
            path.display()
        )));
    }

    let n = channels as usize;
    let frames = data_len / (2 * n);
    let mut out = vec![Vec::with_capacity(frames); n];
    for f in 0..frames {
        for (c, ch) in out.iter_mut().enumerate() {
            let o = data_at + (f * n + c) * 2;
            let v = i16::from_le_bytes([buf[o], buf[o + 1]]);
            ch.push(v as f32 / i16::MAX as f32);
        }
    }
    Ok(WavData {
        sample_rate,
        channels: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ca-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn a_written_file_reads_back_with_both_channels_intact() {
        let path = tmp("both.wav");
        let mut w = StereoWavWriter::create(&path, 16_000).unwrap();
        w.push(Channel::Agent, &[1.0, 1.0, 1.0]).unwrap();
        w.push(Channel::Customer, &[-1.0, -1.0, -1.0]).unwrap();
        w.finish().unwrap();

        let data = read_pcm16(&path).unwrap();
        assert_eq!(data.sample_rate, 16_000);
        assert_eq!(data.channels.len(), 2);
        assert!(data.channels[0][0] > 0.9, "agent channel lost");
        assert!(data.channels[1][0] < -0.9, "customer channel lost");
    }

    #[test]
    fn channels_stay_aligned_when_one_device_runs_ahead() {
        // Separate devices deliver at different times. Writing whatever arrived
        // first would slide the two sides of the conversation apart.
        let path = tmp("skew.wav");
        let mut w = StereoWavWriter::create(&path, 16_000).unwrap();
        w.push(Channel::Agent, &[1.0; 100]).unwrap();
        w.push(Channel::Customer, &[-1.0; 10]).unwrap();
        w.push(Channel::Customer, &[-1.0; 90]).unwrap();
        w.finish().unwrap();

        let data = read_pcm16(&path).unwrap();
        assert_eq!(data.channels[0].len(), 100);
        assert_eq!(data.channels[1].len(), 100);
        assert!(data.channels[1].iter().all(|s| *s < -0.9), "sides skewed");
    }

    #[test]
    fn a_channel_that_never_produced_audio_is_padded_with_silence() {
        // An unplugged microphone must still yield a playable call rather than
        // a file the decoder rejects.
        let path = tmp("onesided.wav");
        let mut w = StereoWavWriter::create(&path, 16_000).unwrap();
        w.push(Channel::Customer, &[0.5; 64]).unwrap();
        w.finish().unwrap();

        let data = read_pcm16(&path).unwrap();
        assert_eq!(data.channels[0].len(), 64);
        assert!(data.channels[0].iter().all(|s| *s == 0.0));
        assert!(data.channels[1].iter().all(|s| *s > 0.4));
    }

    #[test]
    fn an_empty_session_still_writes_a_valid_file() {
        let path = tmp("empty.wav");
        StereoWavWriter::create(&path, 16_000)
            .unwrap()
            .finish()
            .unwrap();
        let data = read_pcm16(&path).unwrap();
        assert!(data.channels[0].is_empty());
    }

    #[test]
    fn the_header_reports_the_real_length_after_finish() {
        let path = tmp("len.wav");
        let mut w = StereoWavWriter::create(&path, 16_000).unwrap();
        w.push(Channel::Agent, &[0.25; 800]).unwrap();
        w.push(Channel::Customer, &[0.25; 800]).unwrap();
        w.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let data_len = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]);
        assert_eq!(
            data_len,
            800 * 4,
            "header still claims the placeholder size"
        );
        assert_eq!(bytes.len() as u32, 44 + data_len);
    }

    #[test]
    fn a_mono_wav_for_the_transcriber_round_trips() {
        let path = tmp("mono.wav");
        std::fs::write(&path, mono_wav_bytes(&[0.5, -0.5], 16_000)).unwrap();
        let data = read_pcm16(&path).unwrap();
        assert_eq!(data.channels.len(), 1);
        assert!((data.channels[0][0] - 0.5).abs() < 0.01);
    }

    #[test]
    fn a_file_that_is_not_a_wav_is_an_error_rather_than_a_panic() {
        let path = tmp("notawav.bin");
        std::fs::write(&path, b"this is not audio").unwrap();
        assert!(read_pcm16(&path).is_err());
    }
}
