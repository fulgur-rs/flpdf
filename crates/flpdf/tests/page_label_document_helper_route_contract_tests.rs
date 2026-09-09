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
