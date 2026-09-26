use flpdf::{job, pages, rebuild_page_tree, PageDocumentHelper, Pdf};
use std::path::Path;

#[test]
fn outline_destination_remap_is_owned_by_the_job_boundary() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let mut pdf = Pdf::open(std::fs::File::open(fixture).expect("open fixture"))
        .expect("one-page fixture should open");
    let page_ref = pages::page_refs(&mut pdf).expect("one-page fixture has a page tree")[0];

    // Selecting no pages drops every original leaf, driving `RebuildResult`
    // through the same public constructor production callers use, rather
    // than hand-building the struct's raw-identity field directly.
    let result = rebuild_page_tree(&mut pdf, &[]).expect("rebuild with an empty selection");

    job::remap_outline_and_dests(&mut pdf, &result).expect("job route should be callable");
    assert!(pdf
        .get_object_handle(page_ref)
        .try_is_null()
        .expect("removed page null state"));
}

#[test]
fn final_page_removal_nulls_the_removed_page_through_the_job_boundary() {
    // `remove_page` of the last remaining page takes `clear_page_tree`'s
    // empty-tree fast path, which builds its own `RebuildResult` rather than
    // going through the canonical rebuild constructor. That constructor must
    // populate the same raw-identity removed set, or the job boundary's
    // null-out silently skips the page it just dropped.
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let mut pdf = Pdf::open(std::fs::File::open(fixture).expect("open fixture"))
        .expect("one-page fixture should open");
    let page_ref = pages::page_refs(&mut pdf).expect("one-page fixture has a page tree")[0];

    let result = PageDocumentHelper::new(&mut pdf)
        .remove_page(page_ref)
        .expect("removing the only page takes the empty-tree path");

    job::remap_outline_and_dests(&mut pdf, &result).expect("job route should be callable");
    assert!(pdf
        .get_object_handle(page_ref)
        .try_is_null()
        .expect("removed page null state"));
}
