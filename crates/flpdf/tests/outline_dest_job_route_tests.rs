use flpdf::{job, Pdf, RebuildResult};
use std::collections::BTreeMap;

#[test]
fn outline_destination_remap_is_owned_by_the_job_boundary() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let root_ref = pdf.root_ref().expect("empty PDF has a root");
    let mut result = RebuildResult::default();
    result.new_kids = Vec::new();
    result.ref_map = BTreeMap::new();
    result.removed_pages = [root_ref].into_iter().collect();

    job::remap_outline_and_dests(&mut pdf, &result).expect("job route should be callable");
    assert!(pdf
        .get_object_handle(root_ref)
        .try_is_null()
        .expect("legacy removed page null state"));
}
