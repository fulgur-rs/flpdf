use std::fs;

#[test]
fn appearance_adjustment_uses_the_live_resource_replacer_route() {
    let source = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/overlay_appearance_stream.rs"
    ))
    .expect("read appearance adjustment source");
    let start = source
        .find("pub(crate) fn adjust_appearance_stream_handle")
        .expect("find appearance adjustment function");
    let end = source[start..]
        .find("\nfn canonical_resource_name")
        .map(|offset| start + offset)
        .expect("find appearance adjustment function end");
    let production = &source[start..end];

    assert!(
        production.contains("ResourceReplacer::new"),
        "appearance adjustment must construct the canonical ResourceReplacer"
    );
    assert!(
        production.contains("add_token_filter"),
        "appearance adjustment must attach a live token filter"
    );
    assert!(
        production.contains("parse_as_contents"),
        "appearance adjustment must discover names through the content parser"
    );
    assert!(
        !production.contains("filterable_stream_data("),
        "appearance adjustment must not eagerly decode through a compatibility helper"
    );
    assert!(
        !production.contains("filter_resource_names("),
        "appearance adjustment must not eagerly rewrite decoded bytes"
    );
    assert!(
        !production.contains("encode_stream_data_from_handle("),
        "appearance adjustment must not eagerly re-encode the stream"
    );
}
