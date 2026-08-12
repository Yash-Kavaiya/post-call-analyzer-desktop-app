# Call Analyzer

Windows desktop app for contact-center call intelligence: local transcription/diarization/PII redaction, multi-vendor LLM scoring (default **NVIDIA NIM**), NVIDIA-themed dashboard.

## Phase 2 (multi-agent battery)

See [docs/PHASE2_AGENTS.md](docs/PHASE2_AGENTS.md).

```bash
# QA + sentiment + compliance + root cause on one redacted call
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" \
  score --provider nvidia_nim --model meta/llama-3.1-8b-instruct --limit 1

./target/release/ca.exe --db ... report <call_id>
```

Prebuilt binaries (when present):

- `target/release/call-analyzer-desktop.exe` — UI
- `target/release/ca.exe` — headless discover / run / score

## Layout

```
apps/desktop/          Tauri 2 + React UI
crates/ca-core         domain types
crates/ca-audio        decode / waveform helpers
crates/ca-asr          Whisper CLI ASR
crates/ca-diarize      speaker diarization
crates/ca-privacy      PII scrub
crates/ca-agents       QA / sentiment / compliance agents
crates/ca-llm          NIM + OpenAI-compat + Anthropic + Gemini
crates/ca-pipeline     orchestration
crates/ca-store        SQLite corpus
crates/ca-cli          batch runner
```

## Security

- API keys never round-trip through the UI after save (Credential Manager only).
- Cloud providers require redaction before send (`redact_before_send`).
- `.env` is gitignored; use `.env.example` as the template.
