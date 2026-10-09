#!/usr/bin/env python3
"""Run the unmodified upstream runner, recording its real commands and outcomes.

Observation hooks only: upstream selection, comparison, timeout (10s), seeds,
12-worker concurrency, assertions and exit code are retained. The runner may
create missing goldens, so it runs in a fresh disposable copy, never the vendor.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import threading
import time
import zipfile

HERE = Path(__file__).resolve().parent
VENDOR = HERE.parents[1] / "third_party/quirrel"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_snapshot():
    manifest = json.loads((VENDOR / "UPSTREAM.json").read_text())
    for name, expected in manifest["files_sha256"].items():
        if digest(VENDOR / name) != expected:
            raise RuntimeError(f"snapshot modified: {name}")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sq", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True,
                        help="must not exist; disposable upstream copy")
    args = parser.parse_args()
    sq, out, work = args.sq.resolve(), args.out.resolve(), args.work.resolve()
    manifest = verify_snapshot()
    if work.exists():
        parser.error("--work already exists; choose a fresh directory")
    if (out / "upstream.json").exists():
        parser.error("--out already contains an upstream run; choose a fresh directory")
    shutil.copytree(VENDOR, work)
    out.mkdir(parents=True, exist_ok=True)
    spec = importlib.util.spec_from_file_location("quirrel_runner", work / "testRunner.py")
    runner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(runner)
    local = threading.local()
    records = []
    lock = threading.Lock()
    real_popen = runner.Popen
    real_compare = runner.compareFilesLineByLine
    real_run = runner.runTestCommand

    class ObservedProcess(real_popen):
        def __init__(self, command, **kwargs):
            super().__init__(command, **kwargs)
            row = getattr(local, "row", None)
            if row is not None:
                row["command"] = command

        def communicate(self, *a, **kw):
            row = getattr(local, "row", None)
            try:
                stdout, stderr = super().communicate(*a, **kw)
            except subprocess.TimeoutExpired:
                if row is not None:
                    row["timed_out"] = True
                raise
            if row is not None:
                row["returncode"] = self.returncode
                for key, value in (("process_stdout", stdout), ("process_stderr", stderr)):
                    if value is not None:
                        target = out / row["artifact_dir"] / (key + ".txt")
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_bytes(value)
                        row[key] = target.relative_to(out).as_posix()
            return stdout, stderr

    def compare(*a):
        matched = real_compare(*a)
        local.row["comparison_matched"] = matched
        return matched

    def observe(compiler, source, actual, expected, kind, seeds, extra, stdout_file):
        seed = "default" if not seeds else seeds[0].split(":", 1)[1]
        row = {"fixture": source, "kind": kind, "seed": seed,
               "artifact_dir": f"raw/{source}/{seed}",
               "golden": expected, "golden_present": Path(expected).exists(),
               "timed_out": False}
        local.row = row
        start = time.monotonic()
        try:
            real_run(compiler, source, actual, expected, kind, seeds, extra, stdout_file)
        except Exception as exc:
            row["observer_error"] = repr(exc)
            raise
        finally:
            row["elapsed_seconds"] = round(time.monotonic() - start, 6)
            if Path(actual).exists():
                target = out / row["artifact_dir"] / "actual.txt"
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(actual, target)
                row["actual"] = target.relative_to(out).as_posix()
                row["actual_sha256"] = digest(target)
            row["status"] = (
                "observer_error" if "observer_error" in row else
                "timeout" if row["timed_out"] else
                "crash" if row.get("returncode") != 0 else
                "unasserted" if not row["golden_present"] else
                "pass" if row.get("comparison_matched") else "mismatch")
            with lock:
                records.append(row)
            del local.row

    runner.Popen = ObservedProcess
    runner.compareFilesLineByLine = compare
    runner.runTestCommand = observe
    previous_dir, previous_argv = Path.cwd(), sys.argv
    start = time.monotonic()
    code = 2
    try:
        os.chdir(work)
        sys.argv = ["testRunner.py", "-sq", str(sq), "-wd", str(work / "actual"), "-ci", "-v"]
        with (out / "upstream.log").open("w") as log:
            previous_stdout, previous_stderr = sys.stdout, sys.stderr
            try:
                sys.stdout = sys.stderr = log
                try:
                    runner.main()
                except SystemExit as exc:
                    code = int(exc.code or 0)
            finally:
                sys.stdout, sys.stderr = previous_stdout, previous_stderr
    finally:
        os.chdir(previous_dir)
        sys.argv = previous_argv
        verify_snapshot()
        changed = [name for name, value in manifest["files_sha256"].items()
                   if not (work / name).exists() or digest(work / name) != value]
        created = sorted(p.relative_to(work).as_posix() for root in ("testData", "doc/examples")
                         for p in (work / root).rglob("*")
                         if p.is_file() and p.relative_to(work).as_posix() not in manifest["files_sha256"])
        summary = {"upstream_commit": manifest["commit"], "sq_sha256": digest(sq),
                   "sq_version": subprocess.run([str(sq), "-v"], capture_output=True,
                                                text=True, check=True).stdout.strip(),
                   "platform": platform.platform(), "python": sys.version,
                   "runner_exit_code": code, "threads": runner.THREADS,
                   "timeout_seconds": 10, "elapsed_seconds": round(time.monotonic() - start, 6),
                   "changed_snapshot_files_in_work": changed, "created_goldens_in_work": created,
                   "runs": sorted(records, key=lambda r: (r["fixture"], r["seed"]))}
        # Keep every seed's bytes in a small archive, and failures directly readable.
        summary["artifact_archive"] = "upstream-artifacts.zip"
        with zipfile.ZipFile(out / summary["artifact_archive"], "w", zipfile.ZIP_DEFLATED) as archive:
            for path in sorted((out / "raw").rglob("*")):
                if path.is_file():
                    info = zipfile.ZipInfo(path.relative_to(out).as_posix(), (1980, 1, 1, 0, 0, 0))
                    info.compress_type = zipfile.ZIP_DEFLATED
                    archive.writestr(info, path.read_bytes())
        for row in records:
            if row["status"] != "pass" and "actual" in row:
                path = out / "failures" / row["fixture"] / (row["seed"] + ".actual.txt")
                path.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(out / row["actual"], path)
                row["readable_actual"] = path.relative_to(out).as_posix()
        shutil.rmtree(out / "raw")
        (out / "upstream.json").write_text(json.dumps(summary, indent=2) + "\n")
    counts = {}
    for row in records:
        counts[row["status"]] = counts.get(row["status"], 0) + 1
    print(json.dumps({"runner_exit_code": code, "runs": len(records), "statuses": counts,
                      "changed_snapshot_files": changed, "created_goldens": created}, indent=2))
    return code or (1 if changed or created else 0)


if __name__ == "__main__":
    sys.exit(main())
