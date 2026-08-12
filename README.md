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

### Phase 2 — Multi-Agent Battery 🚧

See [docs/PHASE2_AGENTS.md](docs/PHASE2_AGENTS.md) for details.

- QA scoring
- Sentiment analysis
- Compliance checking
- Root cause analysis

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
│   └── PHASE2_AGENTS.md
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
- Browse scoring results on the dashboard

### Headless CLI

```bash
# Discover calls in the database
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" discover

# Score a call using NVIDIA NIM
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" \
  score --provider nvidia_nim --model meta/llama-3.1-8b-instruct --limit 1

# Generate a report for a specific call
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" report <call_id>
```

---

## Roadmap

- [x] Phase 1: Foundations (transcription, diarization, PII, LLM scoring, dashboard)
- [ ] Phase 2: Multi-agent battery (QA, sentiment, compliance, root cause)
- [ ] Phase 3: Real-time streaming analysis
- [ ] Phase 4: Multi-tenant & cloud deployment
- [ ] Phase 5: Plugin system for custom agents

See [docs/PHASE2_AGENTS.md](docs/PHASE2_AGENTS.md) for Phase 2 details.

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
"# post-call-analyzer-desktop-app" 
#   p o s t - c a l l - a n a l y z e r - d e s k t o p - a p p  
 # post-call-analyzer-desktop-app
