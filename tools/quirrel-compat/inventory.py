#!/usr/bin/env python3
"""Join the complete upstream fixture universe with three independent result columns."""
import argparse
import collections
import csv
import json
from pathlib import Path
import types

from convert import sha
from run_upstream import VENDOR, verify_snapshot

HERE = Path(__file__).resolve().parent
CATEGORIES = {"preserved", "syntax_converted", "harness_adapted", "deliberate_delta", "compiler_specific", "blocked"}
ROOTS = [("testData/exec", "Exec"), ("testData/diagnostics", "Diagnostics"),
         ("testData/ast", "AST"), ("testData/static_analyzer", "Static Analyzer"),
         ("testData/types", "Parse Types"), ("doc/examples", "Exec")]


def build(results):
    manifest = verify_snapshot()
    upstream = json.loads((results / "upstream.json").read_text())
    # Reuse the exact pinned runner's seed predicate, including its conservative
    # text heuristic. This is test selection, never a source conversion rule.
    runner = types.ModuleType("pinned_quirrel_runner")
    runner_path = VENDOR / "testRunner.py"
    exec(compile(runner_path.read_text(), str(runner_path), "exec"), runner.__dict__)
    frontend = json.loads((results / "frontend.json").read_text())
    if upstream["upstream_commit"] != manifest["commit"]:
        raise ValueError("upstream results use another snapshot")
    runs = collections.defaultdict(list)
    for run in upstream["runs"]:
        runs[run["fixture"]].append(run)
    parsed = {record["original"]: (name, record) for name, record in frontend["fixtures"].items()}
    recipes = {recipe["source"]: recipe for recipe in json.loads((HERE / "recipes.json").read_text())}
    deltas = json.loads((HERE / "deliberate-deltas.json").read_text())
    delta_index = collections.defaultdict(list)
    for delta in deltas:
        for name in delta["upstream_fixtures"]:
            if not (VENDOR / name).is_file():
                raise ValueError(f"delta refers to nonexistent fixture: {name}")
            delta_index[name].append(delta["id"])
    rows = []
    for root, kind in ROOTS:
        for path in sorted((VENDOR / root).rglob("*")):
            if not path.is_file() or not (path.name.endswith(".nut") or path.name.endswith(".nut.txt")):
                continue
            name = path.relative_to(VENDOR).as_posix()
            actual = runs.get(name, [])
            if not actual or any(r["kind"] != kind for r in actual):
                raise ValueError(f"missing or wrong-mode original result: {name}")
            expected_seeds = {"default", "0", "255"} if runner.needsRandomizedIterationSeeds(str(path)) else {"default"}
            if len(actual) != len(expected_seeds) or {r["seed"] for r in actual} != expected_seeds:
                raise ValueError(f"missing or duplicate upstream seed run: {name}")
            golden = VENDOR / actual[0]["golden"]
            status = "pass" if all(r["status"] == "pass" for r in actual) else "failed"
            if name in recipes:
                recipe = recipes[name]
                category, reason = recipe["category"], recipe["review_note"] or "Reviewed exact source recipe; original output oracle retained."
                review = "first_batch_reviewed"
            elif name in delta_index:
                category, reason = "deliberate_delta", ", ".join(delta_index[name])
                review = "difference_identified_conversion_pending"
            elif kind != "Exec":
                category, reason = "compiler_specific", f"Original {kind} oracle depends on Quirrel compiler/analyzer/types/AST output; requires a separate Aside diagnostic contract."
                review = "compiler_suite_conversion_pending"
            else:
                category, reason = "blocked", "Not converted: binding/semantic review and an explicit source recipe are pending. Original success is not Aside coverage."
                review = "pending_binding_and_semantic_review"
            if category not in CATEGORIES:
                raise ValueError("invalid category")
            parsed_name, parse = parsed.get(name, (None, None))
            mapping = None
            if name in recipes:
                if parse is None:
                    raise ValueError(f"converted fixture was not parsed: {name}")
                aside = HERE / parsed_name
                if sha(aside.read_bytes()) != parse["source_sha256"]:
                    raise ValueError("stale frontend result")
                mapping = json.loads(aside.with_suffix(".map.json").read_text())
            rows.append({"fixture": name, "upstream_commit": manifest["commit"],
                "source_sha256": manifest["files_sha256"][name], "upstream_kind": kind,
                "category": category, "classification_reason": reason, "review_status": review,
                "deliberate_deltas": delta_index[name], "original_run": status,
                "original_seeds": [{"seed": r["seed"], "status": r["status"], "returncode": r["returncode"],
                                    "actual": r.get("actual"), "actual_sha256": r.get("actual_sha256")} for r in actual],
                "golden": golden.relative_to(VENDOR).as_posix(), "golden_sha256": sha(golden.read_bytes()),
                "conversion": "converted" if mapping else "not_converted",
                "converted_path": parsed_name, "source_map": str(Path(parsed_name).with_suffix(".map.json")) if mapping else None,
                "original_assert_calls_retained": len(mapping["assertions"]) if mapping else None,
                "aside_parse": parse["status"] if parse else "not_converted",
                "aside_execution": "waiting_runtime"})
    if {r["fixture"] for r in rows} != set(runs):
        raise ValueError("fixture universe differs from actual runner selection")
    if set(recipes) != set(parsed):
        raise ValueError("recipes and parsed derivative sets differ")
    summary = {"fixtures": len(rows), "original_runs": len(upstream["runs"]),
               "original_statuses": dict(collections.Counter(r["status"] for r in upstream["runs"])),
               "categories": dict(collections.Counter(r["category"] for r in rows)),
               "aside_parse": dict(collections.Counter(r["aside_parse"] for r in rows)),
               "aside_execution": {"waiting_runtime": len(rows)},
               "assert_calls_preserved_in_first_batch": sum(r["original_assert_calls_retained"] or 0 for r in rows)}
    return {"schema_version": 1, "language_spec": "Aside 1.1", "upstream_commit": manifest["commit"],
            "frontend_commit": frontend["frontend_commit"], "original_artifacts": upstream["artifact_archive"],
            "summary": summary, "fixtures": rows}


def products(result):
    import io
    stream = io.StringIO(newline="")
    fields = ["fixture", "upstream_kind", "category", "original_run", "aside_parse", "aside_execution",
              "conversion", "converted_path", "original_assert_calls_retained", "classification_reason", "source_sha256", "golden_sha256"]
    writer = csv.DictWriter(stream, fieldnames=fields, extrasaction="ignore", lineterminator="\n")
    writer.writeheader()
    writer.writerows(result["fixtures"])
    return {"inventory.json": json.dumps(result, indent=2, ensure_ascii=False) + "\n",
            "inventory.csv": stream.getvalue()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results", type=Path, default=HERE / "results")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = build(args.results)
    for name, content in products(result).items():
        path = args.results / name
        if args.check:
            if path.read_text() != content:
                raise ValueError(f"stale inventory: {path}")
        else:
            path.write_text(content)
    print(json.dumps(result["summary"], indent=2))


if __name__ == "__main__":
    main()
