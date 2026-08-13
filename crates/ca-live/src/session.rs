//! Driving one live call.
//!
//! The loop is deliberately single-threaded over a channel of frames: capture
//! runs on its own thread and pushes, and everything else — chunking, writing,
//! transcription, rules, the monitor — happens in arrival order here. Ordering
//! matters more than parallelism, because segments must reach the transcript in
//! time order for the rules to read them correctly.
//!
//! # What is allowed to end a session
//!
//! Almost nothing. Capture is the only part of a call that cannot be redone, so
//! a failed transcription, a dead provider or a wedged server all degrade to a
//! [`LiveEvent::Warning`] and the recording continues. The call still becomes
//! ordinary corpus work at hang-up, where the batch pipeline transcribes it
//! properly. A live session that threw away the audio because a model was
//! unreachable would be the worst possible failure.

use crate::asr::ChunkTranscriber;
use crate::chunker::Chunker;
use crate::event::LiveEvent;
use crate::source::AudioSource;
use crate::wav::StereoWavWriter;
use crate::{Channel, SAMPLE_RATE};

use ca_core::analysis::{Finding, LiveMonitorReport};
use ca_core::error::{CaError, Result};
use ca_core::ids::{CallId, SegmentId};
use ca_core::transcript::{ChannelTimeline, Segment, SpeechSpan, Transcript};
use ca_store::Store;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How much transcript the monitor is shown. Beyond this it is reading history
/// rather than the situation, and paying for the privilege every pass.
const MONITOR_WINDOW_SECS: f64 = 90.0;

pub struct LiveConfig {
    pub call_id: CallId,
    /// Where the stereo recording is written.
    pub media_path: PathBuf,
    /// Phrases the rules tier treats as unsupportable commitments.
    pub banned_phrases: Vec<String>,
    /// How often the monitor runs, in call time.
    pub monitor_every: Duration,
    /// Floor between monitor calls, so a burst of rule hits cannot cause a
    /// burst of provider calls.
    pub monitor_min_gap: Duration,
}

impl LiveConfig {
    pub fn new(call_id: CallId, media_path: PathBuf) -> Self {
        Self {
            call_id,
            media_path,
            banned_phrases: ca_rules::default_banned_phrases(),
            monitor_every: Duration::from_secs(25),
            monitor_min_gap: Duration::from_secs(8),
        }
    }
}

/// The judgement tier: sentiment, risk, and what to say next.
///
/// A trait rather than a concrete agent so the session can be tested without a
/// provider, and so the redaction step lives in the implementation where the
/// provider is known — a local model needs none, a remote one must never see
/// raw text.
#[async_trait::async_trait]
pub trait LiveAdvisor: Send + Sync {
    async fn advise(
        &self,
        window: &Transcript,
        previous_headline: Option<&str>,
        rule_hits: &[Finding],
    ) -> Result<LiveMonitorReport>;
}

pub struct SessionOutcome {
    pub call_id: CallId,
    pub duration_secs: f64,
    pub media_path: PathBuf,
    pub segments: usize,
    pub findings: usize,
}

pub struct LiveSession;

impl LiveSession {
    /// Run until the source finishes or `stop` is set.
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        cfg: LiveConfig,
        store: Arc<Store>,
        mut source: Box<dyn AudioSource>,
        transcriber: Arc<dyn ChunkTranscriber>,
        advisor: Option<Arc<dyn LiveAdvisor>>,
        stop: Arc<AtomicBool>,
        on_event: Arc<dyn Fn(LiveEvent) + Send + Sync>,
    ) -> Result<SessionOutcome> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (agent_device, customer_device) = source.devices();
        source.start(tx)?;

        on_event(LiveEvent::Started {
            call_id: cfg.call_id.as_str().to_string(),
            agent_device,
            customer_device,
        });

        let mut writer = StereoWavWriter::create(&cfg.media_path, SAMPLE_RATE)?;
        let mut chunkers = [Chunker::new(SAMPLE_RATE), Chunker::new(SAMPLE_RATE)];
        let mut speaking = [false, false];
        let mut levels = [f32::NEG_INFINITY, f32::NEG_INFINITY];

        let mut segments: Vec<Segment> = Vec::new();
        let mut timeline = ChannelTimeline::default();
        let mut seen_findings: HashSet<String> = HashSet::new();
        let mut all_findings: Vec<Finding> = Vec::new();

        let mut headline: Option<String> = None;
        // Measured from call start, so the first cadence pass lands one full
        // interval in rather than immediately.
        let mut last_monitor = Duration::ZERO;
        let mut monitor_has_run = false;
        let mut monitor_due = false;
        let mut last_level_emit = 0.0f64;

        while let Some(frame) = rx.recv().await {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let channel = frame.channel;
            let idx = channel.index() as usize;

            writer.push(channel, &frame.samples)?;
            levels[idx] = peak_dbfs(&frame.samples);

            let closed = chunkers[idx].push(&frame.samples);

            // The listening marker, driven by voice activity rather than by a
            // provisional transcription that does not exist.
            let now_speaking = chunkers[idx].speaking();
            if now_speaking != speaking[idx] {
                speaking[idx] = now_speaking;
                on_event(LiveEvent::Speaking {
                    channel,
                    speaking: now_speaking,
                });
            }

            let elapsed = chunkers
                .iter()
                .map(|c| c.elapsed_secs())
                .fold(0.0f64, f64::max);

            if elapsed - last_level_emit >= 0.5 {
                last_level_emit = elapsed;
                on_event(LiveEvent::Level {
                    agent_dbfs: levels[0],
                    customer_dbfs: levels[1],
                    elapsed_secs: elapsed,
                });
            }

            for chunk in closed {
                timeline.spans.push(SpeechSpan {
                    channel: channel.index(),
                    start: chunk.start_secs,
                    end: chunk.end_secs,
                });

                let text = match transcriber.transcribe(&chunk.samples, SAMPLE_RATE).await {
                    Ok(t) => t,
                    Err(e) => {
                        // The audio is already on disk and the batch pass will
                        // transcribe it properly. Losing the live line is a
                        // degradation, not a reason to end the call.
                        tracing::warn!(error = %e, "live transcription failed");
                        on_event(LiveEvent::Warning {
                            message: format!("Live transcription unavailable: {e}"),
                        });
                        continue;
                    }
                };
                if text.is_empty() {
                    continue;
                }

                let segment = Segment {
                    id: SegmentId::new(),
                    speaker_index: channel.index(),
                    role: channel.role(),
                    start: chunk.start_secs,
                    end: chunk.end_secs,
                    text,
                    redacted_text: None,
                    words: vec![],
                    confidence: None,
                    language: None,
                };
                on_event(LiveEvent::Stable {
                    segment_id: segment.id.as_str().to_string(),
                    channel,
                    start_secs: segment.start,
                    end_secs: segment.end,
                    text: segment.text.clone(),
                });

                segments.push(segment);
                // Time order, not arrival order: the two channels close chunks
                // independently, and the rules read neighbouring segments.
                segments.sort_by(|a, b| {
                    a.start
                        .partial_cmp(&b.start)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                let fresh = Self::new_findings(&cfg, &segments, elapsed, &mut seen_findings);
                for f in &fresh {
                    if f.severity.is_blocking() {
                        monitor_due = true;
                    }
                    on_event(LiveEvent::Rule { finding: f.clone() });
                }
                if !fresh.is_empty() {
                    all_findings.extend(fresh);
                    // Persisted as they happen so a crash mid-call still leaves
                    // the alerts that were raised.
                    if let Err(e) = store.save_findings(&cfg.call_id, &all_findings) {
                        tracing::warn!(error = %e, "could not persist live findings");
                    }
                }
            }

            // Monitor on cadence, or immediately when something blocking fired.
            //
            // The minimum gap exists to stop a burst of rule hits causing a
            // burst of provider calls, so it only applies *between* calls. A
            // critical breach ninety seconds into a call must not wait for the
            // cadence, and one three seconds in must not wait for the floor
            // against a call that never happened.
            let elapsed_dur = Duration::from_secs_f64(elapsed.max(0.0));
            let since = elapsed_dur.saturating_sub(last_monitor);
            let due = monitor_due || since >= cfg.monitor_every;
            let allowed = !monitor_has_run || since >= cfg.monitor_min_gap;
            if due && allowed && !segments.is_empty() {
                if let Some(advisor) = &advisor {
                    last_monitor = elapsed_dur;
                    monitor_has_run = true;
                    monitor_due = false;
                    let window = Self::window(&cfg.call_id, &segments, elapsed);
                    match advisor
                        .advise(&window, headline.as_deref(), &all_findings)
                        .await
                    {
                        Ok(report) => {
                            headline = Some(report.headline.clone());
                            on_event(LiveEvent::Monitor {
                                sentiment: report.sentiment,
                                risk: report.risk,
                                headline: report.headline,
                                say_next: report.say_next,
                                elapsed_secs: elapsed,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "live monitor failed");
                            on_event(LiveEvent::Warning {
                                message: format!("Live monitor unavailable: {e}"),
                            });
                        }
                    }
                }
            }
        }

        source.stop();

        // Whatever was mid-utterance when the call ended is still worth having.
        for channel in Channel::all() {
            let idx = channel.index() as usize;
            if let Some(chunk) = chunkers[idx].flush() {
                timeline.spans.push(SpeechSpan {
                    channel: channel.index(),
                    start: chunk.start_secs,
                    end: chunk.end_secs,
                });
                if let Ok(text) = transcriber.transcribe(&chunk.samples, SAMPLE_RATE).await {
                    if !text.is_empty() {
                        let segment = Segment {
                            id: SegmentId::new(),
                            speaker_index: channel.index(),
                            role: channel.role(),
                            start: chunk.start_secs,
                            end: chunk.end_secs,
                            text,
                            redacted_text: None,
                            words: vec![],
                            confidence: None,
                            language: None,
                        };
                        on_event(LiveEvent::Stable {
                            segment_id: segment.id.as_str().to_string(),
                            channel,
                            start_secs: segment.start,
                            end_secs: segment.end,
                            text: segment.text.clone(),
                        });
                        segments.push(segment);
                    }
                }
            }
        }

        let duration_secs = chunkers
            .iter()
            .map(|c| c.elapsed_secs())
            .fold(0.0f64, f64::max);

        writer.finish()?;
        timeline.spans.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        store.save_channel_timeline(&cfg.call_id, &timeline)?;

        on_event(LiveEvent::Ended {
            call_id: cfg.call_id.as_str().to_string(),
            duration_secs,
            media_path: cfg.media_path.display().to_string(),
        });

        Ok(SessionOutcome {
            call_id: cfg.call_id,
            duration_secs,
            media_path: cfg.media_path,
            segments: segments.len(),
            findings: all_findings.len(),
        })
    }

    /// Findings not already reported this call.
    ///
    /// `ca_rules::evaluate` is pure and re-reports everything it can still see,
    /// so the diff happens here on a key that survives the finding id being
    /// regenerated each pass.
    fn new_findings(
        cfg: &LiveConfig,
        segments: &[Segment],
        elapsed: f64,
        seen: &mut HashSet<String>,
    ) -> Vec<Finding> {
        let ctx = ca_rules::RuleContext {
            call_id: &cfg.call_id,
            segments,
            elapsed_secs: elapsed,
            banned_phrases: &cfg.banned_phrases,
        };
        ca_rules::evaluate(&ctx)
            .into_iter()
            .filter(|f| seen.insert(ca_rules::dedupe_key(f)))
            .collect()
    }

    /// The recent transcript, as a `Transcript` the agent layer can validate
    /// citations against.
    fn window(call_id: &CallId, segments: &[Segment], elapsed: f64) -> Transcript {
        let cutoff = elapsed - MONITOR_WINDOW_SECS;
        let recent: Vec<Segment> = segments
            .iter()
            .filter(|s| s.end >= cutoff)
            .cloned()
            .collect();
        Transcript {
            call_id: call_id.clone(),
            segments: recent,
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "live".into(),
            model: "live".into(),
            duration_secs: elapsed,
        }
    }
}

fn peak_dbfs(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 0.0 {
        return f32::NEG_INFINITY;
    }
    20.0 * peak.log10()
}

/// Guard the redact-before-cloud promise on the live path.
///
/// Live is the easiest place in the product to break it: windows are small,
/// frequent, and easy to treat as ephemeral. They are not — they are the same
/// customer's personal data, sent more often. Same hard error as the batch
/// analyzer, deliberately, rather than a warning.
pub fn guard_redaction(provider_name: &str, local: bool, redacted: bool) -> Result<()> {
    if !local && !redacted {
        return Err(CaError::Config(format!(
            "refusing to send an unredacted live window to remote provider `{provider_name}`"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::StubTranscriber;
    use crate::source::{write_stereo_fixture, ReplaySource};
    use ca_core::call::Call;
    use std::sync::Mutex;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ca-live-sess-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn speech(secs: f64) -> Vec<f32> {
        let n = (SAMPLE_RATE as f64 * secs) as usize;
        (0..n).map(|i| (i as f32 / 40.0).sin() * 0.4).collect()
    }

    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (SAMPLE_RATE as f64 * secs) as usize]
    }

    /// A two-turn call: the agent talks, then the customer.
    fn fixture(name: &str) -> PathBuf {
        let path = tmp(name);
        let mut agent = speech(2.0);
        agent.extend(silence(3.0));
        let mut customer = silence(3.0);
        customer.extend(speech(2.0));
        customer.extend(silence(1.0));
        agent.resize(customer.len(), 0.0);
        write_stereo_fixture(&path, SAMPLE_RATE, &agent, &customer).unwrap();
        path
    }

    struct Recorder(Mutex<Vec<LiveEvent>>);

    /// Every test gets its own fixture and output path.
    ///
    /// Tests run in parallel, and sharing one temp filename means one test
    /// reads a WAV while another is part-way through writing it — which fails
    /// intermittently and looks like a bug in the session.
    async fn run_session(
        name: &str,
        lines: Vec<&str>,
        advisor: Option<Arc<dyn LiveAdvisor>>,
    ) -> (Vec<LiveEvent>, SessionOutcome, Arc<Store>) {
        let store = Arc::new(Store::in_memory().unwrap());
        let call = store
            .upsert_call(&Call::new(name, "live", "live session"))
            .unwrap();

        let events = Arc::new(Recorder(Mutex::new(Vec::new())));
        let sink = events.clone();

        let cfg = LiveConfig::new(call, tmp(&format!("{name}-out.wav")));
        let outcome = LiveSession::run(
            cfg,
            store.clone(),
            Box::new(ReplaySource::instant(fixture(&format!("{name}-in.wav")))),
            Arc::new(StubTranscriber::new(lines)),
            advisor,
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |e| sink.0.lock().unwrap().push(e)),
        )
        .await
        .unwrap();

        let out = events.0.lock().unwrap().clone();
        (out, outcome, store)
    }

    #[tokio::test]
    async fn a_replayed_call_produces_transcript_and_a_recording() {
        let (events, outcome, _) = run_session(
            "recording",
            vec!["hello there", "I was charged twice"],
            None,
        )
        .await;

        assert!(matches!(events.first(), Some(LiveEvent::Started { .. })));
        assert!(matches!(events.last(), Some(LiveEvent::Ended { .. })));
        assert!(outcome.segments >= 2, "got {} segments", outcome.segments);
        assert!(outcome.media_path.exists(), "no recording was written");
    }

    #[tokio::test]
    async fn speaker_labels_come_from_the_channel_not_a_diarizer() {
        let (events, _, _) =
            run_session("labels", vec!["agent speaking", "customer speaking"], None).await;
        let stable: Vec<(Channel, String)> = events
            .iter()
            .filter_map(|e| match e {
                LiveEvent::Stable { channel, text, .. } => Some((*channel, text.clone())),
                _ => None,
            })
            .collect();
        assert!(stable.iter().any(|(c, _)| *c == Channel::Agent));
        assert!(stable.iter().any(|(c, _)| *c == Channel::Customer));
    }

    #[tokio::test]
    async fn a_planted_card_number_raises_a_blocking_finding() {
        // The reason the rules tier exists: caught locally, on the chunk, with
        // no model involved and no network.
        let (events, outcome, store) = run_session(
            "pan",
            vec!["my card is 4111 1111 1111 1111", "thanks"],
            None,
        )
        .await;

        let rules: Vec<&Finding> = events
            .iter()
            .filter_map(|e| match e {
                LiveEvent::Rule { finding } => Some(finding),
                _ => None,
            })
            .collect();
        assert!(
            rules.iter().any(|f| f.code == "PCI_PAN_SPOKEN"),
            "no PAN alert in {rules:#?}"
        );
        assert!(outcome.findings >= 1);
        // And it survives the session, so the risk register sees it.
        assert!(!store.findings(&outcome.call_id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_rule_alerts_once_rather_than_on_every_chunk() {
        // evaluate() re-reports everything it can still see, so without the
        // dedupe the same alert would fire for the rest of the call.
        let (events, _, _) = run_session(
            "dedupe",
            vec!["my card is 4111 1111 1111 1111", "still talking"],
            None,
        )
        .await;
        let pans = events
            .iter()
            .filter(
                |e| matches!(e, LiveEvent::Rule { finding } if finding.code == "PCI_PAN_SPOKEN"),
            )
            .count();
        assert_eq!(pans, 1, "the same alert fired {pans} times");
    }

    #[tokio::test]
    async fn the_channel_timeline_is_persisted_for_the_authoritative_pass() {
        let (_, outcome, store) = run_session("timeline", vec!["one", "two"], None).await;
        let timeline = store
            .channel_timeline(&outcome.call_id)
            .unwrap()
            .expect("no timeline stored");
        assert!(timeline.spans.iter().any(|s| s.channel == 0));
        assert!(timeline.spans.iter().any(|s| s.channel == 1));
    }

    struct FailingTranscriber;

    #[async_trait::async_trait]
    impl ChunkTranscriber for FailingTranscriber {
        async fn transcribe(&self, _: &[f32], _: u32) -> Result<String> {
            Err(CaError::Config("server is wedged".into()))
        }
        fn model(&self) -> String {
            "broken".into()
        }
    }

    #[tokio::test]
    async fn a_broken_transcriber_warns_but_never_loses_the_recording() {
        // Capture is the only thing that cannot be redone. The batch pass will
        // transcribe this call properly at hang-up.
        let store = Arc::new(Store::in_memory().unwrap());
        let call = store
            .upsert_call(&Call::new("live-2", "live", "live session"))
            .unwrap();
        let events = Arc::new(Recorder(Mutex::new(Vec::new())));
        let sink = events.clone();

        let outcome = LiveSession::run(
            LiveConfig::new(call, tmp("broken.wav")),
            store,
            Box::new(ReplaySource::instant(fixture("broken-in.wav"))),
            Arc::new(FailingTranscriber),
            None,
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |e| sink.0.lock().unwrap().push(e)),
        )
        .await
        .unwrap();

        let events = events.0.lock().unwrap().clone();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LiveEvent::Warning { .. })),
            "no warning was surfaced"
        );
        assert!(
            matches!(events.last(), Some(LiveEvent::Ended { .. })),
            "the session did not end cleanly"
        );
        assert!(outcome.media_path.exists(), "the audio was lost");
    }

    struct StubAdvisor(Arc<Mutex<usize>>);

    #[async_trait::async_trait]
    impl LiveAdvisor for StubAdvisor {
        async fn advise(
            &self,
            window: &Transcript,
            _previous: Option<&str>,
            _hits: &[Finding],
        ) -> Result<LiveMonitorReport> {
            *self.0.lock().unwrap() += 1;
            Ok(LiveMonitorReport {
                call_id: window.call_id.clone(),
                sentiment: -0.3,
                risk: ca_core::analysis::Severity::Medium,
                headline: "Customer is frustrated".into(),
                say_next: Some("Let me fix that now.".into()),
                citations: vec![],
            })
        }
    }

    #[tokio::test]
    async fn a_blocking_rule_brings_the_monitor_forward() {
        // Waiting the full cadence to advise on a critical breach would mean
        // the useful moment has passed.
        let calls = Arc::new(Mutex::new(0usize));
        let advisor = Arc::new(StubAdvisor(calls.clone()));
        let (events, _, _) = run_session(
            "monitor-forward",
            vec!["my card is 4111 1111 1111 1111", "ok"],
            Some(advisor),
        )
        .await;

        assert!(*calls.lock().unwrap() >= 1, "the monitor never ran");
        assert!(events
            .iter()
            .any(|e| matches!(e, LiveEvent::Monitor { .. })));
    }

    #[tokio::test]
    async fn a_short_call_does_not_pay_for_a_monitor_pass() {
        // Nothing blocking happened and the cadence has not elapsed, so the
        // session should not have spent a provider call.
        let calls = Arc::new(Mutex::new(0usize));
        let advisor = Arc::new(StubAdvisor(calls.clone()));
        run_session("short-call", vec!["hello", "hi there"], Some(advisor)).await;
        assert_eq!(*calls.lock().unwrap(), 0, "spent a call it did not need");
    }

    #[test]
    fn a_remote_provider_may_not_see_an_unredacted_window() {
        let err = guard_redaction("nvidia-nim", false, false).unwrap_err();
        assert!(format!("{err}").contains("unredacted"), "got {err}");
        assert!(guard_redaction("nvidia-nim", false, true).is_ok());
        // A local model never leaves the machine, so it is exempt.
        assert!(guard_redaction("ollama", true, false).is_ok());
    }
}
