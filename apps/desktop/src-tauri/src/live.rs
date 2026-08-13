//! Live call assist, as commands.
//!
//! Thin over `ca-live`, like every other command module here. The one piece of
//! real logic is the hand-off at hang-up: a finished session is not a special
//! kind of thing, it is a call, so it goes straight into the pipeline that
//! every ingested recording goes through. One corpus, one call list, one risk
//! register.

use ca_config::{AppSettings, SETTINGS_KEY};
use ca_core::call::{Call, CallStage};
use ca_core::ids::CallId;
use ca_live::{
    AudioSource, ChunkTranscriber, LiveAdvisor, LiveConfig, LiveEvent, LiveSession,
    ProviderAdvisor, ReplaySource, ServerHandle, StubTranscriber, WhisperServer,
};
use ca_llm::secrets;
use ca_pipeline::{Analyzer, Pipeline, PipelineOptions};
use ca_store::Store;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, State};

/// Port the live whisper server listens on.
///
/// Fixed rather than negotiated: only one session can run at a time, and a
/// stable port makes a stuck server easy to find and kill by hand.
const WHISPER_PORT: u16 = 8477;

/// What a running session exposes to the command layer.
pub struct LiveState {
    /// Set to end the session. The loop checks it between frames.
    stop: Arc<AtomicBool>,
    call_id: CallId,
    /// Kept alive for the session's lifetime; dropping it kills the server.
    _server: Option<ServerHandle>,
}

#[derive(Default)]
pub struct LiveHandle(pub Mutex<Option<LiveState>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// False when this build cannot capture at all, so the UI can say why
    /// rather than showing two empty pickers.
    pub supported: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStarted {
    pub call_id: String,
    /// True when a model is transcribing live. False still records the call and
    /// still runs the deterministic rules — it just has no live transcript.
    pub transcribing: bool,
    /// True when the monitor tier is available.
    pub advising: bool,
}

/// Microphones and playback devices, for the pickers.
#[tauri::command]
pub fn list_audio_devices() -> Result<AudioDevices, String> {
    match ca_live::capture::list_devices() {
        Ok((inputs, outputs)) => Ok(AudioDevices {
            inputs,
            outputs,
            supported: true,
        }),
        Err(e) => {
            tracing::warn!(error = %e, "audio devices unavailable");
            Ok(AudioDevices {
                inputs: vec![],
                outputs: vec![],
                supported: false,
            })
        }
    }
}

/// Build the live transcriber, or explain why there is not one.
///
/// A missing model is not fatal. The call is still captured and the rules still
/// run offline; only the live transcript is absent, and the batch pass at
/// hang-up produces the real one anyway.
fn build_transcriber(
    live_model: &std::path::Path,
    whisper_server: &std::path::Path,
    threads: usize,
) -> (Arc<dyn ChunkTranscriber>, Option<ServerHandle>) {
    match WhisperServer::spawn(whisper_server, live_model, WHISPER_PORT, threads) {
        Ok(handle) => (
            Arc::new(WhisperServer::client(WHISPER_PORT, "ggml-base.en")),
            Some(handle),
        ),
        Err(e) => {
            tracing::warn!(error = %e, "no live transcription");
            // Silence rather than failure: every other tier still works.
            (Arc::new(StubTranscriber::new(Vec::<String>::new())), None)
        }
    }
}

/// Build the monitor tier, or `None` when no model is configured.
fn build_advisor(store: &Store) -> Option<Arc<dyn LiveAdvisor>> {
    let cfg: AppSettings = store.get_setting(SETTINGS_KEY).ok().flatten()?;
    let (key, _) = secrets::resolve_key(cfg.provider);
    if key.is_none() && cfg.provider.requires_api_key() {
        return None;
    }
    match ca_llm::build_provider(&cfg.spec(), key.as_deref()) {
        Ok(llm) => Some(Arc::new(ProviderAdvisor::new(llm, cfg.provider.is_local()))),
        Err(e) => {
            tracing::warn!(error = %e, "no live monitor");
            None
        }
    }
}

/// Begin capturing. Returns as soon as the session is running.
///
/// `replay_path` replays a recording instead of capturing from devices. It
/// drives the identical path, so the screen can be exercised without a headset
/// or a second person on the line.
#[tauri::command]
pub async fn start_live_session(
    app: tauri::AppHandle,
    state: State<'_, crate::AppState>,
    live: State<'_, LiveHandle>,
    input_device: Option<String>,
    output_device: Option<String>,
    replay_path: Option<String>,
) -> Result<LiveStarted, String> {
    if live.0.lock().map_err(|_| "live state poisoned")?.is_some() {
        return Err("a live session is already running".into());
    }

    let store = state.store.clone();
    let recordings = ca_store::default_cache_dir()
        .parent()
        .map(|p| p.join("live"))
        .unwrap_or_else(|| PathBuf::from("live"));

    // The call row exists from the first second, so a crash mid-call leaves
    // recoverable work rather than an orphaned file nothing knows about.
    let started = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let display = format!("Live call {started}");
    let call_id = store
        .upsert_call(&Call::new(format!("live-{started}"), "live", &display))
        .map_err(|e| e.to_string())?;
    let media_path = recordings.join(format!("{}.wav", call_id.as_str()));

    let source: Box<dyn AudioSource> = match &replay_path {
        Some(path) => Box::new(ReplaySource::new(path, 1.0)),
        None => Box::new(
            ca_live::capture::CaptureSource::new(input_device.as_deref(), output_device.as_deref())
                .map_err(|e| e.to_string())?,
        ),
    };

    // Eight threads matches what the batch transcriber uses and leaves the
    // machine responsive; the live tier is nowhere near saturating it.
    let (transcriber, server) = build_transcriber(&state.live_model, &state.whisper_server, 8);
    if let Some(handle) = &server {
        // The model takes seconds to load; requests sent before it is ready
        // look like a broken transcriber rather than a slow start.
        if let Err(e) = handle
            .wait_until_ready(std::time::Duration::from_secs(60))
            .await
        {
            tracing::warn!(error = %e, "whisper server never became ready");
        }
    }
    let transcribing = server.is_some();
    let advisor = build_advisor(&store);
    let advising = advisor.is_some();

    // Built here rather than in the spawned task: `State` cannot cross an
    // await boundary, and a missing whisper binary should fail the start
    // rather than surface an hour later when the call ends.
    let asr = crate::build_transcriber(&state)?;

    let stop = Arc::new(AtomicBool::new(false));
    *live.0.lock().map_err(|_| "live state poisoned")? = Some(LiveState {
        stop: stop.clone(),
        call_id: call_id.clone(),
        _server: server,
    });

    let cfg = LiveConfig::new(call_id.clone(), media_path);
    let emitter = app.clone();
    let on_event = Arc::new(move |event: LiveEvent| {
        let _ = emitter.emit("live:event", &event);
    });

    let session_store = store.clone();
    let finished_app = app.clone();
    let finished_id = call_id.clone();
    tauri::async_runtime::spawn(async move {
        let outcome = LiveSession::run(
            cfg,
            session_store.clone(),
            source,
            transcriber,
            advisor,
            stop,
            on_event,
        )
        .await;

        match outcome {
            Ok(outcome) => {
                // A finished session is just a call. Everything from here is
                // the ordinary pipeline: decode, transcribe properly, redact,
                // run the battery.
                if let Err(e) = session_store
                    .attach_media(&outcome.call_id, &outcome.media_path.display().to_string())
                {
                    tracing::error!(error = %e, "could not attach the recording to the call");
                }
                analyse_finished_call(finished_app, session_store, outcome.call_id, asr).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "live session failed");
                let _ = finished_app.emit(
                    "live:event",
                    &LiveEvent::Warning {
                        message: format!("Live session ended unexpectedly: {e}"),
                    },
                );
                let _ = finished_app.emit("pipeline:runFinished", finished_id.as_str().to_string());
            }
        }
    });

    Ok(LiveStarted {
        call_id: call_id.as_str().to_string(),
        transcribing,
        advising,
    })
}

/// Stop capturing. The battery then runs in the background.
#[tauri::command]
pub fn stop_live_session(live: State<'_, LiveHandle>) -> Result<Option<String>, String> {
    let mut guard = live.0.lock().map_err(|_| "live state poisoned")?;
    let Some(session) = guard.take() else {
        return Ok(None);
    };
    session.stop.store(true, Ordering::Relaxed);
    Ok(Some(session.call_id.as_str().to_string()))
}

/// Take a finished live call through the normal pipeline.
async fn analyse_finished_call(
    app: tauri::AppHandle,
    store: Arc<Store>,
    call_id: CallId,
    asr: Arc<ca_asr::WhisperCliTranscriber>,
) {
    let cfg: AppSettings = store
        .get_setting(SETTINGS_KEY)
        .ok()
        .flatten()
        .unwrap_or_default();

    let (key, _) = secrets::resolve_key(cfg.provider);
    let analyzer = if key.is_some() || !cfg.provider.requires_api_key() {
        ca_llm::build_provider(&cfg.spec(), key.as_deref())
            .ok()
            .map(|llm| {
                Arc::new(Analyzer::new(
                    llm,
                    ca_agents::QaScorecardAgent::new(ca_agents::Rubric::default()),
                    cfg.provider.is_local(),
                ))
            })
    } else {
        None
    };

    let target = if analyzer.is_some() {
        CallStage::Analyzed
    } else {
        CallStage::Redacted
    };
    let opts = PipelineOptions {
        concurrency: 1,
        only_calls: vec![call_id.clone()],
        target_stage: target,
        ..Default::default()
    };

    let run_id = match store.start_run("live", "Live call", target.as_str(), Some(1), 1) {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "could not record the live run");
            return;
        }
    };

    let mut pipeline = Pipeline::new(store.clone(), asr);
    if let Some(a) = analyzer {
        pipeline = pipeline.with_analyzer(a);
    }
    let (mut processed, mut failed) = (0usize, 0usize);
    let report = pipeline
        .run(&opts, |p| {
            crate::record(&store, &run_id, &p, &mut processed, &mut failed);
            let _ = app.emit("pipeline:progress", crate::progress_payload(&p));
        })
        .await;
    if let Ok(r) = &report {
        processed = r.processed;
        failed = r.failed;
    }
    let _ = store.finish_run(&run_id, processed, failed);
    let _ = app.emit("pipeline:runFinished", run_id.as_str().to_string());
}
