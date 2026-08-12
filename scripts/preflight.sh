#!/usr/bin/env bash
# Runs the same checks CI runs, before you push.
#
# The PowerShell version (scripts/preflight.ps1) is the fuller one and is what
# most contributors on Windows should use. This exists for Git Bash and WSL.
#
# Usage:
#   scripts/preflight.sh            full run
#   scripts/preflight.sh --fix      apply formatting instead of checking it
#   scripts/preflight.sh --fast     skip doctests, rustdoc, cargo-deny and the UI
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

FIX=0
FAST=0
for arg in "$@"; do
  case "$arg" in
    --fix)  FIX=1 ;;
    --fast) FAST=1 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg" >&2; exit 2 ;;
  esac
done

# cargo is installed per-user and is not always on PATH in a non-login shell.
if ! command -v cargo >/dev/null 2>&1; then
  if [ -x "$HOME/.cargo/bin/cargo" ]; then
    export PATH="$HOME/.cargo/bin:$PATH"
  else
    echo "cargo not found. Install Rust from https://rustup.rs and re-run." >&2
    exit 1
  fi
fi

FAILED=0
declare -a SUMMARY=()

gate() {
  local name="$1"; shift
  local hint="$1"; shift

  printf '\n\033[36m── %s \033[0m' "$name"
  printf '%.0s─' $(seq 1 $((60 - ${#name}))); printf '\n'

  local start; start=$(date +%s)
  if "$@"; then
    SUMMARY+=("pass  $name  ($(( $(date +%s) - start ))s)")
  else
    SUMMARY+=("FAIL  $name  ($(( $(date +%s) - start ))s)")
    FAILED=$((FAILED + 1))
    printf '\033[31m%s failed.\033[0m\n' "$name"
    [ -n "$hint" ] && printf '\033[33m  %s\033[0m\n' "$hint"
  fi
}

# rust-toolchain.toml declares these, but a stale rustup will not have picked
# them up, and the failure that follows is cryptic.
for component in rustfmt clippy; do
  if ! rustup component list --installed 2>/dev/null | grep -q "^$component"; then
    echo "Installing missing component: $component"
    rustup component add "$component"
  fi
done

if [ "$FIX" -eq 1 ]; then
  gate "Format (applying)" "" cargo fmt --all
else
  gate "Format" "Run: cargo fmt --all   (or re-run with --fix)" \
    cargo fmt --all -- --check
fi

gate "Lockfile" "Cargo.lock is out of date. Run: cargo update --workspace  and commit the result." \
  cargo metadata --locked --format-version 1

gate "Clippy" "CI treats every clippy warning as an error." \
  cargo clippy --workspace --all-targets --locked -- -D warnings

if command -v cargo-nextest >/dev/null 2>&1; then
  gate "Tests" "See the failure output above." \
    cargo nextest run --workspace --all-targets --locked
else
  echo "cargo-nextest not installed; falling back to cargo test."
  echo "  Install it for faster runs: cargo install cargo-nextest --locked"
  gate "Tests" "See the failure output above." \
    cargo test --workspace --locked
fi

if [ "$FAST" -eq 0 ]; then
  gate "Doctests" "An example in a doc comment no longer compiles." \
    cargo test --doc --workspace --locked

  # Exported rather than prefixed: a `VAR=x func` prefix on a shell function
  # leaks the variable into the rest of the script in bash.
  export RUSTDOCFLAGS="-D warnings"
  gate "Rustdoc" "Usually a broken [Type] link in a doc comment." \
    cargo doc --workspace --no-deps --locked
  unset RUSTDOCFLAGS

  if command -v cargo-deny >/dev/null 2>&1; then
    gate "Supply chain" "See deny.toml. A new dependency may carry a licence this product cannot ship." \
      cargo deny check
  else
    echo "cargo-deny not installed; skipping the supply chain gate."
    echo "  Install it: cargo install cargo-deny --locked"
  fi

  if [ -d apps/desktop/ui ]; then
    [ -d apps/desktop/ui/node_modules ] || (cd apps/desktop/ui && npm ci)
    gate "UI lint"  "Run: npm run lint  in apps/desktop/ui" \
      bash -c 'cd apps/desktop/ui && npm run lint'
    gate "UI build" "tsc or vite failed; see the output above." \
      bash -c 'cd apps/desktop/ui && npm run build'
  fi
fi

printf '\n'
printf '═%.0s' $(seq 1 64); printf '\n'
printf '%s\n' "${SUMMARY[@]}"

if [ "$FAILED" -gt 0 ]; then
  printf '\n\033[31m%d gate(s) failed. CI would reject this.\033[0m\n' "$FAILED"
  exit 1
fi

printf '\n\033[32mAll gates passed.\033[0m\n'
[ "$FAST" -eq 1 ] && printf '\033[33mNote: --fast skipped several gates. Run in full before pushing.\033[0m\n'
exit 0
