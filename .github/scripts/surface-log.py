#!/usr/bin/env python3
"""Publish cargo diagnostics in a place the sandbox can read.

Workflow logs themselves are served from a host this sandbox cannot reach, so
this script (a) emits ``::error::`` annotations for the web UI and (b) creates a
check run whose ``output.text`` carries the tail of the cargo log; check-run
output *is* readable through the REST API.

Usage: surface-log.py <log file> <TAG>
Requires GITHUB_TOKEN with ``checks: write`` plus GITHUB_REPOSITORY/GITHUB_SHA.
"""
import json
import os
import sys
import urllib.request
from pathlib import Path

log_file = Path(sys.argv[1])
tag = sys.argv[2] if len(sys.argv) > 2 else "CARGO"
text = log_file.read_text(errors="replace") if log_file.exists() else "(no log)"


def escape(chunk: str) -> str:
    return chunk.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


# Annotations (web UI), newest-last so the interesting lines are kept.
print(f"::error title={tag}_TAIL::{escape(text[-8000:])}")
interesting = [ln for ln in text.splitlines() if ln.strip() and ("error" in ln or "warning:" in ln)]
for index, line in enumerate(interesting[-20:]):
    print(f"::error title={tag}_{index}::{escape(line[:900])}")

# Check run carrying the whole tail of the log (API readable).
token = os.environ.get("GITHUB_TOKEN", "")
repo = os.environ.get("GITHUB_REPOSITORY", "")
sha = os.environ.get("GITHUB_SHA", "")
if not (token and repo and sha):
    print("no token/repo/sha; skipping check run")
    sys.exit(0)

payload = {
    "name": f"cargo diagnostics ({tag})",
    "head_sha": sha,
    "status": "completed",
    "conclusion": "failure",
    "output": {
        "title": f"cargo {tag} failure",
        "summary": f"tail of the cargo {tag} log",
        "text": text[-60000:],
    },
}
request = urllib.request.Request(
    f"https://api.github.com/repos/{repo}/check-runs",
    data=json.dumps(payload).encode(),
    method="POST",
    headers={
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github+json",
        "Content-Type": "application/json",
        "User-Agent": "ci-log-surface",
    },
)
try:
    with urllib.request.urlopen(request) as response:
        print("posted check run:", response.status)
except Exception as exc:  # noqa: BLE001 - best effort diagnostics channel
    print("failed to post check run:", exc)
