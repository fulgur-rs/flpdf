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
    fn multiple_divergences_collected_in_order() {
        let a = json!({"a": 1, "b": 2, "c": 3});
        let b = json!({"a": 9, "b": 2, "c": 8});
        let d = diff_values(&a, &b);
        assert_eq!(paths(&d), vec!["$.a", "$.c"]);
    }
}
