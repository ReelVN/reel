#!/usr/bin/env python3
"""Run the built stage contracts alone, measuring Linux whole-process peak RSS."""
import argparse
import json
import resource
import subprocess
import sys
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("build_json", type=Path, help="cargo test --no-run JSON output")
parser.add_argument("output", type=Path)
args = parser.parse_args()
exe = None
for line in args.build_json.read_text().splitlines():
    obj = json.loads(line)
    if (obj.get("reason") == "compiler-artifact"
            and obj["target"]["name"] == "contracts"
            and "/reel-stage/" in obj["manifest_path"]
            and obj["executable"]):
        exe = obj["executable"]
assert exe, "stage contracts executable missing from build output"
with args.output.open("w") as log:
    run = subprocess.run([exe, "--test-threads=1", "--nocapture"],
                         stdout=log, stderr=subprocess.STDOUT)
    rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    log.write(f"\nWhole test process peak RSS (Linux KiB, compiler excluded): {rss}\n")
print(f"stage contracts exit={run.returncode}, whole-process peak RSS={rss} KiB")
sys.exit(run.returncode)
