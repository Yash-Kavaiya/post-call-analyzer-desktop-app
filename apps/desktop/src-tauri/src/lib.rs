//! Desktop shell.
//!
//! A thin command layer over the `ca-*` crates. Deliberately thin: every
//! behaviour lives in a library crate so the CLI, this app, and any future
//! server share one implementation rather than three that drift.

use ca_asr::{WhisperCliConfig, WhisperCliTranscriber};
use ca_config::{self as settings, AppSettings, ProviderInfo, ProviderTest, SETTINGS_KEY};
use ca_core::call::CallStage;
use ca_core::ids::CallId;
use ca_ingest::{CloudProvider, CloudSpec, LocalFolderSource, ObjectStoreSource};
use ca_llm::secrets;
use ca_llm::ProviderKind;
use ca_pipeline::{Analyzer, Pipeline, PipelineOptions, Progress};
use ca_store::{CallSummary, Startup, Stats, Store};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

/// Everything the commands need, resolved once at startup.
mod live;

pub(crate) struct AppState {
    store: Arc<Store>,
    whisper_binary: PathBuf,
    whisper_model: PathBuf,
    /// `whisper-server`, used only by live assist. Separate from the CLI binary
    /// because the live tier needs a warm model between chunks.
    whisper_server: PathBuf,
    /// The smaller, faster weights the live tier transcribes with. The
    /// authoritative pass still uses `whisper_model`.
    live_model: PathBuf,
}

/// Errors cross the IPC boundary as strings; the UI shows them verbatim, so
/// they must read like something a user can act on.
type CmdResult<T> = std::result::Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoverResult {
    total: usize,
    added: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CallDetail {
    id: String,
    display_name: String,
    duration_secs: Option<f64>,
    features: Option<ca_core::audio::AcousticFeatures>,
    segments: Vec<SegmentDto>,
    speakers: Vec<ca_core::transcript::Speaker>,
    engine: Option<String>,
    model: Option<String>,
    /// Absolute path, so the UI can play the original audio.
    media_path: Option<String>,
    /// Talk ratio, interruptions, silences, keywords and the rest.
    ///
    /// Computed on read from the transcript rather than stored: it is
    /// arithmetic over timings, so recomputing costs microseconds and can never
    /// go stale against a transcript that was re-run.
    metrics: Option<ca_metrics::ConversationMetrics>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentDto {
    id: String,
    speaker_index: u8,
    role: String,
    start: f64,
    end: f64,
    text: String,
    redacted_text: Option<String>,
    words: Vec<ca_core::transcript::Word>,
    confidence: Option<f32>,
    language: Option<String>,
}

#[tauri::command]
fn get_stats(state: State<'_, AppState>) -> CmdResult<Stats> {
    state.store.stats().map_err(err)
}

#[tauri::command]
fn list_calls(
    state: State<'_, AppState>,
    offset: usize,
    limit: usize,
    search: Option<String>,
) -> CmdResult<Vec<CallSummary>> {
    state
        .store
        .list_calls(offset, limit, search.as_deref())
        .map_err(err)
}

#[tauri::command]
fn get_call(state: State<'_, AppState>, call_id: String) -> CmdResult<CallDetail> {
    let id = CallId::from(call_id);
    let transcript = state.store.load_transcript(&id).map_err(err)?;
    let features = state.store.features(&id).map_err(err)?;
    let media_path = state.store.media_path(&id).map_err(err)?;

    let summaries = state.store.list_calls(0, 1_000_000, None).map_err(err)?;
    let summary = summaries
        .into_iter()
        .find(|c| c.id == id.as_str())
        .ok_or_else(|| format!("call {id} not found"))?;

    let metrics = transcript.as_ref().map(ca_metrics::analyse);

    let (segments, speakers, engine, model) = match transcript {
        Some(t) => (
            t.segments
                .into_iter()
                .map(|s| SegmentDto {
                    id: s.id.as_str().to_string(),
                    speaker_index: s.speaker_index,
                    role: match s.role {
                        ca_core::transcript::SpeakerRole::Agent => "agent",
                        ca_core::transcript::SpeakerRole::Customer => "customer",
                        ca_core::transcript::SpeakerRole::Unknown => "unknown",
                    }
                    .to_string(),
                    start: s.start,
                    end: s.end,
                    text: s.text,
                    redacted_text: s.redacted_text,
                    words: s.words,
                    confidence: s.confidence,
                    language: s.language,
                })
                .collect(),
            t.speakers,
            Some(t.engine),
            Some(t.model),
        ),
        None => (vec![], vec![], None, None),
    };

    Ok(CallDetail {
        id: summary.id,
        display_name: summary.display_name,
        duration_secs: summary.duration_secs,
        features,
        segments,
        speakers,
        engine,
        model,
        media_path,
        metrics,
    })
}

#[tauri::command]
fn get_waveform(
    state: State<'_, AppState>,
    call_id: String,
) -> CmdResult<Option<ca_core::audio::WaveformPeaks>> {
    state.store.waveform(&CallId::from(call_id)).map_err(err)
}

#[tauri::command]
fn get_scorecard(
    state: State<'_, AppState>,
    call_id: String,
) -> CmdResult<Option<serde_json::Value>> {
    state
        .store
        .load_analysis(
            &CallId::from(call_id),
            ca_core::analysis::AgentKind::QaScorecard,
        )
        .map_err(err)
}

#[tauri::command]
async fn discover_folder(state: State<'_, AppState>, path: String) -> CmdResult<DiscoverResult> {
    let source = LocalFolderSource::new(&path);
    let asr = build_transcriber(&state)?;
    let pipeline = Pipeline::new(state.store.clone(), asr);
    let (total, added) = pipeline.discover(&source).await.map_err(err)?;
    Ok(DiscoverResult { total, added })
}

/// Register a single file, or a folder. The same command handles both because
/// the source does: pointing at one file is a scan of a folder holding only it.
#[tauri::command]
async fn discover_path(state: State<'_, AppState>, path: String) -> CmdResult<DiscoverResult> {
    let source = LocalFolderSource::new(&path);
    let asr = build_transcriber(&state)?;
    let pipeline = Pipeline::new(state.store.clone(), asr);
    let (total, added) = pipeline.discover(&source).await.map_err(err)?;
    Ok(DiscoverResult { total, added })
}

/// The object-storage services this build can read, for the source picker.
#[tauri::command]
fn list_cloud_providers() -> Vec<serde_json::Value> {
    CloudProvider::all()
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.as_str(),
                "label": p.label(),
            })
        })
        .collect()
}

/// Scan a public bucket and register what it holds.
///
/// Nothing is downloaded here. Objects are pulled lazily when a batch reaches
/// them, so scanning a bucket of thousands is a listing call, not a transfer.
#[tauri::command]
async fn discover_cloud(state: State<'_, AppState>, spec: CloudSpec) -> CmdResult<DiscoverResult> {
    spec.validate()?;
    let source = ObjectStoreSource::new(spec, ca_store::default_cache_dir()).map_err(err)?;
    let asr = build_transcriber(&state)?;
    let pipeline = Pipeline::new(state.store.clone(), asr);
    let (total, added) = pipeline.discover(&source).await.map_err(err)?;
    Ok(DiscoverResult { total, added })
}

/// Kick off a batch. Progress streams back as `pipeline:progress` events
/// rather than blocking the call, so the UI stays responsive across a run that
/// may last hours.
#[tauri::command]
async fn run_batch(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    limit: Option<usize>,
    concurrency: Option<usize>,
) -> CmdResult<()> {
    let asr = build_transcriber(&state)?;
    let store = state.store.clone();
    let concurrency = concurrency.unwrap_or(2);
    let opts = PipelineOptions {
        limit,
        concurrency,
        target_stage: CallStage::Redacted,
        ..Default::default()
    };

    // Opened before the work starts, so a run that is killed on its first call
    // still appears in the history as an interrupted attempt rather than
    // vanishing.
    let run_id = store
        .start_run(
            "transcribe",
            "Transcribe & redact",
            CallStage::Redacted.as_str(),
            limit,
            concurrency,
        )
        .map_err(err)?;

    tauri::async_runtime::spawn(async move {
        let pipeline = Pipeline::new(store.clone(), asr);
        let (mut processed, mut failed) = (0usize, 0usize);

        let report = pipeline
            .run(&opts, |p| {
                // Persist first, emit second. A subscriber that misses an event
                // can reload; a history with a hole in it cannot be recovered.
                record(&store, &run_id, &p, &mut processed, &mut failed);
                let _ = app.emit("pipeline:progress", progress_payload(&p));
            })
            .await;

        if let Ok(r) = &report {
            processed = r.processed;
            failed = r.failed;
        }
        let _ = store.finish_run(&run_id, processed, failed);
        let _ = app.emit("pipeline:runFinished", run_id.as_str().to_string());
    });

    Ok(())
}

/// Mirror one progress event into the run history.
///
/// Failures are swallowed deliberately: a batch that cannot write its own
/// bookkeeping should still transcribe. The alternative — aborting a 60-hour
/// run because a history row failed — trades the valuable thing for the
/// record of it.
pub(crate) fn record(
    store: &Store,
    run: &ca_core::ids::JobId,
    p: &Progress,
    processed: &mut usize,
    failed: &mut usize,
) {
    let outcome = match p {
        Progress::Discovered { total, .. } => store.set_run_total(run, *total),
        Progress::Started { call, name } => store.run_item_started(run, call, name),
        Progress::Stage { call, stage } => store.run_item_stage(run, call, stage.as_str()),
        Progress::Finished { call, secs, .. } => {
            *processed += 1;
            store.run_item_finished(run, call, Some(*secs))
        }
        Progress::Failed { call, error, .. } => {
            *failed += 1;
            store.run_item_failed(run, call, error)
        }
        Progress::Done { .. } => Ok(()),
    };
    if let Err(e) = outcome {
        tracing::warn!(error = %e, "could not record run history");
    }
}

// ------------------------------------------------------------------ settings

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> CmdResult<AppSettings> {
    Ok(state
        .store
        .get_setting::<AppSettings>(SETTINGS_KEY)
        .map_err(err)?
        .unwrap_or_default())
}

#[tauri::command]
fn save_settings(state: State<'_, AppState>, settings: AppSettings) -> CmdResult<AppSettings> {
    // Validated here rather than only in the form: the command is the boundary,
    // and a stored configuration that cannot run is worse than a rejected one.
    settings.validate()?;
    state
        .store
        .put_setting(SETTINGS_KEY, &settings)
        .map_err(err)?;
    Ok(settings)
}

/// Every vendor, with its endpoint, suggested models and whether a key is
/// already available. Never includes a key.
#[tauri::command]
fn list_providers() -> CmdResult<Vec<ProviderInfo>> {
    Ok(settings::provider_catalogue())
}

/// Store a BYOK key in the OS credential manager. An empty value clears it.
///
/// The key is taken by value and dropped at the end of this function; it is
/// never written to the database, never logged, and never sent back.
#[tauri::command]
fn set_api_key(provider: String, key: String) -> CmdResult<Vec<ProviderInfo>> {
    let kind =
        ProviderKind::parse(&provider).ok_or_else(|| format!("unknown provider `{provider}`"))?;
    secrets::store_key(kind, &key).map_err(err)?;
    Ok(settings::provider_catalogue())
}

#[tauri::command]
fn clear_api_key(provider: String) -> CmdResult<Vec<ProviderInfo>> {
    let kind =
        ProviderKind::parse(&provider).ok_or_else(|| format!("unknown provider `{provider}`"))?;
    secrets::delete_key(kind).map_err(err)?;
    Ok(settings::provider_catalogue())
}

/// Send the smallest possible completion, so a bad key or an unreachable
/// endpoint is found by pressing a button rather than three hours into a batch.
#[tauri::command]
async fn test_provider(settings: AppSettings) -> CmdResult<ProviderTest> {
    use ca_core::provider::{ChatRequest, Message};

    if let Err(e) = settings.validate() {
        return Ok(ProviderTest {
            ok: false,
            model: None,
            latency_ms: None,
            message: e,
        });
    }

    let (key, source) = secrets::resolve_key(settings.provider);
    if settings.provider.requires_api_key() && key.is_none() {
        return Ok(ProviderTest {
            ok: false,
            model: None,
            latency_ms: None,
            message: format!(
                "No API key for {}. Paste one above, or set ${}.",
                settings.provider.label(),
                settings.provider.env_var().unwrap_or("")
            ),
        });
    }

    let provider = match ca_llm::build_provider(&settings.spec(), key.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            return Ok(ProviderTest {
                ok: false,
                model: None,
                latency_ms: None,
                message: e.to_string(),
            })
        }
    };

    let req = ChatRequest::new(vec![Message::user("Reply with the single word: ready")]);
    match provider.complete(req).await {
        Ok(resp) => Ok(ProviderTest {
            ok: true,
            model: Some(resp.model.clone()),
            latency_ms: Some(resp.latency_ms),
            message: format!(
                "{} answered in {} ms as `{}` (key from {}).",
                settings.provider.label(),
                resp.latency_ms,
                resp.model,
                match source {
                    secrets::KeySource::CredentialStore => "Credential Manager",
                    secrets::KeySource::Environment => "the environment",
                    secrets::KeySource::NotRequired => "no key needed",
                    secrets::KeySource::Missing => "nowhere",
                }
            ),
        }),
        Err(e) => Ok(ProviderTest {
            ok: false,
            model: None,
            latency_ms: None,
            message: e.to_string(),
        }),
    }
}

/// Score calls with the configured provider — the task the settings exist for.
///
/// Shares the pipeline, the progress events and the run history with the
/// transcription batch, so the ingest screen shows this run exactly as it shows
/// the others.
#[tauri::command]
async fn run_scoring(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> CmdResult<()> {
    let cfg: AppSettings = state
        .store
        .get_setting(SETTINGS_KEY)
        .map_err(err)?
        .unwrap_or_default();
    cfg.validate()?;

    let (key, _) = secrets::resolve_key(cfg.provider);
    if cfg.provider.requires_api_key() && key.is_none() {
        return Err(format!(
            "No API key for {}. Add one in Settings before scoring.",
            cfg.provider.label()
        ));
    }

    let llm = ca_llm::build_provider(&cfg.spec(), key.as_deref()).map_err(err)?;
    let rubric = ca_agents::Rubric::default();
    let analyzer = Arc::new(Analyzer::new(
        llm,
        ca_agents::QaScorecardAgent::new(rubric),
        cfg.provider.is_local(),
    ));

    let asr = build_transcriber(&state)?;
    let store = state.store.clone();
    let opts = PipelineOptions {
        limit,
        concurrency: cfg.concurrency,
        target_stage: CallStage::Analyzed,
        require_stage: Some(settings::required_stage(&cfg)),
        ..Default::default()
    };

    let run_id = store
        .start_run(
            "score",
            &format!("QA scorecard · {} / {}", cfg.provider.label(), cfg.model),
            CallStage::Analyzed.as_str(),
            limit,
            cfg.concurrency,
        )
        .map_err(err)?;

    tauri::async_runtime::spawn(async move {
        let pipeline = Pipeline::new(store.clone(), asr).with_analyzer(analyzer);
        let (mut processed, mut failed) = (0usize, 0usize);

        let report = pipeline
            .run(&opts, |p| {
                record(&store, &run_id, &p, &mut processed, &mut failed);
                let _ = app.emit("pipeline:progress", progress_payload(&p));
            })
            .await;

        if let Ok(r) = &report {
            processed = r.processed;
            failed = r.failed;
        }
        let _ = store.finish_run(&run_id, processed, failed);
        let _ = app.emit("pipeline:runFinished", run_id.as_str().to_string());
    });

    Ok(())
}

/// Take one call through the pipeline: transcribe, diarize, redact, and then
/// score it if a provider is configured.
///
/// The batch runner answers "process what is outstanding". This answers "process
/// *this*", which is the question someone asks while looking at a call that has
/// no transcript yet — and it runs the stages in order rather than making them
/// find the right screen for each one.
#[tauri::command]
async fn process_call(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    call_id: String,
    analyze: bool,
) -> CmdResult<()> {
    let id = CallId::from(call_id);
    let asr = build_transcriber(&state)?;
    let store = state.store.clone();

    // Analysis is best-effort: a call should still get its transcript when no
    // model is configured, rather than refusing the whole request.
    let cfg: AppSettings = store
        .get_setting(SETTINGS_KEY)
        .map_err(err)?
        .unwrap_or_default();
    let analyzer = if analyze {
        match secrets::resolve_key(cfg.provider) {
            (key, _) if key.is_some() || !cfg.provider.requires_api_key() => {
                match ca_llm::build_provider(&cfg.spec(), key.as_deref()) {
                    Ok(llm) => Some(Arc::new(Analyzer::new(
                        llm,
                        ca_agents::QaScorecardAgent::new(ca_agents::Rubric::default()),
                        cfg.provider.is_local(),
                    ))),
                    Err(e) => {
                        tracing::warn!(error = %e, "no analyzer; transcribing only");
                        None
                    }
                }
            }
            _ => None,
        }
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
        only_calls: vec![id.clone()],
        target_stage: target,
        ..Default::default()
    };

    let run_id = store
        .start_run("call", "Single call", target.as_str(), Some(1), 1)
        .map_err(err)?;

    tauri::async_runtime::spawn(async move {
        let mut pipeline = Pipeline::new(store.clone(), asr);
        if let Some(a) = analyzer {
            pipeline = pipeline.with_analyzer(a);
        }
        let (mut processed, mut failed) = (0usize, 0usize);
        let report = pipeline
            .run(&opts, |p| {
                record(&store, &run_id, &p, &mut processed, &mut failed);
                let _ = app.emit("pipeline:progress", progress_payload(&p));
            })
            .await;
        if let Ok(r) = &report {
            processed = r.processed;
            failed = r.failed;
        }
        let _ = store.finish_run(&run_id, processed, failed);
        let _ = app.emit("pipeline:runFinished", run_id.as_str().to_string());
    });

    Ok(())
}

/// Ask a question about one call, answered from its transcript.
///
/// Uses the configured analysis model. The redaction guard applies exactly as
/// it does for the agents: a remote model is handed the redacted transcript, so
/// a question about a phone number gets an answer about `[PHONE_1]`.
#[tauri::command]
async fn ask_call(
    state: State<'_, AppState>,
    call_id: String,
    question: String,
    history: Vec<ca_agents::CopilotTurn>,
) -> CmdResult<ca_agents::CopilotAnswer> {
    let id = CallId::from(call_id);
    let transcript = state
        .store
        .load_transcript(&id)
        .map_err(err)?
        .ok_or_else(|| "This call has no transcript yet.".to_string())?;

    let cfg: AppSettings = state
        .store
        .get_setting(SETTINGS_KEY)
        .map_err(err)?
        .unwrap_or_default();
    cfg.validate()?;

    let (key, _) = secrets::resolve_key(cfg.provider);
    if cfg.provider.requires_api_key() && key.is_none() {
        return Err(format!(
            "No API key for {}. Add one in Settings.",
            cfg.provider.label()
        ));
    }
    let provider = ca_llm::build_provider(&cfg.spec(), key.as_deref()).map_err(err)?;

    // Same rule as the battery: nothing unredacted leaves the machine.
    let redacted = transcript
        .segments
        .iter()
        .all(|s| s.redacted_text.is_some());
    if !cfg.provider.is_local() && !redacted {
        return Err(
            "This call has not been redacted yet, so it cannot be sent to a remote model."
                .to_string(),
        );
    }

    let (answer, _resp) = ca_agents::CallCopilot
        .ask(provider.as_ref(), &transcript, &question, &history)
        .await
        .map_err(err)?;
    Ok(answer)
}

/// Every agent's output for one call, with what it cost and whether it worked.
#[tauri::command]
fn get_agent_runs(
    state: State<'_, AppState>,
    call_id: String,
) -> CmdResult<Vec<ca_store::AgentRun>> {
    state.store.agent_runs(&CallId::from(call_id)).map_err(err)
}

/// The compliance ledger for one call, worst finding first.
///
/// Separate from `get_agent_runs` even though the compliance agent's payload
/// carries the same list, because the ledger is the durable record: it survives
/// the agent being re-run under a different model, and it is what the corpus
/// risk register is counted from.
#[tauri::command]
fn get_findings(
    state: State<'_, AppState>,
    call_id: String,
) -> CmdResult<Vec<ca_store::StoredFinding>> {
    state.store.findings(&CallId::from(call_id)).map_err(err)
}

/// Which rules the corpus breaks, and how many calls break each.
#[tauri::command]
fn get_risk_register(state: State<'_, AppState>) -> CmdResult<Vec<ca_store::FindingCount>> {
    state.store.findings_summary(20).map_err(err)
}

/// Aggregate view across every analysed call.
///
/// Arithmetic over facts the agents already extracted, not a model summarising
/// summaries — so it is free, instant, and identical on every run.
#[tauri::command]
fn get_corpus_report(state: State<'_, AppState>) -> CmdResult<ca_insights::CorpusReport> {
    let facts = state.store.corpus_facts(50_000).map_err(err)?;
    Ok(ca_insights::analyse_corpus(&facts))
}

/// Health and spend per agent across the corpus.
///
/// A batch that silently degraded — one agent failing on every call, or a
/// router substituting a cheaper model — looks identical to a healthy one from
/// the call list, because the calls still reach `analyzed`. This is where that
/// shows up.
#[tauri::command]
fn get_agent_health(state: State<'_, AppState>) -> CmdResult<Vec<ca_store::AgentHealth>> {
    state.store.agent_health().map_err(err)
}

/// Past runs, newest first.
#[tauri::command]
fn list_runs(
    state: State<'_, AppState>,
    offset: usize,
    limit: usize,
) -> CmdResult<Vec<ca_store::RunSummary>> {
    state.store.list_runs(offset, limit).map_err(err)
}

/// One run's calls.
#[tauri::command]
fn get_run_items(
    state: State<'_, AppState>,
    run_id: String,
    offset: usize,
    limit: usize,
) -> CmdResult<Vec<ca_store::RunItem>> {
    state
        .store
        .run_items(&ca_core::ids::JobId::from(run_id), offset, limit)
        .map_err(err)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    kind: String,
    call_id: String,
    name: String,
    stage: Option<String>,
    secs: Option<f64>,
    error: Option<String>,
}

pub(crate) fn progress_payload(p: &Progress) -> ProgressPayload {
    let base = |kind: &str| ProgressPayload {
        kind: kind.to_string(),
        call_id: String::new(),
        name: String::new(),
        stage: None,
        secs: None,
        error: None,
    };
    match p {
        Progress::Discovered { total, .. } => ProgressPayload {
            name: total.to_string(),
            ..base("discovered")
        },
        Progress::Started { call, name } => ProgressPayload {
            call_id: call.as_str().to_string(),
            name: name.clone(),
            ..base("started")
        },
        Progress::Stage { call, stage } => ProgressPayload {
            call_id: call.as_str().to_string(),
            stage: Some(stage.as_str().to_string()),
            ..base("stage")
        },
        Progress::Finished { call, name, secs } => ProgressPayload {
            call_id: call.as_str().to_string(),
            name: name.clone(),
            secs: Some(*secs),
            ..base("finished")
        },
        Progress::Failed { call, name, error } => ProgressPayload {
            call_id: call.as_str().to_string(),
            name: name.clone(),
            error: Some(error.clone()),
            ..base("failed")
        },
        Progress::Done { processed, failed } => ProgressPayload {
            name: format!("{processed}/{failed}"),
            ..base("done")
        },
    }
}

/// Report what the app found at startup so the UI can warn rather than fail
/// mysteriously when the model or binary is missing.
#[tauri::command]
fn get_startup(state: State<'_, AppState>) -> CmdResult<Startup> {
    Ok(Startup {
        whisper_binary: state.whisper_binary.display().to_string(),
        whisper_binary_present: state.whisper_binary.exists(),
        whisper_model: state.whisper_model.display().to_string(),
        whisper_model_present: state.whisper_model.exists(),
        english_only: state
            .whisper_model
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains(".en"))
            .unwrap_or(false),
    })
}

pub(crate) fn build_transcriber(
    state: &State<'_, AppState>,
) -> CmdResult<Arc<WhisperCliTranscriber>> {
    let cfg = WhisperCliConfig::new(&state.whisper_binary, &state.whisper_model);
    WhisperCliTranscriber::new(cfg).map(Arc::new).map_err(err)
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Data lives beside the app, not in the repo, so a packaged build
            // behaves the same as a dev one.
            // Same file the CLI uses, so a batch started in the terminal
            // shows up in the window and vice versa.
            let db_path = ca_store::default_db_path();
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let store = Arc::new(Store::open(&db_path).expect("failed to open the database"));

            // A run still marked `running` belongs to a process that is gone —
            // this one is only just starting. Left alone it would show a
            // spinner in the history for ever.
            match store.close_stale_runs() {
                Ok(0) => {}
                Ok(n) => tracing::info!(count = n, "marked abandoned runs as interrupted"),
                Err(e) => tracing::warn!(error = %e, "could not close stale runs"),
            }

            app.manage(AppState {
                store,
                whisper_binary: locate(app.handle(), "vendor/whisper/Release/whisper-cli.exe"),
                whisper_model: locate(app.handle(), "models/ggml-small.en.bin"),
                whisper_server: locate(app.handle(), "vendor/whisper/Release/whisper-server.exe"),
                live_model: locate(app.handle(), "models/ggml-base.en.bin"),
            });
            app.manage(live::LiveHandle::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_stats,
            list_calls,
            get_call,
            get_waveform,
            get_scorecard,
            discover_folder,
            discover_path,
            discover_cloud,
            list_cloud_providers,
            run_batch,
            get_startup,
            list_runs,
            get_run_items,
            get_settings,
            save_settings,
            list_providers,
            set_api_key,
            clear_api_key,
            test_provider,
            run_scoring,
            process_call,
            ask_call,
            get_agent_runs,
            get_agent_health,
            get_corpus_report,
            get_findings,
            get_risk_register,
            live::list_audio_devices,
            live::start_live_session,
            live::stop_live_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Find a bundled asset, wherever this build happens to keep it.
///
/// Three locations, in order of how much they can be trusted:
///
/// 1. **The resource directory.** Where the installer puts things. This is the
///    only one that holds on a machine that has never seen the source.
/// 2. **The app's data directory.** Where a large asset that is deliberately
///    not in the installer goes — the speech model is 465 MB, so shipping it
///    inside a download is a worse trade than fetching it once.
/// 3. **The checkout**, debug builds only.
///
/// The compile-time `CARGO_MANIFEST_DIR` used to be the *only* rule. It names a
/// path on whichever machine ran the compiler, so an installed build looked for
/// its transcriber inside a source tree that isn't there — working perfectly on
/// the build machine and nowhere else.
///
/// A path is returned even when nothing matches: the startup payload reports
/// what was looked for and whether it is present, so a missing model surfaces
/// in the window as a named path rather than as a failure at first use.
fn locate(app: &tauri::AppHandle, relative: &str) -> PathBuf {
    use tauri::Manager;

    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join(relative));
    }
    if let Ok(dir) = app.path().app_data_dir() {
        candidates.push(dir.join(relative));
    }
    if cfg!(debug_assertions) {
        if let Some(root) = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .map(Path::to_path_buf)
        {
            candidates.push(root.join(relative));
        }
    }

    let fallback = candidates
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from(relative));

    candidates
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or(fallback)
}
