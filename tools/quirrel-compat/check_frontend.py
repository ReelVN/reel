#!/usr/bin/env python3
"""Build an external probe against exactly PR #9 fc418d9, then parse derivatives."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

from convert import map_range

HERE = Path(__file__).resolve().parent
COMMIT = "fc418d9a26ebf5f0dfdbde2d6c25670e78c94926"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--frontend", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True, help="new directory for probe project")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--cargo", default="cargo")
    args = parser.parse_args()
    frontend, work, out = args.frontend.resolve(), args.work.resolve(), args.out.resolve()
    head = subprocess.check_output(["git", "-C", str(frontend), "rev-parse", "HEAD"], text=True).strip()
    if head != COMMIT:
        parser.error(f"frontend must be pinned to {COMMIT}, got {head}")
    if subprocess.check_output(["git", "-C", str(frontend), "status", "--porcelain"]):
        parser.error("frontend checkout has local changes")
    if work.exists():
        parser.error("--work must be a new directory")
    (work / "src").mkdir(parents=True)
    out.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(HERE / "probe.rs", work / "src/main.rs")
    # JSON quoted paths are valid TOML basic strings, including escaped separators.
    (work / "Cargo.toml").write_text(
        '[package]\nname = "quirrel-aside-syntax-probe"\nversion = "0.0.0"\nedition = "2024"\n'
        '[dependencies]\naside-compiler = { path = ' + json.dumps(str(frontend / "crates/aside-compiler")) + ' }\n')
    shutil.copyfile(HERE / "probe.lock", work / "Cargo.lock")
    with (out / "frontend-build.log").open("w") as log:
        subprocess.run([args.cargo, "build", "--manifest-path", str(work / "Cargo.toml"), "--release", "--locked"],
                       check=True, stdout=log, stderr=subprocess.STDOUT)
    shutil.copyfile(work / "Cargo.lock", out / "frontend-probe.lock")
    paths = sorted((HERE / "converted").rglob("*.aside"))
    binary = work / "target/release/quirrel-aside-syntax-probe"
    proc = subprocess.run([str(binary), *(str(p) for p in paths)], capture_output=True, text=True, timeout=30)
    (out / "frontend-probe.log").write_text(proc.stdout + proc.stderr)
    records = {}
    for line in proc.stdout.splitlines():
        fields = line.split("\t", 5)
        tag, path = fields[:2]
        key = Path(path).relative_to(HERE).as_posix()
        if tag == "RESULT":
            _, _, status, size, count, exhausted = fields
            mapping = json.loads(Path(path).with_suffix(".map.json").read_text())
            records[key] = {"status": status, "source_sha256": sha(Path(path)),
                            "original": mapping["source"], "bytes": int(size),
                            "diagnostic_count": int(count), "exhausted": exhausted == "true",
                            "diagnostics": [], "aside_execution": "waiting_runtime"}
        elif tag == "DIAG":
            _, _, code, start, end, message = fields
            mapping = json.loads(Path(path).with_suffix(".map.json").read_text())
            records[key]["diagnostics"].append({"code": code, "range": [int(start), int(end)],
                "original_ranges": map_range(mapping, "aside", int(start), int(end)), "message": message})
        else:
            raise RuntimeError(f"unknown probe output: {line}")
    if len(records) != len(paths):
        raise RuntimeError("probe did not report every derivative")
    if subprocess.check_output(["git", "-C", str(frontend), "status", "--porcelain"]):
        raise RuntimeError("probe changed the frontend checkout")
    result = {"frontend_repository": "https://github.com/jhq223/reel",
              "frontend_commit": COMMIT, "probe_sha256": sha(HERE / "probe.rs"),
              "cargo_lock_sha256": sha(work / "Cargo.lock"),
              "toolchain": subprocess.check_output([args.cargo, "--version"], text=True).strip(),
              "probe_exit_code": proc.returncode, "binding_checked": False,
              "semantics_executed": False, "fixtures": records}
    (out / "frontend.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"frontend_commit": COMMIT, "parsed": len(records),
                      "statuses": {s: sum(r["status"] == s for r in records.values())
                                   for s in sorted({r["status"] for r in records.values()})},
                      "aside_execution": "waiting_runtime"}, indent=2))
    return proc.returncode


if __name__ == "__main__":
    raise SystemExit(main())
