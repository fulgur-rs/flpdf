use std::fs;
use std::path::PathBuf;

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn function_body<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_at = source.find(start).expect("target function must exist");
    let body = &source[start_at..];
    let end_at = body.find(end).expect("next function boundary must exist");
    &body[..end_at]
}

#[test]
fn attachment_consumers_use_canonical_handle_routes() {
    let src = source_root();
    let attachment_list = fs::read_to_string(src.join("job/attachment_list.rs"))
        .expect("attachment_list.rs must be readable");
    let listing = function_body(
        &attachment_list,
        "pub(crate) fn format_attachment_list_with_sink",
        "struct ListingOutput",
    );
    for forbidden in [
        "pdf.resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !listing.contains(forbidden),
            "attachment listing retains non-canonical route {forbidden}"
        );
    }
    assert!(listing.contains("stream.try_dereference()?"));
    assert!(listing.contains("stream.try_is_null()?"));

    let attachments = fs::read_to_string(src.join("job/attachments.rs"))
        .expect("attachments.rs must be readable");
    let page_mode = function_body(
        &attachments,
        "fn set_attachment_page_mode",
        "/// List embedded files",
    );
    for forbidden in [
        "pdf.resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !page_mode.contains(forbidden),
            "attachment mutation retains non-canonical route {forbidden}"
        );
    }
    assert!(page_mode.contains("pdf.root_handle()?"));
    assert!(page_mode.contains("try_get_key(b\"/PageMode\")?"));
    assert!(page_mode.contains("try_is_null()?"));
}

#[test]
fn internal_job_helpers_are_not_publicly_reexported() {
    let job = fs::read_to_string(source_root().join("job/mod.rs")).expect("job/mod.rs");
    let lib = fs::read_to_string(source_root().join("lib.rs")).expect("lib.rs");
    let prune = fs::read_to_string(source_root().join("job/acroform_field_prune.rs"))
        .expect("acroform_field_prune.rs");
    let listing = fs::read_to_string(source_root().join("job/attachment_list.rs"))
        .expect("attachment_list.rs");

    assert!(
        !job.contains("pub use json::{write_json"),
        "job JSON free writer must be reached through QPDFJob"
    );
    assert!(
        !job.contains("pub use acroform_field_prune::{"),
        "AcroForm free helpers must remain crate-internal"
    );
    assert!(
        !job.contains("pub use attachment_list::{format_attachment_list_with_sink"),
        "attachment sink must remain crate-internal"
    );
    assert!(
        !lib.contains("format_attachment_list_with_sink")
            && !lib.contains("prune_acroform_after_subset"),
        "internal job helpers must not be re-exported from the crate root"
    );
    assert!(
        prune.contains("pub(crate) fn prune_acroform_after_subset<")
            && prune.contains("pub(crate) fn prune_acroform_after_subset_with_max_depth<")
    );
    assert!(listing.contains("pub(crate) fn format_attachment_list_with_sink<R"));
}
