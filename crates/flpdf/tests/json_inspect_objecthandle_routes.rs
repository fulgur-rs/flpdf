//! Contract tests for the bounded json_inspect ObjectHandle cutover.

const SELECTED_FUNCTIONS: &[&str] = &[
    "object_handle_write_json_keeps_indirect_reference_before_value_dispatch",
    "already_resolved_indirect_child_still_reports_n_g_r",
    "pdf_dest_to_json_dereferences_a_resolved_indirect_array",
];

fn selected_function(source: &str, name: &str) -> String {
    let start = source
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("selected json_inspect function must exist: {name}"));
    let rest = &source[start..];
    let end = [
        rest.find("\n    #[test]"),
        rest.find("\n    fn "),
        rest.find("\n    ///"),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[test]
fn selected_json_inspect_tests_use_canonical_handle_resolution() {
    let source = include_str!("../src/json_inspect.rs");

    for name in SELECTED_FUNCTIONS {
        let function = selected_function(source, name);
        let normalized = function.split_whitespace().collect::<String>();

        for forbidden in [
            "resolve_object(",
            "resolve_borrowed(",
            "set_object(",
            "Object::",
            "materialize(",
        ] {
            assert!(
                !normalized.contains(forbidden),
                "selected json_inspect function {name} still uses legacy route marker {forbidden:?}"
            );
        }
        for required in [
            "ObjectHandle",
            "set_object_handle(",
            "get_object_handle(",
            "resolve(&",
        ] {
            assert!(
                normalized.contains(required),
                "selected json_inspect function {name} must retain canonical marker {required:?}"
            );
        }
    }
}
