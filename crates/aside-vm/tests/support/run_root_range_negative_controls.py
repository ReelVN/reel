#!/usr/bin/env python3
"""Run destructive controls only in a temporary source copy, preserving this checkout."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

repo = Path(__file__).resolve().parents[4]
watched = [repo / 'crates/aside-vm/src' / name for name in ('frame_slots.rs', 'gc.rs', 'root_ranges.rs')]
digests = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in watched}

with tempfile.TemporaryDirectory(prefix='aside-root-negative-') as temporary:
    copy = Path(temporary) / 'repo'
    shutil.copytree(repo, copy, ignore=shutil.ignore_patterns('.git', 'target'))
    env = dict(os.environ, CARGO_TARGET_DIR=str(Path(temporary) / 'target'))
    frame = copy / 'crates/aside-vm/src/frame_slots.rs'
    collector = copy / 'crates/aside-vm/src/gc.rs'
    book = copy / 'crates/aside-vm/src/root_ranges.rs'
    frame_source, gc_source, book_source = (p.read_text() for p in (frame, collector, book))
    cases = [
        ('write', 'clear', ['value'], 'scanned_register_overwrite_alone_preserves_an_otherwise_white_object'),
        ('swap', 'move_to_host_root', ['a_v', 'b_v'], 'swap_alone_preserves_the_sole_unscanned_root_moved_behind_the_cursor'),
        ('move_value', 'swap', ['value'], 'move_alone_preserves_the_sole_unscanned_root_and_its_initialization_state'),
    ]
    def check(label, command, expected):
        result = subprocess.run(command, cwd=copy, env=env, capture_output=True, text=True)
        print(f'{label}: command={command!r}, exit={result.returncode}', flush=True)
        print(result.stdout + result.stderr, flush=True)
        assert result.returncode != 0 and expected in result.stdout + result.stderr, label
    for method, following, values, test in cases:
        start = frame_source.index('    pub fn ' + method + '(')
        end = frame_source.index('    pub fn ' + following + '(', start)
        changed = frame_source[start:end]
        for value in values:
            line = f'heap.root_insertion_barrier({value})?;'
            assert changed.count(line) == 1
            changed = changed.replace(line, '// Negative control: insertion barrier disabled.')
        frame.write_text(frame_source[:start] + changed + frame_source[end:])
        for release in (False, True):
            command = ['cargo', 'test', '-p', 'aside-vm', '--locked']
            if release: command.append('--release')
            command += ['--test', 'frame_slots', test, '--', '--exact']
            check(f'{method} barrier, {"release" if release else "debug"}', command, 'InvalidHandle')
        frame.write_text(frame_source)
    condition = 'self.gc.range_key != Some(key)'
    assert gc_source.count(condition) == 1
    collector.write_text(gc_source.replace(condition,
        'self.gc.range_key.map(crate::slots::index) != Some(crate::slots::index(key))'))
    for release in (False, True):
        command = ['cargo', 'test', '-p', 'aside-vm', '--locked']
        if release: command.append('--release')
        command += ['--test', 'frame_slots', 'publication_null_initialization_and_generation_reuse_have_real_distinct_work', '--', '--exact']
        check(f'generation cursor, {"release" if release else "debug"}', command, 'index out of bounds')
    collector.write_text(gc_source)
    fields = '    pub data: Vec<T>,\n    charge: Reservation,'
    assert book_source.count(fields) == 1
    book.write_text(book_source.replace(fields, '    charge: Reservation,\n    pub data: Vec<T>,'))
    check('premature byte refund, debug', ['bash', 'crates/aside-vm/tests/support/run_root_range_dealloc_probe.sh'],
          'budget refunded while allocator still owns bytes')
    book.write_text(book_source)

assert all(hashlib.sha256(p.read_bytes()).hexdigest() == digest for p, digest in digests.items())
print('Production checkout unchanged; all nine negative controls failed for the expected reason.')
