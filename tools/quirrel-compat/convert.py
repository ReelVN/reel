#!/usr/bin/env python3
"""Apply only hash-pinned, manually reviewed, token-boundary fixture recipes."""
import argparse
import hashlib
import json
from pathlib import Path

from lexical import assertions, check_boundary, tokens
from run_upstream import VENDOR, verify_snapshot

HERE = Path(__file__).resolve().parent


def sha(data):
    return hashlib.sha256(data).hexdigest()


def convert(source, recipe):
    original = source.encode("utf-8")
    if sha(original) != recipe["source_sha256"]:
        raise ValueError(f"source changed: {recipe['source']}")
    stream = tokens(source)
    header = (f"// Source: third_party/quirrel/{recipe['source']}\n"
              "// Upstream: 19f17df9b6ee43c91ef5c635fd45970ec76c5dbe; license: third_party/quirrel/COPYRIGHT\n"
              "// Reviewed syntax conversion. Aside execution: waiting_runtime.\n").encode()
    output = bytearray(header)
    segments = [{"original": [0, 0], "aside": [0, len(header)], "kind": "generated_header"}]
    cursor = 0
    for edit in recipe["edits"]:
        start, end = edit["range"]
        if not (cursor <= start <= end <= len(original)):
            raise ValueError("overlapping or invalid edits")
        check_boundary(stream, start)
        check_boundary(stream, end)
        if original[start:end] != edit["before"].encode("utf-8"):
            raise ValueError("edit does not match pinned source")
        if start > cursor:
            segments.append({"original": [cursor, start], "aside": [len(output), len(output) + start - cursor],
                             "kind": "copied"})
            output.extend(original[cursor:start])
        replacement = edit["after"].encode("utf-8")
        segments.append({"original": [start, end], "aside": [len(output), len(output) + len(replacement)],
                         "kind": "reviewed_edit", "reason": edit["reason"]})
        output.extend(replacement)
        cursor = end
    if cursor < len(original):
        segments.append({"original": [cursor, len(original)],
                         "aside": [len(output), len(output) + len(original) - cursor], "kind": "copied"})
        output.extend(original[cursor:])
    result = output.decode("utf-8")
    old_asserts, new_asserts = assertions(source), assertions(result)
    if [a["tokens"] for a in old_asserts] != [a["tokens"] for a in new_asserts]:
        raise ValueError("original assertion tokens were changed, reordered or removed")
    mapping = {"source": recipe["source"], "source_sha256": sha(original), "aside_sha256": sha(output),
               "coordinate_unit": "UTF-8 bytes; zero-based half-open ranges",
               "mapping_rule": "copied spans are offset-exact; edited spans map conservatively to the entire reviewed span",
               "segments": segments, "assertions": [
                   {"original": old["range"], "aside": new["range"], "tokens": old["tokens"]}
                   for old, new in zip(old_asserts, new_asserts)]}
    return bytes(output), mapping


def map_range(mapping, side, start, end):
    """Use the same complete segment table in either direction; edits are many-to-many."""
    other = "aside" if side == "original" else "original"
    ranges = []
    for segment in mapping["segments"]:
        lo, hi = segment[side]
        if lo < end and start < hi or start == end and lo <= start <= hi:
            if segment["kind"] == "copied":
                base = segment[other][0] - lo
                ranges.append([max(start, lo) + base, min(end, hi) + base])
            else:
                ranges.append(segment[other])
    return ranges


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify committed derivatives without writing")
    args = parser.parse_args()
    manifest = verify_snapshot()
    recipes = json.loads((HERE / "recipes.json").read_text())
    for recipe in recipes:
        source = VENDOR / recipe["source"]
        if manifest["files_sha256"][recipe["source"]] != recipe["source_sha256"]:
            raise ValueError("recipe is not pinned to imported source")
        output, mapping = convert(source.read_text(), recipe)
        target = HERE / "converted" / Path(recipe["source"]).with_suffix(".aside")
        products = {target: output, target.with_suffix(".map.json"):
                    (json.dumps(mapping, indent=2, ensure_ascii=False) + "\n").encode()}
        for path, content in products.items():
            if args.check:
                if path.read_bytes() != content:
                    raise ValueError(f"stale conversion: {path}")
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
        print(f"{recipe['source']}: {len(mapping['assertions'])} unchanged assert calls; {len(recipe['edits'])} reviewed edits")


if __name__ == "__main__":
    main()
