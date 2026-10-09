"""Check the evidence and fail-closed conversion boundaries, not a mock VM."""
import io
import json
from pathlib import Path
import unittest
import zipfile

from convert import HERE, convert, map_range, sha
from inventory import build
from run_upstream import VENDOR, verify_snapshot


class Integrity(unittest.TestCase):
    def test_snapshot_and_fixture_universe(self):
        manifest = verify_snapshot()
        self.assertEqual(manifest["local_modifications"], [])
        self.assertFalse(any(p.name == ".git" for p in VENDOR.rglob("*")))
        result = build(HERE / "results")
        self.assertEqual(len(result["fixtures"]), 1296)
        self.assertTrue(all(row["aside_execution"] == "waiting_runtime" for row in result["fixtures"]))
        self.assertEqual(set(row["category"] for row in result["fixtures"]),
                         {"preserved", "syntax_converted", "harness_adapted", "deliberate_delta", "compiler_specific", "blocked"})

    def test_raw_bytes_and_original_comparisons(self):
        result = json.loads((HERE / "results/upstream.json").read_text())
        with zipfile.ZipFile(HERE / "results" / result["artifact_archive"]) as archive:
            failures = []
            for run in result["runs"]:
                actual = archive.read(run["actual"])
                self.assertEqual(sha(actual), run["actual_sha256"])
                expected = (VENDOR / run["golden"]).read_text()
                actual_lines = io.StringIO(actual.decode("utf-8")).readlines()
                expected_lines = io.StringIO(expected).readlines()
                matched = (len(actual_lines) == len(expected_lines) and
                           all(a.rstrip() == e.rstrip() for a, e in zip(actual_lines, expected_lines)))
                self.assertEqual(run["status"], "pass" if matched else "mismatch")
                self.assertEqual(run["returncode"], 0)
                if not matched:
                    failures.append(run["fixture"])
                    self.assertEqual((HERE / "results" / run["readable_actual"]).read_bytes(), actual)
        self.assertEqual(len(failures), 8)
        self.assertEqual(result["runner_exit_code"], 1)
        self.assertEqual(result["created_goldens_in_work"], [])
        self.assertEqual(result["changed_snapshot_files_in_work"], [])

    def test_every_conversion_and_complete_bidirectional_map(self):
        for recipe in json.loads((HERE / "recipes.json").read_text()):
            source = (VENDOR / recipe["source"]).read_text()
            output, mapping = convert(source, recipe)
            aside = HERE / "converted" / Path(recipe["source"]).with_suffix(".aside")
            self.assertEqual(output, aside.read_bytes())
            original_cursor = aside_cursor = 0
            for segment in mapping["segments"]:
                self.assertEqual(segment["original"][0], original_cursor)
                self.assertEqual(segment["aside"][0], aside_cursor)
                original_cursor = segment["original"][1]
                aside_cursor = segment["aside"][1]
                if segment["kind"] == "copied":
                    start, end = segment["original"]
                    lo, hi = segment["aside"]
                    self.assertEqual(source.encode()[start:end], output[lo:hi])
                    self.assertEqual(map_range(mapping, "original", start, end), [[lo, hi]])
                    self.assertEqual(map_range(mapping, "aside", lo, hi), [[start, end]])
                elif segment["kind"] == "reviewed_edit":
                    self.assertIn(segment["aside"], map_range(mapping, "original", *segment["original"]))
                    self.assertIn(segment["original"], map_range(mapping, "aside", *segment["aside"]))
            self.assertEqual(original_cursor, len(source.encode()))
            self.assertEqual(aside_cursor, len(output))

    def test_source_pin_rejects_changes(self):
        recipe = json.loads((HERE / "recipes.json").read_text())[0]
        with self.assertRaisesRegex(ValueError, "source changed"):
            convert((VENDOR / recipe["source"]).read_text() + " ", recipe)

    def test_assertion_weakening_is_rejected(self):
        source = "assert(1 == 2);\n"
        for after in ("assert(true);", "", "assert(1 == 1);"):
            recipe = {"source": "negative.nut", "source_sha256": sha(source.encode()),
                      "edits": [{"range": [0, 15], "before": source[:15], "after": after, "reason": "negative check"}]}
            with self.assertRaisesRegex(ValueError, "assertion tokens"):
                convert(source, recipe)

    def test_literal_boundary_and_unicode_bytes(self):
        source = 'let 名称 = "hello";\n'
        data = source.encode()
        start = data.index(b"ell")
        recipe = {"source": "unicode.nut", "source_sha256": sha(data),
                  "edits": [{"range": [start, start + 3], "before": "ell", "after": "a", "reason": "negative check"}]}
        with self.assertRaisesRegex(ValueError, "cuts literal token"):
            convert(source, recipe)
        recipe["edits"] = [{"range": [0, 3], "before": "let", "after": "let mut", "reason": "reviewed binding"}]
        output, mapping = convert(source, recipe)
        name_start, name_end = data.index("名称".encode()), data.index("名称".encode()) + len("名称".encode())
        ranges = map_range(mapping, "original", name_start, name_end)
        self.assertEqual(len(ranges), 1)
        self.assertEqual(output[slice(*ranges[0])], "名称".encode())
        self.assertEqual(map_range(mapping, "aside", *ranges[0]), [[name_start, name_end]])

    def test_comment_tokens_are_not_assertions_or_edit_interiors(self):
        source = '// assert(false);\nassert(true);\n'
        recipe = {"source": "comment.nut", "source_sha256": sha(source.encode()), "edits": []}
        _, mapping = convert(source, recipe)
        self.assertEqual(len(mapping["assertions"]), 1)
        recipe["edits"] = [{"range": [3, 9], "before": "assert", "after": "a", "reason": "negative check"}]
        with self.assertRaisesRegex(ValueError, "cuts comment token"):
            convert(source, recipe)


if __name__ == "__main__":
    unittest.main()
