<!--
The PR title must follow Conventional Commits — CI checks it.
  feat(asr): add whisper.cpp backend
  fix(privacy): handle empty transcript in redaction
-->

## What this changes

<!-- One or two sentences. What is different after this merges? -->

## Why

<!-- The problem, not the solution. Link the issue if there is one. -->

Closes #

## How it was verified

<!-- Say what you actually ran, not what you intend to. -->

- [ ] `cargo nextest run --workspace` (or `cargo test --workspace`)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo fmt --all`
- [ ] Ran the desktop app and exercised the change by hand
- [ ] Not applicable — explain below

## Risk

<!-- Delete the lines that do not apply. -->

- [ ] Touches PII redaction (`ca-privacy`) — describe what was tested with real-shaped data
- [ ] Changes the database schema (`ca-store`) — describe the migration path for existing installs
- [ ] Changes credential handling (`ca-llm/secrets`) — confirm no key can reach a log, the database, or the UI
- [ ] Changes packaging (`tauri.conf.json`, capabilities) — add the `full-build` label so CI builds an installer
- [ ] Adds a dependency — say why, and what it replaces or saves
- [ ] None of the above

## Screenshots

<!-- Required for UI changes. Before and after. -->
