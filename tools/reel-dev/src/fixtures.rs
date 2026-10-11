use crate::{files, json, root, sha};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

const PIN: &str = "19f17df9b6ee43c91ef5c635fd45970ec76c5dbe";
const CATEGORIES: [&str; 6] = [
    "preserved",
    "syntax_converted",
    "harness_adapted",
    "deliberate_delta",
    "compiler_specific",
    "blocked",
];

#[derive(Debug)]
struct Token<'a> {
    start: usize,
    end: usize,
    text: &'a str,
    kind: &'static str,
}

// Deliberately a boundary scanner for reviewed recipes, not a Quirrel parser.
fn tokens(source: &str) -> Result<Vec<Token<'_>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < source.len() {
        let start = i;
        let c = source[i..].chars().next().unwrap();
        let mut kind = "code";
        if c.is_whitespace() {
            kind = "trivia";
            while i < source.len() && source[i..].chars().next().unwrap().is_whitespace() {
                i += source[i..].chars().next().unwrap().len_utf8();
            }
        } else if source[i..].starts_with("//") {
            kind = "comment";
            i += source[i..].find('\n').unwrap_or(source.len() - i);
        } else if source[i..].starts_with("/*") {
            kind = "comment";
            i += 2;
            let mut depth = 1;
            while i < source.len() && depth > 0 {
                if source[i..].starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else if source[i..].starts_with("*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += source[i..].chars().next().unwrap().len_utf8();
                }
            }
            ensure!(depth == 0, "unclosed comment in reviewed fixture");
        } else if c == '\'' || c == '"' {
            kind = "literal";
            i += 1;
            let mut closed = false;
            while i < source.len() {
                let next = source[i..].chars().next().unwrap();
                i += next.len_utf8();
                if next == '\\' {
                    ensure!(i < source.len(), "unclosed literal in reviewed fixture");
                    i += source[i..].chars().next().unwrap().len_utf8();
                } else if next == c {
                    closed = true;
                    break;
                }
            }
            ensure!(closed, "unclosed literal in reviewed fixture");
        } else if c.is_alphanumeric() || c == '_' {
            i += c.len_utf8();
            while i < source.len() {
                let next = source[i..].chars().next().unwrap();
                if !(next.is_alphanumeric() || next == '_') {
                    break;
                }
                i += next.len_utf8();
            }
        } else {
            let operators = [
                "...", ">>>", "<=>", "..=", "==", "!=", "<=", ">=", "&&", "||", "??", "+=", "-=",
                "*=", "/=", "++", "--", "<-", "..",
            ];
            i += operators
                .iter()
                .find(|op| source[i..].starts_with(**op))
                .map_or(c.len_utf8(), |op| op.len());
        }
        out.push(Token {
            start,
            end: i,
            text: &source[start..i],
            kind,
        });
    }
    Ok(out)
}

fn assertions(source: &str) -> Result<Vec<Value>> {
    let stream: Vec<_> = tokens(source)?
        .into_iter()
        .filter(|t| t.kind != "trivia" && t.kind != "comment")
        .collect();
    let mut out = Vec::new();
    for (i, token) in stream.iter().enumerate() {
        if token.text != "assert" || stream.get(i + 1).is_none_or(|t| t.text != "(") {
            continue;
        }
        let mut depth = 0;
        let mut found = false;
        for j in i + 1..stream.len() {
            if stream[j].text == "(" {
                depth += 1;
            }
            if stream[j].text == ")" {
                depth -= 1;
                if depth == 0 {
                    out.push(json!({"range": [token.start, stream[j].end], "tokens": stream[i..=j].iter().map(|t| t.text).collect::<Vec<_>>() }));
                    found = true;
                    break;
                }
            }
        }
        ensure!(found, "unclosed assert call");
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct Recipe {
    pub source: String,
    pub category: String,
    edits: Vec<Edit>,
}
#[derive(Debug, Deserialize)]
struct Edit {
    range: [usize; 2],
    before: String,
    after: String,
    reason: String,
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    original: [usize; 2],
    aside: [usize; 2],
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Mapping {
    source: String,
    source_sha256: String,
    aside_sha256: String,
    coordinate_unit: String,
    mapping_rule: String,
    segments: Vec<Segment>,
    assertions: Vec<Value>,
}

pub fn convert(source: &str, recipe: &Recipe) -> Result<(String, Mapping)> {
    let stream = tokens(source)?;
    let mut output = format!(
        "// Source: third_party/quirrel/{}\n// Upstream: {PIN}; license: third_party/quirrel/COPYRIGHT\n// Reviewed syntax conversion. Aside execution: waiting_runtime.\n",
        recipe.source
    );
    let mut segments = vec![Segment {
        original: [0, 0],
        aside: [0, output.len()],
        kind: "generated_header".into(),
        reason: None,
    }];
    let mut cursor = 0;
    for edit in &recipe.edits {
        let [start, end] = edit.range;
        ensure!(
            cursor <= start && start <= end && end <= source.len(),
            "overlapping or invalid edits"
        );
        for offset in [start, end] {
            for token in &stream {
                ensure!(
                    token.kind == "trivia" || !(token.start < offset && offset < token.end),
                    "edit cuts {} token",
                    token.kind
                );
            }
        }
        ensure!(
            source.get(start..end) == Some(edit.before.as_str()),
            "edit does not match pinned source"
        );
        if start > cursor {
            segments.push(Segment {
                original: [cursor, start],
                aside: [output.len(), output.len() + start - cursor],
                kind: "copied".into(),
                reason: None,
            });
            output.push_str(&source[cursor..start]);
        }
        segments.push(Segment {
            original: [start, end],
            aside: [output.len(), output.len() + edit.after.len()],
            kind: "reviewed_edit".into(),
            reason: Some(edit.reason.clone()),
        });
        output.push_str(&edit.after);
        cursor = end;
    }
    if cursor < source.len() {
        segments.push(Segment {
            original: [cursor, source.len()],
            aside: [output.len(), output.len() + source.len() - cursor],
            kind: "copied".into(),
            reason: None,
        });
        output.push_str(&source[cursor..]);
    }
    let old = assertions(source)?;
    let new = assertions(&output)?;
    ensure!(
        old.iter()
            .map(|a| &a["tokens"])
            .eq(new.iter().map(|a| &a["tokens"])),
        "original assertion tokens were changed, reordered or removed"
    );
    let mapping = Mapping {
        source: recipe.source.clone(), source_sha256: sha(source.as_bytes()), aside_sha256: sha(output.as_bytes()),
        coordinate_unit: "UTF-8 bytes; zero-based half-open ranges".into(),
        mapping_rule: "copied spans are offset-exact; edited spans map conservatively to the entire reviewed span".into(),
        segments, assertions: old.iter().zip(new.iter()).map(|(a,b)| json!({"original": a["range"], "aside": b["range"], "tokens": a["tokens"]})).collect(),
    };
    Ok((output, mapping))
}

fn map_range(mapping: &Mapping, original: bool, start: usize, end: usize) -> Vec<[usize; 2]> {
    mapping
        .segments
        .iter()
        .filter_map(|segment| {
            let (side, other) = if original {
                (segment.original, segment.aside)
            } else {
                (segment.aside, segment.original)
            };
            let [lo, hi] = side;
            if (lo < end && start < hi) || (start == end && lo <= start && start <= hi) {
                Some(if segment.kind == "copied" {
                    [other[0] + start.max(lo) - lo, other[0] + end.min(hi) - lo]
                } else {
                    other
                })
            } else {
                None
            }
        })
        .collect()
}

pub fn snapshot() -> Result<Value> {
    let vendor = root().join("third_party/quirrel");
    let manifest = json(&vendor.join("UPSTREAM.json"))?;
    ensure!(
        manifest["commit"].as_str().is_some_and(|s| !s.is_empty()),
        "missing Quirrel origin"
    );
    ensure!(
        !fs::read(vendor.join("COPYRIGHT"))?.is_empty(),
        "missing Quirrel copyright"
    );
    files(&vendor)?; // Preserve the source tree boundary, without freezing its bytes.
    Ok(manifest)
}

pub fn conversions(write: bool) -> Result<usize> {
    snapshot()?;
    let base = root().join("tools/quirrel-compat");
    let recipes: Vec<Recipe> = serde_json::from_value(json(&base.join("recipes.json"))?)?;
    let mut count = 0;
    for recipe in recipes {
        let source = fs::read_to_string(root().join("third_party/quirrel").join(&recipe.source))?;
        let (output, mapping) = convert(&source, &recipe)?;
        let target = base
            .join("converted")
            .join(&recipe.source)
            .with_extension("aside");
        if write {
            fs::create_dir_all(target.parent().unwrap())?;
            fs::write(&target, &output)?;
            fs::write(
                target.with_extension("map.json"),
                serde_json::to_string_pretty(&mapping)? + "\n",
            )?;
        } else {
            ensure!(
                fs::read_to_string(&target)? == output,
                "stale conversion: {}",
                target.display()
            );
            let existing: Mapping =
                serde_json::from_value(json(&target.with_extension("map.json"))?)?;
            ensure!(
                existing == mapping,
                "stale source map: {}",
                target.display()
            );
        }
        let mut original_cursor = 0;
        let mut aside_cursor = 0;
        for segment in &mapping.segments {
            ensure!(
                segment.original[0] == original_cursor && segment.aside[0] == aside_cursor,
                "non-contiguous map"
            );
            original_cursor = segment.original[1];
            aside_cursor = segment.aside[1];
            if segment.kind == "copied" {
                ensure!(
                    source[segment.original[0]..segment.original[1]]
                        == output[segment.aside[0]..segment.aside[1]],
                    "copied bytes differ"
                );
                ensure!(
                    map_range(&mapping, true, segment.original[0], segment.original[1])
                        == vec![segment.aside],
                    "forward map"
                );
                ensure!(
                    map_range(&mapping, false, segment.aside[0], segment.aside[1])
                        == vec![segment.original],
                    "reverse map"
                );
            } else if segment.kind == "reviewed_edit" {
                ensure!(
                    map_range(&mapping, true, segment.original[0], segment.original[1])
                        .contains(&segment.aside),
                    "edited forward map"
                );
                ensure!(
                    map_range(&mapping, false, segment.aside[0], segment.aside[1])
                        .contains(&segment.original),
                    "edited reverse map"
                );
            }
        }
        ensure!(
            original_cursor == source.len() && aside_cursor == output.len(),
            "incomplete source map"
        );
        count += mapping.assertions.len();
    }
    Ok(count)
}

pub fn check_t01() -> Result<()> {
    let base = root().join("crates/aside-compiler/tests/fixtures/quirrel");
    let provenance = json(&base.join("provenance.json"))?;
    let rows = provenance["files"].as_array().context("T01 files")?;
    let mut count = 0;
    for row in rows {
        let path = base.join(row["path"].as_str().context("T01 path")?);
        let source = fs::read_to_string(&path)?;
        let mapping = json(&path.with_extension("map.json"))?;
        let actual = assertions(&source)?;
        let mapped = mapping["assertions"].as_array().context("T01 assertions")?;
        ensure!(
            actual
                .iter()
                .map(|a| &a["tokens"])
                .eq(mapped.iter().map(|a| &a["tokens"])),
            "T01 assertion tokens changed"
        );
        // Both copies must retain the accepted map, including all original assertions.
        let origin = root().join(row["source"].as_str().context("T01 origin")?);
        ensure!(
            json(&origin.with_extension("map.json"))? == mapping,
            "T01 map differs from conversion"
        );
        count += mapping["assertions"]
            .as_array()
            .context("T01 assertions")?
            .len();
    }
    ensure!(
        rows.len() == 10 && count == 32,
        "T01 fixture/assertion count"
    );
    Ok(())
}

pub fn inventory() -> Result<Value> {
    let manifest = snapshot()?;
    let base = root().join("tools/quirrel-compat");
    let recipes: Vec<Recipe> = serde_json::from_value(json(&base.join("recipes.json"))?)?;
    let deltas = json(&base.join("deliberate-deltas.json"))?;
    let mut delta_sources = BTreeSet::new();
    for delta in deltas.as_array().context("deltas")? {
        for name in delta["upstream_fixtures"]
            .as_array()
            .context("delta fixtures")?
        {
            let name = name.as_str().context("delta path")?;
            ensure!(
                manifest["files_sha256"][name].is_string(),
                "delta refers to nonexistent fixture: {name}"
            );
            delta_sources.insert(name.to_owned());
        }
    }
    let mut rows = Vec::new();
    for (name, hash) in manifest["files_sha256"].as_object().unwrap() {
        if !(name.ends_with(".nut") || name.ends_with(".nut.txt")) {
            continue;
        }
        let kind = if name.starts_with("testData/exec/") || name.starts_with("doc/examples/") {
            "Exec"
        } else if name.starts_with("testData/diagnostics/") {
            "Diagnostics"
        } else if name.starts_with("testData/ast/") {
            "AST"
        } else if name.starts_with("testData/static_analyzer/") {
            "Static Analyzer"
        } else if name.starts_with("testData/types/") {
            "Parse Types"
        } else {
            continue;
        };
        let recipe = recipes.iter().find(|r| r.source == *name);
        let category = recipe.map(|r| r.category.as_str()).unwrap_or_else(|| {
            if delta_sources.contains(name) {
                "deliberate_delta"
            } else if kind != "Exec" {
                "compiler_specific"
            } else {
                "blocked"
            }
        });
        ensure!(CATEGORIES.contains(&category), "invalid category");
        rows.push(json!({"fixture":name,"source_sha256":hash,"upstream_kind":kind,"category":category,"conversion":if recipe.is_some(){"converted"}else{"not_converted"},"original_run":"not_run","aside_parse":"not_run","aside_execution":"waiting_runtime"}));
    }
    ensure!(rows.len() == 1296, "fixture universe changed");
    for recipe in &recipes {
        ensure!(
            rows.iter().any(|r| r["fixture"] == recipe.source),
            "recipe outside fixture universe"
        );
    }
    let categories: BTreeSet<_> = rows
        .iter()
        .map(|r| r["category"].as_str().unwrap())
        .collect();
    ensure!(
        categories == BTreeSet::from(CATEGORIES),
        "classification coverage"
    );
    Ok(
        json!({"schema_version":2,"upstream_commit":PIN,"note":"Source inventory only. Execution and parse results require an explicit separate run.","fixtures":rows}),
    )
}

// Check externally supplied historical observations; ordinary tests need no run logs.
pub fn verify_results(dir: &Path) -> Result<usize> {
    let inv = inventory()?;
    let result = json(&dir.join("upstream.json"))?;
    let frontend = json(&dir.join("frontend.json"))?;
    ensure!(
        result["upstream_commit"] == PIN,
        "results use another snapshot"
    );
    ensure!(
        result["created_goldens_in_work"] == json!([])
            && result["changed_snapshot_files_in_work"] == json!([]),
        "runner changed source/goldens"
    );
    let (failures, reached) = verify_observations(dir, &result)?;
    let rows = inv["fixtures"].as_array().unwrap();
    ensure!(
        reached.len() == rows.len(),
        "fixture universe differs from runner selection"
    );
    for row in rows {
        let name = row["fixture"].as_str().unwrap();
        let runs = reached
            .get(name)
            .with_context(|| format!("missing original result: {name}"))?;
        ensure!(
            runs.iter().all(|r| r["kind"] == row["upstream_kind"]),
            "wrong-mode original result: {name}"
        );
        let source = fs::read_to_string(root().join("third_party/quirrel").join(name))?;
        let expected = expected_seeds(&source);
        ensure!(
            runs.len() == expected.len()
                && runs
                    .iter()
                    .map(|r| r["seed"].as_str().unwrap_or(""))
                    .collect::<BTreeSet<_>>()
                    == expected,
            "missing or duplicate seed: {name}"
        );
    }
    ensure!(
        result["runner_exit_code"] == if failures == 0 { 0 } else { 1 },
        "runner exit differs from comparisons"
    );
    let recipes: Vec<Recipe> =
        serde_json::from_value(json(&root().join("tools/quirrel-compat/recipes.json"))?)?;
    let parsed = frontend["fixtures"]
        .as_object()
        .context("parsed fixtures")?;
    ensure!(parsed.len() == recipes.len(), "frontend recipe count");
    for recipe in recipes {
        let path = Path::new("converted")
            .join(&recipe.source)
            .with_extension("aside");
        let record = parsed
            .get(path.to_str().unwrap())
            .context("missing parsed conversion")?;
        ensure!(
            record["original"] == recipe.source
                && record["source_sha256"]
                    == sha(&fs::read(root().join("tools/quirrel-compat").join(path))?),
            "stale frontend result"
        );
    }
    Ok(failures)
}

fn verify_observations<'a>(
    dir: &Path,
    result: &'a Value,
) -> Result<(usize, BTreeMap<String, Vec<&'a Value>>)> {
    ensure!(
        result["created_goldens_in_work"] == json!([])
            && result["changed_snapshot_files_in_work"] == json!([]),
        "runner changed source/goldens"
    );
    let mut archive = zip::ZipArchive::new(fs::File::open(
        dir.join(result["artifact_archive"].as_str().context("archive")?),
    )?)?;
    let mut reached: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut failures = 0;
    for run in result["runs"].as_array().context("runs")? {
        let name = run["fixture"].as_str().context("fixture")?;
        let mut actual = Vec::new();
        archive
            .by_name(run["actual"].as_str().context("actual")?)?
            .read_to_end(&mut actual)?;
        ensure!(sha(&actual) == run["actual_sha256"], "actual hash: {name}");
        let expected = fs::read_to_string(
            root()
                .join("third_party/quirrel")
                .join(run["golden"].as_str().context("golden")?),
        )?;
        let matched = compare_output(std::str::from_utf8(&actual)?, &expected);
        ensure!(
            run["status"] == if matched { "pass" } else { "mismatch" },
            "comparison changed: {name}"
        );
        ensure!(run["returncode"] == 0, "upstream exit: {name}");
        if !matched {
            failures += 1;
            ensure!(
                fs::read(
                    dir.join(
                        run["readable_actual"]
                            .as_str()
                            .context("readable failure")?
                    )
                )? == actual,
                "failure bytes: {name}"
            );
        }
        reached.entry(name.to_owned()).or_default().push(run);
    }
    ensure!(
        result["runner_exit_code"] == if failures == 0 { 0 } else { 1 },
        "runner exit differs from comparisons"
    );
    Ok((failures, reached))
}

fn compare_output(actual: &str, expected: &str) -> bool {
    actual
        .lines()
        .map(str::trim_end)
        .eq(expected.lines().map(str::trim_end))
}

fn expected_seeds(source: &str) -> BTreeSet<&'static str> {
    // Exact pinned testRunner.py text heuristic, deliberately not token analysis.
    // Keep in sync with its hash-verified RANDOMIZED_ITERATION_PATTERNS.
    let randomized = source.lines().any(|line| {
        line.find("foreach")
            .is_some_and(|i| line[i + 7..].contains(','))
    }) || [".map", ".reduce", ".each"]
        .iter()
        .any(|s| source.contains(s));
    if randomized {
        BTreeSet::from(["default", "0", "255"])
    } else {
        BTreeSet::from(["default"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe(source: &str, edits: Value) -> Recipe {
        serde_json::from_value(json!({"source":"negative.nut","source_sha256":sha(source.as_bytes()),"category":"syntax_converted","edits":edits})).unwrap()
    }
    #[test]
    fn pinned_snapshot_conversions_and_t01() {
        snapshot().unwrap();
        assert_eq!(conversions(false).unwrap(), 32);
        check_t01().unwrap();
        inventory().unwrap();
    }
    #[test]
    fn allows_source_edits_but_rejects_weakened_assertions() {
        let source = "assert(1 == 2);\n";
        let r = recipe(source, json!([]));
        let (updated, mapping) = convert(&(source.to_owned() + " "), &r).unwrap();
        assert!(updated.ends_with("\n "));
        assert_eq!(mapping.assertions.len(), 1);
        for after in ["assert(true);", "", "assert(1 == 1);"] {
            let r = recipe(
                source,
                json!([{"range":[0,15],"before":&source[..15],"after":after,"reason":"negative"}]),
            );
            assert!(
                convert(source, &r)
                    .unwrap_err()
                    .to_string()
                    .contains("assertion tokens")
            );
        }
    }
    #[test]
    fn unicode_and_literal_and_comment_boundaries() {
        let source = "let 名称 = \"hello\";\n";
        let at = source.find("ell").unwrap();
        let r = recipe(
            source,
            json!([{"range":[at,at+3],"before":"ell","after":"a","reason":"negative"}]),
        );
        assert!(
            convert(source, &r)
                .unwrap_err()
                .to_string()
                .contains("cuts literal token")
        );
        let r = recipe(
            source,
            json!([{"range":[0,3],"before":"let","after":"let mut","reason":"binding"}]),
        );
        let (out, map) = convert(source, &r).unwrap();
        let start = source.find('名').unwrap();
        let range = map_range(&map, true, start, start + "名称".len());
        assert_eq!(range.len(), 1);
        assert_eq!(&out[range[0][0]..range[0][1]], "名称");
        assert_eq!(
            map_range(&map, false, range[0][0], range[0][1]),
            vec![[start, start + 6]]
        );
        let source = "// assert(false);\nassert(true);\n";
        assert_eq!(
            convert(source, &recipe(source, json!([])))
                .unwrap()
                .1
                .assertions
                .len(),
            1
        );
        let r = recipe(
            source,
            json!([{"range":[3,9],"before":"assert","after":"a","reason":"negative"}]),
        );
        assert!(
            convert(source, &r)
                .unwrap_err()
                .to_string()
                .contains("cuts comment token")
        );
    }
    #[test]
    fn comparisons_preserve_line_counts_and_trailing_whitespace_rule() {
        assert!(compare_output("a  \nb\n", "a\nb\n"));
        assert!(!compare_output("a\nb\n\n", "a\nb\n"));
        assert!(!compare_output("a\nchanged\n", "a\nb\n"));
    }
    #[test]
    fn seed_predicate_matches_pinned_runner_text_heuristic() {
        for source in [
            "foreach (key,value in table)",
            "// .map",
            "text.reduce",
            "x.eachThing",
            "\"foreach x,y\"",
        ] {
            assert_eq!(
                expected_seeds(source),
                BTreeSet::from(["default", "0", "255"]),
                "{source}"
            );
        }
        for source in [
            "foreach (value in array)",
            "foreach key\n,value",
            ".keys()",
            ".values()",
            "map(x)",
        ] {
            assert_eq!(
                expected_seeds(source),
                BTreeSet::from(["default"]),
                "{source}"
            );
        }
    }
    #[test]
    fn raw_observation_hashes_mismatches_and_failure_guards() {
        let dir = root().join("tools/reel-dev/tests/fixtures/quirrel-observations");
        let result = json(&dir.join("upstream.json")).unwrap();
        let (mismatches, runs) = verify_observations(&dir, &result).unwrap();
        assert_eq!(mismatches, 8);
        assert_eq!(result["runner_exit_code"], 1);
        assert!(runs.values().any(|rows| {
            rows.iter()
                .map(|r| r["seed"].as_str().unwrap())
                .collect::<BTreeSet<_>>()
                == BTreeSet::from(["default", "0", "255"])
        }));
        for (field, value) in [
            ("actual_sha256", json!("forged")),
            ("returncode", json!(1)),
            ("status", json!("invented-pass")),
            ("golden", json!("missing-golden")),
        ] {
            let mut changed = result.clone();
            changed["runs"][0][field] = value;
            assert!(verify_observations(&dir, &changed).is_err(), "{field}");
        }
        for (field, value) in [
            ("created_goldens_in_work", json!(["added"])),
            ("changed_snapshot_files_in_work", json!(["changed"])),
            ("runner_exit_code", json!(0)),
        ] {
            let mut changed = result.clone();
            changed[field] = value;
            assert!(verify_observations(&dir, &changed).is_err(), "{field}");
        }
    }
    #[test]
    fn rejects_reordered_assertions_and_invalid_edits() {
        let source = "assert(1); assert(2);";
        let r = recipe(
            source,
            json!([{"range":[0,source.len()],"before":source,"after":"assert(2); assert(1);","reason":"negative"}]),
        );
        assert!(
            convert(source, &r)
                .unwrap_err()
                .to_string()
                .contains("assertion tokens")
        );
        for edits in [
            json!([{"range":[0,6],"before":"wrong!","after":"assert","reason":"negative"}]),
            json!([{"range":[0,6],"before":"assert","after":"assert","reason":"negative"},{"range":[0,6],"before":"assert","after":"assert","reason":"overlap"}]),
        ] {
            assert!(convert(source, &recipe(source, edits)).is_err());
        }
        for source in ["/* unterminated", "\"unterminated", "assert("] {
            assert!(convert(source, &recipe(source, json!([]))).is_err());
        }
    }
}
