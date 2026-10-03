# flpdf patch notes

This directory contains the crates.io `libjpeg-turbo-rs` 0.8.0 source used by
`Cargo.lock` before the local patch. The original registry checksum was
`a0d8a1c652b51dbb85c3c3164b1da63b88dafcc3fc12ecceb52f7577738c21f1`.

The local changes keep the Rust decoder aligned with the libjpeg linked by
qpdf 11.9.0:

- Frame parsing accepts the linked libjpeg limit of ten components, while SOS
  parsing retains the independent four-components-per-scan limit. The SOS
  frame-ID lookup bound matches the qpdf package for each target: first four
  frame slots on Linux/macOS, all ten on Windows.
- Huffman, progressive, and arithmetic DC predictor state is indexed for all
  ten frame components.
- The color-converted `Image` API returns `Unsupported` above four channels
  instead of indexing its fixed Grayscale/RGB/CMYK output arrays; callers
  that need unknown-color-space samples use `decode_raw`.
- A frame above the linked limit reports `Too many color components: N, max
  10`, matching libjpeg's component-count diagnostic.
- SOS length/count failures and component IDs outside the first four frame
  slots preserve libjpeg's `Bogus marker length` and `Invalid component ID N in
  SOS` diagnostics on Linux/macOS. Windows follows its qpdf package's broader
  SOS frame-ID lookup.

The DCT pipeline in flpdf owns qpdf's default output behavior for unknown
color spaces: it interleaves the decoded planes in frame order. The canonical
CLI differential is in `crates/flpdf-cli/tests/dct_component_count_qpdf.rs`.
