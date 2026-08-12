//! The analysis stage, end to end, against a stub model.
//!
//! What this pins down is the part no vendor test can: that scoring an
//! already-transcribed corpus reads the stored transcript instead of decoding
//! audio again, that the result is persisted where the dashboard looks for it,
//! and that the stage marker advances so a resumed run does not pay twice.
//!
//! The model is a stub on purpose. Wire formats are tested in `ca-llm`; what is
//! under test here is the orchestration around them.

use ca_core::analysis::AgentKind;
use ca_core::call::{Call, CallStage};
use ca_core::error::Result;
use ca_core::ids::{CallId, SegmentId};
use ca_core::provider::{Caps, ChatRequest, ChatResponse, LlmProvider};
use ca_core::transcript::{Segment, SpeakerRole, Transcript};
use ca_store::Store;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Model requests per call.
///
/// The analysis stage runs one agent per [`AgentKind`], so "did this call reach
/// the model" is a whole battery of requests, not one. Derived rather than
/// written down: adding an agent used to silently break three tests that had a
/// number hardcoded in them.
fn agents_per_call() -> usize {
    AgentKind::all().len()
}

/// Returns a fixed scorecard and counts how often it was asked.
struct StubModel {
    calls: AtomicUsize,
    /// Captured so a test can assert what actually left the machine.
    last_prompt: std::sync::Mutex<String>,
}

impl StubModel {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            last_prompt: std::sync::Mutex::new(String::new()),
        }
    }
}

const STUB_SCORECARD: &str = concat!(
    r#"{"criteria":["#,
    r#"{"key":"greeting","score":1.0,"reasoning":"Named the company.","#,
    r#""citations":[{"segment_id":"seg_a","quote":"Thanks for calling"}]},"#,
    r#"{"key":"resolution","score":0.5,"reasoning":"Partly resolved.","#,
    r#""citations":[{"segment_id":"seg_b","quote":"I will check"}]}"#,
    r#"],"summary":"Adequate handling."}"#,
);

#[async_trait::async_trait]
impl LlmProvider for StubModel {
    fn name(&self) -> &str {
        "stub"
    }
    fn model(&self) -> &str {
        "stub-1"
    }
    fn capabilities(&self) -> Caps {
        Caps {
            json_mode: true,
            input_cost_per_mtok: Some(1.0),
            output_cost_per_mtok: Some(2.0),
            ..Caps::default()
        }
    }
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_prompt.lock().unwrap() = req
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ChatResponse {
            content: STUB_SCORECARD.to_string(),
            input_tokens: 1_000,
            output_tokens: 200,
            model: "stub-1".into(),
            latency_ms: 12,
        })
    }
}

fn segment(id: &str, start: f64, text: &str) -> Segment {
    Segment {
        id: SegmentId::from(id),
        speaker_index: 0,
        role: SpeakerRole::Agent,
        start,
        end: start + 4.0,
        text: text.into(),
        redacted_text: Some(text.replace("9876543210", "[PHONE_1]")),
        words: vec![],
        confidence: None,
        language: None,
    }
}

/// A store holding one call that is already transcribed, diarized and redacted.
fn store_with_redacted_call() -> (Arc<Store>, CallId) {
    let store = Arc::new(Store::in_memory().unwrap());

    let mut call = Call::new("stub://call-a", "test", "call-a");
    call.media_path = Some("stub://call-a".into());
    let id = store.upsert_call(&call).unwrap();

    let transcript = Transcript {
        call_id: id.clone(),
        segments: vec![
            segment("seg_a", 0.0, "Thanks for calling Acme, this is Ravi."),
            segment("seg_b", 6.0, "I will check and call you on 9876543210."),
        ],
        speakers: vec![],
        primary_language: Some("en".into()),
        languages: vec!["en".into()],
        engine: "stub".into(),
        model: "stub".into(),
        duration_secs: 42.0,
    };
    store.save_transcript(&transcript).unwrap();
    store.set_stage(&id, CallStage::Redacted, None).unwrap();

    (store, id)
}

/// A transcriber whose binary exists but is not whisper.
///
/// The paths have to exist because the transcriber validates them up front —
/// that check is what stops a 60-hour batch from starting against a missing
/// model. Pointing them at empty files satisfies construction while guaranteeing
/// that any attempt to actually transcribe fails loudly, which is precisely the
/// regression these tests are here to catch.
fn stub_transcriber() -> Arc<ca_asr::WhisperCliTranscriber> {
    let dir = std::env::temp_dir().join("ca-pipeline-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("not-whisper.exe");
    let model = dir.join("not-a-model.bin");
    for p in [&bin, &model] {
        if !p.exists() {
            std::fs::write(p, b"").unwrap();
        }
    }

    Arc::new(ca_asr::WhisperCliTranscriber::new(ca_asr::WhisperCliConfig::new(bin, model)).unwrap())
}

fn pipeline_with(store: Arc<Store>, model: Arc<StubModel>) -> ca_pipeline::Pipeline {
    let analyzer = Arc::new(ca_pipeline::Analyzer::new(
        model,
        ca_agents::QaScorecardAgent::new(ca_agents::Rubric::default()),
        // Treated as remote, so the redaction guard is live.
        false,
    ));

    ca_pipeline::Pipeline::new(store, stub_transcriber()).with_analyzer(analyzer)
}

fn score_options() -> ca_pipeline::PipelineOptions {
    ca_pipeline::PipelineOptions {
        concurrency: 1,
        target_stage: CallStage::Analyzed,
        require_stage: Some(CallStage::Redacted),
        ..Default::default()
    }
}

#[tokio::test]
async fn scoring_a_redacted_call_persists_a_scorecard_without_re_transcribing() {
    let (store, id) = store_with_redacted_call();
    let model = Arc::new(StubModel::new());
    let pipeline = pipeline_with(store.clone(), model.clone());

    let report = pipeline.run(&score_options(), |_| {}).await.unwrap();

    assert_eq!(report.processed, 1, "{report:?}");
    assert_eq!(report.failed, 0);
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        agents_per_call(),
        "one call should run the whole battery exactly once"
    );

    assert_eq!(store.stage_of(&id).unwrap(), Some(CallStage::Analyzed));

    let payload = store
        .load_analysis(&id, AgentKind::QaScorecard)
        .unwrap()
        .expect("no scorecard persisted");
    let card: ca_core::analysis::Scorecard = serde_json::from_value(payload).unwrap();

    // greeting 1.0 (weight 1) and resolution 0.5 (weight 2) are evidenced; the
    // other three are absent from the stub's output and so excluded.
    assert!((card.overall - 66.667).abs() < 0.01, "got {}", card.overall);
    assert_eq!(card.criteria.len(), 5, "the rubric fixes the shape");
    assert_eq!(card.rubric_id, "default");
}

#[tokio::test]
async fn a_second_run_leaves_the_scored_call_alone() {
    let (store, _id) = store_with_redacted_call();
    let model = Arc::new(StubModel::new());
    let pipeline = pipeline_with(store.clone(), model.clone());

    pipeline.run(&score_options(), |_| {}).await.unwrap();
    let report = pipeline.run(&score_options(), |_| {}).await.unwrap();

    // The whole point of committing per stage: resuming a 1,968-call batch must
    // not re-bill every call that already succeeded.
    assert_eq!(
        report.discovered, 0,
        "already-scored call was selected again"
    );
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        agents_per_call(),
        "a resumed run re-billed a call that had already finished"
    );
}

#[tokio::test]
async fn restarting_at_analysis_rescores_without_touching_the_transcript() {
    let (store, id) = store_with_redacted_call();
    let model = Arc::new(StubModel::new());
    let pipeline = pipeline_with(store.clone(), model.clone());

    pipeline.run(&score_options(), |_| {}).await.unwrap();

    let opts = ca_pipeline::PipelineOptions {
        restart_from: Some(CallStage::Analyzed),
        ..score_options()
    };
    let report = pipeline.run(&opts, |_| {}).await.unwrap();

    assert_eq!(report.processed, 1);
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        agents_per_call() * 2,
        "re-score did not run the battery a second time"
    );
    assert_eq!(store.stage_of(&id).unwrap(), Some(CallStage::Analyzed));
}

#[tokio::test]
async fn untranscribed_calls_are_not_selected_by_an_analysis_run() {
    let (store, _id) = store_with_redacted_call();

    // A second call that has only been discovered. Selecting it would mean
    // starting whisper inside a metered, network-concurrency batch.
    let mut raw = Call::new("stub://call-b", "test", "call-b");
    raw.media_path = Some("stub://call-b".into());
    store.upsert_call(&raw).unwrap();

    let model = Arc::new(StubModel::new());
    let pipeline = pipeline_with(store.clone(), model.clone());
    let report = pipeline.run(&score_options(), |_| {}).await.unwrap();

    assert_eq!(report.discovered, 1, "an untranscribed call was picked up");
    assert_eq!(report.processed, 1);
}

#[tokio::test]
async fn only_redacted_text_reaches_the_model() {
    let (store, _id) = store_with_redacted_call();
    let model = Arc::new(StubModel::new());
    let pipeline = pipeline_with(store.clone(), model.clone());

    pipeline.run(&score_options(), |_| {}).await.unwrap();

    let prompt = model.last_prompt.lock().unwrap().clone();
    assert!(prompt.contains("[PHONE_1]"), "redacted token missing");
    assert!(
        !prompt.contains("9876543210"),
        "a real phone number was sent to a remote provider"
    );
}

#[tokio::test]
async fn an_analysis_target_without_an_analyzer_fails_before_any_work() {
    let (store, _id) = store_with_redacted_call();
    let pipeline = ca_pipeline::Pipeline::new(store, stub_transcriber());

    let err = pipeline.run(&score_options(), |_| {}).await.err().unwrap();
    assert!(format!("{err}").contains("no analyzer"), "got {err}");
}
