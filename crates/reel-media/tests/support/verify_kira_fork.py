#!/usr/bin/env python3
"""Verify the imported crate and reversible two-file patch, without the registry."""
from pathlib import Path
import hashlib, json, shutil, subprocess, tempfile
repo = Path(__file__).resolve().parents[4]
fork = repo / 'third_party/kira'
original = json.loads((fork/'UPSTREAM-SHA256.json').read_text())
modified = {'src/backend/resources.rs', 'src/track/main/handle.rs'}
with tempfile.TemporaryDirectory(prefix='reel-kira-provenance-') as temporary:
    restored = Path(temporary)
    for name in modified:
        target = restored/name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(fork/name, target)
    subprocess.run(['git','apply','--reverse','--whitespace=nowarn', str(fork/'REEL-PATCH.patch')], cwd=restored, check=True)
    for name, expected in original.items():
        path = restored/name if name in modified else fork/name
        assert hashlib.sha256(path.read_bytes()).hexdigest() == expected, name
for name, source in json.loads((fork/'LICENSE-SOURCES.json').read_text()).items():
    assert hashlib.sha256((fork/name).read_bytes()).hexdigest() == source['sha256'], name
print(f'fork provenance: {len(original)} original crate file hashes verified; only two patched source files; license hashes verified')
