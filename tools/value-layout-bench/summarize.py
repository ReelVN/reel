#!/usr/bin/env python3
"""stdlib-only CSV aggregation; medians, ranges and ratios, never a VM score."""
import csv
import statistics
import sys
from collections import defaultdict

groups = defaultdict(list)
for path in sys.argv[1:]:
    with open(path, newline="") as source:
        for row in csv.DictReader(source):
            groups[(row["case"], row["repr"])].append(row)

print("| case | construct ns/value E / N | read ns/value E / N | release ns/value E / N | read N/E | requested bytes E / N |")
print("|---|---:|---:|---:|---:|---:|")
for name in dict.fromkeys(key[0] for key in groups):
    pair = [groups[(name, rep)] for rep in ("explicit16", "nanbox8")]
    for rows in pair:
        assert len({r["checksum"] for r in rows}) == 1
        for key in ("array_bytes", "heap_bytes", "total_bytes", "allocation_requests", "reallocation_requests"):
            assert len({r[key] for r in rows}) == 1
    assert pair[0][0]["checksum"] == pair[1][0]["checksum"]
    metrics = {}
    for phase in ("construct", "read", "release"):
        metrics[phase] = [statistics.median(
            int(r[phase + "_ns"]) / (int(r["n"]) * (int(r["passes"]) if phase == "read" else 1))
            for r in rows) for rows in pair]
    rendered = [" / ".join(f"{v:.3f}" for v in metrics[phase]) for phase in ("construct", "read", "release")]
    memory = " / ".join(pair[k][0]["total_bytes"] for k in (0, 1))
    print(f"| {name} | {' | '.join(rendered)} | {metrics['read'][1] / metrics['read'][0]:.3f} | {memory} |")

print("\nTime min–max relative to median (construct / read / release; all samples retained):")
for (name, rep), rows in groups.items():
    ranges = []
    for phase in ("construct", "read", "release"):
        times = [int(r[phase + "_ns"]) for r in rows]
        median = statistics.median(times)
        ranges.append(f"{min(times)/median:.3f}–{max(times)/median:.3f}")
    print(f"- {name} {rep}: {' / '.join(ranges)} ({len(rows)} samples)")
