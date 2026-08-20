//! The page-job merge API is owned by `flpdf::job`, matching qpdf's
//! `QPDFJob::handlePageSpecs` responsibility boundary.

use flpdf::{job, MergeInput};
use std::io::Cursor;

#[test]
fn page_merge_public_route_is_owned_by_job_module() {
    let mut inputs: [MergeInput<'_, Cursor<Vec<u8>>>; 0] = [];
    assert!(job::merge_documents(&mut inputs).is_err());
}
