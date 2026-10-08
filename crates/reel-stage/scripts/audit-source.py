#!/usr/bin/env python3
"""Verify the pinned source manifest and reproduce the production Masonry diff."""
import argparse
import difflib
import hashlib
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("upstream", type=Path, help="extracted fixed upstream source tree")
parser.add_argument("--write-patch", action="store_true")
args = parser.parse_args()
vendor = Path(__file__).resolve().parents[3] / "third_party/masonry"
manifest = vendor / "UPSTREAM-SOURCE.sha256"
originals = {}
for line in manifest.read_text().splitlines():
    digest, name = line.split("  ", 1)
    source = (args.upstream / name).read_bytes()
    assert hashlib.sha256(source).hexdigest() == digest, f"upstream hash: {name}"
    assert (vendor / name).is_file(), f"missing retained source: {name}"
    originals[name] = source

local_source = {"masonry_core/src/app/presented_geometry.rs"}
retained_source = {
    str(p.relative_to(vendor))
    for p in vendor.rglob("*")
    if p.is_file() and (p.suffix == ".rs" or p.name == "Cargo.toml")
}
assert retained_source == {
    name for name in originals if name.endswith(".rs") or name.endswith("Cargo.toml")
} | local_source, "source closure differs from its provenance manifest"
assert not list(vendor.rglob(".git")), "nested Git checkout"

diff = []
for name in sorted(originals.keys() | local_source):
    before = originals.get(name, b"")
    after = (vendor / name).read_bytes()
    if before == after:
        continue
    diff.extend(difflib.unified_diff(
        before.decode().splitlines(keepends=True),
        after.decode().splitlines(keepends=True),
        fromfile=f"a/{name}" if name in originals else "/dev/null",
        tofile=f"b/{name}",
    ))
patch = "".join(diff)
target = vendor / "reel-production.patch"
if args.write_patch:
    target.write_text(patch)
else:
    assert target.read_text() == patch, "production diff is stale"
print(f"verified {len(originals)} fixed-source files, {len(local_source)} local source file; diff {len(patch.encode())} bytes")
