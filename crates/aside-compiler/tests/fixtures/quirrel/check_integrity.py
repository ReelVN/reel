#!/usr/bin/env python3
"""Verify immutable T01 bytes and assertion mappings; never rewrite golden files."""
import hashlib,json,pathlib
root=pathlib.Path(__file__).resolve().parent
provenance=json.loads((root/'provenance.json').read_text());assert provenance['accepted_T01']=='f92dfa2555dcadd89f53fb4b9c6de0931b47a531'
assertions=0
for row in provenance['files']:
 path=root/row['path'];digest=hashlib.sha256(path.read_bytes()).hexdigest()
 assert digest==row['sha256'],path
 mapping=json.loads(path.with_suffix('.map.json').read_text());assert digest==mapping['aside_sha256'],path
 assertions+=len(mapping['assertions'])
 assert row['execution']=='waiting_runtime'
assert len(provenance['files'])==10 and assertions==32
print('10 immutable T01 sources and maps; 32 unchanged assertion mappings; execution=waiting_runtime')
