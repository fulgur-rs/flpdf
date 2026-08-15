//! Architectural guards for the qpdf-shaped linearization writer route.
//!
//! These assertions are migration contracts, not a replacement for the qpdf
//! differential tests.  The latter prove observable output; this target proves
//! that the migrated QPDFWriter body/stream responsibility is actually routed
//! through the live ObjectHandle writer boundary instead of a legacy bridge.

const WRITER_SOURCE: &str = include_str!("../src/linearization/writer.rs");

fn body_and_stream_production_slice() -> &'static str {
    let start_marker = "fn append_object";
    let end_marker = "fn write_part1_xref_and_trailer";
    let start = WRITER_SOURCE
        .find(start_marker)
        .expect("linearization writer body marker must remain present");
    let end = WRITER_SOURCE
        .find(end_marker)
        .expect("linearization writer public result marker must remain present");
    &WRITER_SOURCE[start..end]
}

#[test]
fn linearization_body_stream_route_has_no_legacy_object_bridge() {
    let production = body_and_stream_production_slice();
    for forbidden in [
        "Object::",
        "resolve_borrowed",
        "decode_stream_data",
        "encode_stream_data",
    ] {
        assert!(
            !production.contains(forbidden),
            "canonical linearization body/stream route still contains legacy token {forbidden:?}"
        );
    }
}
