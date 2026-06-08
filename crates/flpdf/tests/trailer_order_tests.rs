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
    haystack.windows(needle.len()).any(|w| w == needle)
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
        // qpdf order for these fixtures: /Info /Root /Size then /ID last.
        assert!(
            contains(&out, b"trailer << /Info ")
                && contains(&out, b"/Root ")
                && contains(&out, b"/Size "),
            "{fixture}: expected sorted /Info /Root /Size leading keys"
        );
        // /ID must appear AFTER /Size (i.e. last), never as the first key.
        let pos_id = out
            .windows(4)
            .position(|w| w == b"/ID ")
            .expect("trailer /ID present");
        let pos_size = out
            .windows(6)
            .position(|w| w == b"/Size ")
            .expect("trailer /Size present");
        assert!(
            pos_id > pos_size,
            "{fixture}: /ID must be emitted after /Size (forced last), got /ID@{pos_id} /Size@{pos_size}"
        );
        assert!(
            !contains(&out, b"trailer << /ID"),
            "{fixture}: /ID must not be the first trailer key"
        );
    }
}
