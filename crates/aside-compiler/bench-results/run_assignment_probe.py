#!/usr/bin/env python3
"""Record actual before/after check CLI trees and diagnostics in both entries."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

if len(sys.argv) != 3:
    raise SystemExit("usage: run_assignment_probe.py BEFORE_CHECK AFTER_CHECK")

cases = [
    ("call-arguments-location", "(f(a, b).x) = value;", "location", True),
    ("tuple-index-location", "(a[(i, j)]) = value;", "location", True),
    ("array-receiver-location", "([1][0]) = value;", "location", True),
    ("object-receiver-location", "({x: 0}.x) = value;", "location", True),
    ("call-location-control", "(f(a).x) = value;", "location", False),
    ("index-location-control", "(a[i]) = value;", "location", False),
    ("grouped-tuple-pattern", "((a, b)) = pair;", "pattern", False),
    ("grouped-array-pattern", "(([a, ...rest])) = pair;", "pattern", False),
    ("grouped-object-pattern", "(({name, ...rest})) = pair;", "pattern", False),
]
report = {
    "before_revision": "fc418d9a26ebf5f0dfdbde2d6c25670e78c94926",
    "after_revision": "d453c6a7faefe684ec82cbf692eba11a8bf4bc54",
    "environment": {"ASIDE_DUMP_TREE": "1"},
    "runs": [],
}
env = os.environ.copy()
env["ASIDE_DUMP_TREE"] = "1"
with tempfile.TemporaryDirectory(prefix="reel-assignment-probe-") as directory:
    for name, source, category, failed_before in cases:
        path = Path(directory) / f"{name}.aside"
        path.write_text(source, encoding="utf-8")
        for entry in ("module", "block"):
            for stage, binary in zip(("before", "after"), sys.argv[1:]):
                command = [str(Path(binary).resolve()), str(path), entry]
                result = subprocess.run(command, env=env, text=True, capture_output=True)
                report["runs"].append({
                    "case": name, "source": source, "category": category,
                    "entry": entry, "stage": stage, "argv": command,
                    "exit_code": result.returncode,
                    "stdout": result.stdout, "stderr": result.stderr,
                })
                expected_exit = 1 if stage == "before" and failed_before else 0
                assert result.returncode == expected_exit, (name, entry, stage, result.stderr)
                if stage == "after" or not failed_before:
                    assert "AssignExpr@" in result.stdout, (name, entry, stage)
                    if category == "location":
                        assert "GroupExpr@" in result.stdout and "Pattern@" not in result.stdout
                    else:
                        assert "GroupPattern@" in result.stdout
print(json.dumps(report, ensure_ascii=False, indent=2))
