#!/usr/bin/env python3
"""Phase 1 — validate NVIDIA_API_KEY against hosted NIM (build.nvidia.com).

Fail-closed: exits non-zero if the key is missing, rejected, or no usable
chat model answers. Never prints the secret.

Usage (git-bash / PowerShell):
  export NVIDIA_API_KEY=nvapi-...   # or load from hermes/.env
  python scripts/validate_nvidia.py
  python scripts/validate_nvidia.py --model nvidia/nemotron-3-nano-30b-a3b
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

DEFAULT_BASE = "https://integrate.api.nvidia.com/v1"
# Fast, free-tier friendly default used for smoke tests.
DEFAULT_MODEL = "meta/llama-3.1-8b-instruct"
# Architecture / agentic defaults worth proving when the key has quota.
AGENTIC_MODELS = (
    "nvidia/nemotron-3-nano-30b-a3b",
    "nvidia/nemotron-3-super-120b-a12b",
)


def load_dotenv(path: Path) -> None:
    if not path.is_file():
        return
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, _, v = line.partition("=")
        k, v = k.strip(), v.strip().strip('"').strip("'")
        if k and k not in os.environ:
            os.environ[k] = v


def redact(key: str) -> str:
    if len(key) <= 12:
        return "***"
    return f"{key[:8]}...{key[-4:]} (len={len(key)})"


def http_json(method: str, url: str, key: str, payload: dict | None = None, timeout: int = 90):
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {key}",
            "Accept": "application/json",
            **({"Content-Type": "application/json"} if data is not None else {}),
        },
    )
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            body = resp.read().decode("utf-8", errors="replace")
            return resp.status, json.loads(body) if body else {}, int((time.time() - t0) * 1000)
    except urllib.error.HTTPError as e:
        err = e.read().decode("utf-8", errors="replace")[:400]
        raise RuntimeError(f"HTTP {e.code} from {url}: {err}") from e
    except Exception as e:
        raise RuntimeError(f"{type(e).__name__} calling {url}: {e}") from e


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate NVIDIA_API_KEY against hosted NIM")
    parser.add_argument("--base-url", default=os.environ.get("NVIDIA_BASE_URL", DEFAULT_BASE))
    parser.add_argument("--model", default=os.environ.get("NVIDIA_CHAT_MODEL", DEFAULT_MODEL))
    parser.add_argument(
        "--agentic",
        action="store_true",
        help="Also smoke-test Nemotron-3 nano + super (slower / more tokens)",
    )
    parser.add_argument(
        "--dotenv",
        action="append",
        default=[],
        help="Optional .env path to load if NVIDIA_API_KEY is unset",
    )
    args = parser.parse_args()

    # Project-local then Hermes profile env (never print values).
    roots = [
        Path.cwd() / ".env",
        Path(__file__).resolve().parents[1] / ".env",
        Path.home() / "AppData" / "Local" / "hermes" / ".env",
    ]
    for extra in args.dotenv:
        roots.append(Path(extra))
    for p in roots:
        load_dotenv(p)

    key = (os.environ.get("NVIDIA_API_KEY") or "").strip()
    if not key:
        print("FAIL: NVIDIA_API_KEY is not set.")
        print("  1) Create a free account at https://build.nvidia.com")
        print("  2) Generate an API key (nvapi-...)")
        print("  3) export NVIDIA_API_KEY=nvapi-...   # or put it in .env (never commit)")
        return 2
    if not key.startswith("nvapi-"):
        print(f"WARN: key does not start with nvapi- ({redact(key)})")

    base = args.base_url.rstrip("/")
    print("Phase 1 — NVIDIA NIM validation")
    print(f"  endpoint : {base}")
    print(f"  key      : {redact(key)}")
    print(f"  model    : {args.model}")

    # 1) Catalogue reachability
    status, models_payload, ms = http_json("GET", f"{base}/models", key, timeout=30)
    models = models_payload.get("data") or []
    ids = sorted(m.get("id", "") for m in models if isinstance(m, dict))
    print(f"  models   : HTTP {status} in {ms} ms — {len(ids)} models visible")
    for needle in ("nemotron-3", "llama-3.1-8b", "nv-embedqa", "nemoguard", "riva-translate"):
        hits = [i for i in ids if needle in i]
        print(f"    contains '{needle}': {len(hits)}  e.g. {hits[:3]}")

    # 2) Minimal chat completion (matches desktop Settings → Test connection intent)
    payload = {
        "model": args.model,
        "messages": [{"role": "user", "content": "Reply with the single word: ready"}],
        "max_tokens": 16,
        "temperature": 0,
    }
    status, chat, ms = http_json("POST", f"{base}/chat/completions", key, payload, timeout=120)
    choice = (chat.get("choices") or [{}])[0]
    message = choice.get("message") or {}
    content = (message.get("content") or message.get("reasoning_content") or "").strip()
    used = chat.get("model") or args.model
    print(f"  chat     : HTTP {status} in {ms} ms — model={used!r}")
    print(f"  reply    : {content[:160]!r}")
    print(f"  usage    : {chat.get('usage')}")

    if status != 200 or not content:
        print("FAIL: chat completion did not return usable content")
        return 1

    if args.agentic:
        print("  agentic smoke:")
        for model in AGENTIC_MODELS:
            try:
                st, body, lat = http_json(
                    "POST",
                    f"{base}/chat/completions",
                    key,
                    {
                        "model": model,
                        "messages": [{"role": "user", "content": "Say OK"}],
                        "max_tokens": 24,
                        "temperature": 0,
                    },
                    timeout=180,
                )
                msg = ((body.get("choices") or [{}])[0].get("message") or {})
                text = (msg.get("content") or msg.get("reasoning_content") or "")[:80]
                print(f"    OK  {model}  {lat} ms  {text!r}")
            except Exception as e:
                print(f"    FAIL {model}: {e}")
                return 1

    print("PASS: NVIDIA_API_KEY is valid against hosted NIM.")
    print("Next: launch Call Analyzer desktop and use Settings → NVIDIA NIM → Test connection.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
