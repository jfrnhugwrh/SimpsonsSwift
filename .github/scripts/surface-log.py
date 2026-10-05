#!/usr/bin/env python3
"""Emit cargo output as GitHub Actions error annotations.

Workflow logs live on a host this sandbox cannot reach, but check-run
annotations are readable through the REST API.  Escape a chunk of the log and
print it as an ``::error::`` workflow command so it lands in an annotation.
"""
import sys
from pathlib import Path

log_path = Path(sys.argv[1])
tag = sys.argv[2] if len(sys.argv) > 2 else "CARGO"
text = log_path.read_text(errors="replace") if log_path.exists() else "(no log)"


def escape(chunk: str) -> str:
    return (
        chunk.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    )


# One big annotation with the tail of the log (errors are usually last).
tail = text[-8000:]
print(f"::error title={tag}_TAIL::{escape(tail)}")

# Plus individual annotations for lines that look like diagnostics, newest last
# so the most interesting ones survive if GitHub drops some.
interesting = [
    line
    for line in text.splitlines()
    if ("error" in line or "warning:" in line) and line.strip()
]
for index, line in enumerate(interesting[-20:]):
    print(f"::error title={tag}_{index}::{escape(line[:900])}")
