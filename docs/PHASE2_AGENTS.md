# Phase 2 — Multi-agent analysis battery

## Status: end to end (2026-08-13)

Phase 1 foundations remain complete. Phase 2 is the battery: every agent's output
is produced, persisted, aggregated across the corpus, and rendered in the desktop
app and the CLI. Nothing in the battery is write-only any more.

### Agents

`Analyzer::run_battery` runs all eight, in this order. Per-agent failures become
error envelopes so one flaky agent does not discard the rest of a paid batch; the
run only hard-fails if every agent failed.

| Agent | Kind | Payload | Local guarantees |
| --- | --- | --- | --- |
| QA scorecard | `qa_scorecard` | `Scorecard` | `overall` recomputed here; N/A criteria excluded and weights renormalised |
| Sentiment | `sentiment` | `SentimentReport` | Trajectory points timed from validated citations; unevidenced points dropped |
| Compliance | `compliance` | `ComplianceReport` + `findings` rows | `passed` recomputed from surviving findings; high/critical fail the call |
| Root cause | `root_cause` | `RootCauseReport` | Category normalised to billing/product/process/agent/other; confidence clamped |
| Action items | `action_items` | `ActionItemsReport` | Owner/priority normalised; unevidenced items dropped; sorted high→low |
| Coaching | `coaching` | `CoachingReport` | Strengths and improvements, each with an observation and a recommendation |
| Customer experience | `customer_experience` | `CustomerExperienceReport` | Every score clamped to 0..1 |
| Executive summary | `executive_summary` | `ExecutiveSummary` | Runs last, so it reads the freshest pass |
| CRM sync / RAG | — | — | Later phases |

Two rules hold across all of them: citations are validated against real segment
ids before anything is stored, and any arithmetic is done locally rather than
trusted from the model.

`ca score` means **agent battery**, not QA-only.

### Where the output surfaces

**Call detail.** Every agent gets its own formatter in
`components/agents/AgentResults.tsx` — the sentiment trajectory is a clickable
plot, compliance findings carry severity and a pass/fail verdict, root cause shows
its category, contributing factors and recommended fix, and action items show
owner, priority and due. An agent with no formatter in this build still renders
its raw payload, so adding one to the battery never produces a blank panel.

Blocking findings also appear as a banner above the waveform
(`components/agents/RiskBanner.tsx`), read from the findings ledger rather than
from the compliance payload — it covers any agent that files findings, and it is
the thing to see before deciding whether to listen to the call at all.

**Call list.** Sentiment, the compliance verdict and the blocking-finding count sit
beside the QA score, so calls can be triaged without opening them. A blank column
means the agent has not run, which is shown differently from a neutral score or a
pass.

**Insights.** Mean sentiment, share of calls that left the customer unhappy,
compliance pass rate and failing-call count join the QA tiles. Root causes roll up
into their five buckets. The risk register lists which rules the corpus breaks,
each at the worst severity it has ever been raised at, blocking rules first.

**CLI.** `ca report <call_id>` digests all eight agents and prints the findings
ledger; `--json` adds the full payloads.

### Things worth knowing

**Sentiment is never reported as a ratio.** `ca-insights` finds associations by
comparing group means as a lift, which requires a non-negative measure. Sentiment
runs −1..1, so −0.4 against +0.2 would be published as "2× lower sentiment" for
two groups sitting on opposite sides of neutral. It is reported as a signed mean
and a share of negative calls instead, where the sign survives.

**Rates are shares of the calls that have the measure**, never of the corpus. A
batch half-way through the sentiment agent would otherwise read as improving
purely because the agent had not caught up.

**Stored payloads are normalised on read.** Rows written before the report types
gained `#[serde(rename_all = "camelCase")]` are on disk in snake_case, and a real
corpus holds both shapes — the `analyses` table is keyed `(call_id, agent)`, so an
old row only changes when that agent is re-run. `ca_store::payload::normalise`
converts keys on read so every reader sees one shape. Without it, a
`RootCauseReport` written by an older build renders as an empty cause.

**Fixed in this phase:** `corpus_facts` read the root-cause label from
`primaryIssue`, but `RootCauseReport` serialises `primaryCause`. Every root-cause
row missed, and the Insights "root causes" panel was quietly being filled by the
executive summary — a different question. There is now a test that builds the
payload from the real report type, so a rename fails the test rather than
silently emptying the panel.

### Verified samples

**Long call — full battery (2026-08-11)**

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

**Digest output (2026-08-13)**

```text
$ ca report call_5cebb8162c7345088434400b4f560305

  QA scorecard         50.0% over 5 criteria
  Compliance & risk    pass — 0 finding(s)
  Sentiment            +0.40 across 10 moments
  Root cause & intent  process (90% confidence) — Incomplete documentation for account verification
  Action items         3 item(s), 0 high
  Coaching             (not run)
  Customer experience  (not run)
  Executive summary    (not run)
```

Coaching, customer experience and the executive summary read `(not run)` on these
rows because they predate those agents. Re-running `ca score --force` fills them.

### Commands

```bash
export NVIDIA_API_KEY=nvapi-...
cd /c/Users/yashk/Downloads/call-analyzer

# Multi-agent pass on the next redacted call
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" \
  score --provider nvidia_nim --model meta/llama-3.1-8b-instruct --limit 1

# Re-run the battery on already-analysed calls
./target/release/ca.exe --db ... score --force --limit 3

# Digest, then the full payloads
./target/release/ca.exe --db ... report <call_id>
./target/release/ca.exe --db ... report <call_id> --json
```

### UI without a backend

`npm run dev` in `apps/desktop/ui` installs a fake Tauri runtime
(`src/dev/mockBackend.ts`) with fixtures for all eight payload shapes, the
findings ledger and the risk register. Every visual state the panels can reach is
represented, including an agent this build has no formatter for and an agent that
failed — the two states a healthy fixture never shows. This is how the panels are
checked when a full Tauri rebuild is not worth the disk.

### Gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, and the UI's `tsc -b`, `oxlint` and `vite build` all pass
locally as of 2026-08-13. C: has ~21 GB free, which is enough for
`cargo check`/`clippy` across the workspace including the desktop crate; a full
`cargo tauri build` is still the expensive one.
