#!/usr/bin/env python3
"""Materialize only the two-crate experimental source closure; never touch production."""
import argparse
import hashlib
import io
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import urllib.request

REV = '271a27a6d4a930f7878d404f9014e3c50a3a9b88'
BASE = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, help='existing Xilem git checkout (archive reads the fixed commit, not working changes)')
args = parser.parse_args()
if args.source:
    archive = subprocess.check_output(['git', '-C', str(args.source), 'archive', REV])
else:
    with urllib.request.urlopen(f'https://codeload.github.com/linebender/xilem/tar.gz/{REV}') as response:
        archive = response.read()
expected = dict(line.split('  ', 1)[::-1] for line in (BASE/'upstream-source.sha256').read_text().splitlines())
files = {}
with tarfile.open(fileobj=io.BytesIO(archive), mode='r:*') as tar:
    for entry in tar:
        if not entry.isfile():
            continue
        path = entry.name
        if not args.source:
            path = path.split('/', 1)[-1]
        if path in expected:
            files[path] = tar.extractfile(entry).read()
assert files.keys() == expected.keys(), 'fixed source archive is incomplete'
for path, data in files.items():
    assert hashlib.sha256(data).hexdigest() == expected[path], f'source mismatch: {path}'
experiment = BASE/'experiment'
if experiment.exists():
    shutil.rmtree(experiment)
upstream = experiment/'upstream'
for path, data in files.items():
    dest=upstream/path
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(data)
manifest = (upstream/'Cargo.toml').read_text()
manifest = re.sub(r'members = \[.*?\]', 'members = ["masonry_core", "tree_arena"]', manifest, count=1, flags=re.S)
(upstream/'Cargo.toml').write_text(manifest)
subprocess.run(['patch','-p1','--dry-run','--batch','--input',str(BASE/'patches/presented-geometry.patch')],cwd=upstream,check=True)
subprocess.run(['patch','-p1','--batch','--input',str(BASE/'patches/presented-geometry.patch')],cwd=upstream,check=True)
for crate in ['masonry_core', 'tree_arena']:
    path=upstream/crate/'Cargo.toml'
    path.write_text(path.read_text().replace('[package]', '[package]\nworkspace = \"..\"', 1))
manifest = (BASE/'Cargo.toml').read_text() + '''\n[[bin]]
name = "masonry-contract-check"
path = "../src/main.rs"

[patch."https://github.com/linebender/xilem"]
masonry_core = { path = "upstream/masonry_core" }
tree_arena = { path = "upstream/tree_arena" }
'''
(experiment/'Cargo.toml').write_text(manifest)
lock = BASE/'locks/experimental.lock'
shutil.copyfile(lock if lock.exists() else BASE/'Cargo.lock', experiment/'Cargo.lock')
print(f'fixed source verified: {len(files)} files, {sum(map(len,files.values()))} bytes; patch applied to masonry_core only')
