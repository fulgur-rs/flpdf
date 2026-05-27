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
use std::path::Path;
use std::process::Command;

/// Invoke `qpdf --json=2 --json-stream-data=none [--password=X] <fixture>`
/// and return the parsed JSON.
///
/// The caller is responsible for checking `super::is_qpdf_available()` first;
/// this helper does not skip on its own.
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

/// Invoke the local `flpdf` binary (via assert_cmd::cargo_bin) with
/// `--json=2 --json-stream-data=none [--password=X] <fixture>` and return the
/// parsed JSON.
///
/// flpdf's clap surface accepts `--json=2` with `require_equals = true` and
/// `--json-stream-data=none`; see `crates/flpdf-cli/src/main.rs` for the flag
/// definitions. Verified by pre-flight check during Task 5 (flpdf-9hc.11.14).
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

#[derive(Debug, Clone, PartialEq)]
pub struct Divergence {
    pub path: String,
    pub qpdf: Value,
    pub flpdf: Value,
}

#[derive(Debug, Clone)]
pub struct AllowlistEntry {
    pub fixture: String,
    pub path: String,
    pub category: String,
    #[allow(dead_code)]
    pub beads_ref: String,
    #[allow(dead_code)]
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
            return Ok(Self {
                entries: vec![],
                matched: vec![],
            });
        }
        let v: Value = serde_json::from_str(s).map_err(|e| format!("allowlist parse: {e}"))?;
        let arr = v
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| "allowlist missing 'entries' array".to_string())?;
        let mut entries = Vec::with_capacity(arr.len());
        for (i, item) in arr.iter().enumerate() {
            let getstr_req = |k: &str| -> Result<String, String> {
                item.get(k)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| format!("allowlist entry {i} missing string field '{k}'"))
            };
            let getstr_opt = |k: &str| -> String {
                item.get(k)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default()
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                entries: vec![],
                matched: vec![],
            }),
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
        self.entries
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.matched[*i])
            .map(|(_, e)| e)
            .collect()
    }
}

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
        format!("{parent}.{:?}", key)
    }
}

fn is_simple_key(k: &str) -> bool {
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn top_level_keys() -> &'static [&'static str] {
    &[
        "acroform",
        "attachments",
        "encrypt",
        "outlines",
        "pagelabels",
        "pages",
        "parameters",
        "qpdf",
        "version",
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
    Known {
        #[allow(dead_code)]
        divergences: Vec<Divergence>,
    },
    /// Subtree present but at least one divergence is not allowlisted.
    Unknown { divergences: Vec<Divergence> },
    /// Key missing in both qpdf and flpdf output.
    Missing,
    /// Key present in only one side — counted as a divergence.
    PresentOnOneSide {
        #[allow(dead_code)]
        qpdf_present: bool,
    },
}

pub fn compute_matrix(
    fixture: &str,
    qpdf: &Value,
    flpdf: &Value,
    allowlist: &mut Allowlist,
) -> Vec<MatrixCell> {
    top_level_keys()
        .iter()
        .map(|&key| {
            let a = qpdf.get(key);
            let b = flpdf.get(key);
            let status = match (a, b) {
                (None, None) => CellStatus::Missing,
                (Some(_), None) => CellStatus::PresentOnOneSide { qpdf_present: true },
                (None, Some(_)) => CellStatus::PresentOnOneSide {
                    qpdf_present: false,
                },
                (Some(av), Some(bv)) => {
                    let raw = diff_values(av, bv);
                    let divs: Vec<Divergence> = raw
                        .into_iter()
                        .map(|d| {
                            let new_path = if d.path == "$" {
                                format!("$.{key}")
                            } else {
                                format!("$.{key}{}", &d.path[1..])
                            };
                            Divergence {
                                path: new_path,
                                ..d
                            }
                        })
                        .collect();

                    if divs.is_empty() {
                        CellStatus::Pass
                    } else {
                        // Drain ALL divergences against the allowlist before classifying, so every
                        // allowlist entry that should match gets its `matched` flag set. Using
                        // Iterator::any would short-circuit on the first unknown and miss later
                        // allowlisted siblings, leading to spurious stale-allowlist failures.
                        let unknown_count = divs
                            .iter()
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
        })
        .collect()
}

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
    fn multiple_divergences_collected() {
        let a = json!({"a": 1, "b": 2, "c": 3});
        let b = json!({"a": 9, "b": 2, "c": 8});
        let d = diff_values(&a, &b);
        let mut got = paths(&d);
        got.sort();
        assert_eq!(got, vec!["$.a", "$.c"]);
    }

    #[test]
    fn object_inside_array_path() {
        let a = json!([{"x": 1}]);
        let b = json!([{"x": 2}]);
        let d = diff_values(&a, &b);
        assert_eq!(paths(&d), vec!["$[0].x"]);
    }

    #[test]
    fn nested_arrays_path() {
        let a = json!([[1, 2]]);
        let b = json!([[1, 9]]);
        let d = diff_values(&a, &b);
        assert_eq!(paths(&d), vec!["$[0][1]"]);
    }

    #[test]
    fn type_mismatch_under_object_key() {
        let a = json!({"x": [1]});
        let b = json!({"x": {"a": 1}});
        let d = diff_values(&a, &b);
        assert_eq!(paths(&d), vec!["$.x"]);
        assert_eq!(d[0].qpdf, json!([1]));
        assert_eq!(d[0].flpdf, json!({"a": 1}));
    }

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
        let div = Divergence {
            path: "$.parameters.version".to_string(),
            qpdf: json!(1),
            flpdf: json!(2),
        };
        let m = al.match_divergence("compat/foo.pdf", &div);
        assert!(m.is_some());
        assert_eq!(m.unwrap().category, "flpdf-feature-gap");
    }

    #[test]
    fn allowlist_no_match_for_different_fixture() {
        let mut al = Allowlist::from_json_str(SAMPLE_ALLOWLIST).unwrap();
        let div = Divergence {
            path: "$.parameters.version".to_string(),
            qpdf: json!(1),
            flpdf: json!(2),
        };
        assert!(al.match_divergence("compat/other.pdf", &div).is_none());
    }

    #[test]
    fn allowlist_no_match_for_different_path() {
        let mut al = Allowlist::from_json_str(SAMPLE_ALLOWLIST).unwrap();
        let div = Divergence {
            path: "$.pages[0].mediabox".to_string(),
            qpdf: json!(1),
            flpdf: json!(2),
        };
        assert!(al.match_divergence("compat/foo.pdf", &div).is_none());
    }

    #[test]
    fn allowlist_tracks_unused_entries() {
        let mut al = Allowlist::from_json_str(SAMPLE_ALLOWLIST).unwrap();
        let stale = al.stale_entries();
        assert_eq!(stale.len(), 1);

        let div = Divergence {
            path: "$.parameters.version".to_string(),
            qpdf: json!(1),
            flpdf: json!(2),
        };
        let _ = al.match_divergence("compat/foo.pdf", &div);
        assert!(al.stale_entries().is_empty());
    }

    const QPDF_V2_KEYS: &[&str] = &[
        "acroform",
        "attachments",
        "encrypt",
        "outlines",
        "pagelabels",
        "pages",
        "parameters",
        "qpdf",
        "version",
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
        let by_key: std::collections::HashMap<_, _> =
            cells.iter().map(|c| (c.key, &c.status)).collect();
        assert!(matches!(
            by_key.get("parameters").unwrap(),
            CellStatus::Pass
        ));
        assert!(matches!(by_key.get("pages").unwrap(), CellStatus::Pass));
        assert!(matches!(
            by_key.get("version").unwrap(),
            CellStatus::Missing
        ));
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

    #[test]
    fn qpdf_returns_object_with_top_level_keys() {
        if !super::super::is_qpdf_available() {
            eprintln!("skipping: qpdf not on PATH");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/minimal.pdf");
        let v = run_qpdf_json(&path, None).expect("qpdf --json=2 on minimal.pdf");
        assert!(v.is_object());
        let obj = v.as_object().unwrap();
        assert!(
            obj.contains_key("parameters"),
            "missing parameters: {:?}",
            obj.keys().collect::<Vec<_>>()
        );
        assert!(
            obj.contains_key("qpdf"),
            "missing 'qpdf' top-level key (qpdf JSON v2 schema): {:?}",
            obj.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn flpdf_returns_object_with_top_level_keys() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/minimal.pdf");
        let v = run_flpdf_json(&path, None).expect("flpdf --json=2 on minimal.pdf");
        assert!(v.is_object());
        assert!(v.as_object().unwrap().contains_key("parameters"));
    }

    #[test]
    fn matrix_marks_later_allowlisted_div_even_when_earlier_is_unknown() {
        // Regression: compute_matrix must visit every divergence in a cell to
        // mark allowlist entries as used. If classification short-circuits on
        // the first unknown, a sibling allowlisted divergence is left unmarked
        // and stale_entries() returns a false positive — which would fail the
        // Task 7 corpus runner's stale_allowlist.is_empty() assertion.
        //
        // Key names a_unknown / z_allowed force alphabetical ordering inside
        // serde_json::Map (a BTreeMap by default) so the unknown appears first.
        let qpdf = json!({"parameters": {"a_unknown": 1, "z_allowed": 2}});
        let flpdf = json!({"parameters": {"a_unknown": 9, "z_allowed": 8}});
        let allowlist_json = r#"{"entries":[{
            "fixture":"f.pdf","path":"$.parameters.z_allowed",
            "category":"x","beads_ref":"","reason":""
        }]}"#;
        let mut al = Allowlist::from_json_str(allowlist_json).unwrap();
        let _ = compute_matrix("f.pdf", &qpdf, &flpdf, &mut al);
        assert!(
            al.stale_entries().is_empty(),
            "allowlist entry must be matched even when a sibling is unknown; stale = {:?}",
            al.stale_entries()
                .iter()
                .map(|e| &e.path)
                .collect::<Vec<_>>(),
        );
    }
}
