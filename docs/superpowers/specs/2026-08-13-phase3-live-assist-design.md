# Phase 3 — Live call assist

**Status:** design approved 2026-08-13. Not yet implemented.

Phase 1 gave the app a corpus and a pipeline. Phase 2 made every agent's output
visible end to end. Phase 3 moves the same analysis to a call that is still in
progress: capture both sides of a live conversation, transcribe it as it
happens, and surface compliance breaches, sentiment and a suggested next line
while the agent can still act on them.

## Target

An agent takes a call on any softphone — Teams, Zoom, a browser dialler. They
press **Start** in Call Analyzer. While they talk they see a live transcript
with exact speaker labels, alerts when something goes wrong, a sentiment
trajectory, and one suggested line to say next. On hang-up the session becomes
an ordinary call in the corpus, with the full Phase 2 battery run against it.

Live capture is a new *source* of calls, not a parallel system. There is one
corpus, one call list, one risk register.

## Measurements that shaped this design

Taken on the development machine (8 threads, no GPU) against real corpus audio,
2026-08-13. These are the numbers the design is built on; re-measure before
assuming them on other hardware.

| Configuration | 5 s chunk | 10 s chunk | 15 s chunk | Transcript quality |
| --- | --- | --- | --- | --- |
| `small.en`, cold CLI | — | — | — | 1.3× realtime including model load |
| `small.en`, warm server, full context | 3.0 s | 3.8 s | — | best available |
| `small.en`, warm server, `-ac 512` | 1.2 s | 1.1 s | — | **degenerate repetition** |
| `base.en`, warm server, full context | 0.85 s | 1.19 s | 1.04 s | good structure, local word errors |

Three findings follow, and each one closes off a design that would otherwise
look reasonable:

1. **There is a ~3 s floor per inference on `small.en`, independent of chunk
   length.** whisper always processes a 30-second mel window internally, so a
   3-second chunk costs nearly what a 10-second chunk costs. A short fixed
   cadence is therefore the *worst* option: 3-second chunks run at 1× realtime
   and fall behind on the first hesitation.

2. **`-ac 512` is rejected.** It is three times faster, but on dense speech it
   dropped a clause and looped:

   ```text
   full context:  "...Can I resettle in two, third or fourth? Can you give me
                   the second, third or fourth so I can do drug testing?"
   -ac 512:       "...can I resettle in two third or fourth can you give me
                   that second third third can you give me that second third third"
   ```

   For a tier that feeds compliance detection, a silently dropped phrase is a
   missed breach. Speed bought this way is not usable.

3. **`base.en` at full context is the live model.** ~12× realtime, and its
   failure mode is local word errors (`resettle` → `res aleur`) rather than
   structural collapse. Fetched via `scripts/fetch-models.ps1 -Model base.en`
   (141.1 MB, SHA-256
   `a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002`).

## Architecture

Two tiers throughout, for transcription and for analysis. The live tier is fast
and approximate and never reaches storage; the authoritative tier is the
existing Phase 1/2 pipeline and is unchanged.

```text
 mic device ─────▶ resample 16k mono ─▶ ring buffer ─┐
                                                      ├─▶ VAD chunker ─▶ whisper-server (base.en)
 output device ──▶ (LOOPBACK) ───────▶ ring buffer ─┘        │
        │                                                     ▼
        │                                          stable segments
        │                                                     │
        │                              ┌──────────────────────┼───────────────┐
        │                              ▼                      ▼               ▼
        │                         ca-rules            LiveMonitorAgent   live transcript
        │                       (every chunk)      (~25 s / on high rule)      │
        │                              │                      │               │
        │                              └──────────▶ live:event ◀──────────────┘
        │                                                │
        ▼                                                ▼
  stereo WAV (L=agent, R=customer)                 LiveScreen
  + channel speech timeline
        │
        │  on hang-up
        ▼
  existing pipeline: decode ─▶ transcribe (small.en) ─▶ roles from timeline
                     ─▶ redact ─▶ Phase 2 battery ─▶ Calls / Insights / risk register
```

### Crates

| Crate | Status | Responsibility |
| --- | --- | --- |
| `ca-live` | new | Capture, chunking, session lifecycle, event stream |
| `ca-rules` | new | Deterministic detectors over a transcript window → `Finding` |
| `ca-agents` | extended | `LiveMonitorAgent` |
| `ca-store` | extended | Channel timeline storage, scoped `save_findings`. No live-session table — a live call *is* a `Call` |
| `ca-diarize` | extended | Accept a channel timeline instead of inferring roles |

`ca-live` and `ca-rules` are libraries for the same reason every other
behaviour here is: the desktop app, the CLI and any future server must share one
implementation rather than three that drift.

## Capture

`cpal` provides WASAPI loopback by passing an **output** device to
`build_input_stream()`, which sets `AUDCLNT_STREAMFLAGS_LOOPBACK`
([RustAudio/cpal#339](https://github.com/RustAudio/cpal/pull/339)). Two streams
are opened:

- the default (or chosen) **input** device — the agent;
- the default (or chosen) **output** device in loopback — the customer, whatever
  the softphone is playing.

Speaker labels therefore come from *which device the audio arrived on*. They are
exact rather than inferred, and no diarizer runs on the live path.

Devices deliver 44.1/48 kHz stereo `f32`; `ca-audio`'s existing resample and
downmix bring each to 16 kHz mono.

### Chunking

Chunks are cut by voice activity, not on a fixed interval. `ca_audio::vad::detect`
already finds speech regions. A chunk closes when:

- ≥0.4 s of silence follows ≥1.5 s of speech, **or**
- the buffer reaches a 15 s hard cap, so a monologue still produces text.

Cutting at pauses means chunk boundaries never fall mid-word, which is what
removes the need to re-transcribe an overlapping window. That single decision is
what keeps the design inside the latency budget — overlap re-transcription at
these inference costs would not fit. Silence costs nothing, and because the two
sides mostly alternate, total ASR load is roughly one stream's worth.

### Storage

Holding a call in memory as `f32` would cost ~230 MB per hour per stream, so
audio is written incrementally to a single 16 kHz 16-bit **stereo** WAV — left
agent, right customer. That is 64 KB/s, or **~230 MB per hour of call**. The
per-channel speech timeline is persisted alongside it.

230 MB/hour is not free on a machine that has run short of disk before. It is
accepted here because WAV needs no encoder dependency and the existing pipeline
already reads it, but a session length warning and an encoded format are the
obvious follow-ups if live calls become routine.

That timeline is worth more than it costs. On hang-up the authoritative pass
assigns speaker roles from the channel each utterance arrived on, rather than
running the acoustic diarizer. Live-captured calls therefore carry *better*
speaker data than ingested ones, using information already known for free.

### Session lifecycle

The `Call` row is created at session **start** — `source_name = "live"`, stage
`Discovered` — not at stop. A crash mid-call then leaves a partial WAV and a
recoverable row that the next launch picks up as ordinary unprocessed work,
rather than an orphaned file nothing knows about.

On stop: finalise the WAV, set `media_path`, and run the existing pipeline for
that one call.

## Live analysis

### Tier 1 — `ca-rules`, deterministic

Pure functions from a transcript window plus the channel timeline to
`Vec<Finding>`. They emit the **existing** `Finding` type with the existing
`Severity`, so everything Phase 2 built works unchanged: the findings ledger,
`RiskBanner`, the corpus risk register and `failing_call_count` all light up
with no new persistence.

| Rule | Code | Severity | Reuses |
| --- | --- | --- | --- |
| Card / Aadhaar spoken aloud | `PCI_PAN_SPOKEN` | critical | `ca-privacy` Luhn + checksum matchers |
| Banned phrase | `BANNED_PHRASE` | medium | list in `AppSettings`, with a built-in default set |
| No disclosure in first 30 s | `NO_DISCLOSURE` | medium | channel timeline |
| Hold with no stated duration | `HOLD_NOT_TIMEBOXED` | high | timeline + phrase match |
| Sustained talk-over | `TALK_OVER` | low | both channels hot >1.5 s |
| Monologue >90 s | `MONOLOGUE` | low | channel timeline |

`PCI_PAN_SPOKEN` is the justification for having a rules tier at all: it is the
alert where fifteen seconds late is the difference between stopping a breach and
logging one. It must not depend on a model noticing.

Because these are pure functions over a window, the same crate can run over
already-ingested calls as a free pre-pass. That is out of scope for Phase 3 but
the crate boundary is drawn so it costs nothing later.

### Tier 2 — `LiveMonitorAgent`

One new agent in `ca-agents`, following the shape of every existing one.

- **In:** redacted rolling window (~90 s), the `headline` from the previous
  monitor call (carried forward so the model has continuity without re-sending
  the whole call), and the rule hits so far.
- **Out:** `{ sentiment, risk, headline, sayNext, citations }` — one call, not
  eight.
- **Cadence:** every ~25 s of call time, or immediately when a rule of severity
  ≥ high trips, with an 8 s floor so a burst of rule hits cannot cause a burst of
  LLM calls. Skipped entirely when no new speech has arrived since the last call.

Roughly 25 calls for a ten-minute call.

Citations are validated against real segment ids exactly as in Phase 2, and
`sayNext` is dropped rather than shown when the window does not support it.

### Redaction

Every window is redacted with `ca-privacy::Redactor` before it leaves the
machine, behind the same hard-error guard as `Analyzer::guard_redaction`: a
remote provider is never handed an unredacted window, and violating it is an
error rather than a warning.

Live is the easiest place in the product to quietly break the
redact-before-cloud promise, because windows are small, frequent and easy to
treat as ephemeral. They are not — they are the same customer's PII, sent more
often.

### Phase 2 change required

`Store::save_findings` currently deletes **all** findings for a call before
inserting. With rules writing during the call and the compliance agent writing at
hang-up, the rule findings would be silently wiped.

Scope the delete by agent: `DELETE FROM findings WHERE call_id = ?1 AND agent = ?2`.
Two agents filing findings against one call is now a real scenario rather than a
hypothetical one, and the current behaviour is wrong independently of Phase 3.

## UI

A new `LiveScreen` with three states: idle (device pickers and Start), running,
and ending.

```text
LIVE CALL  ● 04:12                    [ end ]
─────────────────────────────────────────────
  ⚠  HOLD NOT TIME-BOXED           @ 03:58
  ⚠  card number requested          @ 02:11
─────────────────────────────────────────────
 sentiment  ▁▂▃▂▁▁▂▄▅▆   +0.31 ▲
─────────────────────────────────────────────
 04:02 CUST  ...so it charged me twice again
 04:09 AGENT let me pull that up for you
 04:12 AGENT ▌(listening)
─────────────────────────────────────────────
 SAY NEXT ▸ "I can see the duplicate — I'll
            reverse it now, about 2 minutes."
```

Events reach the screen over a `live:event` channel, using the same
`emit`/`listen` pattern as `pipeline:progress`, as a tagged enum.

Note what is **not** here: there is no partial-hypothesis event. Because chunks
are cut at pauses and transcribed whole, text arrives already final — there is
no interim guess that later changes. The `▌(listening)` marker in the mockup is
driven by `Speaking`, which reports that a channel has voice activity right now,
not provisional text. Designing for revisable partials would mean overlap
re-transcription, which the measurements ruled out.

| Event | Carries |
| --- | --- |
| `Started` | session id, device names |
| `Level` | per-channel dBFS, for a VU meter and a "is this working" signal |
| `Speaking` | channel, and when the current utterance began |
| `Stable` | a finalised segment with channel and timings |
| `Rule` | a `Finding` |
| `Monitor` | sentiment, risk, headline, sayNext |
| `Ended` | call id |

The sentiment sparkline is the series of `Monitor` events, so it gains a point
roughly every 25 s — about 24 points on a ten-minute call. It is a trend line,
not a continuous signal, and should be drawn as one.

On `Ended` the screen hands off to Call Detail for that call once the battery
finishes, closing the loop back into Phase 2 rather than leaving live as a dead
end.

Device pickers are explicit and remembered. "Default output" is right for most
softphones and wrong for anyone routing audio elsewhere, and a silent wrong
default here produces a call recorded with one empty channel.

## Failure handling

Every failure degrades rather than losing the call. Capture is the only part
that cannot be redone.

| Failure | Behaviour |
| --- | --- |
| Device unplugged mid-call | Warn in the alert strip, keep the other channel recording, session survives |
| `whisper-server` crashes | Live transcript stops; capture continues; end-of-call pass still produces the full transcript. Restart with backoff |
| cpal silent callback ([#516](https://github.com/RustAudio/cpal/issues/516)) | Watchdog: no samples on a stream for >3 s while running ⇒ warn and rebuild the stream |
| No API key or provider down | Rules tier keeps working; monitor tier reports unavailable |
| Disk full | Stop writing, keep the session alive, warn loudly |

The provider-down row is why tier 1 is deterministic: with no network, live
assist degrades to an offline alerting surface rather than an empty screen.

## Testing

The design concentrates everything untestable into one thin seam.

- **`ca-rules`** — pure functions over transcript windows. Unit-tested on
  literals in the style of `ca-insights`, with a fires / does-not-fire pair per
  detector.
- **VAD chunker** — pure over sample slices. Unit-tested for boundary
  behaviour: silence-only input, speech running past the hard cap, a pause
  shorter than the threshold.
- **`LiveMonitorAgent`** — citation validation and field normalisation tested
  with the existing stub-provider pattern, like every Phase 2 agent.
- **Replay `AudioSource`** — feeds a recorded stereo WAV through the identical
  chunker → rules → monitor path at 1× or faster. Makes the whole live pipeline
  deterministic and runnable in CI with no microphone, and doubles as a way to
  regression-test rules against the existing corpus.
- **cpal capture** sits behind the same `AudioSource` trait and is the only part
  requiring a physical device. It is deliberately thin: open streams, resample,
  hand off samples.

## Milestones

Delivered in three cycles. Each is independently shippable, and each de-risks
the next.

**3a — Capture and session lifecycle.** `ca-live` with two-stream capture, the
replay source, incremental stereo WAV writing, the channel timeline, session
start/stop, the `ca-diarize` change that assigns roles from that timeline
instead of inferring them, and hand-off to the existing pipeline. Deliverable: press Start,
talk, press Stop, and the call appears in the corpus with a full Phase 2 battery
on it. No live transcript yet. Proves the audio path, which is the genuine
unknown.

**3b — Incremental transcription.** Warm `whisper-server` management, VAD
chunking, the partial/stable segment model, and the live transcript in the UI
with channel-derived speaker labels. Proves the latency budget against the
measurements above.

**3c — Live analysis.** `ca-rules`, `LiveMonitorAgent`, per-window redaction,
the scoped `save_findings` fix, and the alerts / sentiment / say-next surface.
Mostly reuse of Phase 2 machinery.

## Out of scope

Named so they are decisions rather than omissions:

- **Acoustic echo cancellation.** v1 assumes a headset. Without one the mic
  picks up the customer from the speakers and both channels transcribe the same
  words. Stated as a limitation; energy-correlation gating is a later refinement.
- **Softphone / CTI integration.** No vendor SDK, no SIPREC. Loopback capture
  works with every softphone at the cost of not knowing the customer's identity.
- **Multilingual live transcription.** `base.en` is English-only, matching the
  current `small.en` default. Hindi and code-switched calls need the
  multilingual weights and a separate latency measurement.
- **Running `ca-rules` over the back catalogue.** The crate boundary makes it
  cheap later; doing it now widens the phase.
- **GPU inference.** All measurements are CPU. A CUDA build would change every
  number here and none of the structure.
