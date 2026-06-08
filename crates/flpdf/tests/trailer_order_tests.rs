//! Classic full-rewrite trailer parity with `qpdf --static-id`.
//!
//! qpdf emits the trailer dictionary on the same line as the `trailer` keyword
//! (`trailer << ... >>`), with keys in sorted order but `/ID` forced last:
//! `trailer << /Info 2 0 R /Root 1 0 R /Size N /ID [<..><..>] >>`. flpdf
//! previously wrote `trailer\n<< ... >>` with plain lexicographic keys (`/ID`
//! first). These tests pin the qpdf ordering for the full-rewrite classic-xref
//! path. The deflate-dependent stream bytes and `/ID` value are out of scope.

use flpdf::{write_pdf_with_options, Pdf, WriteOptions};
use std::fs::File;
use std::io::BufReader;

fn full_rewrite_bytes(fixture: &str) -> Vec<u8> {
    let path = format!("../../tests/fixtures/compat/{fixture}");
    let file = File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Return the classic trailer dictionary slice — from the `trailer ` keyword to
/// the `startxref` that follows it — so key-order assertions are scoped to the
/// trailer and cannot be confused by `/ID` / `/Size` occurrences elsewhere in
/// the file (e.g. inside object bodies).
fn trailer_slice(out: &[u8]) -> &[u8] {
    let start = out
        .windows(b"trailer".len())
        .rposition(|w| w == b"trailer")
        .expect("trailer keyword present");
    let after = &out[start..];
    let end = after
        .windows(b"startxref".len())
        .position(|w| w == b"startxref")
        .expect("startxref after trailer");
    &after[..end]
}

#[test]
fn classic_trailer_dict_is_on_the_trailer_line() {
    for fixture in ["one-page.pdf", "two-page.pdf", "three-page.pdf"] {
        let out = full_rewrite_bytes(fixture);
        assert!(
            contains(&out, b"trailer << "),
            "{fixture}: trailer dict must be on the `trailer ` line (qpdf style)"
        );
        assert!(
            !contains(&out, b"trailer\n<<"),
            "{fixture}: trailer dict must not be on its own line"
        );
    }
}

#[test]
fn classic_trailer_keys_sorted_with_id_last() {
    for fixture in ["one-page.pdf", "two-page.pdf", "three-page.pdf"] {
        let out = full_rewrite_bytes(fixture);
        let trailer = trailer_slice(&out);
        // qpdf order for these fixtures: /Info /Root /Size then /ID last.
        assert!(
            contains(trailer, b"trailer << /Info ")
                && contains(trailer, b"/Root ")
                && contains(trailer, b"/Size "),
            "{fixture}: expected sorted /Info /Root /Size leading keys"
        );
        // /ID must appear AFTER /Size (i.e. last), never as the first key —
        // searched within the trailer slice only.
        let pos_id = trailer
            .windows(4)
            .position(|w| w == b"/ID ")
            .expect("trailer /ID present");
        let pos_size = trailer
            .windows(6)
            .position(|w| w == b"/Size ")
            .expect("trailer /Size present");
        assert!(
            pos_id > pos_size,
            "{fixture}: /ID must be emitted after /Size (forced last), got /ID@{pos_id} /Size@{pos_size}"
        );
        assert!(
            !contains(trailer, b"trailer << /ID"),
            "{fixture}: /ID must not be the first trailer key"
        );
    }
}
