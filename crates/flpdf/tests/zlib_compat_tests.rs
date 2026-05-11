//! Pins flate2's backend selection under the `qpdf-zlib-compat` feature.
//!
//! With the feature enabled, flate2 must link against classic libz so that
//! `ZlibEncoder` at `Compression::new(6)` produces output byte-identical to
//! `compress2()`. If flate2 ever silently falls back to miniz_oxide (or a
//! zlib variant with different internals), this test surfaces the divergence
//! immediately instead of at the full-PDF byte-comparison layer where the
//! cause is hard to localize.
//!
//! The reference bytes below were captured on Ubuntu 24.04 with zlib1g
//! 1:1.3.dfsg-3.1ubuntu2.1 and qpdf 11.9.0; if the CI base image changes its
//! libz version the values may need re-capturing.

#![cfg(feature = "qpdf-zlib-compat")]

use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write;

const SAMPLE_UNCOMPRESSED: &[u8] =
    b"1 0 0 1 0 0 cm  BT /F1 12 Tf 14.4 TL ET\nBT 1 0 0 1 72 720 Tm (Fixture page 1) Tj T* ET\n \n";

const QPDF_COMPRESSED_L6: [u8; 82] = [
    0x78, 0x9c, 0x33, 0x54, 0x30, 0x00, 0x42, 0x43, 0x30, 0x99, 0x9c, 0xab, 0xa0, 0xe0, 0x14, 0xa2,
    0xa0, 0xef, 0x66, 0xa8, 0x60, 0x68, 0xa4, 0x10, 0x92, 0xa6, 0x60, 0x68, 0xa2, 0x67, 0xa2, 0x10,
    0xe2, 0xa3, 0xe0, 0x1a, 0xc2, 0x05, 0x14, 0x37, 0x84, 0x2a, 0x35, 0x37, 0x02, 0x22, 0x03, 0x85,
    0x90, 0x5c, 0x05, 0x0d, 0xb7, 0xcc, 0x8a, 0x92, 0xd2, 0xa2, 0x54, 0x85, 0x82, 0xc4, 0xf4, 0x54,
    0x05, 0x43, 0x4d, 0x85, 0x90, 0x2c, 0x85, 0x10, 0x2d, 0x90, 0x72, 0x05, 0x2e, 0x00, 0x4a, 0xfd,
    0x13, 0xe0,
];

#[test]
fn flate2_zlib_feature_matches_qpdf_default_compression() {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(6));
    enc.write_all(SAMPLE_UNCOMPRESSED).unwrap();
    let out = enc.finish().unwrap();
    assert_eq!(
        out.as_slice(),
        &QPDF_COMPRESSED_L6[..],
        "flate2 zlib backend deflate output diverged from captured qpdf reference"
    );
}
