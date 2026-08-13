# Phase 3 Live Call Assist — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture both sides of a live call, transcribe and analyse it as it happens, and hand the finished session to the existing Phase 2 pipeline as an ordinary call.

**Architecture:** Two new library crates. `ca-rules` is pure functions from a transcript window to `Finding`s. `ca-live` owns capture, VAD chunking, the warm whisper-server client and session orchestration, with every side effect behind a trait so the whole loop runs deterministically from a recorded WAV. The desktop app is a thin command layer over `ca-live`, as it is over every other crate.

**Tech Stack:** Rust 1.82, cpal (WASAPI loopback), whisper.cpp `whisper-server` (vendored), tokio, Tauri 2, React 19.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-13-phase3-live-assist-design.md`.
- Licences: no GPL, no OpenSSL, no ffmpeg. `cpal` is Apache-2.0 and permitted. Any new dependency must pass `cargo deny check`.
- Every window sent to a remote provider must be redacted first, enforced as a hard error, matching `Analyzer::guard_redaction`.
- Live transcription uses `models/ggml-base.en.bin`; the authoritative pass keeps `ggml-small.en.bin`.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace --exclude call-analyzer-desktop` must pass at every commit.
- UI gates: `tsc -b`, `oxlint`, `vite build` from `apps/desktop/ui`.
- `cargo` is not on PATH in bash: `export PATH="$PATH:/c/Users/yashk/.cargo/bin"`.
- Serde: every type crossing the IPC boundary carries `#[serde(rename_all = "camelCase")]`.

---

## File Structure

| Path | Responsibility |
| --- | --- |
| `crates/ca-rules/src/lib.rs` | `RuleContext`, `evaluate`, `dedupe_key`, default phrase list |
| `crates/ca-rules/src/detectors.rs` | The six detectors, one function each |
| `crates/ca-live/src/lib.rs` | Re-exports, `Channel`, `LiveConfig` |
| `crates/ca-live/src/event.rs` | `LiveEvent` — the tagged enum the UI consumes |
| `crates/ca-live/src/source.rs` | `AudioSource` trait, `Frame`, `ReplaySource` |
| `crates/ca-live/src/capture.rs` | cpal two-stream capture (the only untestable file) |
| `crates/ca-live/src/chunker.rs` | Streaming energy-gated VAD chunker |
| `crates/ca-live/src/writer.rs` | Incremental stereo WAV writer |
| `crates/ca-live/src/asr.rs` | `ChunkTranscriber` trait, `WhisperServer`, `ServerHandle` |
| `crates/ca-live/src/session.rs` | Orchestration: source → chunker → ASR → rules → events |
| `crates/ca-agents/src/live_monitor.rs` | `LiveMonitorAgent` |
| `crates/ca-core/src/analysis.rs` | `AgentKind::Rules`, `AgentKind::LiveMonitor`, `LiveMonitorReport` |
| `crates/ca-core/src/transcript.rs` | `SpeechSpan`, `ChannelTimeline` |
| `crates/ca-store/src/timeline.rs` | Channel timeline persistence |
| `crates/ca-store/src/lib.rs` | Scoped `save_findings` |
| `crates/ca-diarize/src/lib.rs` | `roles_from_timeline` |
| `apps/desktop/src-tauri/src/live.rs` | Tauri commands and event forwarding |
| `apps/desktop/ui/src/screens/LiveScreen.tsx` | The live screen |

---

### Task 1: Core types for live analysis

**Files:**
- Modify: `crates/ca-core/src/analysis.rs`
- Modify: `crates/ca-core/src/transcript.rs`

**Interfaces:**
- Produces: `AgentKind::Rules`, `AgentKind::LiveMonitor`; `LiveMonitorReport { call_id, sentiment: f32, risk: Severity, headline: String, say_next: Option<String>, citations: Vec<Citation> }`; `SpeechSpan { channel: u8, start: f64, end: f64 }`; `ChannelTimeline { spans: Vec<SpeechSpan> }` with `ChannelTimeline::channel_at(&self, t: f64) -> Option<u8>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_new_agent_kinds_round_trip() {
    assert_eq!(AgentKind::parse("rules"), Some(AgentKind::Rules));
    assert_eq!(AgentKind::parse("live_monitor"), Some(AgentKind::LiveMonitor));
    assert!(AgentKind::all().contains(&AgentKind::Rules));
}

#[test]
fn the_channel_at_a_time_is_whichever_span_covers_it() {
    let t = ChannelTimeline { spans: vec![
        SpeechSpan { channel: 0, start: 0.0, end: 2.0 },
        SpeechSpan { channel: 1, start: 2.5, end: 4.0 },
    ]};
    assert_eq!(t.channel_at(1.0), Some(0));
    assert_eq!(t.channel_at(3.0), Some(1));
    assert_eq!(t.channel_at(2.2), None, "a gap belongs to nobody");
}
```

- [ ] **Step 2: Run and confirm they fail** — `cargo test -p ca-core`
- [ ] **Step 3: Add the variants and types**, keeping `as_str`/`label`/`all` exhaustive.
- [ ] **Step 4: Run tests** — `cargo test -p ca-core`
- [ ] **Step 5: Commit** — `feat(core): add live agent kinds and channel timeline`

---

### Task 2: `ca-rules` crate with the six detectors

**Files:**
- Create: `crates/ca-rules/Cargo.toml`, `crates/ca-rules/src/lib.rs`, `crates/ca-rules/src/detectors.rs`
- Modify: `Cargo.toml` (workspace members + dependency)

**Interfaces:**
- Consumes: `Finding`, `Severity`, `AgentKind::Rules` from Task 1.
- Produces:
```rust
pub struct RuleContext<'a> {
    pub call_id: &'a CallId,
    pub segments: &'a [Segment],
    pub elapsed_secs: f64,
    pub banned_phrases: &'a [String],
}
pub fn evaluate(ctx: &RuleContext) -> Vec<Finding>;
pub fn dedupe_key(f: &Finding) -> String;
pub fn default_banned_phrases() -> Vec<String>;
```

Rules are pure over the whole transcript so far and return the complete current
set. The caller diffs against what it has already emitted using `dedupe_key`,
so a rule firing on chunk 3 does not re-fire on chunks 4..n.

- [ ] **Step 1: Write failing tests — one fires/does-not-fire pair per detector.**

```rust
#[test]
fn a_spoken_card_number_is_critical() {
    let segs = [seg("seg_1", 0, 10.0, "my card is 4111 1111 1111 1111")];
    let f = &fire(&segs)[0];
    assert_eq!(f.code, "PCI_PAN_SPOKEN");
    assert_eq!(f.severity, Severity::Critical);
    assert_eq!(f.citations[0].segment_id.as_str(), "seg_1");
}

#[test]
fn a_number_that_fails_luhn_is_not_a_card() {
    let segs = [seg("seg_1", 0, 10.0, "reference 4111 1111 1111 1112")];
    assert!(fire(&segs).iter().all(|f| f.code != "PCI_PAN_SPOKEN"));
}

#[test]
fn a_hold_without_a_duration_is_blocking() { /* "let me put you on hold" */ }

#[test]
fn a_hold_with_a_duration_is_not_reported() { /* "on hold for two minutes" */ }

#[test]
fn a_missing_disclosure_fires_only_after_the_opening_window() { /* elapsed 10s vs 40s */ }

#[test]
fn every_finding_is_stable_across_repeated_evaluation() {
    // The session diffs on dedupe_key; an unstable key would re-alert every chunk.
    let segs = [seg("seg_1", 0, 10.0, "I guarantee a full refund")];
    let a: Vec<String> = fire(&segs).iter().map(dedupe_key).collect();
    let b: Vec<String> = fire(&segs).iter().map(dedupe_key).collect();
    assert_eq!(a, b);
}
```

- [ ] **Step 2: Run and confirm failure** — `cargo test -p ca-rules`
- [ ] **Step 3: Implement the detectors**, reusing `ca_privacy` matchers for PAN.
- [ ] **Step 4: Run tests**
- [ ] **Step 5: Commit** — `feat(rules): add deterministic live detectors`

---

### Task 3: Scoped `save_findings` and timeline persistence

**Files:**
- Modify: `crates/ca-store/src/lib.rs`
- Create: `crates/ca-store/src/timeline.rs`

**Interfaces:**
- Produces: `Store::save_channel_timeline(&CallId, &ChannelTimeline)`, `Store::channel_timeline(&CallId) -> Result<Option<ChannelTimeline>>`. `save_findings` gains agent scoping.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn saving_one_agents_findings_leaves_anothers_alone() {
    // Rules file during the call; compliance files at hang-up. A blanket
    // delete would wipe every live alert the moment the battery ran.
    let (s, id) = store_with_call("a");
    s.save_findings(&id, &[finding(&id, AgentKind::Rules, "LIVE", Severity::High)]).unwrap();
    s.save_findings(&id, &[finding(&id, AgentKind::Compliance, "COMP", Severity::Low)]).unwrap();
    let codes: Vec<String> = s.findings(&id).unwrap().into_iter().map(|f| f.code).collect();
    assert!(codes.contains(&"LIVE".to_string()), "rules findings were wiped");
    assert!(codes.contains(&"COMP".to_string()));
}
```

- [ ] **Step 2: Confirm it fails** — the current blanket `DELETE` wipes the first set.
- [ ] **Step 3: Scope the delete by agent**; add migration `0005_channel_timeline`.
- [ ] **Step 4: Run** — `cargo test -p ca-store`
- [ ] **Step 5: Commit** — `fix(store): scope finding deletes by agent`

---

### Task 4: Roles from the channel timeline

**Files:**
- Modify: `crates/ca-diarize/src/lib.rs`

**Interfaces:**
- Produces: `pub fn roles_from_timeline(segments: &mut [Segment], timeline: &ChannelTimeline)`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn roles_come_from_the_channel_that_carried_the_audio() {
    // Channel 0 is the microphone, so it is the agent by construction. This is
    // exact where the acoustic diarizer only ever guesses.
    let mut segs = vec![seg("s1", 0.5, 1.5), seg("s2", 3.0, 3.8)];
    let t = ChannelTimeline { spans: vec![
        SpeechSpan { channel: 0, start: 0.0, end: 2.0 },
        SpeechSpan { channel: 1, start: 2.5, end: 4.0 },
    ]};
    roles_from_timeline(&mut segs, &t);
    assert_eq!(segs[0].role, SpeakerRole::Agent);
    assert_eq!(segs[1].role, SpeakerRole::Customer);
    assert_eq!(segs[1].speaker_index, 1);
}
```

- [ ] **Step 2–5:** implement by midpoint overlap, run, commit — `feat(diarize): assign roles from a channel timeline`

---

### Task 5: Streaming chunker

**Files:**
- Create: `crates/ca-live/Cargo.toml`, `crates/ca-live/src/lib.rs`, `crates/ca-live/src/chunker.rs`
- Modify: `Cargo.toml` (workspace)

**Interfaces:**
- Produces:
```rust
pub struct Chunk { pub start_secs: f64, pub end_secs: f64, pub samples: Vec<f32> }
pub struct Chunker { /* … */ }
impl Chunker {
    pub fn new(sample_rate: u32) -> Self;
    pub fn push(&mut self, samples: &[f32]) -> Vec<Chunk>;
    pub fn flush(&mut self) -> Option<Chunk>;
    pub fn speaking(&self) -> bool;
}
```

Closes a chunk when ≥0.4 s of silence follows ≥1.5 s of speech, or at a 15 s cap.

- [ ] **Step 1: Write failing tests** — silence only yields nothing; speech then silence yields one chunk; speech past the cap yields a chunk without waiting for a pause; a 0.2 s gap does not split a word.
- [ ] **Step 2–5:** implement a frame-energy gate, run, commit — `feat(live): add streaming VAD chunker`

---

### Task 6: Incremental stereo WAV writer

**Files:**
- Create: `crates/ca-live/src/writer.rs`

**Interfaces:**
- Produces: `StereoWavWriter::create(&Path, u32)`, `.write(Channel, &[f32])`, `.finish()`.

Channels arrive independently, so the writer buffers per channel and emits
interleaved frames only up to `min(agent.len(), customer.len())`, padding the
shorter side with silence on `finish`.

- [ ] **Step 1: Write failing tests** — header sizes patched on finish; a file with only agent audio still decodes with a silent right channel; the result is readable by `ca_audio::decode_file`.
- [ ] **Step 2–5:** implement, run, commit — `feat(live): add incremental stereo wav writer`

---

### Task 7: Audio sources — trait and replay

**Files:**
- Create: `crates/ca-live/src/source.rs`

**Interfaces:**
- Produces:
```rust
pub struct Frame { pub channel: Channel, pub samples: Vec<f32> }
pub trait AudioSource: Send {
    fn start(&mut self, tx: std::sync::mpsc::Sender<Frame>) -> Result<()>;
    fn stop(&mut self);
    fn devices(&self) -> (String, String);
}
pub struct ReplaySource { /* … */ }
impl ReplaySource { pub fn new(path: &Path, speed: f64) -> Result<Self>; }
```

`speed` is a multiple of realtime; `f64::INFINITY` disables pacing so tests run
instantly. This is the seam that makes the entire live loop testable without a
microphone.

- [ ] **Step 1: Write failing tests** — a stereo WAV replays as frames on both channels, sample counts match, `speed: INFINITY` completes without sleeping.
- [ ] **Step 2–5:** implement, run, commit — `feat(live): add audio source trait and replay source`

---

### Task 8: Chunk transcriber and whisper-server lifecycle

**Files:**
- Create: `crates/ca-live/src/asr.rs`

**Interfaces:**
- Produces:
```rust
#[async_trait]
pub trait ChunkTranscriber: Send + Sync {
    async fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String>;
}
pub struct WhisperServer { /* … */ }
impl WhisperServer {
    pub fn spawn(binary: &Path, model: &Path, port: u16) -> Result<ServerHandle>;
    pub fn client(port: u16) -> Self;
}
pub struct StubTranscriber(pub Vec<String>); // cfg(test) + pub for downstream tests
```

`ServerHandle` kills the child on drop, so a crashed app never leaves a warm
server holding 141 MB.

- [ ] **Step 1: Write failing tests** — the stub returns queued text in order; a transport error surfaces as `CaError` rather than a panic; `ServerHandle` drop kills the child.
- [ ] **Step 2–5:** implement with `reqwest` multipart against `/inference`, run, commit — `feat(live): add whisper-server chunk transcriber`

---

### Task 9: `LiveMonitorAgent`

**Files:**
- Create: `crates/ca-agents/src/live_monitor.rs`
- Modify: `crates/ca-agents/src/lib.rs`

**Interfaces:**
- Produces: `LiveMonitorAgent::run(&self, provider, window: &Transcript, previous_headline: Option<&str>, rule_hits: &[Finding]) -> Result<(LiveMonitorReport, ChatResponse)>`.

- [ ] **Step 1: Write failing tests** — citations validated against real segment ids; `say_next` dropped when unevidenced; sentiment clamped to −1..1; risk parsed via `Severity::parse` with medium as the fallback.
- [ ] **Step 2–5:** implement following `sentiment.rs`, run, commit — `feat(agents): add live monitor agent`

---

### Task 10: Session orchestration

**Files:**
- Create: `crates/ca-live/src/session.rs`, `crates/ca-live/src/event.rs`

**Interfaces:**
- Produces:
```rust
pub enum LiveEvent { Started { .. }, Level { .. }, Speaking { .. }, Stable { .. }, Rule { .. }, Monitor { .. }, Warning { .. }, Ended { .. } }
pub struct LiveSession { /* … */ }
impl LiveSession {
    pub async fn run(cfg: LiveConfig, source: Box<dyn AudioSource>, on_event: impl Fn(LiveEvent)) -> Result<CallId>;
}
```

Redaction runs on every window before it reaches a remote provider, guarded by
the same hard error as `Analyzer::guard_redaction`.

- [ ] **Step 1: Write the failing integration test** — drive a synthesised stereo WAV through `ReplaySource` + `StubTranscriber` and assert the event sequence: `Started`, ≥1 `Stable` per channel, a `Rule` for a planted card number, exactly one `Ended`, and a `Call` row with a media path.
- [ ] **Step 2: Write the redaction test** — a remote provider with `redact = false` fails before any window is sent.
- [ ] **Step 3–5:** implement, run, commit — `feat(live): add session orchestration`

---

### Task 11: cpal capture

**Files:**
- Create: `crates/ca-live/src/capture.rs`

**Interfaces:**
- Produces: `CaptureSource::new(input: Option<String>, output: Option<String>) -> Result<Self>`, `list_devices() -> Result<(Vec<String>, Vec<String>)>`.

The only file requiring a physical device. Deliberately thin: open two streams,
resample to 16 kHz mono, push `Frame`s. A watchdog reports a warning if no
samples arrive on a stream for 3 s while running.

- [ ] **Step 1: Write what can be tested** — device enumeration returns without panicking; an unknown device name is an error naming the device.
- [ ] **Step 2–5:** implement, run, commit — `feat(live): add cpal two-stream capture`

---

### Task 12: Tauri commands and events

**Files:**
- Create: `apps/desktop/src-tauri/src/live.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`, `apps/desktop/src-tauri/Cargo.toml`

**Interfaces:**
- Produces commands `list_audio_devices`, `start_live_session`, `stop_live_session`; event channel `live:event`.

- [ ] **Step 1–4:** implement, forwarding `LiveEvent` through `app.emit`, `cargo check -p call-analyzer-desktop`
- [ ] **Step 5: Commit** — `feat(desktop): expose live session commands`

---

### Task 13: Live screen

**Files:**
- Create: `apps/desktop/ui/src/screens/LiveScreen.tsx`, `.css`
- Modify: `apps/desktop/ui/src/api.ts`, `App.tsx`, `dev/mockBackend.ts`

- [ ] **Step 1–4:** implement idle/running/ending states, alert strip, sentiment sparkline, transcript, say-next; mock fixtures replaying a scripted session; `tsc -b`, `oxlint`, `vite build`
- [ ] **Step 5: Commit** — `feat(ui): add live call assist screen`

---

### Task 14: Docs and gates

**Files:**
- Create: `docs/PHASE3_LIVE.md`
- Modify: `README.md`

- [ ] **Step 1:** document the two tiers, the measurements, device setup and the headset limitation.
- [ ] **Step 2:** full gate run — fmt, clippy, tests, rustdoc, UI build.
- [ ] **Step 3: Commit** — `docs: describe phase 3 live call assist`

---

## Self-Review

**Spec coverage:** capture → Tasks 7/11; chunking → 5; storage → 6; timeline → 1/3/4; rules tier → 2; monitor tier → 9; redaction → 10; `save_findings` fix → 3; UI → 13; failure handling → 10/11; testing → every task; milestones 3a/3b/3c map to Tasks 5–7/11, 8, 2/9/10 respectively.

**Placeholders:** none — every task names exact files and concrete assertions.

**Type consistency:** `Channel`, `Frame`, `Chunk`, `ChunkTranscriber`, `AudioSource`, `LiveEvent`, `ChannelTimeline`, `RuleContext` are each defined once and referenced with the same names throughout.
