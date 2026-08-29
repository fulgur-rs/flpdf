use flpdf::{job, Pdf, RebuildResult};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn outline_destination_remap_is_owned_by_the_job_boundary() {
    let mut pdf = Pdf::empty().expect("empty PDF");
    let result = RebuildResult {
        new_kids: Vec::new(),
        ref_map: BTreeMap::new(),
        removed_pages: BTreeSet::new(),
    };

    job::remap_outline_and_dests(&mut pdf, &result).expect("job route should be callable");
}
