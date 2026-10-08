#!/usr/bin/env python3
"""Verify active dependency closure and isolation, rather than infer them from feature names."""
import json
from pathlib import Path
import subprocess

base=Path(__file__).resolve().parents[1]
for manifest in [base/'Cargo.toml',base/'experiment/Cargo.toml']:
    metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--manifest-path',str(manifest),'--format-version','1']))
    assert Path(metadata['workspace_root'])==manifest.parent
    packages={p['id']:p for p in metadata['packages']}
    nodes={n['id']:n for n in metadata['resolve']['nodes']}
    reached=set()
    def visit(id):
        if id in reached:return
        reached.add(id)
        for dep in nodes[id]['deps']:
            # Build and normal dependencies matter for the runnable native probe.
            if any(k['kind']!='dev' for k in dep['dep_kinds']):visit(dep['pkg'])
    visit(metadata['resolve']['root'])
    for name in ['masonry','masonry_core','tree_arena','imaging','peniko','kurbo']:
        found=[packages[id] for id in reached if packages[id]['name']==name]
        assert len(found)==1,(name,found)
        print(f"{manifest.parent.name}: {name} {found[0]['version']} source={found[0]['source'] or found[0]['manifest_path']}")
    # Cargo tree applies target cfg filtering, unlike a raw metadata traversal.
    tree=subprocess.check_output(['cargo','tree','--locked','--manifest-path',str(manifest),'-e','normal,build','--prefix','none'],text=True)
    assert not any(line.split()[0] in {'winit','wgpu','masonry_winit'} for line in tree.splitlines())
    print(f'{manifest.parent.name}: isolated workspace; no active winit/wgpu/masonry_winit; Fontconfig remains a Linux transitive dependency')
