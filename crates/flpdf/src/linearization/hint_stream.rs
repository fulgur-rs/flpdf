//! Hint stream encoder — FlateDecode-compressed binary hint tables.
//!
//! This module takes fully-populated [`PageOffsetHintTable`] and
//! [`SharedObjectHintTable`] values and serialises them into a single
//! zlib-compressed byte sequence that forms the body of the PDF hint stream
//! object (Annex F.4).
//!
//! # Layout of the uncompressed stream (Annex F.4)
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │  Page Offset Hint Table   (Annex F.3.1, bit-packed)     │
//! │  … byte-aligned at start                                 │
//! ├─────────────────────────────────────────────────────────┤  ← /S value
//! │  Shared Object Hint Table (Annex F.3.2, bit-packed)     │
//! │  … byte-aligned at start                                 │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! The byte offset of the Shared Object Hint Table within the **uncompressed**
//! stream is stored as the `/S` key in the hint stream's dictionary.
//!
//! Thumbnail and Outline hint tables (Annex F.3.3, F.3.4) are **not** generated
//! by this implementation.
//!
//! # Bit-packing
//!
//! Fields are packed **MSB-first** (most-significant bit first within each byte),
//! following the direction required by ISO 32000-1 Annex F.  `write_bits(value,
//! n)` takes the `n` least-significant bits of `value` and appends them to the
//! output buffer starting from the most-significant available position in the
//! current byte.
//!
//! # Fixed field widths (Annex F.3.1 / F.3.2 header tables)
//!
//! The hint-table headers use fixed widths that the spec defines per-item:
//!
//! ## Page Offset Header (Annex F.3.1, Table F.4)
//!
//! | Item | Field | Bits |
//! |------|-------|------|
//! | 1 | `first_page_object_number` | 32 |
//! | 2 | `location_of_first_page` | 32 |
//! | 3 | `bits_object_count_delta` | 16 |
//! | 4 | `least_object_count` | 32 |
//! | 5 | `bits_page_length_delta` | 16 |
//! | 6 | `least_page_length` | 32 |
//! | 7 | `bits_shared_object_count` | 16 |
//! | 8 | `bits_shared_object_id` | 16 |
//! | 9 | `bits_numerator` | 16 |
//! | 10 | `denominator` | 16 |
//! | 11 | `bits_content_offset` | 16 |
//! | 12 | `bits_content_length` | 16 |
//!
//! ## Shared Object Header (Annex F.3.2, Table F.9)
//!
//! | Item | Field | Bits |
//! |------|-------|------|
//! | 1 | `first_object_number` | 32 |
//! | 2 | `location` | 32 |
//! | 3 | `first_page_entries` | 32 |
//! | 4 | `section_entries` | 32 |
//! | 5 | `bits_group_object_count` | 16 |
//! | 6 | `least_length` | 32 |
//! | 7 | `bits_length_delta` | 16 |
//!
//! ## Encoding decision (2026-05-10, sub-2.7)
//!
//! 現時点ではバイト互換よりも構造的妥当性 (observable equivalence) を優先する。
//! flate2 デフォルト設定 (Compression::default = level 6) を採用。
//! qpdf とのバイト一致が必要になった場合、qpdf の zlib (level 9, default strategy)
//! を vendoring するか、qpdf 互換 deflate parameter を試すこと。
//! テスト戦略: qpdf --check-linearization (sub-2.11 の round-trip テスト) で構造妥当性を確認する。

use super::hint_page::PageOffsetHintTable;
use super::hint_shared::SharedObjectHintTable;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write;

// ---------------------------------------------------------------------------
// HintStreamBuilder — MSB-first bit-packing buffer
// ---------------------------------------------------------------------------

/// A write-only MSB-first bit-packing buffer.
///
/// Bits are appended from the most-significant position of the current byte
/// downward.  When a byte is full it is flushed to the internal `Vec<u8>`.
///
/// Calling [`HintStreamBuilder::align_to_byte`] pads with zero bits until the
/// current byte boundary is reached.
pub struct HintStreamBuilder {
    buf: Vec<u8>,
    /// Number of bits already written into the *current* (pending) byte.
    /// Range: 0..=7.  When `0` there is no pending byte.
    pending_bits: u32,
    /// The pending byte, partially filled.
    /// The top `pending_bits` bits hold the data written so far;
    /// the remaining `(8 - pending_bits)` low bits are zero.
    pending_byte: u8,
}

impl HintStreamBuilder {
    /// Create a new, empty builder.
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            pending_bits: 0,
            pending_byte: 0,
        }
    }

    /// Append the `bits` least-significant bits of `value`, MSB-first.
    ///
    /// `bits = 0` is a no-op (used for placeholder fields whose bit-width
    /// has not yet been back-patched and is therefore still 0).
    pub fn write_bits(&mut self, value: u64, bits: u32) {
        if bits == 0 {
            return;
        }

        // We iterate from the most significant requested bit to the least,
        // filling `pending_byte` one bit at a time and flushing to `buf`
        // whenever it becomes full.
        //
        // For efficiency we work in chunks: determine how many bits fit into
        // the current pending slot, fill them, flush, then continue.
        //
        // `remaining` tracks how many bits of `value` (from the LSB side,
        // counting the `bits` least-significant bits of `value`) are still
        // to be written.
        let mut remaining = bits;

        while remaining > 0 {
            // How many free bit positions remain in the current pending byte?
            let free = 8 - self.pending_bits;

            if remaining >= free {
                // Fill the remaining `free` positions of the current byte.
                //
                // We need the `free` bits from `value` that correspond to
                // positions [remaining-1 .. remaining-free] (0-indexed from LSB).
                let shift = remaining - free;
                // Extract those `free` bits; mask to `free` bits wide.
                // Use u64 arithmetic throughout to avoid overflow.
                let mask_u64: u64 = if free >= 64 {
                    u64::MAX
                } else {
                    (1u64 << free) - 1
                };
                let chunk = ((value >> shift) & mask_u64) as u8;
                self.pending_byte |= chunk;
                self.buf.push(self.pending_byte);
                self.pending_byte = 0;
                self.pending_bits = 0;
                remaining -= free;
            } else {
                // Remaining bits fit entirely within the current pending byte.
                // Place them in the top `remaining` of the `free` available
                // positions (i.e. shift them left by `free - remaining`).
                let shift = free - remaining;
                let mask_u64: u64 = if remaining >= 64 {
                    u64::MAX
                } else {
                    (1u64 << remaining) - 1
                };
                let chunk = (value & mask_u64) as u8;
                self.pending_byte |= chunk << shift;
                self.pending_bits += remaining;
                remaining = 0;
            }
        }
    }

    /// Pad with zero bits until the current byte boundary.
    ///
    /// If the builder is already on a byte boundary this is a no-op.
    pub fn align_to_byte(&mut self) {
        if self.pending_bits > 0 {
            self.buf.push(self.pending_byte);
            self.pending_byte = 0;
            self.pending_bits = 0;
        }
    }

    /// Consume the builder and return the fully byte-aligned output.
    ///
    /// Calls [`align_to_byte`] before returning.
    pub fn finish(mut self) -> Vec<u8> {
        self.align_to_byte();
        self.buf
    }

    /// Current byte length of already-completed (flushed) bytes.
    ///
    /// Does *not* count a partial pending byte.
    pub fn byte_len(&self) -> usize {
        self.buf.len()
    }
}

impl Default for HintStreamBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// HintStreamBytes — result type
// ---------------------------------------------------------------------------

/// Output of [`encode_hint_stream`].
///
/// Contains both the uncompressed and FlateDecode-compressed forms of the
/// hint stream, plus the byte offset of the Shared Object section within the
/// uncompressed stream (the PDF `/S` key value).
pub struct HintStreamBytes {
    /// The raw, uncompressed bit-packed hint stream.
    pub uncompressed: Vec<u8>,
    /// The FlateDecode-compressed hint stream (zlib/deflate wrapped).
    pub compressed: Vec<u8>,
    /// Byte offset of the Shared Object Hint Table section within
    /// `uncompressed`.  This is the value to write as `/S` in the hint
    /// stream object's dictionary.
    pub shared_section_offset_in_uncompressed: usize,
}

// ---------------------------------------------------------------------------
// Page Offset Hint Table encoder (Annex F.3.1)
// ---------------------------------------------------------------------------

fn encode_page_offset_header(b: &mut HintStreamBuilder, t: &PageOffsetHintTable) {
    let h = &t.header;
    // Items numbered as in Annex F.3.1, Table F.4.
    b.write_bits(h.first_page_object_number as u64, 32); // item 1  (32-bit)
    b.write_bits(h.location_of_first_page, 32); // item 2  (32-bit)
    b.write_bits(h.bits_object_count_delta as u64, 16); // item 3  (16-bit)
    b.write_bits(h.least_object_count as u64, 32); // item 4  (32-bit)
    b.write_bits(h.bits_page_length_delta as u64, 16); // item 5  (16-bit)
    b.write_bits(h.least_page_length, 32); // item 6  (32-bit)
    b.write_bits(h.bits_shared_object_count as u64, 16); // item 7  (16-bit)
    b.write_bits(h.bits_shared_object_id as u64, 16); // item 8  (16-bit)
    b.write_bits(h.bits_numerator as u64, 16); // item 9  (16-bit)
    b.write_bits(h.denominator as u64, 16); // item 10 (16-bit)
    b.write_bits(h.bits_content_offset as u64, 16); // item 11 (16-bit)
    b.write_bits(h.bits_content_length as u64, 16); // item 12 (16-bit)
                                                    // Header total: 4×32 + 8×16 = 128 + 128 = 256 bits = 32 bytes (always aligned).
}

fn encode_page_offset_entries(b: &mut HintStreamBuilder, t: &PageOffsetHintTable) {
    let h = &t.header;
    for entry in &t.entries {
        // Each per-page entry starts on a byte boundary (qpdf convention).
        b.align_to_byte();

        // item 1: object_count_minus_least
        b.write_bits(
            entry.object_count_minus_least as u64,
            h.bits_object_count_delta,
        );
        // item 2: page_length_minus_least
        b.write_bits(entry.page_length_minus_least, h.bits_page_length_delta);
        // item 3: shared_object_count
        b.write_bits(entry.shared_object_count as u64, h.bits_shared_object_count);
        // item 4: shared_object_ids (one per shared object reference)
        for &id in &entry.shared_object_ids {
            b.write_bits(id as u64, h.bits_shared_object_id);
        }
        // item 5: shared_object_numerators (one per shared object reference)
        for &num in &entry.shared_object_numerators {
            b.write_bits(num as u64, h.bits_numerator);
        }
        // item 6: content_stream_offset
        b.write_bits(entry.content_stream_offset, h.bits_content_offset);
        // item 7: content_stream_length
        b.write_bits(entry.content_stream_length, h.bits_content_length);
    }
}

// ---------------------------------------------------------------------------
// Shared Object Hint Table encoder (Annex F.3.2)
// ---------------------------------------------------------------------------

fn encode_shared_object_header(b: &mut HintStreamBuilder, t: &SharedObjectHintTable) {
    let h = &t.header;
    // Items numbered as in Annex F.3.2, Table F.9.
    b.write_bits(h.first_object_number as u64, 32); // item 1  (32-bit)
    b.write_bits(h.location, 32); // item 2  (32-bit)
    b.write_bits(h.first_page_entries as u64, 32); // item 3  (32-bit)
    b.write_bits(h.section_entries as u64, 32); // item 4  (32-bit)
    b.write_bits(h.bits_group_object_count as u64, 16); // item 5  (16-bit)
    b.write_bits(h.least_length, 32); // item 6  (32-bit)
    b.write_bits(h.bits_length_delta as u64, 16); // item 7  (16-bit)
                                                  // Header total: 5×32 + 2×16 = 160 + 32 = 192 bits = 24 bytes (always aligned).
}

fn encode_shared_object_groups(b: &mut HintStreamBuilder, t: &SharedObjectHintTable) {
    let h = &t.header;
    for group in &t.groups {
        b.write_bits(group.object_count as u64, h.bits_group_object_count);
    }
}

fn encode_shared_object_entries(b: &mut HintStreamBuilder, t: &SharedObjectHintTable) {
    let h = &t.header;
    for entry in &t.objects {
        // Each per-object entry starts on a byte boundary (qpdf convention).
        b.align_to_byte();

        // item 1: signature_present flag (1 bit)
        b.write_bits(if entry.signature_present { 1 } else { 0 }, 1);
        // item 2: length_minus_least
        b.write_bits(entry.length_minus_least as u64, h.bits_length_delta);
        // item 3: MD5 signature (16 bytes) — emitted only when signature_present is true
        if entry.signature_present {
            if let Some(sig) = &entry.signature {
                for &byte in sig {
                    b.write_bits(byte as u64, 8);
                }
            }
        }
        // item 4: group_offset
        // The spec does not assign a fixed bit width for the group_offset field
        // beyond saying it is a non-negative integer.  qpdf encodes it as 32 bits.
        b.write_bits(entry.group_offset as u64, 32);
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Encode a Page Offset Hint Table and a Shared Object Hint Table into a
/// single hint stream, compressed with FlateDecode (zlib deflate).
///
/// Returns [`HintStreamBytes`] which contains:
/// - the raw uncompressed bit-packed stream (`uncompressed`)
/// - the FlateDecode-compressed form (`compressed`)
/// - the byte offset of the Shared Object section within `uncompressed`
///   (the `/S` value for the hint stream dictionary)
///
/// # Encoding details
///
/// - Bit order: MSB-first (most-significant bit first within each byte)
/// - Section alignment: each hint-table section begins on a byte boundary
/// - Per-entry alignment: each per-page and per-object entry begins on a byte boundary
/// - Compression: `flate2::ZlibEncoder` with `Compression::default()` (level 6)
///
/// # Placeholder fields
///
/// Fields that have not yet been back-patched (sub-task 2.9) are encoded as
/// `0`.  For fields whose bit-width is `0` (e.g. `bits_page_length_delta = 0`
/// while lengths are still placeholder), `write_bits(value, 0)` is a no-op;
/// those bits are simply absent until the stream is regenerated after
/// back-patching.
pub fn encode_hint_stream(
    page_offset: &PageOffsetHintTable,
    shared_object: &SharedObjectHintTable,
) -> HintStreamBytes {
    let mut builder = HintStreamBuilder::new();

    // -----------------------------------------------------------------------
    // Section 1: Page Offset Hint Table (starts at byte offset 0)
    // -----------------------------------------------------------------------
    encode_page_offset_header(&mut builder, page_offset);
    // The 12-field header is exactly 256 bits = 32 bytes (already aligned).
    // We call align_to_byte for defensive correctness in case future changes
    // alter the field widths.
    builder.align_to_byte();
    encode_page_offset_entries(&mut builder, page_offset);
    // Ensure byte alignment before recording the shared section offset.
    builder.align_to_byte();

    // -----------------------------------------------------------------------
    // Section 2: Shared Object Hint Table — record its byte offset as /S
    // -----------------------------------------------------------------------
    let shared_section_offset = builder.byte_len();

    encode_shared_object_header(&mut builder, shared_object);
    builder.align_to_byte();
    encode_shared_object_groups(&mut builder, shared_object);
    builder.align_to_byte();
    encode_shared_object_entries(&mut builder, shared_object);
    builder.align_to_byte();

    // -----------------------------------------------------------------------
    // Finalise uncompressed stream
    // -----------------------------------------------------------------------
    let uncompressed = builder.finish();

    // -----------------------------------------------------------------------
    // Compress with FlateDecode (zlib, level 6 = Compression::default())
    //
    // ## Encoding decision (2026-05-10)
    // We use flate2 default settings (level 6).  qpdf uses zlib level 9.
    // These produce structurally identical streams (qpdf --check-linearization
    // validates the uncompressed content, not the compressed bytes), but the
    // compressed byte sequences differ.  Byte-identical output requires either
    // matching qpdf's zlib parameters or vendoring qpdf's zlib.  This is
    // deferred; observable equivalence is the current acceptance criterion.
    // -----------------------------------------------------------------------
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&uncompressed)
        .expect("ZlibEncoder::write_all on Vec<u8> must not fail");
    let compressed = encoder
        .finish()
        .expect("ZlibEncoder::finish on Vec<u8> must not fail");

    HintStreamBytes {
        uncompressed,
        compressed,
        shared_section_offset_in_uncompressed: shared_section_offset,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::hint_page::PageOffsetHintTable;
    use crate::linearization::hint_shared::SharedObjectHintTable;
    use crate::linearization::plan::{LinearizationPlan, PageHintEntry, SharedObjectHintEntry};
    use crate::linearization::renumber::RenumberMap;
    use crate::ObjectRef;

    // -----------------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------------

    /// Single-page plan with no shared objects.
    fn single_page_plan() -> LinearizationPlan {
        LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![
                ObjectRef::new(3, 0),
                ObjectRef::new(2, 0),
                ObjectRef::new(1, 0),
            ],
            part3_objects: vec![],
            part4_objects: vec![],
            total_object_count: 3,
            root_ref: None,
            page_hints: vec![PageHintEntry {
                page_ref: ObjectRef::new(3, 0),
                first_object_index: 0,
                object_count: 3,
                byte_length: 0,
            }],
            shared_hints: vec![],
        }
    }

    /// Two-page plan with two shared objects referenced by both pages.
    fn two_page_plan_with_shared() -> LinearizationPlan {
        LinearizationPlan {
            part1_objects: vec![],
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(6, 0)],
            part3_objects: vec![ObjectRef::new(5, 0), ObjectRef::new(8, 0)],
            part4_objects: vec![ObjectRef::new(4, 0), ObjectRef::new(7, 0)],
            total_object_count: 8,
            root_ref: None,
            page_hints: vec![
                PageHintEntry {
                    page_ref: ObjectRef::new(3, 0),
                    first_object_index: 0,
                    object_count: 3,
                    byte_length: 0,
                },
                PageHintEntry {
                    page_ref: ObjectRef::new(4, 0),
                    first_object_index: 0,
                    object_count: 5,
                    byte_length: 0,
                },
            ],
            shared_hints: vec![
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(5, 0),
                    referencing_pages: vec![0, 1],
                },
                SharedObjectHintEntry {
                    object_ref: ObjectRef::new(8, 0),
                    referencing_pages: vec![0, 1],
                },
            ],
        }
    }

    // -----------------------------------------------------------------------
    // HintStreamBuilder unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn builder_write_zero_bits_is_noop() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(0xFF, 0); // should not change state
        let out = b.finish();
        assert!(out.is_empty(), "write_bits(x, 0) must be a no-op");
    }

    #[test]
    fn builder_write_8_bits_produces_one_byte() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(0xAB, 8);
        let out = b.finish();
        assert_eq!(out, vec![0xAB]);
    }

    #[test]
    fn builder_write_16_bits_produces_two_bytes() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(0xABCD, 16);
        let out = b.finish();
        assert_eq!(out, vec![0xAB, 0xCD]);
    }

    #[test]
    fn builder_write_4_plus_4_bits() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(0b1010, 4); // upper nibble = 0xA
        b.write_bits(0b0101, 4); // lower nibble = 0x5
        let out = b.finish();
        assert_eq!(out, vec![0xA5]);
    }

    #[test]
    fn builder_write_1_bit_msb_first() {
        let mut b = HintStreamBuilder::new();
        // Write bits: 1 0 1 0 0 0 0 0 → 0b1010_0000 = 0xA0
        b.write_bits(1, 1);
        b.write_bits(0, 1);
        b.write_bits(1, 1);
        b.write_bits(0, 1);
        b.write_bits(0, 1);
        b.write_bits(0, 1);
        b.write_bits(0, 1);
        b.write_bits(0, 1);
        let out = b.finish();
        assert_eq!(out, vec![0xA0]);
    }

    #[test]
    fn builder_align_to_byte_pads_with_zeros() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(1, 1); // 1 bit written → top bit = 1 → 0b1000_0000 = 0x80
        b.align_to_byte();
        let out = b.finish();
        assert_eq!(out, vec![0x80]);
    }

    #[test]
    fn builder_align_on_boundary_is_noop() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(0xAB, 8); // exactly one byte — already on boundary
        b.align_to_byte(); // should NOT add an extra byte
        let out = b.finish();
        assert_eq!(out, vec![0xAB]);
    }

    #[test]
    fn builder_cross_byte_boundary() {
        // Write 12 bits: 0b1100_1010_0011
        //   First byte:  1100_1010 = 0xCA
        //   Second byte: 0011_xxxx → 0011_0000 = 0x30 (padded with zeros on finish)
        let mut b = HintStreamBuilder::new();
        b.write_bits(0b1100_1010_0011, 12);
        let out = b.finish();
        assert_eq!(out, vec![0xCA, 0x30]);
    }

    #[test]
    fn builder_write_32_bits() {
        let mut b = HintStreamBuilder::new();
        b.write_bits(0xDEAD_BEEF, 32);
        let out = b.finish();
        assert_eq!(out, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn builder_sequence_3_bits_then_5_bits() {
        // 3 bits: 0b101  → occupies top 3 bits of byte: 101x_xxxx
        // 5 bits: 0b10110 → fills remaining 5 bits:      101_10110 = 0xB6
        let mut b = HintStreamBuilder::new();
        b.write_bits(0b101, 3);
        b.write_bits(0b10110, 5);
        let out = b.finish();
        assert_eq!(out, vec![0b1011_0110]); // 0xB6
    }

    // -----------------------------------------------------------------------
    // encode_hint_stream: single-page fixture
    // -----------------------------------------------------------------------

    #[test]
    fn single_page_encode_does_not_panic() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let _ = encode_hint_stream(&po, &so); // must not panic
    }

    #[test]
    fn single_page_shared_section_offset_is_positive() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let result = encode_hint_stream(&po, &so);
        assert!(
            result.shared_section_offset_in_uncompressed > 0,
            "shared section must start after the page offset section"
        );
    }

    #[test]
    fn single_page_compressed_starts_with_zlib_header() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let result = encode_hint_stream(&po, &so);
        assert!(
            result.compressed.len() >= 2,
            "compressed output must have at least 2 bytes (zlib header)"
        );
        // zlib CMF byte: 0x78 (deflate, window size 32 KB).
        // FLG byte encodes level: 0x9C (level 1-6), 0xDA (level 7-9), 0x01 (fastest).
        assert_eq!(
            result.compressed[0], 0x78,
            "first byte must be 0x78 (zlib CMF)"
        );
        assert!(
            result.compressed[1] == 0x9C
                || result.compressed[1] == 0xDA
                || result.compressed[1] == 0x01,
            "second byte must be a valid zlib FLG (0x9C, 0xDA, or 0x01), got 0x{:02X}",
            result.compressed[1]
        );
    }

    #[test]
    fn single_page_shared_section_within_uncompressed() {
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let result = encode_hint_stream(&po, &so);
        assert!(
            result.shared_section_offset_in_uncompressed <= result.uncompressed.len(),
            "shared section offset must be within the uncompressed stream"
        );
    }

    // -----------------------------------------------------------------------
    // encode_hint_stream: two-page with shared objects
    // -----------------------------------------------------------------------

    #[test]
    fn two_page_encode_does_not_panic() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let _ = encode_hint_stream(&po, &so);
    }

    #[test]
    fn two_page_shared_section_offset_is_positive() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let result = encode_hint_stream(&po, &so);
        assert!(result.shared_section_offset_in_uncompressed > 0);
    }

    #[test]
    fn two_page_compressed_starts_with_zlib_header() {
        let plan = two_page_plan_with_shared();
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let result = encode_hint_stream(&po, &so);
        assert!(result.compressed.len() >= 2);
        assert_eq!(result.compressed[0], 0x78);
    }

    #[test]
    fn two_page_uncompressed_larger_than_single_page() {
        let single_plan = single_page_plan();
        let renumber_s = RenumberMap::from_plan(&single_plan);
        let po_s = PageOffsetHintTable::from_plan(&single_plan, &renumber_s);
        let so_s = SharedObjectHintTable::from_plan(&single_plan, &renumber_s);
        let single_result = encode_hint_stream(&po_s, &so_s);

        let two_plan = two_page_plan_with_shared();
        let renumber_t = RenumberMap::from_plan(&two_plan);
        let po_t = PageOffsetHintTable::from_plan(&two_plan, &renumber_t);
        let so_t = SharedObjectHintTable::from_plan(&two_plan, &renumber_t);
        let two_result = encode_hint_stream(&po_t, &so_t);

        assert!(
            two_result.uncompressed.len() > single_result.uncompressed.len(),
            "two-page plan with shared objects must produce a larger stream"
        );
    }

    // -----------------------------------------------------------------------
    // encode_hint_stream: degenerate (no shared objects)
    // -----------------------------------------------------------------------

    #[test]
    fn no_shared_encode_succeeds() {
        let plan = single_page_plan(); // has no shared objects
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let result = encode_hint_stream(&po, &so);
        // Even with no shared objects the shared section header is emitted,
        // so shared_section_offset_in_uncompressed must be > 0.
        assert!(result.shared_section_offset_in_uncompressed > 0);
        assert!(!result.uncompressed.is_empty());
        assert!(!result.compressed.is_empty());
    }

    // -----------------------------------------------------------------------
    // Page Offset Header fixed size check (32 bytes with placeholder data)
    // -----------------------------------------------------------------------

    #[test]
    fn page_offset_header_size_is_32_bytes() {
        // The header has:
        //   4 × 32-bit fields = 128 bits
        //   8 × 16-bit fields = 128 bits
        //   Total = 256 bits = 32 bytes
        //
        // A single-page plan with all bit-widths = 0 (placeholders):
        //   - Header: 32 bytes
        //   - Entries: 1 entry, byte-aligned start (noop since header already aligned),
        //     then 0-bit fields → 0 bytes emitted in entries
        //   - align_to_byte after entries: noop
        //   → shared section starts at byte 32
        let plan = single_page_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let result = encode_hint_stream(&po, &so);

        assert_eq!(
            result.shared_section_offset_in_uncompressed, 32,
            "single-page plan with all-zero bit widths: shared section must start at byte 32"
        );
    }

    // -----------------------------------------------------------------------
    // Shared Object Header fixed size check (24 bytes)
    // -----------------------------------------------------------------------

    #[test]
    fn shared_object_header_total_size() {
        // The shared object header has:
        //   5 × 32-bit fields = 160 bits
        //   2 × 16-bit fields = 32 bits
        //   Total = 192 bits = 24 bytes
        //
        // With no shared objects (degenerate), no groups or object entries
        // are emitted.  So the total uncompressed length is:
        //   32 (page offset header) + 24 (shared object header) = 56 bytes.
        let plan = single_page_plan(); // no shared objects
        let renumber = RenumberMap::from_plan(&plan);
        let po = PageOffsetHintTable::from_plan(&plan, &renumber);
        let so = SharedObjectHintTable::from_plan(&plan, &renumber);
        let result = encode_hint_stream(&po, &so);

        assert_eq!(
            result.uncompressed.len(),
            56,
            "single-page no-shared plan: uncompressed stream must be 56 bytes (32 + 24)"
        );
    }
}
