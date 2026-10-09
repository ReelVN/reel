#!/usr/bin/env python3
"""Run one release probe process; report that child's maximum RSS, not cargo's."""
import pathlib
import resource
import subprocess
import sys

mode = sys.argv[1] if len(sys.argv) > 1 else "parse"
if mode not in ("parse", "lex", "named"):
    raise SystemExit("usage: run_probe.py [parse|lex|named]")
root = pathlib.Path(__file__).resolve().parents[3]
result = subprocess.run([str(root / "target/release/examples/frontend_probe"), mode], check=True)
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
print(f"child_max_rss_kib={usage.ru_maxrss}", file=sys.stderr)
print(f"child_user_seconds={usage.ru_utime:.6f}", file=sys.stderr)
print(f"child_system_seconds={usage.ru_stime:.6f}", file=sys.stderr)
