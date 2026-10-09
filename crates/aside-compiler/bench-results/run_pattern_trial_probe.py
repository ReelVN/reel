#!/usr/bin/env python3
"""Capture the Type/OR regression and location/pattern controls with real CLIs."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

if len(sys.argv) != 6:
    raise SystemExit("usage: run_pattern_trial_probe.py BEFORE_DEBUG BEFORE_RELEASE AFTER_DEBUG AFTER_RELEASE AFTER_REVISION")

cases = [
    ("tuple-type", "((a,b) is Pair) = value;", "TypePattern", True),
    ("tuple-or", "((a,b) | (a,b)) = value;", "OrPattern", True),
    ("array-type-union", "(([a,...rest]) is (Array|null)) = value;", "TypePattern", True),
    ("array-or", "(([a,...rest]) | ([a,...rest])) = value;", "OrPattern", True),
    ("call-location", "(f(a, b).x) = value;", "GroupExpr", False),
    ("index-location", "(a[(i, j)]) = value;", "GroupExpr", False),
    ("array-location", "([1][0]) = value;", "GroupExpr", False),
    ("object-location", "({x: 0}.x) = value;", "GroupExpr", False),
    ("grouped-tuple", "((a,b)) = pair;", "TuplePattern", False),
    ("grouped-array-rest", "(([a,...rest])) = items;", "ArrayPattern", False),
    ("grouped-object-rest", "(({name,...rest})) = record;", "ObjectPattern", False),
]
binaries = [Path(p).resolve() for p in sys.argv[1:5]]
report = {
    "before_revision": "b6463fa150363ccaa0e627e45a1c04846976364d",
    "after_revision": sys.argv[5],
    "binary_sha256": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in binaries},
    "environment": {"ASIDE_DUMP_TREE": "1"},
    "runs": [],
}
env = os.environ.copy()
env["ASIDE_DUMP_TREE"] = "1"
with tempfile.TemporaryDirectory(prefix="reel-pattern-trial-") as directory:
    for name, source, kind, failed_before in cases:
        path = Path(directory) / f"{name}.aside"
        path.write_text(source, encoding="utf-8")
        for entry in ("module", "block"):
            for index, binary in enumerate(binaries):
                stage = "before" if index < 2 else "after"
                profile = "debug" if index % 2 == 0 else "release"
                command = [str(binary), str(path), entry]
                result = subprocess.run(command, env=env, text=True, capture_output=True)
                report["runs"].append({
                    "case": name, "source": source, "entry": entry,
                    "stage": stage, "profile": profile, "argv": command,
                    "exit_code": result.returncode,
                    "stdout": result.stdout, "stderr": result.stderr,
                })
                expected_exit = 1 if stage == "before" and failed_before else 0
                assert result.returncode == expected_exit, (name, entry, stage, profile, result.stderr)
                if expected_exit:
                    assert "ASP009" in result.stderr and "GroupExpr@" in result.stdout
                else:
                    assert "AssignExpr@" in result.stdout and f"{kind}@" in result.stdout
                    assert not result.stderr, (name, entry, stage, profile)
                    if kind == "GroupExpr":
                        assert "Pattern@" not in result.stdout
print(json.dumps(report, ensure_ascii=False, indent=2))
