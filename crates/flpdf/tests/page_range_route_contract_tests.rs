#[test]
fn page_range_uses_qutil_as_the_single_parse_and_resolve_owner() {
    let source = include_str!("../src/job/page_range.rs");

    assert!(
        source.contains("crate::qutil::parse_numrange(input.as_bytes(), 0)?;"),
        "PageRange::parse_numrange must delegate syntax validation to qutil::parse_numrange"
    );
    assert!(
        source.contains("crate::qutil::parse_numrange(&self.raw, max)?"),
        "PageRange::resolve must delegate bounded resolution to qutil::parse_numrange"
    );
    for forbidden in [
        "struct RangeParser",
        "fn split_parity_suffix",
        "fn parse_entries",
        "fn resolve_entry",
    ] {
        assert!(
            !source.contains(forbidden),
            "page_range.rs retains a duplicate parser helper: {forbidden}"
        );
    }
}
