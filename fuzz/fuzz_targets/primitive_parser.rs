#![no_main]

//! Fuzz the public standalone PDF object parser with arbitrary bytes and
//! direct object bodies extracted from PDF fixture seeds.
//!
//! Malformed input is an expected `Err`. A panic, abort, sanitizer failure,
//! out-of-memory condition, or timeout is a defect.

use flpdf::ObjectHandle;
use libfuzzer_sys::fuzz_target;

const MAX_EMBEDDED_OBJECTS: usize = 64;
const OBJECT_MARKER: &[u8] = b" obj";
const END_OBJECT_MARKER: &[u8] = b"endobj";

fn find_bytes(input: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    input
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn parse_fixture_objects(input: &[u8]) {
    let mut cursor = 0;

    for _ in 0..MAX_EMBEDDED_OBJECTS {
        let Some(marker_start) = find_bytes(input, OBJECT_MARKER, cursor) else {
            return;
        };
        let mut body_start = marker_start + OBJECT_MARKER.len();
        while input
            .get(body_start)
            .copied()
            .is_some_and(is_pdf_whitespace)
        {
            body_start += 1;
        }

        let Some(body) = input.get(body_start..) else {
            return;
        };
        let Some(end_marker_offset) = find_bytes(body, END_OBJECT_MARKER, 0) else {
            let _ = ObjectHandle::parse(body);
            return;
        };
        let end_marker_start = body_start + end_marker_offset;
        let _ = ObjectHandle::parse(&input[body_start..end_marker_start]);
        cursor = end_marker_start.saturating_add(END_OBJECT_MARKER.len());
    }
}

fuzz_target!(|input: &[u8]| {
    // Exercise the standalone-object boundary directly for arbitrary bytes.
    let _ = ObjectHandle::parse(input);

    // Fixture seeds are complete small PDFs. Also parse each indirect object's
    // body so the seed corpus reaches numbers, names, strings, arrays, and
    // dictionaries instead of stopping at the `%PDF` header.
    parse_fixture_objects(input);
});
