# Phase 1 — Foundations and Environment Setup

Contact Center Intelligence Platform (Windows)
Project root: `C:\Users\yashk\Downloads\call-analyzer`

## Goal

Prove the free NVIDIA stack path works before any pipeline/agent work:

1. Free account on [build.nvidia.com](https://build.nvidia.com)
2. API credits + API Catalog access (100+ hosted models)
3. `NVIDIA_API_KEY` exported and validated against hosted NIM
4. Windows desktop app can use that key (Settings → Test connection)

## What already exists (do not re-scaffold)

This repo is the Windows product for the architecture:

| Layer | Location |
| --- | --- |
| Desktop (Tauri 2 + React) | `apps/desktop/` — product name **Call Analyzer** |
| Release binary | `target/release/call-analyzer-desktop.exe` |
| Headless batch CLI | `target/release/ca.exe` |
| Ingest / ASR / diarize / PII / agents / store | `crates/ca-*` |
| Default LLM vendor | **NVIDIA NIM** → `https://integrate.api.nvidia.com/v1` |
| Key storage | Windows Credential Manager (`call-analyzer:nvidia_nim`) or env `NVIDIA_API_KEY` |

UI already has waveform viewers (wavesurfer), ECharts scorecards, ingest, calls, dashboard, settings.

## Account + key

1. Open https://build.nvidia.com and sign in (NVIDIA/Google/email).
2. Open any model → **Get API Key** → copy `nvapi-...`.
3. Store it in ONE of these places (never commit):

```bash
# A) Session env (git-bash)
export NVIDIA_API_KEY='nvapi-...'

# B) Project-local (gitignored)
cp .env.example .env
# edit .env

# C) Hermes profile env (already present on this machine)
# C:\Users\yashk\AppData\Local\hermes\.env
```

Resolution order inside the app (`ca-llm::secrets`):

1. Windows Credential Manager (Settings UI paste)
2. Environment variable `NVIDIA_API_KEY`

## Validate (mandatory smoke test)

```bash
cd /c/Users/yashk/Downloads/call-analyzer
# loads hermes .env automatically if project .env missing
python scripts/validate_nvidia.py
# optional deeper agentic models
python scripts/validate_nvidia.py --agentic
```

Evidence recorded on this machine (2026-08-11):

| Check | Result |
| --- | --- |
| Key present | yes (`nvapi-…`, len 70) |
| `GET /v1/models` | OK — **102** models |
| `meta/llama-3.1-8b-instruct` chat | OK ~0.5s → `NVIDIA_API_OK` |
| `nvidia/nemotron-3-nano-30b-a3b` | OK ~0.8s |
| `nvidia/nemotron-3-super-120b-a12b` | OK ~36s (cold/slow; use for supervisor, not UI smoke) |
| Catalog highlights | Nemotron-3 nano/super/ultra, nv-embedqa-e5-v5, NeMoGuard, Riva translate |

## Launch Windows app

```bash
cd /c/Users/yashk/Downloads/call-analyzer
bash scripts/launch_desktop.sh
# or directly (if key already in env / Credential Manager):
./target/release/call-analyzer-desktop.exe
```

In the app:

1. **Settings** → provider **NVIDIA NIM**
2. Model for smoke: `meta/llama-3.1-8b-instruct`
3. Agentic default (later phases): `nvidia/nemotron-3-nano-30b-a3b`
4. Long-context supervisor: `nvidia/nemotron-3-super-120b-a12b`
5. Paste key once (stored in Credential Manager) → **Test connection**

CLI scoring uses the same vendor:

```bash
./target/release/ca.exe score --provider nvidia_nim --model meta/llama-3.1-8b-instruct --limit 1
```

## Hosted stack map (Phase 1 scope only)

| Architecture box | Phase 1 host | Notes |
| --- | --- | --- |
| Agentic LLM | NIM chat | Nemotron-3 nano / super |
| Embeddings (RAG later) | NIM embeddings | `nvidia/nv-embedqa-e5-v5` |
| Guardrails later | NIM safety | NeMoGuard / safety-guard models in catalog |
| ASR / diarize | **Local Whisper** in this app | Hosted Riva ASR is gRPC/NVCF; desktop uses `vendor/whisper` |
| Orchestration later | NeMo Agent Toolkit (`nat`) | Separate from Tauri; same `NVIDIA_API_KEY` |

## Disk / rebuild warning (this machine)

- C: drive was ~**4.9 GB free** when Phase 1 was closed.
- Prefer running prebuilt `target/release/*` binaries.
- Avoid full `cargo clean` / large rebuilds until space is reclaimed.
- Do not vendor large call corpora into this repo (sample-first).

## Phase 1 exit criteria

- [x] build.nvidia.com key available on machine
- [x] `scripts/validate_nvidia.py` PASS
- [x] Hosted completions proven
- [x] Windows desktop binary present and launchable
- [x] App defaults to NVIDIA NIM endpoint
- [x] Live app DB scored via NIM (`ca.exe score`) — end-to-end agent path proven
- [x] API key written to Windows Credential Manager target `call-analyzer:nvidia_nim` (Settings can Test connection without shell env)

### Live evidence (2026-08-11)

```text
DB: %AppData%/com.callanalyzer.desktop/call-analyzer.db
Command:
  ca.exe --db <app-db> score --provider nvidia_nim --model meta/llama-3.1-8b-instruct --limit 1

Result:
  call_871c995db190415a8bb1b42d73a70a45 — 82.9% overall in 6s
  provider nvidia-nim / meta/llama-3.1-8b-instruct
  tokens 1959 in / 733 out (this call); batch log 4099/1558
  rubric default v1 — Greeting 100, Active Listening 80, Empathy 60, Resolution 90, Professionalism 90
```

Corpus snapshot after that run: analyzed=2, redacted=13, discovered≈1952, failed=2
(failed rows were earlier timeouts on `meta/llama-3.3-70b-instruct` — use 8b/nano for smoke).

## Phase 1 status: COMPLETE

Foundations are proven. Do not rebuild the whole workspace until disk is freed (C: was ~4.5 GB free).

## Next (Phase 2 sketch)

Multi-agent battery on redacted calls (QA already works; add sentiment + compliance + root-cause), dashboard refresh, optional NAT supervisor wiring — sample-first, one call, then small batch. Keep metered NIM on fast models.
