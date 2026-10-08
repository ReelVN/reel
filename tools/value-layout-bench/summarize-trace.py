#!/usr/bin/env python3
"""Trace medians and ranges, with exact cross-representation counters checked."""
import csv
import statistics
import sys
from collections import defaultdict

groups = defaultdict(list)
for path in sys.argv[1:]:
    with open(path, newline="") as source:
        for row in csv.DictReader(source):
            groups[row["case"], row["repr"]].append(row)
reps = ("explicit16", "nanbox8_box_rc", "nanbox8_slots_rc")
print("| case | execute ns/instruction E / Box / Slots | Box/E | Slots/E | construct µs E / Box / Slots | release µs E / Box / Slots | peak requested B E / Box / Slots |")
print("|---|---:|---:|---:|---:|---:|---:|")
for name in dict.fromkeys(key[0] for key in groups):
    pair = [groups[name, rep] for rep in reps]
    for key in ("instructions", "type_checks", "copies", "branches_taken", "host_calls", "checksum", "program_checksum"):
        assert len({r[key] for rows in pair for r in rows}) == 1, (name, key)
    for rows in pair:
        for key in ("value_bytes", "peak_requested_total_bytes", "temporary_wide_allocations",
                    "temporary_box_allocations", "wide_retains", "wide_releases"):
            assert len({r[key] for r in rows}) == 1, (name, key)
    times = [statistics.median(int(r["execute_ns"]) / int(r["instructions"]) for r in rows) for rows in pair]
    phases = [" / ".join(f"{statistics.median(int(r[phase+'_ns'])/1000 for r in rows):.3f}" for rows in pair)
              for phase in ("construct", "release")]
    peak = " / ".join(rows[0]["peak_requested_total_bytes"] for rows in pair)
    print(f"| {name} | {' / '.join(f'{t:.3f}' for t in times)} | {times[1]/times[0]:.3f} | {times[2]/times[0]:.3f} | {' | '.join(phases)} | {peak} |")

print("\nTime min–max relative to median (construct / execute / release; all samples retained):")
for (name, rep), rows in groups.items():
    ranges = []
    for phase in ("construct", "execute", "release"):
        times = [int(r[phase+"_ns"]) for r in rows]
        median = statistics.median(times)
        ranges.append(f"{min(times)/median:.3f}–{max(times)/median:.3f}")
    print(f"- {name} {rep}: {' / '.join(ranges)} ({len(rows)} samples)")
