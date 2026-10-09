#!/usr/bin/env python3
"""Run one real R01 residency probe; Linux child peak RSS excludes the compiler."""
import argparse
import json
import os
import resource
import subprocess
import sys
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("build_json", type=Path)
parser.add_argument("output", type=Path)
parser.add_argument("--case", choices=["rect", "path"], required=True)
parser.add_argument("--launch-control", action="store_true",
                    help="run /bin/true with the same launcher to expose its RSS floor")
args = parser.parse_args()
exe = None
for line in args.build_json.read_text().splitlines():
    obj = json.loads(line)
    if (obj.get("reason") == "compiler-artifact"
            and obj["target"]["name"] == "r01_measurements"
            and "/reel-stage/" in obj["manifest_path"]
            and obj["executable"]):
        exe = obj["executable"]
assert exe, "R01 measurement executable missing from build output"
env = os.environ.copy()
env["REEL_R01_PROBE_CASE"] = args.case
with args.output.open("w") as log:
    run = subprocess.run(
        ["/bin/true"] if args.launch_control else
        [exe, "--ignored", "--test-threads=1", "--nocapture"],
        stdout=log, stderr=subprocess.STDOUT, env=env,
    )
    peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    log.write(f"\nWhole probe process peak RSS (Linux KiB, compiler excluded): {peak}\n")
mode = "launch control" if args.launch_control else args.case
print(f"R01 {mode} exit={run.returncode}, whole-process peak RSS={peak} KiB")
sys.exit(run.returncode)
