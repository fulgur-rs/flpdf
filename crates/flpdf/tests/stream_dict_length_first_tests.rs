//! Stream-dictionary key ordering parity with `qpdf --static-id`.
//!
//! qpdf re-encodes the `[/ASCII85Decode /FlateDecode]` content streams in the
//! compat fixtures down to a single `/FlateDecode` filter and emits the stream
//! dictionary as `<< /Length N /Filter /FlateDecode >>` — `/Length` first, then
//! the regenerated `/Filter`. flpdf's full-rewrite path re-encodes the same
//! streams, so its content-stream dictionaries must use the same ordering for
//! byte-identity. The `/Length` *value* (deflate backend dependent) is out of
//! scope here; only the key ordering is asserted.

use flpdf::{write_pdf_with_options, Pdf, WriteOptions};
use std::fs::File;
use std::io::BufReader;

fn full_rewrite_bytes(fixture: &str) -> Vec<u8> {
    let path = format!("../../tests/fixtures/compat/{fixture}");
    let file = File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true; // compress_streams defaults to Yes (re-filter)
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
    out
}

/// Count non-overlapping byte-substring occurrences.
fn count(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

#[test]
fn one_page_content_stream_dict_emits_length_before_filter() {
    let out = full_rewrite_bytes("one-page.pdf");
    // The re-filtered content stream dict must read `/Length <n> /Filter
    // /FlateDecode` (qpdf order), never the lexicographic `/Filter
    // /FlateDecode /Length`.
    assert_eq!(
        count(&out, b"/Filter /FlateDecode /Length"),
        0,
        "found lexicographic `/Filter /FlateDecode /Length` ordering; \
         expected qpdf `/Length .. /Filter /FlateDecode`"
    );
    assert!(
        count(&out, b"/Length ") >= 1 && count(&out, b" /Filter /FlateDecode >>") >= 1,
        "expected at least one `<< /Length .. /Filter /FlateDecode >>` stream dict"
    );
}

#[test]
fn multi_page_all_content_streams_emit_length_before_filter() {
    for fixture in ["two-page.pdf", "three-page.pdf"] {
        let out = full_rewrite_bytes(fixture);
        assert_eq!(
            count(&out, b"/Filter /FlateDecode /Length"),
            0,
            "{fixture}: lexicographic `/Filter /FlateDecode /Length` ordering leaked"
        );
    }
}

/// A source stream that is ALREADY a lone `/FlateDecode` is preserved by qpdf
/// (not re-filtered), so qpdf keeps lexicographic order with `/Length` LAST:
/// `<< /Filter /FlateDecode /Length N >>`. flpdf must match that ordering for
/// such streams — emitting `/Length` first here would *diverge* from qpdf.
/// (attachment-two-page's content streams were produced by qpdf and are already
/// single-Flate.)
#[test]
fn already_flate_source_keeps_length_last() {
    let out = full_rewrite_bytes("attachment-two-page.pdf");
    assert!(
        count(&out, b"/Filter /FlateDecode /Length ") >= 1,
        "expected qpdf preserve-order `<< /Filter /FlateDecode /Length N >>` for already-Flate source"
    );
    // The re-filtered, /Length-first dict form opens with `<< /Length ` — value
    // independent. Every stream in attachment-two-page is already single-Flate
    // (qpdf-produced) and therefore preserved, so no stream dict may open that
    // way regardless of the deflate-backend-dependent length value.
    assert_eq!(
        count(&out, b"<< /Length "),
        0,
        "already-Flate content stream was wrongly emitted in re-filtered /Length-first order"
    );
}
