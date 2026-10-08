#!/usr/bin/env python3
"""Record compiler-only probes; does not dispatch bytecode or invoke a host."""
import argparse
import csv
import io
import json
import pathlib
import platform
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", default="target/release/examples/lowering_probe")
    parser.add_argument("--output", default="crates/aside-compiler/bench-results/lowering")
    parser.add_argument("--label", default="2026-10-08")
    parser.add_argument("--repeats", type=int, default=7)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats must be positive")
    out = pathlib.Path(args.output)
    out.mkdir(parents=True, exist_ok=True)
    rows, failures = [], []
    workloads = [
        ("plain", [100, 1000, 5000, 10000]),
        ("captures", [100, 1000, 2000, 5000]),
        ("named", [100, 1000, 5000, 10000]),
        ("temporaries", [1000, 10000, 25000, 50000]),
    ]
    for mode, sizes in workloads:
        for n in sizes:
            for repeat in range(args.repeats):
                result = subprocess.run(
                    [args.probe, str(n), mode], capture_output=True, text=True, check=False
                )
                if result.returncode or result.stdout.startswith("rejected,"):
                    failures.append(dict(mode=mode, n=n, repeat=repeat,
                                         exit=result.returncode, output=result.stdout,
                                         error=result.stderr))
                    break
                row = next(csv.DictReader(io.StringIO(result.stdout)))
                row["repeat"] = repeat
                rows.append(row)
            print(mode, n, "recorded", flush=True)
    if rows:
        with (out / f"{args.label}.csv").open("w", newline="") as stream:
            writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)
    summary = []
    keys = sorted({(r["mode"], r["n"]) for r in rows}, key=lambda k: (k[0], int(k[1])))
    for mode, n in keys:
        group = [r for r in rows if (r["mode"], r["n"]) == (mode, n)]
        median = {k: statistics.median(int(r[k]) for r in group)
                  for k in group[0] if k not in ("mode", "n", "repeat")}
        median.update(mode=mode, n=int(n), repeats=len(group))
        summary.append(median)
    environment = dict(
        revision=subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        rustc=subprocess.check_output(["rustc", "--version"], text=True).strip(),
        platform=platform.platform(), machine=platform.machine(), repetitions=args.repeats,
        scope="compiler and bytecode verification/codec only; no dispatch or host invocation",
        peak="conservative compiler capacity upper bound, not measured process RSS",
    )
    for suffix, value in [("medians", summary), ("rejections", failures),
                          ("environment", environment)]:
        (out / f"{args.label}-{suffix}.json").write_text(json.dumps(value, indent=2) + "\n")
    print(f"{len(rows)} accepted measurements, {len(failures)} rejection cases")


if __name__ == "__main__":
    main()
