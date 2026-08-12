# Phase 2 — Multi-agent analysis battery

## Status: sample-proven (2026-08-11)

Phase 1 foundations remain complete. Phase 2 adds specialised agents beyond QA.

### Agents implemented

| Agent | Kind | Payload | Notes |
| --- | --- | --- | --- |
| QA scorecard | `qa_scorecard` | `Scorecard` | Existing; citations + local overall |
| Sentiment | `sentiment` | `SentimentReport` | Trajectory points timed from citations |
| Compliance | `compliance` | `ComplianceReport` + `findings` rows | High/critical findings fail the call |
| Root cause | `root_cause` | `RootCauseReport` | Category normalised to billing/product/process/agent/other |
| Action items | `action_items` | `ActionItemsReport` | Owner/priority normalised; unevidenced items dropped |
| CRM sync / RAG | — | — | Later phases |

### Pipeline behaviour

`Analyzer::run_battery` runs all four agents. Per-agent failures become error envelopes so one flaky agent does not discard the rest. Findings from compliance are written via `store.save_findings`.

`ca score` now means **agent battery**, not QA-only.

### Verified samples

**Short call (battery smoke)**

```text
call_81cf83af… / call_94fb40eca…
  QA / sentiment / compliance / root_cause / action_items all persist
```

**Long call — full battery including action items (2026-08-11)**

```text
call_871c995db190415a8bb1b42d73a70a45  (~194s recruiter call)
  model    meta/llama-3.1-8b-instruct (nvidia-nim)
  wall     23s
  QA       80.0%
  sent     +0.40
  comp     pass
  rca      product
  acts     8 action items (owner/priority normalised, citations required)
```

Inspect:

```bash
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" \
  report call_871c995db190415a8bb1b42d73a70a45
```

### Commands

```bash
export NVIDIA_API_KEY=nvapi-...
cd /c/Users/yashk/Downloads/call-analyzer

# Multi-agent pass on next redacted call
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" \
  score --provider nvidia_nim --model meta/llama-3.1-8b-instruct --limit 1

# Re-run battery on already-analyzed calls
./target/release/ca.exe --db ... score --force --limit 3

# Full JSON dump
./target/release/ca.exe --db ... report <call_id>
```

### UI note

Desktop still surfaces **QA scorecard** only (`get_scorecard`). Battery payloads are in SQLite `analyses` / `findings` and via `ca report`. Wiring Call Detail panels for sentiment/compliance/RCA is the next UI step (requires Tauri rebuild when disk allows).

### Disk

C: remains critically low (~4.8 GB free). Prefer `cargo build -p ca-cli --release` only; avoid `cargo clean` / full desktop rebuilds.
