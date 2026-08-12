# Contributing to Call Analyzer

Thank you for your interest in contributing! This document explains how to set up your environment, run the project, and submit changes.

---

## Table of Contents

1. [Code of Conduct](#code-of-conduct)
2. [Getting Started](#getting-started)
3. [Project Structure](#project-structure)
4. [Development Setup](#development-setup)
5. [Running the App](#running-the-app)
6. [Running Tests](#running-tests)
7. [Commit Conventions](#commit-conventions)
8. [Pull Request Process](#pull-request-process)
9. [What CI Checks](#what-ci-checks)
10. [Reporting Bugs](#reporting-bugs)
11. [Feature Requests](#feature-requests)

---

## Code of Conduct

This project adheres to a standard open-source code of conduct. By participating you agree to:

- Be respectful and inclusive.
- Welcome newcomers and help them get started.
- Focus on constructive feedback.

---

## Getting Started

### Prerequisites

| Tool | Version | Notes |
|------|---------|-------|
| Rust | 1.82+ | Install via [rustup](https://rustup.rs); `rust-toolchain.toml` pins the rest |
| Node.js | 20+ | 24 is what CI uses |
| npm | 10+ | Ships with Node. This repo uses npm lockfiles — not pnpm or yarn |
| Tauri CLI | 2.x | A devDependency of `apps/desktop`; `npm ci` installs it |
| Windows SDK | — | Required for desktop builds |
| NVIDIA API key | — | Optional; for LLM features |

Two optional Cargo tools make local checks match CI:

```bash
cargo install cargo-nextest --locked   # faster test runner, used by CI
cargo install cargo-deny --locked      # licence and advisory policy
```

### Clone the Repository

```bash
git clone https://github.com/<your-org>/call-analyzer.git
cd call-analyzer
```

### Install Dependencies

```bash
# Rust crates (workspace)
cargo fetch

# UI dependencies — two package roots: the React app and the Tauri CLI
npm ci --prefix apps/desktop/ui
npm ci --prefix apps/desktop
```

### Fetch the model weights

Whisper weights are not in the repository — they are hundreds of megabytes
and GitHub refuses files over 100 MB. Download them:

```powershell
pwsh scripts/fetch-models.ps1
```

---

## Project Structure

```
call-analyzer/
├── apps/
│   └── desktop/              # Tauri 2 + React desktop shell
│       ├── src-tauri/        # Rust backend (Tauri commands)
│       └── ui/               # React frontend (Vite + TypeScript)
├── crates/
│   ├── ca-core/              # Domain types & shared traits
│   ├── ca-audio/             # Audio decode, waveform helpers
│   ├── ca-asr/               # Whisper CLI ASR integration
│   ├── ca-diarize/           # Speaker diarization
│   ├── ca-privacy/           # PII redaction
│   ├── ca-agents/            # QA / sentiment / compliance agents
│   ├── ca-llm/               # NIM + OpenAI-compat + Anthropic + Gemini
│   ├── ca-pipeline/          # Orchestration pipeline
│   ├── ca-store/             # SQLite corpus layer
│   ├── ca-ingest/            # Source ingestion
│   ├── ca-insights/          # Analytics & reporting
│   ├── ca-metrics/           # Scoring metrics
│   ├── ca-config/            # Configuration management
│   └── ca-cli/               # Headless batch runner
├── docs/
│   ├── PHASE1_FOUNDATIONS.md
│   └── PHASE2_AGENTS.md
├── scripts/                  # Build & helper scripts
├── models/                   # Model configs & prompts
├── vendor/                   # Vendored dependencies
├── Cargo.toml                # Workspace root
├── LICENSE                   # MIT License
└── README.md
```

---

## Development Setup

### 1. Configure Environment Variables

Copy the example env file and fill in your values:

```bash
cp .env.example .env
```

Edit `.env` with your NVIDIA API key (or other provider keys). **Never commit `.env`.**

### 2. Build the Workspace

```bash
# Build all crates
cargo build --workspace

# Build with release optimizations
cargo build --workspace --release
```

### 3. Build the UI

```bash
cd apps/desktop/ui
npm run dev   # starts the Vite dev server with HMR
```

---

## Running the App

### Desktop (Tauri)

```bash
cd apps/desktop
npm run dev
```

This launches the desktop app with hot-reload enabled. `npm run build` in the
same directory produces the MSI and NSIS installers under
`target/release/bundle/`.

### Headless CLI

```bash
# Discover calls in the database
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" discover

# Score a call using NVIDIA NIM
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" \
  score --provider nvidia_nim --model meta/llama-3.1-8b-instruct --limit 1

# Generate a report
./target/release/ca.exe --db "$APPDATA/com.callanalyzer.desktop/call-analyzer.db" report <call_id>
```

---

## Running Tests

```bash
# Everything CI runs, in the same order — start here
pwsh scripts/preflight.ps1

# Or individually:
cargo nextest run --workspace          # what CI uses
cargo test --workspace                 # if nextest is not installed
cargo test -p ca-core                  # one crate
cargo test --workspace -- --nocapture  # with output
cargo test --doc --workspace           # doctests (nextest does not run these)
```

The frontend has no test suite yet; `npm run lint` and `npm run build` in
`apps/desktop/ui` are what CI checks.

---

## Commit Conventions

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

### Types

| Type | Description |
|------|-------------|
| `feat` | New feature |
| `fix` | Bug fix |
| `docs` | Documentation only |
| `refactor` | Code change that neither fixes a bug nor adds a feature |
| `perf` | Performance improvement |
| `test` | Adding or updating tests |
| `chore` | Maintenance tasks (deps, build, CI) |
| `ci` | CI/CD changes |

### Examples

```
feat(asr): add whisper.cpp backend support
fix(privacy): handle empty transcript in PII redaction
docs(readme): update build instructions for Windows
refactor(pipeline): extract stage runner trait
chore(deps): bump reqwest to 0.12
```

**Your pull request title must follow this format too** — CI checks it, and
because merges are squashed with the PR title as the commit subject, that
title is what ends up in the release notes.

---

## Pull Request Process

1. **Fork** the repository and create a feature branch:
   ```bash
   git checkout -b feat/my-new-feature
   ```

2. **Make your changes** following the project's coding style.

3. **Run the checks** — the same ones CI will run, so a failure is something
   you see in ten seconds rather than ten minutes after pushing:
   ```powershell
   pwsh scripts/preflight.ps1        # or scripts/preflight.sh in Git Bash
   pwsh scripts/preflight.ps1 -Fix   # applies formatting and safe clippy fixes
   ```

4. **Commit** with a descriptive message following the conventions above.

5. **Push** to your fork and open a Pull Request against `main`.

6. **Describe your PR** with:
   - What it does
   - Why it's needed
   - How to test it
   - Screenshots (for UI changes)

7. **Address review feedback** promptly.

8. Once approved, a maintainer will merge your PR.

---

## What CI Checks

Two status checks must pass before a pull request can merge. Both are roll-up
jobs, so the individual job that failed is one click away.

**`CI`** — repo hygiene and the PR title, `cargo fmt --check`, clippy with
`-D warnings`, the test suite plus doctests, rustdoc, the UI lint/type-check/
build, and a coverage report. An installer is built too, but only on `main`
or when a PR carries the `full-build` label.

**`Security`** — secret scanning over full history, `cargo-deny` for
advisories/licences/banned crates/sources, `npm audit`, dependency review,
CodeQL over the TypeScript, and actionlint over the workflows.

A nightly run covers the slower and more advisory things — the installer
build, dependency drift, MSRV, spelling, unused dependencies. None of it
blocks a merge.

`scripts/preflight.ps1` runs the same gates locally. Where the two disagree,
CI is right: it builds from a clean checkout with the MSVC toolchain.

Full detail, including what to do about each kind of failure, is in
[docs/CI_CD.md](docs/CI_CD.md).

---

## Reporting Bugs

Please use the [GitHub Issues](https://github.com/Yash-Kavaiya/call-analyzer/issues) tracker.

When filing a bug, include:

- **Title**: Clear, concise description
- **Environment**: OS, Rust version, Node version, Tauri version
- **Steps to reproduce**: Numbered list
- **Expected behavior**: What you expected to happen
- **Actual behavior**: What actually happened
- **Logs / screenshots**: Any relevant output

### Bug Report Template

```markdown
**Describe the bug**
A clear description of what the bug is.

**To Reproduce**
1. Go to '...'
2. Click on '...'
3. See error

**Expected behavior**
A clear description of what you expected to happen.

**Environment**
- OS: [e.g. Windows 11]
- Rust: [e.g. 1.82.0]
- Node: [e.g. 20.10.0]
- Tauri: [e.g. 2.0.0]

**Additional context**
Add any other context about the problem here.
```

---

## Feature Requests

We welcome feature requests! Please open a GitHub Issue with the label `enhancement` and describe:

- The problem you're trying to solve
- Your proposed solution
- Any alternatives you've considered
- How it fits with the project's goals

---

## Style Guidelines

### Rust

- Run `cargo fmt --all` before committing. CI rejects unformatted code.
- Run `cargo clippy --workspace --all-targets -- -D warnings`. CI treats every
  clippy warning as an error; silencing one with `#[allow(...)]` is a valid
  answer when accompanied by a comment explaining why.
- Follow the existing module structure in `crates/`.
- Adding a dependency? `cargo deny check` locally first. The project's licence
  policy is in `deny.toml` and it will reject anything GPL, anything pulling
  in OpenSSL, and anything from outside crates.io.

### TypeScript / React

- Run `npm run lint` (oxlint) in `apps/desktop/ui` before committing.
- Use functional components with hooks.
- Keep components small and focused.

### Documentation

- Update `README.md` for user-facing changes.
- Update `docs/` for architecture or phase changes.
- Add doc comments (`///`) to public Rust APIs.

---

## Questions?

- Open a [Discussion](https://github.com/<your-org>/call-analyzer/discussions) for general questions.
- Check existing [Issues](https://github.com/<your-org>/call-analyzer/issues) before creating a new one.

Thank you for contributing! 🎉
