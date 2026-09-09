//! Error classification for `/Root` and `/Pages` at the page-enumeration entry.
//!
//! qpdf reads the catalog through `QPDF::getRoot`, which throws
//! `unable to find /Root dictionary` for **any** non-dictionary `/Root`,
//! including an indirect reference to a free or missing object
//! (`libqpdf/QPDF.cc:2354-2360`). `QPDF::getAllPages` then reads `/Pages`
//! with no null check at all and enumerates nothing when the value carries no
//! `/Kids` (`libqpdf/QPDF_pages.cc:46,68-72`).
//!
//! So a presence pre-check at this boundary may only reject a *directly*
//! absent entry; resolving an indirect one here would preempt qpdf's own
//! classification.

use flpdf::{pages, Pdf};
use std::collections::BTreeMap;

fn build(trailer_extra: &str, objects: &[(u32, &str)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = BTreeMap::new();
    for (number, body) in objects {
        offsets.insert(*number, out.len());
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len();
    let size = offsets.keys().max().expect("at least one object") + 1;
    out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    for (number, offset) in &offsets {
        out.extend_from_slice(format!("{number} 1\n{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} {trailer_extra} >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

fn page_refs_result(bytes: Vec<u8>) -> Result<usize, String> {
    let mut pdf = Pdf::open_mem_owned(bytes).map_err(|error| format!("open: {error}"))?;
    pages::page_refs(&mut pdf)
        .map(|refs| refs.len())
        .map_err(|error| error.to_string())
}

/// An indirect `/Root` pointing at a missing object resolves to null, but qpdf
/// still reports it through `getRoot`, not as an absent entry.
#[test]
fn indirect_root_resolving_to_null_reports_the_qpdf_root_error() {
    let bytes = build(
        "/Root 9 0 R",
        &[(1, "<< /Type /Pages /Kids [] /Count 0 >>")],
    );
    assert_eq!(
        page_refs_result(bytes),
        Err("unable to find /Root dictionary".to_owned())
    );
}

/// The same holds when the indirect `/Root` resolves to a non-dictionary that
/// is not null.
#[test]
fn indirect_root_resolving_to_a_non_dictionary_reports_the_qpdf_root_error() {
    let bytes = build(
        "/Root 9 0 R",
        &[(1, "<< /Type /Pages /Kids [] /Count 0 >>"), (9, "42")],
    );
    assert_eq!(
        page_refs_result(bytes),
        Err("unable to find /Root dictionary".to_owned())
    );
}

/// A trailer with no `/Root` at all is the one shape this boundary rejects
/// itself.
#[test]
fn absent_root_reports_a_missing_entry() {
    let bytes = build("", &[(1, "<< /Type /Pages /Kids [] /Count 0 >>")]);
    assert_eq!(
        page_refs_result(bytes),
        Err("missing required PDF entry: /Root".to_owned())
    );
}

/// An indirect `/Pages` resolving to null enumerates zero pages, matching
/// `getAllPages`' `hasKey("/Kids")` gate rather than raising an error.
#[test]
fn indirect_pages_resolving_to_null_enumerates_no_pages() {
    let bytes = build("/Root 1 0 R", &[(1, "<< /Type /Catalog /Pages 9 0 R >>")]);
    assert_eq!(page_refs_result(bytes), Ok(0));
}

/// A directly null `/Pages` is still rejected as an absent entry.
#[test]
fn direct_null_pages_reports_a_missing_entry() {
    let bytes = build("/Root 1 0 R", &[(1, "<< /Type /Catalog /Pages null >>")]);
    assert_eq!(
        page_refs_result(bytes),
        Err("missing required PDF entry: /Pages".to_owned())
    );
}
