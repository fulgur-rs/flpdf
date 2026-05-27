# JSON schema-diff against qpdf --json — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Beads issue:** `flpdf-9hc.11.14` — Tests: schema diff against qpdf --json on shared fixtures

**Goal:** Rust integration test (`crates/flpdf-cli/tests/json_schema_diff.rs`) that runs `qpdf --json=2` and `flpdf --json=2` on a curated fixture corpus, reports per-(fixture × top-level key) pass rate, and fails on unknown divergences.

**Architecture:** Single integration test invokes both tools per fixture, parses JSON via `serde_json::Value`, runs a strict-equality recursive diff that records every divergence with a JSON path, classifies each divergence via an allowlist file, and writes a markdown + JSON report under `target/`. Logic lives in `crates/flpdf-cli/tests/support/json_diff/mod.rs` (re-usable from future qtest harness).

**Tech Stack:** Rust 2021, `serde_json` (new dev-dep), `assert_cmd` (existing), `tempfile` (existing). `qpdf 11.x` CLI on PATH (test skips with eprintln if absent).

## qpdf JSON v2 top-level schema

The matrix columns reflect the **qpdf JSON v2** top-level keys as actually emitted by `qpdf --json=2` (verified empirically against qpdf 11.9.0 on `minimal.pdf` and `compat/one-page.pdf`, and cross-checked against `qpdf --json-help=2`). The 9 keys are:

```text
acroform, attachments, encrypt, outlines, pagelabels, pages, parameters, qpdf, version
```

Note: an earlier draft of this plan listed `objects`/`objectinfo` as separate top-level keys — that was the qpdf JSON v1 schema. In v2 those are folded into a single `qpdf` array containing `[metadata_dict, objects_dict]`.

## Deviations from beads `DESIGN`

The beads issue's design specified the allowlist in **TOML**. To avoid adding a `toml` crate dependency (none currently in workspace), this plan uses **JSON** for the allowlist (`tests/fixtures/json-diff/allowed-divergences.json`). Same data, parsed via `serde_json::Value`. Update the issue design with `bd update --design` after Task 1 lands if the deviation is approved.

## TDD discipline

Every implementation task follows: write failing test → run to confirm fail → minimal impl → run to confirm pass → commit. Tests use `#[cfg(test)]` inside `mod.rs` for unit-level coverage; the top-level corpus run is a real `#[test]` in `json_schema_diff.rs`.

---

## Task 1: Scaffold dependencies + empty module + smoke test

**Files:**
- Modify: `Cargo.toml` (workspace) — add `serde_json = "1"` under `[workspace.dependencies]`
- Modify: `crates/flpdf-cli/Cargo.toml` — add `serde_json.workspace = true` under `[dev-dependencies]`
- Create: `crates/flpdf-cli/tests/support/json_diff/mod.rs` — empty module with one placeholder type
- Modify: `crates/flpdf-cli/tests/support/mod.rs` — `pub mod json_diff;` declaration
- Create: `crates/flpdf-cli/tests/json_schema_diff.rs` — placeholder test that compiles

**Step 1: Add workspace dep**

Edit `Cargo.toml` (workspace) to add `serde_json = "1"` in `[workspace.dependencies]`.

**Step 2: Add dev-dep**

Edit `crates/flpdf-cli/Cargo.toml` and add to `[dev-dependencies]`:
```toml
serde_json = { workspace = true }
```

**Step 3: Run build**

Run: `cargo build --tests -p flpdf-cli 2>&1 | tail -20`
Expected: clean build.

**Step 4: Create empty diff module**

Create `crates/flpdf-cli/tests/support/json_diff/mod.rs`:
```rust
//! JSON tree diff + allowlist + report machinery for the qpdf JSON schema-diff
//! corpus test (flpdf-9hc.11.14).
//!
//! Public surface:
//! - [`Divergence`] — one path-level mismatch between qpdf and flpdf JSON.
//! - [`diff_values`] — recursive strict-equality tree diff.
//! - [`Allowlist`] — load + match + stale-entry detection.
//! - [`Report`] — fixture × top-level-key matrix + markdown/json writers.
//!
//! See `docs/plans/2026-05-27-flpdf-9hc-11-14-json-schema-diff.md` for the full
//! plan.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Divergence {
    pub path: String,
    pub qpdf: Value,
    pub flpdf: Value,
}
```

**Step 5: Wire module**

Edit `crates/flpdf-cli/tests/support/mod.rs` and add near the existing `pub mod comparators;` line:
```rust
pub mod json_diff;
```

**Step 6: Placeholder top-level test**

Create `crates/flpdf-cli/tests/json_schema_diff.rs`:
```rust
//! Integration test: qpdf --json=2 vs flpdf --json=2 schema-diff over a curated
//! fixture corpus (beads flpdf-9hc.11.14).

mod support;

#[test]
fn json_schema_diff_corpus_smoke() {
    // Real corpus test added in Task 7. This placeholder verifies the module
    // wiring compiles.
    let _ = std::any::type_name::<support::json_diff::Divergence>();
}
```

**Step 7: Run**

Run: `cargo test -p flpdf-cli --test json_schema_diff 2>&1 | tail -20`
Expected: `1 passed`.

**Step 8: Commit**

```bash
git add Cargo.toml crates/flpdf-cli/Cargo.toml \
        crates/flpdf-cli/tests/support/mod.rs \
        crates/flpdf-cli/tests/support/json_diff/mod.rs \
        crates/flpdf-cli/tests/json_schema_diff.rs
git commit -m "feat(test): scaffold json_diff module for qpdf JSON schema-diff (flpdf-9hc.11.14)"
```

---

## Task 2: JSON tree diff (TDD)

**Files:**
- Modify: `crates/flpdf-cli/tests/support/json_diff/mod.rs`

**Goal:** `diff_values(a, b) -> Vec<Divergence>` performs strict recursive equality. No normalization. Records each leaf-level mismatch as one Divergence with JSON path.

**Step 1: Write failing tests**

Append a `#[cfg(test)] mod tests` block to `mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(divs: &[Divergence]) -> Vec<&str> {
        divs.iter().map(|d| d.path.as_str()).collect()
    }

    #[test]
    fn identical_primitives_no_divergence() {
        assert!(diff_values(&json!(1), &json!(1)).is_empty());
        assert!(diff_values(&json!("a"), &json!("a")).is_empty());
        assert!(diff_values(&json!(null), &json!(null)).is_empty());
        assert!(diff_values(&json!(true), &json!(true)).is_empty());
    }

    #[test]
    fn primitive_mismatch_reports_root_path() {
        let d = diff_values(&json!(1), &json!(2));
        assert_eq!(paths(&d), vec!["$"]);
        assert_eq!(d[0].qpdf, json!(1));
        assert_eq!(d[0].flpdf, json!(2));
    }

    #[test]
    fn integer_vs_float_is_a_divergence() {
        // strict value match: 0 and 0.0 must differ
        let d = diff_values(&json!(0), &json!(0.0));
        assert_eq!(paths(&d), vec!["$"]);
    }

    #[test]
    fn array_length_mismatch_reports_array_path() {
        let d = diff_values(&json!([1, 2]), &json!([1, 2, 3]));
        assert_eq!(paths(&d), vec!["$"]);
    }

    #[test]
    fn array_element_mismatch_reports_indexed_path() {
        let d = diff_values(&json!([1, 2, 3]), &json!([1, 9, 3]));
        assert_eq!(paths(&d), vec!["$[1]"]);
    }

    #[test]
    fn object_value_mismatch_reports_key_path() {
        let a = json!({"x": 1, "y": 2});
        let b = json!({"x": 1, "y": 99});
        let d = diff_values(&a, &b);
        assert_eq!(paths(&d), vec!["$.y"]);
    }

    #[test]
    fn missing_key_in_b_is_a_divergence() {
        let a = json!({"x": 1, "y": 2});
        let b = json!({"x": 1});
        let d = diff_values(&a, &b);
        // Report at $.y with qpdf=2 and flpdf=Null
        assert_eq!(paths(&d), vec!["$.y"]);
        assert_eq!(d[0].qpdf, json!(2));
        assert_eq!(d[0].flpdf, Value::Null);
    }

    #[test]
    fn extra_key_in_b_is_a_divergence() {
        let a = json!({"x": 1});
        let b = json!({"x": 1, "z": 5});
        let d = diff_values(&a, &b);
        assert_eq!(paths(&d), vec!["$.z"]);
        assert_eq!(d[0].qpdf, Value::Null);
        assert_eq!(d[0].flpdf, json!(5));
    }

    #[test]
    fn nested_object_path_uses_dot() {
        let a = json!({"a": {"b": {"c": 1}}});
        let b = json!({"a": {"b": {"c": 2}}});
        let d = diff_values(&a, &b);
        assert_eq!(paths(&d), vec!["$.a.b.c"]);
    }

    #[test]
    fn key_with_special_chars_is_quoted() {
        // PDF object refs like "3 0 R" or PDF names starting with "/" need quoting
        let a = json!({"3 0 R": 1});
        let b = json!({"3 0 R": 2});
        let d = diff_values(&a, &b);
        assert_eq!(paths(&d), vec![r#"$."3 0 R""#]);
    }

    #[test]
    fn type_mismatch_records_both_values() {
        let d = diff_values(&json!(1), &json!("1"));
        assert_eq!(paths(&d), vec!["$"]);
        assert_eq!(d[0].qpdf, json!(1));
        assert_eq!(d[0].flpdf, json!("1"));
    }

    #[test]
    fn multiple_divergences_collected_in_order() {
        let a = json!({"a": 1, "b": 2, "c": 3});
        let b = json!({"a": 9, "b": 2, "c": 8});
        let d = diff_values(&a, &b);
        // Order: object iteration follows serde_json's preserved insertion order.
        // We expect both $.a and $.c with $.a first.
        assert_eq!(paths(&d), vec!["$.a", "$.c"]);
    }
}
```

**Step 2: Run test — should fail to compile (no `diff_values`)**

Run: `cargo test -p flpdf-cli --test json_schema_diff 2>&1 | tail -10`
Expected: compile error `cannot find function diff_values`.

**Step 3: Implement**

Add to `mod.rs` (between the `Divergence` struct and the `#[cfg(test)]` block):

```rust
/// Strict recursive equality diff between two JSON trees.
///
/// Records one [`Divergence`] per mismatched path. No normalization is applied:
/// integers and floats with the same numerical value are reported as different
/// (consistent with the strict-value-match policy in flpdf-9hc.11.14).
pub fn diff_values(qpdf: &Value, flpdf: &Value) -> Vec<Divergence> {
    let mut out = Vec::new();
    diff_at(qpdf, flpdf, "$", &mut out);
    out
}

fn diff_at(a: &Value, b: &Value, path: &str, out: &mut Vec<Divergence>) {
    use serde_json::Value::*;
    match (a, b) {
        (Object(ao), Object(bo)) => {
            // Iterate union of keys preserving insertion order, qpdf first.
            let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
            for (k, av) in ao {
                seen.insert(k.as_str());
                let child_path = join_path(path, k);
                match bo.get(k) {
                    Some(bv) => diff_at(av, bv, &child_path, out),
                    None => out.push(Divergence {
                        path: child_path,
                        qpdf: av.clone(),
                        flpdf: Value::Null,
                    }),
                }
            }
            for (k, bv) in bo {
                if !seen.contains(k.as_str()) {
                    out.push(Divergence {
                        path: join_path(path, k),
                        qpdf: Value::Null,
                        flpdf: bv.clone(),
                    });
                }
            }
        }
        (Array(aa), Array(bb)) if aa.len() == bb.len() => {
            for (i, (av, bv)) in aa.iter().zip(bb.iter()).enumerate() {
                let child_path = format!("{path}[{i}]");
                diff_at(av, bv, &child_path, out);
            }
        }
        // Leaf-level mismatch (including length-mismatched arrays and any
        // type/value disagreement on primitives).
        _ if a != b => out.push(Divergence {
            path: path.to_string(),
            qpdf: a.clone(),
            flpdf: b.clone(),
        }),
        _ => {}
    }
}

fn join_path(parent: &str, key: &str) -> String {
    if is_simple_key(key) {
        format!("{parent}.{key}")
    } else {
        format!("{parent}.{:?}", key) // serde-style escaped quoting via Debug
    }
}

fn is_simple_key(k: &str) -> bool {
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
```

**Step 4: Run test**

Run: `cargo test -p flpdf-cli --test json_schema_diff 2>&1 | tail -20`
Expected: all `diff_values` tests pass.

If `key_with_special_chars_is_quoted` fails, double-check the literal raw-string in the assertion vs. what `{:?}` formatter produces.

**Step 5: Commit**

```bash
git add crates/flpdf-cli/tests/support/json_diff/mod.rs
git commit -m "feat(test/json-diff): strict recursive JSON value diff with path tracking"
```

---

## Task 3: Allowlist (load + match + stale detection)

**Files:**
- Modify: `crates/flpdf-cli/tests/support/json_diff/mod.rs`

**Goal:** Load a JSON allowlist file, match a `(fixture, path)` divergence against it, and detect stale entries (allowlist entry that never matched).

### Allowlist file format

`tests/fixtures/json-diff/allowed-divergences.json`:
```json
{
  "entries": [
    {
      "fixture": "compat/example.pdf",
      "path": "$.parameters.version",
      "category": "flpdf-feature-gap",
      "beads_ref": "flpdf-9hc.11.x",
      "reason": "flpdf reports 1.4, qpdf reports 1.5"
    }
  ]
}
```

The file may not exist at first run — treat missing/empty file as `entries: []`.

**Step 1: Write failing tests**

Append to the `mod tests` block:

```rust
const SAMPLE_ALLOWLIST: &str = r#"{
  "entries": [
    {
      "fixture": "compat/foo.pdf",
      "path": "$.parameters.version",
      "category": "flpdf-feature-gap",
      "beads_ref": "flpdf-9hc.11.x",
      "reason": "version reporting differs"
    }
  ]
}"#;

#[test]
fn allowlist_loads_from_json_string() {
    let al = Allowlist::from_json_str(SAMPLE_ALLOWLIST).unwrap();
    assert_eq!(al.entries().len(), 1);
    assert_eq!(al.entries()[0].fixture, "compat/foo.pdf");
    assert_eq!(al.entries()[0].path, "$.parameters.version");
    assert_eq!(al.entries()[0].category, "flpdf-feature-gap");
}

#[test]
fn allowlist_empty_object_is_empty() {
    let al = Allowlist::from_json_str(r#"{"entries":[]}"#).unwrap();
    assert!(al.entries().is_empty());
}

#[test]
fn allowlist_missing_file_is_empty() {
    let al = Allowlist::from_path(std::path::Path::new("/nonexistent/allowlist.json")).unwrap();
    assert!(al.entries().is_empty());
}

#[test]
fn allowlist_matches_by_fixture_and_path() {
    let mut al = Allowlist::from_json_str(SAMPLE_ALLOWLIST).unwrap();
    let div = Divergence { path: "$.parameters.version".to_string(), qpdf: json!(1), flpdf: json!(2) };
    let m = al.match_divergence("compat/foo.pdf", &div);
    assert!(m.is_some());
    assert_eq!(m.unwrap().category, "flpdf-feature-gap");
}

#[test]
fn allowlist_no_match_for_different_fixture() {
    let mut al = Allowlist::from_json_str(SAMPLE_ALLOWLIST).unwrap();
    let div = Divergence { path: "$.parameters.version".to_string(), qpdf: json!(1), flpdf: json!(2) };
    assert!(al.match_divergence("compat/other.pdf", &div).is_none());
}

#[test]
fn allowlist_no_match_for_different_path() {
    let mut al = Allowlist::from_json_str(SAMPLE_ALLOWLIST).unwrap();
    let div = Divergence { path: "$.pages[0].mediabox".to_string(), qpdf: json!(1), flpdf: json!(2) };
    assert!(al.match_divergence("compat/foo.pdf", &div).is_none());
}

#[test]
fn allowlist_tracks_unused_entries() {
    let mut al = Allowlist::from_json_str(SAMPLE_ALLOWLIST).unwrap();
    let stale = al.stale_entries();
    assert_eq!(stale.len(), 1);

    // After matching, that entry should not be stale.
    let div = Divergence { path: "$.parameters.version".to_string(), qpdf: json!(1), flpdf: json!(2) };
    let _ = al.match_divergence("compat/foo.pdf", &div);
    assert!(al.stale_entries().is_empty());
}
```

**Step 2: Run — fail to compile**

Run: `cargo test -p flpdf-cli --test json_schema_diff 2>&1 | tail -10`
Expected: compile errors for `Allowlist`.

**Step 3: Implement**

Add to `mod.rs`:

```rust
use std::path::Path;

#[derive(Debug, Clone)]
pub struct AllowlistEntry {
    pub fixture: String,
    pub path: String,
    pub category: String,
    pub beads_ref: String,
    pub reason: String,
}

#[derive(Debug)]
pub struct Allowlist {
    entries: Vec<AllowlistEntry>,
    matched: Vec<bool>,
}

impl Allowlist {
    pub fn from_json_str(s: &str) -> Result<Self, String> {
        if s.trim().is_empty() {
            return Ok(Self { entries: vec![], matched: vec![] });
        }
        let v: Value = serde_json::from_str(s).map_err(|e| format!("allowlist parse: {e}"))?;
        let arr = v.get("entries").and_then(Value::as_array)
            .ok_or_else(|| "allowlist missing 'entries' array".to_string())?;
        let mut entries = Vec::with_capacity(arr.len());
        for (i, item) in arr.iter().enumerate() {
            let getstr_req = |k: &str| -> Result<String, String> {
                item.get(k).and_then(Value::as_str).map(str::to_string)
                    .ok_or_else(|| format!("allowlist entry {i} missing string field '{k}'"))
            };
            let getstr_opt = |k: &str| -> String {
                item.get(k).and_then(Value::as_str).map(str::to_string).unwrap_or_default()
            };
            entries.push(AllowlistEntry {
                fixture: getstr_req("fixture")?,
                path: getstr_req("path")?,
                category: getstr_req("category")?,
                beads_ref: getstr_opt("beads_ref"),
                reason: getstr_opt("reason"),
            });
        }
        let matched = vec![false; entries.len()];
        Ok(Self { entries, matched })
    }

    pub fn from_path(p: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(p) {
            Ok(s) => Self::from_json_str(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self { entries: vec![], matched: vec![] })
            }
            Err(e) => Err(format!("allowlist read {}: {e}", p.display())),
        }
    }

    pub fn entries(&self) -> &[AllowlistEntry] {
        &self.entries
    }

    /// Match a divergence against the allowlist. On match, marks the entry as
    /// used and returns it.
    pub fn match_divergence(&mut self, fixture: &str, div: &Divergence) -> Option<&AllowlistEntry> {
        for (i, e) in self.entries.iter().enumerate() {
            if e.fixture == fixture && e.path == div.path {
                self.matched[i] = true;
                return Some(&self.entries[i]);
            }
        }
        None
    }

    /// Entries that never matched a divergence during this run.
    pub fn stale_entries(&self) -> Vec<&AllowlistEntry> {
        self.entries.iter().enumerate()
            .filter(|(i, _)| !self.matched[*i])
            .map(|(_, e)| e)
            .collect()
    }
}
```

**Step 4: Run**

Run: `cargo test -p flpdf-cli --test json_schema_diff 2>&1 | tail -20`
Expected: all `allowlist_*` tests pass.

**Step 5: Commit**

```bash
git add crates/flpdf-cli/tests/support/json_diff/mod.rs
git commit -m "feat(test/json-diff): allowlist load + match + stale-entry detection (JSON format)"
```

---

## Task 4: Top-level key extraction + per-key matrix cell

**Files:**
- Modify: `crates/flpdf-cli/tests/support/json_diff/mod.rs`

**Goal:** Given two top-level JSON v2 documents, produce a `Vec<MatrixCell>` where `key` is one of qpdf JSON v2 top-level keys (acroform/attachments/encrypt/outlines/pagelabels/pages/parameters/qpdf/version — see the schema note at the top of this plan) and `status` indicates pass/known/unknown/missing.

**Step 1: Write failing tests**

Append:

```rust
const QPDF_V2_KEYS: &[&str] = &[
    "acroform", "attachments", "encrypt", "outlines",
    "pagelabels", "pages", "parameters", "qpdf", "version",
];

#[test]
fn matrix_keys_are_all_qpdf_v2_top_level() {
    assert_eq!(top_level_keys(), QPDF_V2_KEYS);
}

#[test]
fn matrix_cell_pass_when_subtrees_equal() {
    let qpdf = json!({"parameters": {"version": 2}, "pages": []});
    let flpdf = json!({"parameters": {"version": 2}, "pages": []});
    let mut al = Allowlist::from_json_str(r#"{"entries":[]}"#).unwrap();
    let cells = compute_matrix("smoke.pdf", &qpdf, &flpdf, &mut al);
    let by_key: std::collections::HashMap<_, _> = cells.iter().map(|c| (c.key, &c.status)).collect();
    assert!(matches!(by_key.get("parameters").unwrap(), CellStatus::Pass));
    assert!(matches!(by_key.get("pages").unwrap(), CellStatus::Pass));
    assert!(matches!(by_key.get("version").unwrap(), CellStatus::Missing));
}

#[test]
fn matrix_cell_unknown_when_divergence_not_in_allowlist() {
    let qpdf = json!({"parameters": {"version": 2}});
    let flpdf = json!({"parameters": {"version": 3}});
    let mut al = Allowlist::from_json_str(r#"{"entries":[]}"#).unwrap();
    let cells = compute_matrix("foo.pdf", &qpdf, &flpdf, &mut al);
    let params = cells.iter().find(|c| c.key == "parameters").unwrap();
    match &params.status {
        CellStatus::Unknown { divergences } => {
            assert_eq!(divergences.len(), 1);
            assert_eq!(divergences[0].path, "$.parameters.version");
        }
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[test]
fn matrix_cell_known_when_divergence_is_allowlisted() {
    let qpdf = json!({"parameters": {"version": 2}});
    let flpdf = json!({"parameters": {"version": 3}});
    let allowlist_json = r#"{"entries":[{
        "fixture":"foo.pdf","path":"$.parameters.version",
        "category":"flpdf-feature-gap","beads_ref":"","reason":""
    }]}"#;
    let mut al = Allowlist::from_json_str(allowlist_json).unwrap();
    let cells = compute_matrix("foo.pdf", &qpdf, &flpdf, &mut al);
    let params = cells.iter().find(|c| c.key == "parameters").unwrap();
    assert!(matches!(params.status, CellStatus::Known { .. }));
}
```

**Step 2: Run — fail**

Expected: `cannot find compute_matrix, CellStatus, top_level_keys`.

**Step 3: Implement**

Add to `mod.rs`:

```rust
pub fn top_level_keys() -> &'static [&'static str] {
    &[
        "acroform", "attachments", "encrypt", "outlines",
        "pagelabels", "pages", "parameters", "qpdf", "version",
    ]
}

#[derive(Debug)]
pub struct MatrixCell {
    pub key: &'static str,
    pub status: CellStatus,
}

#[derive(Debug)]
pub enum CellStatus {
    /// Subtree present in both, no divergences.
    Pass,
    /// Subtree present, all divergences are in the allowlist.
    Known { divergences: Vec<Divergence> },
    /// Subtree present but at least one divergence is not allowlisted.
    Unknown { divergences: Vec<Divergence> },
    /// Key missing in both qpdf and flpdf output.
    Missing,
    /// Key present in only one side — counted as a divergence.
    PresentOnOneSide { qpdf_present: bool },
}

pub fn compute_matrix(
    fixture: &str,
    qpdf: &Value,
    flpdf: &Value,
    allowlist: &mut Allowlist,
) -> Vec<MatrixCell> {
    top_level_keys().iter().map(|&key| {
        let a = qpdf.get(key);
        let b = flpdf.get(key);
        let status = match (a, b) {
            (None, None) => CellStatus::Missing,
            (Some(_), None) => CellStatus::PresentOnOneSide { qpdf_present: true },
            (None, Some(_)) => CellStatus::PresentOnOneSide { qpdf_present: false },
            (Some(av), Some(bv)) => {
                // Build per-key divergences, prefixing path with $.<key>
                let raw = diff_values(av, bv);
                let divs: Vec<Divergence> = raw.into_iter().map(|d| {
                    let new_path = if d.path == "$" {
                        format!("$.{key}")
                    } else {
                        // d.path begins with "$"; splice "$.<key>" + rest
                        format!("$.{key}{}", &d.path[1..])
                    };
                    Divergence { path: new_path, ..d }
                }).collect();

                if divs.is_empty() {
                    CellStatus::Pass
                } else {
                    // Drain ALL divergences against the allowlist before classifying, so every
                    // allowlist entry that should match gets its `matched` flag set. Using
                    // Iterator::any would short-circuit on the first unknown and miss later
                    // allowlisted siblings, leading to spurious stale-allowlist failures.
                    let unknown_count = divs.iter()
                        .filter(|d| allowlist.match_divergence(fixture, d).is_none())
                        .count();
                    if unknown_count > 0 {
                        CellStatus::Unknown { divergences: divs }
                    } else {
                        CellStatus::Known { divergences: divs }
                    }
                }
            }
        };
        MatrixCell { key, status }
    }).collect()
}
```

**Step 4: Run**

Run: `cargo test -p flpdf-cli --test json_schema_diff 2>&1 | tail -20`
Expected: all matrix tests pass.

**Step 5: Commit**

```bash
git add crates/flpdf-cli/tests/support/json_diff/mod.rs
git commit -m "feat(test/json-diff): per-(fixture,top-level-key) matrix with pass/known/unknown classification"
```

---

## Task 5: Process invocation helpers (qpdf + flpdf)

**Files:**
- Modify: `crates/flpdf-cli/tests/support/json_diff/mod.rs`

**Goal:** Two helpers — `run_qpdf_json` and `run_flpdf_json` — that invoke each binary with `--json=2 --json-stream-data=none [--password=X] <fixture>` and return the parsed `serde_json::Value` (or an error string explaining the failure).

**Step 1: Inspect existing qpdf invocation patterns**

```bash
grep -n "is_qpdf_available\|fn run_qpdf\|Command::new" crates/flpdf-cli/tests/support/comparators.rs crates/flpdf-cli/tests/support/mod.rs | head -30
```

Also confirm `flpdf` CLI accepts `--json-stream-data=none`:
```bash
grep -n "json-stream-data" crates/flpdf-cli/src/main.rs | head
```

**Step 2: Write tests**

```rust
#[test]
fn qpdf_returns_object_with_top_level_keys() {
    if !crate::support::is_qpdf_available() {
        eprintln!("skipping: qpdf not on PATH");
        return;
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/minimal.pdf");
    let v = run_qpdf_json(&path, None).expect("qpdf --json=2 on minimal.pdf");
    assert!(v.is_object());
    let obj = v.as_object().unwrap();
    assert!(obj.contains_key("parameters"), "missing parameters: {:?}", obj.keys().collect::<Vec<_>>());
    assert!(obj.contains_key("qpdf"), "missing 'qpdf' top-level key (qpdf JSON v2 schema): {:?}", obj.keys().collect::<Vec<_>>());
}

#[test]
fn flpdf_returns_object_with_top_level_keys() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/minimal.pdf");
    let v = run_flpdf_json(&path, None).expect("flpdf --json=2 on minimal.pdf");
    assert!(v.is_object());
    assert!(v.as_object().unwrap().contains_key("parameters"));
}
```

These touch real binaries and the actual `tests/fixtures/minimal.pdf` file. They are inside `#[cfg(test)] mod tests`, so they run as part of the same `cargo test` invocation. If `crate::support::is_qpdf_available` can't be accessed from inside the support submodule, use `super::super::is_qpdf_available()` or move the qpdf test to the top-level integration test.

**Step 3: Implement**

Add to `mod.rs`:

```rust
use std::process::Command;

/// Invoke `qpdf --json=2 --json-stream-data=none [--password=X] <fixture>`
/// and return the parsed JSON.
pub fn run_qpdf_json(fixture: &Path, password: Option<&str>) -> Result<Value, String> {
    let mut cmd = Command::new("qpdf");
    cmd.arg("--json=2").arg("--json-stream-data=none");
    if let Some(p) = password {
        cmd.arg(format!("--password={p}"));
    }
    cmd.arg(fixture);
    let out = cmd.output().map_err(|e| format!("spawn qpdf: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "qpdf exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("qpdf parse: {e}"))
}

/// Invoke the local `flpdf` binary via assert_cmd and return parsed JSON.
pub fn run_flpdf_json(fixture: &Path, password: Option<&str>) -> Result<Value, String> {
    let mut cmd = assert_cmd::Command::cargo_bin("flpdf").map_err(|e| e.to_string())?;
    cmd.arg("--json=2").arg("--json-stream-data=none");
    if let Some(p) = password {
        cmd.arg(format!("--password={p}"));
    }
    cmd.arg(fixture);
    let out = cmd.output().map_err(|e| format!("spawn flpdf: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "flpdf exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("flpdf parse: {e}"))
}
```

**Step 4: Verify `is_qpdf_available` visibility**

```bash
grep -n "fn is_qpdf_available\|pub fn is_qpdf_available" crates/flpdf-cli/tests/support/*.rs
```

If it's currently `pub` only inside `comparators.rs`, it should already be re-exported by `support/mod.rs`. Confirm and adjust if needed.

**Step 5: Run**

Run: `cargo test -p flpdf-cli --test json_schema_diff 2>&1 | tail -20`
Expected: both helper tests pass. If `flpdf --json=2` flag spelling differs, fix the helper.

**Step 6: Commit**

```bash
git add crates/flpdf-cli/tests/support/json_diff/mod.rs
git commit -m "feat(test/json-diff): qpdf + flpdf invocation helpers for --json=2"
```

---

## Task 6: Report types + markdown writer + JSON writer

**Files:**
- Modify: `crates/flpdf-cli/tests/support/json_diff/mod.rs`

**Goal:** `Report` aggregates per-fixture matrices + overall pass-rate, and serializes to markdown + JSON.

**Step 1: Write tests**

```rust
fn dummy_fixture_result(fixture: &str, cells: Vec<MatrixCell>) -> FixtureResult {
    FixtureResult { fixture: fixture.to_string(), cells, qpdf_error: None, flpdf_error: None }
}

#[test]
fn report_overall_pass_rate_counts_pass_and_known() {
    let cells_a = vec![
        MatrixCell { key: "parameters", status: CellStatus::Pass },
        MatrixCell { key: "qpdf",       status: CellStatus::Pass },
        MatrixCell { key: "pages",      status: CellStatus::Known { divergences: vec![Divergence { path: "$.pages[0].x".into(), qpdf: json!(1), flpdf: json!(2) }] } },
        MatrixCell { key: "encrypt",    status: CellStatus::Missing },
    ];
    let cells_b = vec![
        MatrixCell { key: "parameters", status: CellStatus::Pass },
        MatrixCell { key: "qpdf",       status: CellStatus::Unknown { divergences: vec![Divergence { path: "$.qpdf[1].x".into(), qpdf: json!(1), flpdf: json!(2) }] } },
        MatrixCell { key: "pages",      status: CellStatus::Pass },
        MatrixCell { key: "encrypt",    status: CellStatus::Missing },
    ];
    let report = Report {
        fixtures: vec![
            dummy_fixture_result("a.pdf", cells_a),
            dummy_fixture_result("b.pdf", cells_b),
        ],
        stale_allowlist: vec![],
    };
    // present cells: a has 3 (params Pass, qpdf Pass, pages Known), b has 3
    // (params Pass, qpdf Unknown, pages Pass). Total present = 6.
    // pass-or-known: 3 (a) + 2 (b) = 5. Expected 5/6.
    assert_eq!(report.overall_pass_rate(), (5, 6));
}

#[test]
fn report_unknown_divergences_collected() {
    let cells = vec![MatrixCell {
        key: "qpdf",
        status: CellStatus::Unknown {
            divergences: vec![Divergence { path: "$.qpdf[1].x".into(), qpdf: json!(1), flpdf: json!(2) }],
        },
    }];
    let report = Report {
        fixtures: vec![dummy_fixture_result("a.pdf", cells)],
        stale_allowlist: vec![],
    };
    let unknown = report.unknown_divergences();
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].1, "a.pdf");
    assert_eq!(unknown[0].2.path, "$.qpdf[1].x");
}

#[test]
fn report_markdown_includes_matrix_header() {
    let report = Report { fixtures: vec![], stale_allowlist: vec![] };
    let md = report.to_markdown();
    assert!(md.contains("| fixture"));
    for key in top_level_keys() {
        assert!(md.contains(key), "markdown missing header column '{key}'");
    }
}

#[test]
fn report_json_round_trips() {
    let report = Report {
        fixtures: vec![dummy_fixture_result("a.pdf", vec![
            MatrixCell { key: "parameters", status: CellStatus::Pass },
        ])],
        stale_allowlist: vec![],
    };
    let s = report.to_json();
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["fixtures"][0]["fixture"], "a.pdf");
    assert_eq!(v["fixtures"][0]["cells"][0]["key"], "parameters");
    assert_eq!(v["fixtures"][0]["cells"][0]["status"], "pass");
}
```

**Step 2: Run — fail**

Expected: missing `Report`, `FixtureResult`.

**Step 3: Implement**

Add to `mod.rs`:

```rust
#[derive(Debug)]
pub struct FixtureResult {
    pub fixture: String,
    pub cells: Vec<MatrixCell>,
    pub qpdf_error: Option<String>,
    pub flpdf_error: Option<String>,
}

#[derive(Debug)]
pub struct Report {
    pub fixtures: Vec<FixtureResult>,
    pub stale_allowlist: Vec<AllowlistEntry>,
}

impl Report {
    /// (pass_or_known_count, present_cell_count)
    pub fn overall_pass_rate(&self) -> (usize, usize) {
        let mut pass = 0usize;
        let mut present = 0usize;
        for f in &self.fixtures {
            for c in &f.cells {
                match &c.status {
                    CellStatus::Missing => {} // excluded
                    CellStatus::PresentOnOneSide { .. } => present += 1,
                    CellStatus::Pass | CellStatus::Known { .. } => { pass += 1; present += 1; }
                    CellStatus::Unknown { .. } => { present += 1; }
                }
            }
        }
        (pass, present)
    }

    /// All unknown divergences flattened: (key, fixture, &Divergence).
    pub fn unknown_divergences(&self) -> Vec<(&'static str, &str, &Divergence)> {
        let mut out = Vec::new();
        for f in &self.fixtures {
            for c in &f.cells {
                if let CellStatus::Unknown { divergences } = &c.status {
                    for d in divergences {
                        out.push((c.key, f.fixture.as_str(), d));
                    }
                }
            }
        }
        out
    }

    pub fn unknown_summary(&self) -> String {
        let mut s = String::new();
        for (key, fixture, d) in self.unknown_divergences() {
            s.push_str(&format!("  {fixture}  [{key}]  {}\n    qpdf: {}\n    flpdf: {}\n",
                d.path, d.qpdf, d.flpdf));
        }
        s
    }

    pub fn stale_summary(&self) -> String {
        let mut s = String::new();
        for e in &self.stale_allowlist {
            s.push_str(&format!("  {}  {}  {}\n", e.fixture, e.path, e.category));
        }
        s
    }

    pub fn to_markdown(&self) -> String {
        let keys = top_level_keys();
        let (pass, present) = self.overall_pass_rate();
        let pct = if present == 0 { 0.0 } else { 100.0 * pass as f64 / present as f64 };

        let mut s = String::new();
        s.push_str("# qpdf JSON schema-diff report\n\n");
        s.push_str(&format!(
            "Overall pass rate: **{}/{}** ({:.1}%) — present cells only\n\n",
            pass, present, pct
        ));

        // matrix header
        s.push_str("| fixture |");
        for k in keys { s.push_str(&format!(" {} |", k)); }
        s.push('\n');
        s.push_str("|---|");
        for _ in keys { s.push_str("---|"); }
        s.push('\n');

        for f in &self.fixtures {
            s.push_str(&format!("| `{}` |", f.fixture));
            let cell_by_key: std::collections::HashMap<&str, &MatrixCell>
                = f.cells.iter().map(|c| (c.key, c)).collect();
            for k in keys {
                let glyph = match cell_by_key.get(k).map(|c| &c.status) {
                    Some(CellStatus::Pass) => "ok".to_string(),
                    Some(CellStatus::Known { divergences }) => format!("known({})", divergences.len()),
                    Some(CellStatus::Unknown { divergences }) => format!("FAIL({})", divergences.len()),
                    Some(CellStatus::Missing) => "n/a".to_string(),
                    Some(CellStatus::PresentOnOneSide { qpdf_present: true }) => "qonly".to_string(),
                    Some(CellStatus::PresentOnOneSide { qpdf_present: false }) => "fonly".to_string(),
                    None => "?".to_string(),
                };
                s.push_str(&format!(" {} |", glyph));
            }
            s.push('\n');
        }

        if !self.unknown_divergences().is_empty() {
            s.push_str("\n## Unknown divergences\n\n");
            s.push_str(&self.unknown_summary());
        }

        if !self.stale_allowlist.is_empty() {
            s.push_str("\n## Stale allowlist entries\n\n");
            s.push_str(&self.stale_summary());
        }

        s
    }

    pub fn to_json(&self) -> String {
        let fixtures: Vec<Value> = self.fixtures.iter().map(|f| {
            let cells: Vec<Value> = f.cells.iter().map(|c| {
                let (status, divs): (&str, Vec<&Divergence>) = match &c.status {
                    CellStatus::Pass => ("pass", vec![]),
                    CellStatus::Known { divergences } => ("known", divergences.iter().collect()),
                    CellStatus::Unknown { divergences } => ("unknown", divergences.iter().collect()),
                    CellStatus::Missing => ("missing", vec![]),
                    CellStatus::PresentOnOneSide { qpdf_present: true } => ("qpdf-only", vec![]),
                    CellStatus::PresentOnOneSide { qpdf_present: false } => ("flpdf-only", vec![]),
                };
                let divs_json: Vec<Value> = divs.iter().map(|d| {
                    serde_json::json!({"path": d.path, "qpdf": d.qpdf, "flpdf": d.flpdf})
                }).collect();
                serde_json::json!({"key": c.key, "status": status, "divergences": divs_json})
            }).collect();
            serde_json::json!({
                "fixture": f.fixture,
                "qpdf_error": f.qpdf_error,
                "flpdf_error": f.flpdf_error,
                "cells": cells,
            })
        }).collect();
        let (pass, present) = self.overall_pass_rate();
        let payload = serde_json::json!({
            "overall": {"pass_or_known": pass, "present": present},
            "fixtures": fixtures,
            "stale_allowlist": self.stale_allowlist.iter().map(|e| serde_json::json!({
                "fixture": e.fixture, "path": e.path, "category": e.category,
                "beads_ref": e.beads_ref, "reason": e.reason,
            })).collect::<Vec<_>>(),
        });
        serde_json::to_string_pretty(&payload).unwrap()
    }
}
```

**Step 4: Run tests**

Run: `cargo test -p flpdf-cli --test json_schema_diff 2>&1 | tail -20`
Expected: all report tests pass.

**Step 5: Commit**

```bash
git add crates/flpdf-cli/tests/support/json_diff/mod.rs
git commit -m "feat(test/json-diff): Report aggregator with markdown + JSON writers"
```

---

## Task 7: Top-level corpus test (start with minimal.pdf)

**Files:**
- Modify: `crates/flpdf-cli/tests/json_schema_diff.rs`
- Create: `tests/fixtures/json-diff/allowed-divergences.json`

**Step 1: Empty allowlist file**

Create `tests/fixtures/json-diff/allowed-divergences.json`:
```json
{
  "entries": []
}
```

**Step 2: Replace placeholder test**

Rewrite `crates/flpdf-cli/tests/json_schema_diff.rs`:

```rust
//! Integration test: qpdf --json=2 vs flpdf --json=2 schema-diff over a curated
//! fixture corpus (beads flpdf-9hc.11.14).

mod support;

use std::path::{Path, PathBuf};

use support::json_diff::{
    compute_matrix, run_flpdf_json, run_qpdf_json, Allowlist, FixtureResult, Report,
};

struct FixtureSpec {
    label: &'static str,
    relative_path: &'static str,
    password: Option<&'static str>,
}

const CORPUS: &[FixtureSpec] = &[
    FixtureSpec { label: "minimal.pdf", relative_path: "minimal.pdf", password: None },
];

fn workspace_fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn allowlist_path() -> PathBuf {
    workspace_fixtures_root().join("json-diff/allowed-divergences.json")
}

fn report_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/json-diff")
}

#[test]
fn json_schema_diff_corpus() {
    if !support::is_qpdf_available() {
        eprintln!("skipping json_schema_diff_corpus: qpdf not on PATH");
        return;
    }

    let root = workspace_fixtures_root();
    let mut allowlist = Allowlist::from_path(&allowlist_path())
        .expect("allowlist load");

    let mut fixtures = Vec::new();
    for spec in CORPUS {
        let path = root.join(spec.relative_path);
        let qpdf_out = run_qpdf_json(&path, spec.password);
        let flpdf_out = run_flpdf_json(&path, spec.password);

        let (cells, qpdf_error, flpdf_error) = match (qpdf_out, flpdf_out) {
            (Ok(qv), Ok(fv)) => (compute_matrix(spec.label, &qv, &fv, &mut allowlist), None, None),
            (Err(qe), Ok(_)) => (vec![], Some(qe), None),
            (Ok(_), Err(fe)) => (vec![], None, Some(fe)),
            (Err(qe), Err(fe)) => (vec![], Some(qe), Some(fe)),
        };

        fixtures.push(FixtureResult {
            fixture: spec.label.to_string(),
            cells, qpdf_error, flpdf_error,
        });
    }

    let stale = allowlist.stale_entries().into_iter().cloned().collect::<Vec<_>>();
    let report = Report { fixtures, stale_allowlist: stale };

    std::fs::create_dir_all(report_dir()).ok();
    std::fs::write(report_dir().join("report.md"), report.to_markdown()).ok();
    std::fs::write(report_dir().join("report.json"), report.to_json()).ok();

    let invocation_errors: Vec<String> = report.fixtures.iter().flat_map(|f| {
        let mut v = vec![];
        if let Some(e) = &f.qpdf_error { v.push(format!("{}: qpdf: {e}", f.fixture)); }
        if let Some(e) = &f.flpdf_error { v.push(format!("{}: flpdf: {e}", f.fixture)); }
        v
    }).collect();
    assert!(invocation_errors.is_empty(), "tool errors:\n{}", invocation_errors.join("\n"));

    let unknown = report.unknown_divergences();
    assert!(
        unknown.is_empty(),
        "unknown divergences ({}):\n{}\n\nFull report at {}",
        unknown.len(),
        report.unknown_summary(),
        report_dir().join("report.md").display(),
    );

    assert!(
        report.stale_allowlist.is_empty(),
        "stale allowlist entries:\n{}",
        report.stale_summary(),
    );
}
```

**Step 3: Run**

Run: `cargo test -p flpdf-cli --test json_schema_diff json_schema_diff_corpus -- --nocapture 2>&1 | tail -60`

Possible outcomes:
- **Passes**: minimal.pdf has no divergences. Proceed to Task 8.
- **Fails with unknown divergences**: read `target/json-diff/report.md`, classify each into the allowlist, re-run.

**Step 3b: Populate initial allowlist if needed**

For each unknown divergence, decide:
- **flpdf bug** → file a new beads issue, add to allowlist with category `flpdf-bug` (or `flpdf-feature-gap`), reference the new issue ID in `beads_ref`.
- **qpdf-specific quirk** → category `qpdf-quirk`.
- **acceptable** (e.g. numeric formatting) → category `number-formatting`.

Example entry:
```json
{
  "fixture": "minimal.pdf",
  "path": "$.parameters.version",
  "category": "number-formatting",
  "beads_ref": "",
  "reason": "qpdf renders as int, flpdf renders as float"
}
```

Do **not** allowlist correctness bugs without filing a beads issue first.

**Step 4: Inspect report**

```bash
cat target/json-diff/report.md
head -30 target/json-diff/report.json
```

**Step 5: Commit**

```bash
git add crates/flpdf-cli/tests/json_schema_diff.rs \
        tests/fixtures/json-diff/allowed-divergences.json
git commit -m "test(json-diff): corpus runner with minimal.pdf + empty allowlist seed (flpdf-9hc.11.14)"
```

---

## Task 8: Expand fixture corpus

**Files:**
- Modify: `crates/flpdf-cli/tests/json_schema_diff.rs`
- Modify: `tests/fixtures/json-diff/allowed-divergences.json`

**Step 1: Catalogue candidate fixtures**

```bash
ls tests/fixtures/compat/ tests/fixtures/qdf-fix/ tests/fixtures/qdf-golden/ \
   tests/fixtures/qdf-roundtrip/ tests/fixtures/encrypted/ 2>/dev/null
```

Find passwords for encrypted fixtures (search docs + existing tests):
```bash
grep -rE 'password|--password=' tests/fixtures/encrypted/ \
     crates/flpdf-cli/tests/encrypted_rewrite_tests.rs \
     crates/flpdf-cli/tests/cli_encryption_inspect.rs 2>/dev/null | head -30
```

Use only fixtures whose passwords are documented.

**Step 2: Target distribution (~10–15 fixtures)**

- `minimal.pdf` (already in corpus)
- 2 from `compat/` (basic structure + page-tree variant)
- 1–2 from `qdf-fix/` (linearized)
- 1–2 from `qdf-golden/` (AcroForm if present)
- 1–2 from `qdf-roundtrip/` (outline / pagelabel variants)
- 1 RC4 + 1 AES-256 from `encrypted/`

**Step 3: Extend CORPUS**

```rust
const CORPUS: &[FixtureSpec] = &[
    FixtureSpec { label: "minimal.pdf",                  relative_path: "minimal.pdf",                  password: None },
    FixtureSpec { label: "compat/<file>.pdf",            relative_path: "compat/<file>.pdf",            password: None },
    // ...
    FixtureSpec { label: "encrypted/<file>.pdf",         relative_path: "encrypted/<file>.pdf",         password: Some("user") },
];
```

**Step 4: Iterate per fixture**

Add 1–2 fixtures at a time:
1. Run: `cargo test -p flpdf-cli --test json_schema_diff -- --nocapture 2>&1 | tail -40`
2. If unknown divergences appear, classify and add allowlist entries.
3. If a divergence points to a flpdf bug, **file a beads issue first**, then add `beads_ref` to the allowlist entry.
4. Re-run until passing.

**Step 5: Final run**

Run: `cargo test -p flpdf-cli --test json_schema_diff -- --nocapture 2>&1 | tail -40`
Expected: passes. Inspect `target/json-diff/report.md`.

**Step 6: Commit**

```bash
git add crates/flpdf-cli/tests/json_schema_diff.rs \
        tests/fixtures/json-diff/allowed-divergences.json
git commit -m "test(json-diff): expand fixture corpus; seed allowlist for known divergences"
```

If new beads issues were filed, mention IDs in the commit body.

---

## Task 9: Verify, document, close

**Step 1: Full test suite**

Run: `cargo test -p flpdf-cli 2>&1 | tail -20`
Expected: no regressions.

Run: `cargo fmt --check && cargo clippy -p flpdf-cli --tests -- -D warnings 2>&1 | tail -40`
Expected: clean.

**Step 2: Acceptance checklist**

- [ ] `crates/flpdf-cli/tests/json_schema_diff.rs` exists and is invoked by `cargo test`.
- [ ] Corpus contains ≥10 fixtures spanning compat/qdf-fix/qdf-golden/qdf-roundtrip/encrypted.
- [ ] Matrix columns include all 9 qpdf v2 top-level keys.
- [ ] `target/json-diff/report.md` and `target/json-diff/report.json` produced after `cargo test`.
- [ ] Allowlist at `tests/fixtures/json-diff/allowed-divergences.json` (JSON, per Deviations note).
- [ ] Unknown divergences → test fail; stale allowlist entries → test fail.
- [ ] Any flpdf bug found has a beads issue + allowlist `beads_ref`.

**Step 3: Update beads issue notes**

```bash
bd update flpdf-9hc.11.14 --notes "Implemented as Rust integration test in crates/flpdf-cli/tests/json_schema_diff.rs. Support module: tests/support/json_diff/mod.rs. Allowlist: tests/fixtures/json-diff/allowed-divergences.json (JSON, not TOML — see plan deviation). Reports written to target/json-diff/. Corpus = ${N} fixtures covering all 9 qpdf v2 top-level keys."
```

**Step 4: Close beads issue**

```bash
bd close flpdf-9hc.11.14
```

**Step 5: Push (per CLAUDE.md session close protocol)**

```bash
git pull --rebase
bd dolt push
git push
git status
```

Verify `up to date with origin`.

---

## Reference: existing helpers to reuse

- `crates/flpdf-cli/tests/support/mod.rs`:
  - `is_qpdf_available()` — reuse to skip cleanly.
  - `Fixture`, `Comparator`, `RunOutputs`, etc. — not needed; we run tools directly.
- `crates/flpdf-cli/tests/support/comparators.rs`:
  - `QpdfJsonComparator` — example of how to invoke qpdf. Do **not** depend on its hand-rolled JsonValue.
- `crates/flpdf/src/json.rs` and `json_inspect.rs`:
  - The serializer side. Useful when classifying divergences.

## Reference: relevant skills

- @superpowers:test-driven-development — every implementation task follows TDD red-green-refactor.
- @superpowers:verification-before-completion — required before closing the issue.
- @superpowers:executing-plans — used to execute this plan.
