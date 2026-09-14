//! Route contracts for the page-label A6/A7 accessor cutover.

use std::fs;
use std::path::PathBuf;

fn production_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_label_document_helper.rs");
    let source = fs::read_to_string(path)
        .expect("page_label_document_helper.rs must be readable")
        .replace("\r\n", "\n");
    match source.split_once("\n#[cfg(test)]") {
        Some((production, _)) => production.to_owned(),
        None => source,
    }
}

#[test]
fn production_page_labels_use_canonical_resolving_routes() {
    let production = production_source();
    for forbidden in [".resolve(", ".resolve_handle(", ".resolve_handle_ref("] {
        assert!(
            !production.contains(forbidden),
            "page_label_document_helper production retains non-canonical route {forbidden}"
        );
    }
    assert!(production.contains(".try_dereference()?"));
}

/// `QPDFPageLabelDocumentHelper::getLabelForPage` copies `/P` verbatim without
/// inspecting its type (`QPDFPageLabelDocumentHelper.cc:38,48`), so the typed
/// compatibility view must not raise qpdf's string typeWarning for a
/// non-string prefix. Probed with qpdf 11.9.0 on `/P 42`, `/P /Foo` and
/// `/P [1 2]`: `--pages . 1-2 --` exits 0 with no diagnostics in every case.
#[test]
fn label_prefix_read_stays_on_the_silent_string_accessor() {
    let production = production_source();
    assert!(
        !production.contains(".try_get_string_value()"),
        "the warning-emitting getStringValue port must not read /P"
    );
}

#[test]
fn typed_reconstruction_helpers_are_test_only_after_raw_cutover() {
    let helper_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_label_document_helper.rs");
    let helper = fs::read_to_string(helper_path)
        .expect("page_label_document_helper.rs must be readable")
        .replace("\r\n", "\n");
    for function in [
        "labels_for_page_range",
        "labels_for_selection_with_prefix_presence",
        "label_prefix_is_present",
        "write_reconstructed_labels_with_prefix_presence",
    ] {
        let declaration = format!("#[cfg(test)]\n    pub fn {function}");
        assert!(
            helper.contains(&declaration),
            "typed helper {function} must be compiled only for tests"
        );
    }
    for function in [
        "merge_adjacent_ranges",
        "merge_adjacent_ranges_with_prefix_presence",
    ] {
        assert!(
            !helper.contains(&format!("fn {function}")),
            "obsolete typed merge helper {function} must be removed"
        );
    }

    let lib_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let lib = fs::read_to_string(lib_path)
        .expect("lib.rs must be readable")
        .replace("\r\n", "\n");
    assert!(
        !lib.contains("merge_adjacent_ranges"),
        "typed range merge helpers must not remain crate-root API"
    );

    let production = production_source();
    assert!(
        production.contains("pub fn labels_for_selection")
            && production.contains("self.labels_for_selection_raw(src_indices, out_start_idx)"),
        "public selection labels must project the raw canonical route"
    );
    let selection_body = helper
        .split_once("pub fn labels_for_selection(")
        .and_then(|(_, remainder)| remainder.split_once("\n    /// Batch variant of"))
        .map(|(body, _)| body)
        .expect("labels_for_selection body must be present");
    assert!(
        !selection_body.contains("labels_for_selection_with_prefix_presence("),
        "production selection labels must not call the test-only prefix helper"
    );
}
