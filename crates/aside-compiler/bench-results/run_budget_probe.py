#!/usr/bin/env python3
"""Isolate 5k/50k chains under a 2 MiB stack, core dumps off (Linux)."""
import pathlib
import resource
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parents[3]
binary = sys.argv[1] if len(sys.argv) > 1 else str(root / "target/debug/examples/budget_probe")

def child_limits():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_STACK, (2 * 1024 * 1024, 2 * 1024 * 1024))

for count in (5000, 50000):
    child = subprocess.run([binary, str(count)], capture_output=True, text=True,
                           timeout=15, preexec_fn=child_limits)
    print(f"else_count={count},child_stack_bytes=2097152,exit={child.returncode}")
    print(child.stdout, end="")
    print(child.stderr[:300], end="")
