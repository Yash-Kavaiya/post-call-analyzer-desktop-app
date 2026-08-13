//! Two-stream capture on Windows.
//!
//! The agent is on the default input device and the customer is on the default
//! output device opened in **loopback** — cpal reaches that by handing an output
//! device to `build_input_stream`, which sets `AUDCLNT_STREAMFLAGS_LOOPBACK`
//! (RustAudio/cpal#339). Loopback captures whatever the machine is playing, so
//! this works with Teams, Zoom, a browser dialler or anything else, with no
//! integration and no vendor SDK.
//!
//! Keeping the two sides on separate devices is what makes speaker labels
//! exact: the question a diarizer spends effort guessing at is answered by which
//! stream the samples arrived on.
//!
//! This is the only file in the crate that needs a physical device, so it is
//! deliberately thin — open two streams, downmix, resample, push. Everything
//! worth testing happens downstream of [`crate::source::AudioSource`], which
//! [`crate::ReplaySource`] implements for exactly that reason.
//!
//! **Known limitation: echo.** Without a headset the microphone also picks up
//! the customer coming out of the speakers, so both channels transcribe the same
//! words. v1 assumes a headset and says so rather than pretending otherwise;
//! cancelling it properly is signal processing, not a flag.

use crate::source::{AudioSource, Frame};
use crate::{Channel, SAMPLE_RATE};
use ca_core::error::{CaError, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

/// Silence on a running stream for longer than this means the device stopped
/// delivering. cpal has a known failure where a WASAPI stream builds fine and
/// then never fires its callback (RustAudio/cpal#516), which is otherwise
/// indistinguishable from a very quiet call.
pub const WATCHDOG_SECS: u64 = 3;

/// Input and output device names, for the pickers.
pub fn list_devices() -> Result<(Vec<String>, Vec<String>)> {
    // Input and output iterators are different concrete types in cpal, so this
    // cannot be one shared helper without boxing for no benefit.
    //
    // A host with no devices returns an empty list rather than an error: a
    // machine with no microphone is a machine that cannot take a live call, and
    // the pickers should say so rather than refuse to open.
    let host = cpal::default_host();
    let inputs: Vec<String> = host
        .input_devices()
        .map(|d| d.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    let outputs: Vec<String> = host
        .output_devices()
        .map(|d| d.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    Ok((inputs, outputs))
}

fn find_input(host: &cpal::Host, wanted: Option<&str>) -> Result<cpal::Device> {
    match wanted {
        None => host
            .default_input_device()
            .ok_or_else(|| CaError::Config("no microphone is available".into())),
        Some(name) => host
            .input_devices()
            .map_err(|e| CaError::Config(format!("enumerate inputs: {e}")))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| CaError::Config(format!("input device `{name}` was not found"))),
    }
}

fn find_output(host: &cpal::Host, wanted: Option<&str>) -> Result<cpal::Device> {
    match wanted {
        None => host
            .default_output_device()
            .ok_or_else(|| CaError::Config("no playback device is available".into())),
        Some(name) => host
            .output_devices()
            .map_err(|e| CaError::Config(format!("enumerate outputs: {e}")))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| CaError::Config(format!("output device `{name}` was not found"))),
    }
}

/// Live capture from a microphone and the system mix.
pub struct CaptureSource {
    input: cpal::Device,
    output: cpal::Device,
    input_name: String,
    output_name: String,
    streams: Vec<cpal::Stream>,
    /// Milliseconds since the epoch of the last sample on each channel, so a
    /// watchdog can tell a silent call from a dead device.
    last_seen: [Arc<AtomicU64>; 2],
}

impl CaptureSource {
    /// `None` for either name takes the system default.
    pub fn new(input_name: Option<&str>, output_name: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let input = find_input(&host, input_name)?;
        let output = find_output(&host, output_name)?;
        Ok(Self {
            input_name: input.name().unwrap_or_else(|_| "microphone".into()),
            output_name: output.name().unwrap_or_else(|_| "system audio".into()),
            input,
            output,
            streams: Vec::new(),
            last_seen: [
                Arc::new(AtomicU64::new(now_millis())),
                Arc::new(AtomicU64::new(now_millis())),
            ],
        })
    }

    /// Seconds since this channel last delivered audio.
    pub fn silent_for(&self, channel: Channel) -> f64 {
        let last = self.last_seen[channel.index() as usize].load(Ordering::Relaxed);
        (now_millis().saturating_sub(last)) as f64 / 1000.0
    }

    fn build(
        device: &cpal::Device,
        channel: Channel,
        tx: UnboundedSender<Frame>,
        seen: Arc<AtomicU64>,
    ) -> Result<cpal::Stream> {
        // The output device is queried for an *input* config on purpose: that
        // is what puts cpal into loopback.
        let config = device
            .default_input_config()
            .map_err(|e| CaError::Config(format!("no input config for this device: {e}")))?;
        let src_rate = config.sample_rate().0;
        let src_channels = config.channels() as usize;
        let err_channel = channel;

        let on_error = move |e| {
            // Nothing here may end the session: the other channel is still
            // recording and the audio already captured is on disk.
            tracing::error!(error = %e, channel = ?err_channel, "capture stream error");
        };

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    seen.store(now_millis(), Ordering::Relaxed);
                    let mono = downmix(data, src_channels);
                    let samples = resample_linear(&mono, src_rate, SAMPLE_RATE);
                    let _ = tx.send(Frame { channel, samples });
                },
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    seen.store(now_millis(), Ordering::Relaxed);
                    let floats: Vec<f32> =
                        data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                    let mono = downmix(&floats, src_channels);
                    let samples = resample_linear(&mono, src_rate, SAMPLE_RATE);
                    let _ = tx.send(Frame { channel, samples });
                },
                on_error,
                None,
            ),
            other => {
                return Err(CaError::Config(format!(
                    "unsupported sample format {other:?} on this device"
                )))
            }
        }
        .map_err(|e| CaError::Config(format!("could not open the audio stream: {e}")))?;

        stream
            .play()
            .map_err(|e| CaError::Config(format!("could not start the audio stream: {e}")))?;
        Ok(stream)
    }
}

impl AudioSource for CaptureSource {
    fn start(&mut self, tx: UnboundedSender<Frame>) -> Result<()> {
        let mic = Self::build(
            &self.input,
            Channel::Agent,
            tx.clone(),
            self.last_seen[0].clone(),
        )?;
        // If loopback fails, the call is still worth recording one-sided rather
        // than not at all — but the caller has to know, so this is an error and
        // the session decides.
        let loopback = Self::build(
            &self.output,
            Channel::Customer,
            tx,
            self.last_seen[1].clone(),
        )?;
        self.streams = vec![mic, loopback];
        Ok(())
    }

    fn stop(&mut self) {
        // Dropping a cpal stream stops it.
        self.streams.clear();
    }

    fn devices(&self) -> (String, String) {
        (self.input_name.clone(), self.output_name.clone())
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Average interleaved channels down to mono.
fn downmix(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return data.to_vec();
    }
    data.chunks(channels)
        .map(|f| f.iter().sum::<f32>() / f.len() as f32)
        .collect()
}

/// Linear resampling to 16 kHz.
///
/// Good enough on purpose. The live tier feeds a preview transcript; the
/// authoritative pass re-reads the recorded file through `ca-audio`'s proper
/// resampler, so quality here buys nothing and latency costs everything.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let left = pos.floor() as usize;
        let frac = (pos - left as f64) as f32;
        let a = input.get(left).copied().unwrap_or(0.0);
        let b = input.get(left + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerating_devices_does_not_panic_without_hardware() {
        // CI has no audio devices at all. Returning empty lists is correct;
        // panicking would take the whole test run down.
        let (inputs, outputs) = list_devices().unwrap();
        let _ = (inputs.len(), outputs.len());
    }

    #[test]
    fn an_unknown_device_name_is_an_error_that_names_it() {
        let err = CaptureSource::new(Some("Nonexistent Microphone 9000"), None)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        // On a machine with no audio hardware at all the failure is "no
        // microphone is available", which is also a fair answer.
        assert!(
            err.contains("Nonexistent Microphone 9000") || err.contains("no microphone"),
            "got {err}"
        );
    }

    #[test]
    fn stereo_is_averaged_rather_than_dropping_a_side() {
        // Taking only the left channel would silently lose half of any device
        // that puts the far end on the right.
        assert_eq!(downmix(&[1.0, 0.0, 1.0, 0.0], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.25, 0.75], 1), vec![0.25, 0.75]);
    }

    #[test]
    fn resampling_preserves_duration() {
        let input = vec![0.5f32; 48_000];
        let out = resample_linear(&input, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_matching_rate_is_passed_through_untouched() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&input, 16_000, 16_000), input);
    }

    #[test]
    fn resampling_an_empty_buffer_is_empty_rather_than_a_panic() {
        assert!(resample_linear(&[], 48_000, 16_000).is_empty());
    }
}
