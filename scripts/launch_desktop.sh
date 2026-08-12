#!/usr/bin/env bash
# Launch the Windows Call Analyzer desktop app with NVIDIA_API_KEY available.
# Resolution order for the key:
#   1) already-exported NVIDIA_API_KEY
#   2) repo .env
#   3) %LOCALAPPDATA%/hermes/.env
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

load_env() {
  local f="$1"
  [[ -f "$f" ]] || return 0
  # shellcheck disable=SC1090
  set -a
  # Only export KEY=VALUE lines; ignore comments/blank.
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    [[ "$line" =~ ^[[:space:]]*$ ]] && continue
    if [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]]; then
      export "${BASH_REMATCH[1]}=${BASH_REMATCH[2]}"
    fi
  done < "$f"
  set +a
}

load_env "$ROOT/.env"
load_env "${LOCALAPPDATA:-$HOME/AppData/Local}/hermes/.env"

if [[ -z "${NVIDIA_API_KEY:-}" ]]; then
  echo "ERROR: NVIDIA_API_KEY not set. Run: python scripts/validate_nvidia.py" >&2
  exit 2
fi

echo "NVIDIA_API_KEY present (len=${#NVIDIA_API_KEY})"
echo "Validating hosted NIM before launch..."
python "$ROOT/scripts/validate_nvidia.py" || exit $?

EXE=""
for candidate in \
  "$ROOT/target/release/call-analyzer-desktop.exe" \
  "$ROOT/target/debug/call-analyzer-desktop.exe"
do
  if [[ -x "$candidate" || -f "$candidate" ]]; then
    EXE="$candidate"
    break
  fi
done

if [[ -z "$EXE" ]]; then
  echo "ERROR: call-analyzer-desktop.exe not found. Build with: cargo build -p call-analyzer-desktop --release" >&2
  exit 1
fi

echo "Launching: $EXE"
# Start detached so this shell can return; env is inherited by the child.
if command -v cmd.exe >/dev/null 2>&1; then
  # Windows path for CreateProcess
  win_exe=$(cygpath -w "$EXE" 2>/dev/null || echo "$EXE")
  cmd.exe //c start "" "$win_exe" >/dev/null 2>&1 || "$EXE" &
else
  "$EXE" &
fi
echo "Desktop started. In Settings: provider=NVIDIA NIM, paste/store key if needed, Test connection."
