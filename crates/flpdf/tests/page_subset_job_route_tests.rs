//! The page-subset resource pass belongs to the qpdf job boundary.

use flpdf::{job::QPDFJob, ObjectRef, Pdf, RemoveUnreferencedResources};

#[test]
fn page_subset_pruning_is_exposed_through_qpdf_job() {
    let mut pdf = Pdf::open_mem_owned(
        include_bytes!("../../../tests/fixtures/compat/unref-resources-one-page.pdf").to_vec(),
    )
    .expect("fixture should parse");

    QPDFJob::prune_after_subset(&mut pdf, RemoveUnreferencedResources::Yes)
        .expect("job page-subset pruning should succeed");

    let page_ref = ObjectRef::new(4, 0);
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page).expect("page should resolve");
    let resources = page.get_key(b"/Resources");
    pdf.resolve(&resources).expect("resources should resolve");
    let fonts = resources.get_key(b"/Font");
    pdf.resolve(&fonts).expect("font dictionary should resolve");
    let entries = fonts.as_dictionary().expect("font dictionary");

    assert!(entries.contains_key(b"/F1".as_slice()));
    assert!(!entries.contains_key(b"/F2".as_slice()));
}
