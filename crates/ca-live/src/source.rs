//! Where live audio comes from.
//!
//! One trait with two implementations, and the split is what makes Phase 3
//! testable at all. Real capture needs two physical devices and a person
//! talking, so it can never run in CI; [`ReplaySource`] feeds a recorded stereo
//! WAV through the identical path at any speed. Everything downstream —
//! chunking, transcription, rules, the monitor, persistence — is exercised
//! deterministically, and the untestable surface shrinks to "open two streams
//! and push samples".

use crate::Channel;
use ca_core::error::{CaError, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

/// A burst of samples from one channel: 16 kHz, mono, −1..1.
#[derive(Debug, Clone)]
pub struct Frame {
    pub channel: Channel,
    pub samples: Vec<f32>,
}

/// Something that produces live audio for both sides of a call.
///
/// `start` returns as soon as production has begun; frames arrive on the
/// channel afterwards. Dropping the sender is how a source signals it is done,
/// which is what ends a replay session on its own.
pub trait AudioSource: Send {
    fn start(&mut self, tx: UnboundedSender<Frame>) -> Result<()>;
    fn stop(&mut self);
    /// Human-readable device names, for the session header and the event log.
    fn devices(&self) -> (String, String);
}

/// Replays a recorded stereo WAV as though it were arriving live.
///
/// Left is treated as the agent and right as the customer, matching what
/// [`crate::wav::StereoWavWriter`] produces — so a session recorded today can
/// be replayed through the whole live pipeline tomorrow.
pub struct ReplaySource {
    path: PathBuf,
    /// Multiple of realtime. `f64::INFINITY` disables pacing entirely, which is
    /// what tests use so they finish in milliseconds rather than minutes.
    speed: f64,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ReplaySource {
    pub fn new(path: impl Into<PathBuf>, speed: f64) -> Self {
        Self {
            path: path.into(),
            speed,
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }

    /// Replay as fast as the machine allows. For tests and back-catalogue runs.
    pub fn instant(path: impl Into<PathBuf>) -> Self {
        Self::new(path, f64::INFINITY)
    }
}

impl AudioSource for ReplaySource {
    fn start(&mut self, tx: UnboundedSender<Frame>) -> Result<()> {
        let data = crate::wav::read_pcm16(&self.path)?;
        if data.channels.is_empty() {
            return Err(CaError::Config(format!(
                "{} has no audio channels",
                self.path.display()
            )));
        }
        let sample_rate = data.sample_rate;
        let stop = self.stop.clone();
        let speed = self.speed;

        // 100 ms bursts: close enough to what a capture callback delivers that
        // the chunker sees realistic arrival patterns rather than one huge push.
        let burst = (sample_rate as usize / 10).max(1);
        let agent = data.channels[0].clone();
        // A mono recording replays as an agent-only call rather than failing.
        let customer = data.channels.get(1).cloned().unwrap_or_default();

        self.handle = Some(std::thread::spawn(move || {
            let total = agent.len().max(customer.len());
            let mut at = 0usize;
            while at < total && !stop.load(Ordering::Relaxed) {
                let end = (at + burst).min(total);
                let slice = |v: &Vec<f32>| -> Vec<f32> {
                    if at >= v.len() {
                        vec![0.0; end - at]
                    } else {
                        let stop_at = end.min(v.len());
                        let mut s = v[at..stop_at].to_vec();
                        s.resize(end - at, 0.0);
                        s
                    }
                };
                let a = Frame {
                    channel: Channel::Agent,
                    samples: slice(&agent),
                };
                let c = Frame {
                    channel: Channel::Customer,
                    samples: slice(&customer),
                };
                // A closed receiver means the session ended first; stop quietly
                // rather than filling a dead channel.
                if tx.send(a).is_err() || tx.send(c).is_err() {
                    return;
                }
                if speed.is_finite() && speed > 0.0 {
                    let secs = (end - at) as f64 / sample_rate as f64 / speed;
                    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
                }
                at = end;
            }
        }));
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }

    fn devices(&self) -> (String, String) {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "replay".into());
        (format!("replay: {name} (L)"), format!("replay: {name} (R)"))
    }
}

/// Write a stereo WAV for tests and demos, from per-channel samples.
pub fn write_stereo_fixture(
    path: &Path,
    sample_rate: u32,
    agent: &[f32],
    customer: &[f32],
) -> Result<()> {
    let mut w = crate::wav::StereoWavWriter::create(path, sample_rate)?;
    w.push(Channel::Agent, agent)?;
    w.push(Channel::Customer, customer)?;
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ca-live-src-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    async fn collect(path: &Path, speed: f64) -> Vec<Frame> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut src = ReplaySource::new(path, speed);
        src.start(tx).unwrap();
        let mut out = Vec::new();
        while let Some(f) = rx.recv().await {
            out.push(f);
        }
        src.stop();
        out
    }

    #[tokio::test]
    async fn a_stereo_file_replays_as_frames_on_both_channels() {
        let path = tmp("two.wav");
        write_stereo_fixture(&path, 16_000, &[0.5; 32_000], &[-0.5; 32_000]).unwrap();

        let frames = collect(&path, f64::INFINITY).await;
        let agent: usize = frames
            .iter()
            .filter(|f| f.channel == Channel::Agent)
            .map(|f| f.samples.len())
            .sum();
        let customer: usize = frames
            .iter()
            .filter(|f| f.channel == Channel::Customer)
            .map(|f| f.samples.len())
            .sum();
        assert_eq!(agent, 32_000);
        assert_eq!(customer, 32_000);
    }

    #[tokio::test]
    async fn instant_replay_does_not_wait_for_realtime() {
        // Two seconds of audio must not take two seconds, or no test suite
        // could afford to exercise the live path.
        let path = tmp("fast.wav");
        write_stereo_fixture(&path, 16_000, &[0.1; 32_000], &[0.1; 32_000]).unwrap();

        let started = std::time::Instant::now();
        let frames = collect(&path, f64::INFINITY).await;
        assert!(!frames.is_empty());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "replay paced itself: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_mono_recording_replays_as_an_agent_only_call() {
        let path = tmp("mono.wav");
        std::fs::write(&path, crate::wav::mono_wav_bytes(&[0.4; 16_000], 16_000)).unwrap();

        let frames = collect(&path, f64::INFINITY).await;
        assert!(frames.iter().any(|f| f.channel == Channel::Agent));
        let customer_energy: f32 = frames
            .iter()
            .filter(|f| f.channel == Channel::Customer)
            .flat_map(|f| f.samples.iter())
            .map(|s| s.abs())
            .sum();
        assert_eq!(customer_energy, 0.0, "invented customer audio");
    }

    #[tokio::test]
    async fn a_missing_file_is_an_error_rather_than_a_panic() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut src = ReplaySource::instant(tmp("nope.wav"));
        assert!(src.start(tx).is_err());
    }

    #[test]
    fn device_names_identify_the_replay() {
        let src = ReplaySource::instant(tmp("call.wav"));
        let (a, c) = src.devices();
        assert!(a.contains("call.wav"), "got {a}");
        assert!(c.contains("call.wav"));
    }
}
