# Call Analyzer

[![CI](https://github.com/Yash-Kavaiya/call-analyzer/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Yash-Kavaiya/call-analyzer/actions/workflows/ci.yml)
[![Security](https://github.com/Yash-Kavaiya/call-analyzer/actions/workflows/security.yml/badge.svg?branch=main)](https://github.com/Yash-Kavaiya/call-analyzer/actions/workflows/security.yml)
[![Release](https://github.com/Yash-Kavaiya/call-analyzer/actions/workflows/release.yml/badge.svg)](https://github.com/Yash-Kavaiya/call-analyzer/actions/workflows/release.yml)

![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)
![Rust](https://img.shields.io/badge/Rust-1.82%2B-orange)
![Tauri](https://img.shields.io/badge/Tauri-2.x-green)
![React](https://img.shields.io/badge/React-19-61dafb)

**Call Analyzer** is a Windows desktop application for contact-center call intelligence. It performs local transcription, speaker diarization, PII redaction, and multi-vendor LLM scoring — all with a NVIDIA-themed dashboard. Built with [Tauri 2](https://tauri.app/) + [React](https://react.dev/) for the UI and a modular [Rust](https://www.rust-lang.org/) workspace for the backend.

---

## Table of Contents

- [Features](#features)
- [Screenshots](#screenshots)
- [Architecture](#architecture)
- [Quick Start](#quick-start)
- [Building from Source](#building-from-source)
- [Configuration](#configuration)
- [Usage](#usage)
- [Roadmap](#roadmap)
- [Security](#security)
- [Contributing](#contributing)
- [CI/CD](#cicd)
- [License](#license)

---

## Features

### Phase 1 — Foundations ✅

- 🎙️ **Local Transcription** — Whisper CLI ASR, fully offline
- 👥 **Speaker Diarization** — Identify and label different speakers
- 🔒 **PII Redaction** — Scrub sensitive data before cloud upload
- 🤖 **Multi-Vendor LLM Scoring** — NVIDIA NIM, OpenAI-compatible, Anthropic, Gemini
- 📊 **NVIDIA-Themed Dashboard** — Real-time metrics and insights
- 🖥️ **Windows Desktop App** — Tauri 2 + React, native feel
- 🔐 **Credential Manager** — API keys stored securely, never exposed to the UI

### Phase 4a — Participant Identity ✅

Who was on the call, on both sides — the column every per-agent rollup and
every repeat-contact question needs. See
[docs/PHASE4A_IDENTITY.md](docs/PHASE4A_IDENTITY.md).

- 👤 **Agents and contacts** — derived from the filename, corrected by hand,
  or taken from a live session, with the provenance recorded so a backfill
  never overwrites a correction
- 🕓 **Real call time** — parsed from the same filename, because `created_at`
  is ingest time and a bulk scan gives 1,967 calls the same date
- 🔁 **Repeat-contact rate** — the share of calls whose contact also called in
  the previous 30 days, computable without any analysis at all
- 📈 **Per-person trends** — QA and sentiment by month, absent measures shown
  as a dash rather than invented as a zero
- 🔒 **Local-only** — identity is the same class of data as the redaction map;
  a test asserts a participant's name never reaches a provider

### Phase 3 — Live Call Assist ✅

Capture a call as it happens and surface breaches while the agent can still act
on them. See [docs/PHASE3_LIVE.md](docs/PHASE3_LIVE.md).

- 🎧 **Two-stream capture** — your microphone is the agent, WASAPI loopback is
  the customer, so speaker labels are exact rather than inferred
- ⚡ **Live transcript** — `base.en` on a warm whisper server, chunks cut at
  natural pauses
- 🚨 **Deterministic alerts** — a spoken card number, an unbounded hold or an
  unsupportable promise is caught offline, in milliseconds, with no model
- 🧭 **Live monitor** — sentiment, risk and one suggested line, on a throttle
- 📥 **Ends as an ordinary call** — the full Phase 2 battery runs on hang-up

### Phase 2 — Multi-Agent Battery ✅

Eight agents run over every call, each producing a citable, structured result.
See [docs/PHASE2_AGENTS.md](docs/PHASE2_AGENTS.md) for details.

- 📋 **QA scorecard** — weighted rubric, scored locally rather than trusted from the model
- 🎭 **Sentiment** — signed whole-call score plus a clickable trajectory
- ⚖️ **Compliance** — findings with severity; high or critical fails the call
- 🔍 **Root cause** — normalised category, contributing factors, recommended fix
- ✅ **Action items** — owner, priority and due, evidence required
- 🎓 **Coaching** — strengths and improvements, each with a recommendation
- 💬 **Customer experience** — effort, frustration, escalation and churn estimates
- 📝 **Executive summary** — the paragraph a supervisor reads instead of the call

Every payload renders in Call Detail, sentiment and compliance are on the call
list for triage, and the corpus view carries mean sentiment, compliance pass rate
and a risk register drawn from the findings ledger.

---

## Screenshots

> _Add screenshots or a screen recording here once available._

---

## Architecture

```
call-analyzer/
├── apps/
│   └── desktop/                  # Tauri 2 + React desktop shell
│       ├── src-tauri/            # Rust backend (Tauri commands)
│       └── ui/                   # React frontend (Vite + TypeScript)
├── crates/
│   ├── ca-core/                  # Domain types & shared traits
│   ├── ca-audio/                 # Audio decode, waveform helpers
│   ├── ca-asr/                   # Whisper CLI ASR integration
│   ├── ca-diarize/               # Speaker diarization
│   ├── ca-privacy/               # PII redaction
│   ├── ca-agents/                # QA / sentiment / compliance agents
│   ├── ca-rules/                 # Deterministic live detectors
│   ├── ca-live/                  # Live capture, chunking, session loop
│   ├── ca-llm/                   # NIM + OpenAI-compat + Anthropic + Gemini
│   ├── ca-pipeline/              # Orchestration pipeline
│   ├── ca-store/                 # SQLite corpus layer
│   ├── ca-ingest/                # Source ingestion
│   ├── ca-insights/              # Analytics & reporting
│   ├── ca-metrics/               # Scoring metrics
│   ├── ca-config/                # Configuration management
│   └── ca-cli/                   # Headless batch runner
├── docs/
│   ├── PHASE1_FOUNDATIONS.md
│   ├── PHASE2_AGENTS.md
│   └── PHASE3_LIVE.md
├── scripts/                      # Build & helper scripts
├── models/                       # Model configs & prompts
├── vendor/                       # Vendored dependencies
├── Cargo.toml                    # Workspace root
├── LICENSE                       # MIT License
└── README.md
```

---

## Quick Start

### Prerequisites

| Tool | Version | Purpose |
|------|---------|---------|
| Rust | 1.82+ | Backend compilation |
| Node.js | 18+ | Frontend tooling |
| pnpm | 8+ | Package management |
| Tauri CLI | 2.x | Desktop app bundling |
| Windows SDK | — | Native Windows builds |

### Install

```bash
# Clone the repo
git clone https://github.com/<your-org>/call-analyzer.git
cd call-analyzer

# Install Rust dependencies
cargo fetch

# Install UI dependencies
cd apps/desktop
pnpm install
cd ../..
```

### Configure

```bash
# Copy the example env file
cp .env.example .env

# Edit .env with your API keys
# NVIDIA_API_KEY=nvapi-your-key-here
```

### Run

```bash
# Desktop app (with hot reload)
cd apps/desktop
cargo tauri dev
```

---

## Building from Source

### Build All Crates

```bash
cargo build --workspace
```

### Build Release

```bash
cargo build --workspace --release
```

### Build Desktop App

```bash
cd apps/desktop
cargo tauri build
```

Prebuilt binaries will appear in:
- `apps/desktop/src-tauri/target/release/call-analyzer-desktop.exe` — UI
- `target/release/ca.exe` — Headless CLI

---

## Configuration

All configuration is done via environment variables. See [`.env.example`](.env.example) for the full list.

### Key Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `NVIDIA_API_KEY` | Yes (for NIM) | — | Your NVIDIA API key |
| `NVIDIA_BASE_URL` | No | `https://integrate.api.nvidia.com/v1` | NVIDIA API endpoint |
| `NVIDIA_CHAT_MODEL` | No | `meta/llama-3.1-8b-instruct` | Default chat model |

### Recommended Models (free build.nvidia.com credits, verified 2026-08)

| Model | Use Case |
|-------|----------|
| `meta/llama-3.1-8b-instruct` | Fast smoke / settings test |
| `nvidia/nemotron-3-nano-30b-a3b` | Agentic workers (default NAT pattern) |
| `nvidia/nemotron-3-super-120b-a12b` | Long-context supervisor |
| `nvidia/nv-embedqa-e5-v5` | RAG embeddings |

---

## Usage

### Desktop App

Launch the Tauri desktop app to:
- Ingest call recordings from various sources
- View transcripts with speaker labels
- Run PII redaction before sending to cloud LLMs
- Read every agent's result against the call, with each claim seeking to the audio
  that supports it
- Triage the call list by sentiment and compliance outcome
- Browse corpus insights: score distribution, root-cause categories, risk register
  and per-agent spend

### Headless CLI

```bash
# Discover calls in the database
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" discover

# Score a call using NVIDIA NIM
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" \
  score --provider nvidia_nim --model meta/llama-3.1-8b-instruct --limit 1

# Digest every agent's result for a call, plus its findings ledger
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" report <call_id>

# ...and the full payloads
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" report <call_id> --json
```

---

## Roadmap

- [x] Phase 1: Foundations (transcription, diarization, PII, LLM scoring, dashboard)
- [x] Phase 2: Multi-agent battery (QA, sentiment, compliance, root cause, action items, coaching, CX, exec summary)
- [x] Phase 3: Live call assist (two-stream capture, live transcript, deterministic alerts, live monitor)
- [ ] Phase 4: Multi-tenant & cloud deployment
  - [x] 4a: Participant identity (agents, contacts, real call time, repeat-contact rate)
  - [ ] 4b: Roles and scoped views
  - [ ] 4c: Sync server
  - [ ] 4d: Tenant isolation
  - [ ] 4e: Deployment
- [ ] Phase 5: Plugin system for custom agents

See [docs/PHASE2_AGENTS.md](docs/PHASE2_AGENTS.md),
[docs/PHASE3_LIVE.md](docs/PHASE3_LIVE.md) and
[docs/PHASE4A_IDENTITY.md](docs/PHASE4A_IDENTITY.md) for details.

---

## Security

- **API keys never round-trip through the UI** after save — stored in Windows Credential Manager only.
- **Cloud providers require redaction before send** (`redact_before_send` flag).
- `.env` is gitignored — use `.env.example` as a template.
- All cloud uploads are opt-in and explicitly triggered by the user.

To report a vulnerability, see [SECURITY.md](SECURITY.md). Please do not open a
public issue for anything involving credential exposure or PII.

---

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for:

- Development setup instructions
- Project structure overview
- Commit conventions (Conventional Commits)
- Pull request process
- Bug reporting guidelines

Before pushing, run the same checks CI runs:

```powershell
pwsh scripts/preflight.ps1
```

### Quick Contributing Links

- 🐛 [Report a Bug](https://github.com/Yash-Kavaiya/call-analyzer/issues/new?template=bug_report.yml)
- 💡 [Request a Feature](https://github.com/Yash-Kavaiya/call-analyzer/issues/new?template=feature_request.yml)
- 💬 [Join the Discussion](https://github.com/Yash-Kavaiya/call-analyzer/discussions)

---

## CI/CD

Every pull request must pass two checks before it can merge:

| Check | Covers |
|---|---|
| **CI** | Formatting, clippy (`-D warnings`), tests and doctests, rustdoc, UI lint/type-check/build, coverage, repo hygiene |
| **Security** | Secret scanning, `cargo-deny` (advisories, licences, banned crates, sources), `npm audit`, dependency review, CodeQL, actionlint |

Tagging `v*.*.*` builds and publishes signed MSI and NSIS installers plus the
standalone CLI, with SHA-256 checksums, a CycloneDX SBOM, and a verifiable
build-provenance attestation.

The licence policy in [`deny.toml`](deny.toml) is what keeps the promises this
README makes: no GPL dependency can enter the tree (which is why audio decoding
uses symphonia rather than ffmpeg), and no dependency can pull in OpenSSL.

Full documentation: **[docs/CI_CD.md](docs/CI_CD.md)**.

---

## License

This project is licensed under the **MIT License** — see the [LICENSE](LICENSE) file for details.

---

## Acknowledgments

- [Tauri](https://tauri.app/) — Desktop app framework
- [React](https://react.dev/) — UI library
- [Rust](https://www.rust-lang.org/) — Systems programming language
- [NVIDIA NIM](https://build.nvidia.com/) — LLM inference platform
- [Whisper](https://openai.com/research/whisper) — Speech recognition

---

<p align="center">
  Built with ❤️ for the contact-center community
</p>
