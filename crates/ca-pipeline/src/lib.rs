//! Batch orchestration.
//!
//! Drives each call through the stages and — the part that matters at this
//! scale — commits progress after every one. A 1,968-file run takes many hours;
//! it must survive a reboot, a crashed provider, and being deliberately
//! stopped, then pick up exactly where it left off.
//!
//! Concurrency is deliberately modest. Transcription already saturates several
//! cores, so running many files at once thrashes rather than helps; the useful
//! parallelism is a handful of files in flight, each with a bounded thread
//! budget.

pub mod analyze;

pub use analyze::{Analyzer, BatteryResult};

use ca_asr::WhisperCliTranscriber;
use ca_audio::{process_file, ProcessOptions};
use ca_core::call::{Call, CallStage};
use ca_core::error::Result;
use ca_core::ids::CallId;
use ca_core::provider::{Diarizer, Source, TranscribeOptions, Transcriber};
use ca_core::transcript::Transcript;
use ca_diarize::AcousticDiarizer;
use ca_privacy::Redactor;
use ca_store::Store;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PipelineOptions {
    /// Files in flight at once. Whisper is already multi-threaded, so this is
    /// small on purpose.
    pub concurrency: usize,
    /// Stop after this many calls; `None` processes everything.
    pub limit: Option<usize>,
    /// Redo work from this stage onward, even for calls that already finished.
    ///
    /// `Some(Discovered)` reprocesses a call from the media file;
    /// `Some(Analyzed)` re-scores one without touching its transcript. `None`
    /// is the normal resume: each call picks up at the first stage it has not
    /// reached.
    pub restart_from: Option<CallStage>,
    pub audio: ProcessOptions,
    pub num_speakers: Option<u8>,
    /// Redact before any transcript can reach a cloud provider.
    pub redact: bool,
    /// Stage a call must reach to count as done.
    pub target_stage: CallStage,
    /// Process exactly these calls, ignoring the stage filters.
    ///
    /// An explicit request for one call is a different question from "what is
    /// outstanding" — it should run even for a call that is already finished,
    /// because the person asking is looking at it.
    pub only_calls: Vec<CallId>,
    /// Where remote objects are downloaded to before decoding.
    pub cache_dir: std::path::PathBuf,
    /// Skip calls that have not yet reached this stage.
    ///
    /// An analysis-only pass sets this so it selects transcribed calls rather
    /// than starting whisper on raw audio at network concurrency.
    pub require_stage: Option<CallStage>,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            concurrency: 2,
            limit: None,
            restart_from: None,
            audio: ProcessOptions::default(),
            num_speakers: Some(2),
            redact: true,
            target_stage: CallStage::Redacted,
            only_calls: Vec::new(),
            cache_dir: ca_store::default_cache_dir(),
            require_stage: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Discovered {
        total: usize,
        new: usize,
    },
    Started {
        call: CallId,
        name: String,
    },
    Stage {
        call: CallId,
        stage: CallStage,
    },
    Finished {
        call: CallId,
        name: String,
        secs: f64,
    },
    Failed {
        call: CallId,
        name: String,
        error: String,
    },
    Done {
        processed: usize,
        failed: usize,
    },
}

#[derive(Debug, Default, Clone)]
pub struct RunReport {
    pub discovered: usize,
    pub processed: usize,
    pub failed: usize,
    pub audio_secs: f64,
    pub wall_secs: f64,
}

pub struct Pipeline {
    store: Arc<Store>,
    transcriber: Arc<WhisperCliTranscriber>,
    diarizer: Arc<AcousticDiarizer>,
    /// Absent for a transcription-only run, which is the whole point: filling a
    /// corpus overnight should not require credentials or spend.
    analyzer: Option<Arc<Analyzer>>,
}

impl Pipeline {
    pub fn new(store: Arc<Store>, transcriber: Arc<WhisperCliTranscriber>) -> Self {
        Self {
            store,
            transcriber,
            diarizer: Arc::new(AcousticDiarizer::default()),
            analyzer: None,
        }
    }

    pub fn with_analyzer(mut self, analyzer: Arc<Analyzer>) -> Self {
        self.analyzer = Some(analyzer);
        self
    }

    /// Discover everything a source offers and register it.
    ///
    /// Registration is separate from processing so that a scan of a large
    /// folder is cheap and idempotent: re-running it adds only what is new.
    pub async fn discover(&self, source: &dyn Source) -> Result<(usize, usize)> {
        let items = source.list().await?;
        let total = items.len();
        let before = self.store.count_calls()?;

        // Attribute as each call is registered, so a freshly scanned folder
        // shows who was on every call without the user having to run a
        // separate backfill first.
        let patterns = ca_identity::builtin_patterns();
        for item in items {
            let mut call = Call::new(&item.key, source.name(), &item.display_name);
            call.media_path = Some(item.key.clone());
            let id = self.store.upsert_call(&call)?;
            self.store
                .attribute_call(&id, &item.display_name, &item.key, &patterns)?;
        }

        let after = self.store.count_calls()?;
        Ok((total, after.saturating_sub(before)))
    }

    /// Process outstanding calls, reporting progress through `on_progress`.
    pub async fn run<F>(&self, opts: &PipelineOptions, mut on_progress: F) -> Result<RunReport>
    where
        F: FnMut(Progress) + Send,
    {
        let started = std::time::Instant::now();
        let mut report = RunReport::default();

        // Fail here rather than three hours into a batch, once every call has
        // been transcribed and there is nothing to score it with.
        if opts.target_stage.is_at_least(CallStage::Analyzed) && self.analyzer.is_none() {
            return Err(ca_core::error::CaError::Config(
                "target stage is `analyzed` but no analyzer is configured".into(),
            ));
        }

        let limit = opts.limit.unwrap_or(usize::MAX);

        let todo = if !opts.only_calls.is_empty() {
            // Named calls bypass the stage filters entirely. Someone looking at
            // a call and asking for it to be processed means that call, not
            // "that call if it happens to be outstanding".
            opts.only_calls.clone()
        } else {
            let ceiling = if opts.restart_from.is_some() {
                // Re-runs must be able to pick calls that already finished.
                CallStage::Indexed
            } else {
                opts.target_stage
            };
            self.store
                .calls_in_stage_range(opts.require_stage, ceiling, limit)?
        };
        report.discovered = todo.len();
        on_progress(Progress::Discovered {
            total: todo.len(),
            new: todo.len(),
        });

        let processed = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicUsize::new(0));

        // A semaphore rather than a thread pool: each unit of work is mostly
        // waiting on a subprocess, and the cap is about CPU contention.
        let sem = Arc::new(tokio::sync::Semaphore::new(opts.concurrency.max(1)));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Progress>();

        let mut handles = Vec::new();
        for call_id in todo {
            let permit_src = sem.clone();
            let store = self.store.clone();
            let transcriber = self.transcriber.clone();
            let diarizer = self.diarizer.clone();
            let analyzer = self.analyzer.clone();
            let opts = opts.clone();
            let tx = tx.clone();
            let processed = processed.clone();
            let failed = failed.clone();

            handles.push(tokio::spawn(async move {
                let _permit = permit_src.acquire().await.ok()?;

                let name = call_id.as_str().to_string();
                let _ = tx.send(Progress::Started {
                    call: call_id.clone(),
                    name: name.clone(),
                });

                let ctx = Ctx {
                    store: &store,
                    transcriber: &transcriber,
                    diarizer: &diarizer,
                    analyzer: analyzer.as_deref(),
                };
                match process_one(&ctx, &call_id, &opts, &tx).await {
                    Ok(secs) => {
                        processed.fetch_add(1, Ordering::Relaxed);
                        let _ = tx.send(Progress::Finished {
                            call: call_id,
                            name,
                            secs,
                        });
                        Some(secs)
                    }
                    Err(e) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        // Record the failure so a resumed run can retry it and
                        // the UI can show why it stalled.
                        let _ = store.set_stage(&call_id, CallStage::Failed, Some(&e.to_string()));
                        let _ = tx.send(Progress::Failed {
                            call: call_id,
                            name,
                            error: e.to_string(),
                        });
                        None
                    }
                }
            }));
        }
        drop(tx);

        while let Some(p) = rx.recv().await {
            on_progress(p);
        }

        for h in handles {
            if let Ok(Some(secs)) = h.await {
                report.audio_secs += secs;
            }
        }

        report.processed = processed.load(Ordering::Relaxed);
        report.failed = failed.load(Ordering::Relaxed);
        report.wall_secs = started.elapsed().as_secs_f64();

        on_progress(Progress::Done {
            processed: report.processed,
            failed: report.failed,
        });

        Ok(report)
    }
}

/// Everything one unit of work borrows. Bundled because threading five
/// references plus options plus a channel through each stage reads worse than
/// the code it surrounds.
struct Ctx<'a> {
    store: &'a Store,
    transcriber: &'a WhisperCliTranscriber,
    diarizer: &'a AcousticDiarizer,
    analyzer: Option<&'a Analyzer>,
}

/// Run one call through the stages it has not yet reached.
///
/// The resume check is the reason this reads the stored stage first. Adding an
/// analysis pass to an already-transcribed corpus must cost one model call, not
/// a re-decode and re-transcription of 133 hours of audio.
async fn process_one(
    ctx: &Ctx<'_>,
    call_id: &CallId,
    opts: &PipelineOptions,
    tx: &tokio::sync::mpsc::UnboundedSender<Progress>,
) -> Result<f64> {
    let store = ctx.store;

    let stored = store.stage_of(call_id)?.unwrap_or(CallStage::Discovered);
    let recorded = match opts.restart_from {
        // Rewind to just before the restart point. Everything earlier is still
        // trusted, so a re-score costs one model call rather than a re-decode.
        Some(from) if stored.is_at_least(from) => from.previous(),
        _ => stored,
    };

    // A stage marker is only worth trusting if the artefact it claims exists is
    // actually there; a half-written run should redo the work, not skip it.
    let cached = if recorded.is_at_least(CallStage::Diarized) {
        store.load_transcript(call_id)?
    } else {
        None
    };

    let (mut transcript, duration_secs, reached) = match cached {
        Some(t) => {
            let secs = t.duration_secs;
            (t, secs, recorded)
        }
        None => {
            let (t, secs) = transcribe_and_diarize(ctx, call_id, opts, tx).await?;
            (t, secs, CallStage::Diarized)
        }
    };

    // ---- redact ----
    if opts.redact && !reached.is_at_least(CallStage::Redacted) {
        let mut redactor = Redactor::default();
        let mut kinds = std::collections::HashMap::new();
        for seg in transcript.segments.iter_mut() {
            let out = redactor.redact(&seg.text);
            for d in &out.detections {
                kinds.insert(d.token.clone(), d.kind.as_str().to_string());
            }
            seg.redacted_text = Some(out.text);
        }
        store.save_transcript(&transcript)?;
        store.save_redaction_map(call_id, redactor.map(), &kinds)?;
        store.set_stage(call_id, CallStage::Redacted, None)?;
        let _ = tx.send(Progress::Stage {
            call: call_id.clone(),
            stage: CallStage::Redacted,
        });
    }

    // ---- analyse ----
    if opts.target_stage.is_at_least(CallStage::Analyzed) {
        if let Some(analyzer) = ctx.analyzer {
            // Whether this transcript is *actually* redacted, not whether it was
            // meant to be. The analyzer refuses to send unredacted text to a
            // remote provider, and it can only make that call if told the truth.
            let redacted = transcript
                .segments
                .iter()
                .all(|s| s.redacted_text.is_some());

            let battery = analyzer.run_battery(&transcript, redacted).await?;
            for analysis in &battery.analyses {
                store.save_analysis(analysis)?;
            }
            if !battery.findings.is_empty() {
                store.save_findings(call_id, &battery.findings)?;
            }
            store.set_stage(call_id, CallStage::Analyzed, None)?;
            let _ = tx.send(Progress::Stage {
                call: call_id.clone(),
                stage: CallStage::Analyzed,
            });
        }
    }

    Ok(duration_secs)
}

/// Decode, transcribe and diarize from the media file, committing after each.
async fn transcribe_and_diarize(
    ctx: &Ctx<'_>,
    call_id: &CallId,
    opts: &PipelineOptions,
    tx: &tokio::sync::mpsc::UnboundedSender<Progress>,
) -> Result<(Transcript, f64)> {
    let store = ctx.store;
    let path = call_media_path(store, call_id)?;
    // A call from a bucket carries a URL rather than a path. It is downloaded
    // here rather than at discovery, so scanning a bucket of 1,968 objects
    // stays instant and a stopped batch has only pulled what it processed.
    let local = materialise(&path, &opts.cache_dir).await?;

    // ---- decode + condition ----
    let processed = process_file(&local, &opts.audio)?;
    store.save_audio_meta(
        call_id,
        &processed.meta,
        &processed.features,
        &processed.peaks,
    )?;
    store.set_stage(call_id, CallStage::Decoded, None)?;
    let _ = tx.send(Progress::Stage {
        call: call_id.clone(),
        stage: CallStage::Decoded,
    });

    // ---- transcribe ----
    let mut transcript = ctx
        .transcriber
        .transcribe(&processed.audio, &TranscribeOptions::default())
        .await?;
    transcript.call_id = call_id.clone();
    store.save_transcript(&transcript)?;
    store.set_stage(call_id, CallStage::Transcribed, None)?;
    let _ = tx.send(Progress::Stage {
        call: call_id.clone(),
        stage: CallStage::Transcribed,
    });

    // ---- diarize ----
    let turns = ctx
        .diarizer
        .diarize(&processed.audio, opts.num_speakers)
        .await?;
    // Split at word level: ASR segments span turn boundaries, so one speaker
    // per ASR segment mislabels every question-and-answer pair.
    ca_diarize::resegment_by_speaker(&mut transcript, &turns);
    store.save_transcript(&transcript)?;
    store.set_stage(call_id, CallStage::Diarized, None)?;
    let _ = tx.send(Progress::Stage {
        call: call_id.clone(),
        stage: CallStage::Diarized,
    });

    Ok((transcript, processed.meta.duration_secs))
}

/// Turn whatever was recorded as the media path into a file on disk.
///
/// Local paths pass straight through; an `http(s)` URL is downloaded once and
/// reused on every later run, which is what keeps a resumed cloud batch from
/// re-pulling gigabytes it already has.
async fn materialise(path: &str, cache_dir: &std::path::Path) -> Result<std::path::PathBuf> {
    if !ca_ingest::is_remote(path) {
        return Ok(std::path::PathBuf::from(path));
    }
    // A fresh client per call is wasteful in principle; in practice a download
    // is seconds of transfer against a connection setup of milliseconds, and
    // it keeps this function free of shared state.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| ca_core::error::CaError::Config(format!("http client: {e}")))?;
    ca_ingest::cache::download(&client, path, cache_dir).await
}

fn call_media_path(store: &Store, call_id: &CallId) -> Result<String> {
    store.media_path(call_id)?.ok_or_else(|| {
        ca_core::error::CaError::NotFound(format!("no media path recorded for {call_id}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_target_stage_includes_redaction() {
        // Redaction must be inside the default path, not an opt-in step that
        // is easy to forget before a cloud call.
        let o = PipelineOptions::default();
        assert_eq!(o.target_stage, CallStage::Redacted);
        assert!(o.redact);
    }

    #[test]
    fn a_normal_run_restarts_nothing() {
        assert!(PipelineOptions::default().restart_from.is_none());
    }

    #[test]
    fn restarting_at_analysis_preserves_the_transcript() {
        // The property that makes re-scoring affordable: rewinding to
        // `Analyzed` leaves the call past diarization, so the transcript is
        // loaded rather than recomputed.
        let rewound = CallStage::Analyzed.previous();
        assert!(rewound.is_at_least(CallStage::Diarized));
        assert!(!rewound.is_at_least(CallStage::Analyzed));
    }

    #[test]
    fn restarting_at_the_beginning_reprocesses_the_media() {
        assert_eq!(CallStage::Discovered.previous(), CallStage::Discovered);
        assert!(!CallStage::Discovered.is_at_least(CallStage::Decoded));
    }

    #[test]
    fn default_concurrency_is_conservative() {
        // Whisper already uses several cores; a high fan-out would thrash.
        let o = PipelineOptions::default();
        assert!(o.concurrency <= 4 && o.concurrency >= 1);
    }

    #[test]
    fn progress_variants_compare_by_value() {
        let a = Progress::Done {
            processed: 1,
            failed: 0,
        };
        let b = Progress::Done {
            processed: 1,
            failed: 0,
        };
        assert_eq!(a, b);
    }
}
