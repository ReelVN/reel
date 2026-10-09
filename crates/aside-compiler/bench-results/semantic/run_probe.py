#!/usr/bin/env python3
"""Run the built semantic_probe; preserve raw per-process samples, not execution claims."""
import csv
import pathlib
import subprocess
import sys

binary = pathlib.Path(sys.argv[1]).resolve()
out = pathlib.Path(sys.argv[2])
mode = sys.argv[3] if len(sys.argv) > 3 else "captures"
rows = []
for n in (1000,5000,10000):
    for sample in range(7):
        result = subprocess.run(
            [str(binary), str(n), mode],
            text=True, capture_output=True, check=True, timeout=30,
        )
        row = next(csv.DictReader(result.stdout.splitlines()))
        row["sample"] = sample
        rows.append(row)
with out.open("w", newline="") as f:
    writer = csv.DictWriter(f, fieldnames=list(rows[0]), lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
