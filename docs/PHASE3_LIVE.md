# Phase 3 — Live call assist

## Status: implemented (2026-08-13)

Phase 1 built the corpus. Phase 2 made every agent's output visible end to end.
Phase 3 runs the same analysis against a call that has not finished: capture
both sides, transcribe as it happens, and surface breaches while the agent can
still act on them.

Design: [`docs/superpowers/specs/2026-08-13-phase3-live-assist-design.md`](superpowers/specs/2026-08-13-phase3-live-assist-design.md).
Plan: [`docs/superpowers/plans/2026-08-13-phase3-live-assist.md`](superpowers/plans/2026-08-13-phase3-live-assist.md).

## How it works

```text
 mic device ─────▶ resample 16k mono ─▶ ring buffer ─┐
                                                      ├─▶ VAD chunker ─▶ whisper-server (base.en)
 output device ──▶ (WASAPI LOOPBACK) ─▶ ring buffer ─┘        │
        │                                                     ▼
        │                                          stable segments
        │                              ┌──────────────────────┼───────────────┐
        │                              ▼                      ▼               ▼
        │                         ca-rules            LiveMonitorAgent   live transcript
        │                       (every chunk)      (~25 s / on high rule)      │
        │                              └──────────▶ live:event ◀──────────────┘
        ▼
  stereo WAV (L=agent, R=customer) + channel timeline
        │  on hang-up
        ▼
  existing pipeline: decode ─▶ transcribe (small.en) ─▶ roles from timeline
                     ─▶ redact ─▶ Phase 2 battery ─▶ Calls / Insights / risk register
```

A live session is **not a parallel system**. It registers an ordinary `Call` at
the first second and hands it to the same pipeline every ingested recording goes
through. One corpus, one call list, one risk register.

## Two tiers, twice

### Transcription

| Tier | Model | Purpose |
| --- | --- | --- |
| Live | `ggml-base.en` on a warm `whisper-server` | Preview for the person on the call. Never stored. |
| Authoritative | `ggml-small.en` via the existing pipeline | The transcript that reaches the corpus. |

That split is what makes the live tier's accuracy acceptable: nothing it gets
wrong reaches the scorecard, the insights or the risk register.

### Analysis

| Tier | What | Cost |
| --- | --- | --- |
| `ca-rules` | Six deterministic detectors on every chunk | Free, offline, sub-millisecond |
| `LiveMonitorAgent` | Sentiment, risk, one suggested line | One provider call every ~25 s |

Rule hits are ordinary `Finding`s, so they land in the ledger built in Phase 2
and roll into the corpus risk register with no new persistence.

| Rule | Code | Severity |
| --- | --- | --- |
| Card / Aadhaar spoken aloud | `PCI_PAN_SPOKEN`, `AADHAAR_SPOKEN` | critical |
| Hold with no stated duration | `HOLD_NOT_TIMEBOXED` | high |
| Unsupportable commitment | `BANNED_PHRASE` | medium |
| No identification in the first 30 s | `NO_DISCLOSURE` | medium |
| Sustained talk-over | `TALK_OVER` | low |
| Monologue over 90 s | `MONOLOGUE` | low |

`PCI_PAN_SPOKEN` is why a rules tier exists at all: it is the alert where
fifteen seconds late is the difference between stopping a breach and logging
one, and it must not depend on a model noticing. Detection is checksum-validated
by `ca-privacy`, so a sixteen-digit order reference does not raise a critical
alert — and the number itself is never copied into the finding.

## Measurements

Development machine, 8 threads, no GPU, real corpus audio, 2026-08-13.

| Configuration | 5 s chunk | 10 s chunk | 15 s chunk | Quality |
| --- | --- | --- | --- | --- |
| `small.en` cold CLI | — | — | — | 1.3× realtime incl. model load |
| `small.en` warm, full context | 3.0 s | 3.8 s | — | best |
| `small.en` warm, `-ac 512` | 1.2 s | 1.1 s | — | **degenerate repetition** |
| `base.en` warm, full context | 0.85 s | 1.19 s | 1.04 s | good structure, local word errors |

Three things follow, and each closes off a design that reads fine on paper:

1. **There is a ~3 s floor per inference, independent of chunk length** —
   whisper processes a 30-second mel window internally. A short fixed cadence is
   therefore the worst option: 3-second chunks run at 1× realtime and fall
   behind on the first hesitation. Chunks are cut at pauses instead, which also
   means boundaries never fall mid-word and no overlap re-transcription is
   needed.

2. **`-ac 512` was measured and rejected.** Three times faster, but on dense
   speech it dropped a clause and looped:

   ```text
   full context:  "...Can I resettle in two, third or fourth? Can you give me
                   the second, third or fourth so I can do drug testing?"
   -ac 512:       "...can I resettle in two third or fourth can you give me
                   that second third third can you give me that second third third"
   ```

   A tier that feeds compliance detection cannot silently lose words.

3. **`base.en` at full context is the live model** — ~12× realtime, and its
   failure mode is local word errors rather than structural collapse.

## Setup

```powershell
# The live model is a separate 141 MB download; small.en stays the default.
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1 -Model base.en
```

`whisper-server.exe` is already vendored beside `whisper-cli.exe`. The app spawns
it on port 8477 when a session starts and kills it when the session ends, so a
crash never leaves a model resident.

Then open **Live**, pick a microphone and a playback device, and press *Start
listening*.

## Known limitations

- **Use a headset.** Without one the microphone also picks up the customer from
  the speakers, so both channels transcribe the same words. Echo cancellation is
  a later refinement, not a flag.
- **English only.** `base.en` matches the current `small.en` default. Hindi and
  code-switched calls need multilingual weights and their own latency
  measurement.
- **~230 MB per hour of call.** 16 kHz 16-bit stereo WAV, uncompressed. Worth
  watching on a machine short of disk.
- **CPU only.** Every number above is CPU. A CUDA build changes all of them and
  none of the structure.

## Failure behaviour

Every failure degrades; none of them ends the call. Capture is the only part
that cannot be redone.

| Failure | Behaviour |
| --- | --- |
| Device unplugged mid-call | Warning, other channel keeps recording |
| `whisper-server` crashes | Live transcript stops; capture continues; the batch pass still produces the full transcript |
| No API key or provider down | Rules tier keeps working; monitor reports unavailable |
| Live model missing | Session records and rules run; no live transcript |

## Testing

Everything untestable is concentrated into one file. `AudioSource` and
`ChunkTranscriber` each have a real implementation and a test one, so
`ReplaySource` plus `StubTranscriber` drive the identical path — chunking,
transcription, rules, monitor, persistence — deterministically and with no
microphone. Only `capture.rs` needs a physical device.

```bash
cargo test -p ca-live -p ca-rules      # the whole live loop, no hardware
```

The desktop UI can be exercised without a Tauri build: `npm run dev` in
`apps/desktop/ui` installs a mock backend that replays a scripted live call
reaching every state the screen can be in — a blocking alert, a monitor pass
with and without a suggestion, a warning, the listening marker, and the hand-off
at the end.

## Redaction

Every window is redacted with `ca-privacy` before it leaves the machine, behind
the same hard error as `Analyzer::guard_redaction`. Live is the easiest place in
the product to break the redact-before-cloud promise, because windows are small,
frequent and easy to treat as ephemeral. They are not — they are the same
customer's personal data, sent more often. A test asserts that a real phone
number in a window never appears in what reaches the provider.

The rules tier is the deliberate exception: it runs on **raw** text, because
`PCI_PAN_SPOKEN` exists to see the digits. It is local-only and nothing it reads
is ever sent anywhere.
