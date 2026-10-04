//! Route contracts for the bounded page-object helper A6/A7/A8 cutover.

use std::fs;
use std::path::PathBuf;

fn production_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/page_object_helper.rs");
    let source = fs::read_to_string(path).expect("page_object_helper.rs must be readable");
    source
        .split_once("\n#[cfg(test)]")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

#[test]
fn production_page_object_helper_uses_resolving_accessor_routes() {
    let production = production_source();
    for forbidden in [
        ".resolve(",
        ".resolve_handle(",
        ".resolve_handle_ref(",
        ".get_key(",
        ".has_key(",
    ] {
        assert!(
            !production.contains(forbidden),
            "page_object_helper production retains non-canonical route {forbidden}"
        );
    }
}

#[test]
fn public_page_boxes_remain_qpdf_shaped_raw_handle_accessors() {
    // qpdf exposes getMediaBox/getCropBox/getBleedBox/getTrimBox/getArtBox as
    // raw QPDFObjectHandle values; it does not expose these typed
    // PageObjectHelper projections. Rectangle conversion belongs to
    // QPDFObjectHandle::getArrayAsRectangle instead.
    let production = production_source();
    for qpdf_less in [
        "pub fn media_box(",
        "pub fn crop_box(",
        "pub fn bleed_box(",
        "pub fn trim_box(",
        "pub fn art_box(",
        "fn page_box_from_handle(",
    ] {
        assert!(
            !production.contains(qpdf_less),
            "PageObjectHelper retains qpdf-less typed page-box route {qpdf_less}"
        );
    }
    for qpdf_raw in [
        "pub fn get_media_box(",
        "pub fn get_crop_box(",
        "pub fn get_bleed_box(",
        "pub fn get_trim_box(",
        "pub fn get_art_box(",
    ] {
        assert!(
            production.contains(qpdf_raw),
            "PageObjectHelper lost qpdf raw box route {qpdf_raw}"
        );
    }
}
