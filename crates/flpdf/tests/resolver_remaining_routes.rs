//! Contract test for the aggregate resolver test-route cutover.

const SELECTED_FUNCTIONS: &[&str] = &[
    "canonical_resolver_decrypts_strings_at_parse_time",
    "pipe_time_rc4_and_aes_streams_match_pinned_qpdf_11_9_0",
    "canonical_info_dictionary",
    "canonical_resolver_warns_once_for_an_unknown_string_filter",
    "unknown_string_filter_warning_sink_failure_propagates",
    "a_vended_handle_reaches_its_documents_resolver_rather_than_reporting_a_dropped_pdf",
    "a_reference_already_being_resolved_takes_the_loop_branch_and_leaves_the_outer_mark",
    "a_resolution_failure_warns_and_resolves_null_without_leaking_in_progress_mark",
    "a_malformed_compressed_class_resolves_to_null_without_the_legacy_route",
    "a_detected_loop_warns_with_qpdfs_message_text",
    "loop_warning_sink_failure_propagates_after_collection",
    "resolver_warnings_and_document_warnings_share_one_ordered_collection",
    "a_long_chain_of_indirect_lengths_grows_the_stack_instead_of_aborting",
    "reconstruct_retry_on_header_mismatch_with_recovery_enabled",
    "assert_generation_replacement_matches_qpdf_tombstone_lifetime",
    "reconstruction_warns_and_resolves_to_null_when_the_detected_header_disappears",
    "a_nested_ap_n_reference_resolves_through_the_owning_document",
];

fn selected_function(source: &str, name: &str) -> String {
    let start = source
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("selected resolver function must exist: {name}"));
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
fn selected_resolver_functions_use_one_canonical_handle_route() {
    let source = include_str!("../src/reader/resolver.rs");

    for name in SELECTED_FUNCTIONS {
        let function = selected_function(source, name);
        let normalized = function.split_whitespace().collect::<String>();

        for forbidden in [
            "try_dereference(",
            "resolve_borrowed(",
            "resolve_object(",
            "resolve_to_terminal(",
            "resolve_chain(",
            "materialize(",
            "Object::",
        ] {
            assert!(
                !normalized.contains(forbidden),
                "selected resolver function {name} still uses legacy route marker {forbidden:?}"
            );
        }
        for required in ["ObjectHandle", "get_object_handle(", "resolve(&"] {
            assert!(
                normalized.contains(required),
                "selected resolver function {name} must retain canonical marker {required:?}"
            );
        }
    }
}
