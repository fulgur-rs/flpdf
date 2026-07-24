//! qpdf-faithful cross-reference *stream* encoder (ISO 32000-1 §7.5.8).
//!
//! The linearized object-stream output references compressed objects, which can
//! only be addressed from a cross-reference *stream* (not a classic `xref`
//! table). qpdf 11.9.0 emits those streams in a very specific shape, and this
//! module reproduces it byte-for-byte:
//!
//! * the table is `/W [1 2 1]`-style fixed-width rows (type, field-2, field-3),
//! * under the effective compress-streams policy, rows are PNG "Up"
//!   pre-filtered (`/Predictor 12`, `/Columns Σ/W`) and Flate-compressed;
//!   otherwise the raw `/W` rows are emitted without `/Filter` or
//!   `/DecodeParms`, and
//! * the stream dictionary keys are written in qpdf's fixed order
//!   (`/Type /Length /Filter /DecodeParms /W [/Index]`, then sorted trimmed
//!   trailer entries, then `/ID`), which is *not* the lexicographic order the
//!   generic dictionary serializer would produce — so the dictionary is built
//!   directly.
//!
//! Byte-identity of the compressed payload depends on the deflate backend: it
//! matches qpdf only when flate2 links classic zlib (the `qpdf-zlib-compat`
//! feature). The structural encoding (rows, predictor, key order, field widths)
//! is backend-independent.

use std::collections::BTreeMap;
use std::io::Write as _;

use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::object::{Dictionary, Object, ObjectRef};
use crate::Result;

/// One cross-reference stream entry — a single `/W`-formatted row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct XrefStreamEntry {
    /// Field 1, the entry type: 0 (free), 1 (uncompressed), or 2 (compressed).
    pub entry_type: u8,
    /// Field 2 — type-0: next free object number; type-1: byte offset of the
    /// object; type-2: the object-stream container's object number.
    pub field2: u64,
    /// Field 3 — type-0/1: generation number; type-2: index within the
    /// containing object stream.
    pub field3: u64,
}

/// `/W` field widths `[type, field2, field3]` in bytes.
pub(crate) type XrefWidths = [u8; 3];

/// Width of the `/Prev` value field. qpdf 11.9.0 left-justifies the offset in a
/// fixed-width run (observed: a 21-character field) so the value can be
/// back-patched in place once the previous xref offset is known, without
/// shifting any later byte.
pub(crate) const PREV_FIELD_WIDTH: usize = 21;

/// Minimum number of big-endian bytes needed to represent `value` (at least 1).
fn bytes_needed(value: u64) -> u8 {
    let mut width = 1u8;
    let mut remaining = value >> 8;
    while remaining > 0 {
        width += 1;
        remaining >>= 8;
    }
    width
}

/// Total row width (`/Columns`) for the given field widths.
fn columns(widths: XrefWidths) -> usize {
    widths[0] as usize + widths[1] as usize + widths[2] as usize
}

/// Append the low `width` big-endian bytes of `value` to `out`.
fn push_be(out: &mut Vec<u8>, value: u64, width: u8) {
    let bytes = value.to_be_bytes();
    out.extend_from_slice(&bytes[bytes.len() - width as usize..]);
}

/// Build the raw (un-predicted) `/W`-formatted rows: each entry is `Σ/W` bytes,
/// big-endian, one field after another.
fn build_rows(entries: &[XrefStreamEntry], widths: XrefWidths) -> Vec<u8> {
    let mut rows = Vec::with_capacity(entries.len() * columns(widths));
    for entry in entries {
        push_be(&mut rows, u64::from(entry.entry_type), widths[0]);
        push_be(&mut rows, entry.field2, widths[1]);
        push_be(&mut rows, entry.field3, widths[2]);
    }
    rows
}

/// Apply the PNG "Up" predictor (`/Predictor 12`): prefix each `cols`-byte row
/// with the filter-type tag `2` and replace each byte with the difference from
/// the byte directly above (the first row predicts against an all-zero row).
fn png_up_predict(rows: &[u8], cols: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.len() + rows.len() / cols.max(1));
    let mut previous = vec![0u8; cols];
    for row in rows.chunks(cols) {
        out.push(2);
        for (i, &byte) in row.iter().enumerate() {
            out.push(byte.wrapping_sub(previous[i]));
        }
        previous.copy_from_slice(row);
    }
    out
}

/// Flate-compress with zlib at `Compression::default()` (level 6), matching
/// qpdf's `Z_DEFAULT_COMPRESSION`.
fn flate_compress(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .expect("ZlibEncoder::write_all on Vec<u8> cannot fail");
    encoder
        .finish()
        .expect("ZlibEncoder::finish on Vec<u8> cannot fail")
}

/// Encode the cross-reference stream payload: PNG-Up predictor over the
/// `/W`-formatted rows, then Flate. This is the body written between `stream`
/// and `endstream`.
pub(crate) fn encode_payload(entries: &[XrefStreamEntry], widths: XrefWidths) -> Vec<u8> {
    let rows = build_rows(entries, widths);
    flate_compress(&png_up_predict(&rows, columns(widths)))
}

/// Encode the unfiltered cross-reference payload used when qpdf's global
/// stream-compression policy is disabled.
pub(crate) fn encode_payload_raw(entries: &[XrefStreamEntry], widths: XrefWidths) -> Vec<u8> {
    build_rows(entries, widths)
}

/// PNG-Up-predicted rows WITHOUT Flate — qpdf's pass-1 (`skip_compression`) xref
/// stream payload. qpdf still declares `/Filter /FlateDecode` on the pass-1
/// object (an invalid but throwaway buffer used only to size the region and seed
/// the deterministic `/ID`), so the payload is the predictor output alone.
pub(crate) fn encode_payload_uncompressed(
    entries: &[XrefStreamEntry],
    widths: XrefWidths,
) -> Vec<u8> {
    png_up_predict(&build_rows(entries, widths), columns(widths))
}

/// Stream-dictionary metadata for a cross-reference stream, in qpdf key order.
pub(crate) struct XrefStreamDict<'a> {
    /// Whether `/Filter /FlateDecode` plus PNG `/Predictor 12` are declared.
    pub filtered: bool,
    /// `/W` field widths.
    pub widths: XrefWidths,
    /// `/Index [start count]`; `None` omits `/Index` (readers default to
    /// `[0 /Size]`), matching qpdf's main (second-half) xref stream.
    pub index: Option<(u32, u32)>,
    /// `/Info` reference, when present.
    pub info: Option<ObjectRef>,
    /// `/Root` reference, when present (omitted on the main xref stream, which
    /// is reached only via the first-page stream's `/Prev` chain).
    pub root: Option<ObjectRef>,
    /// `/Size` — the highest object number plus one.
    pub size: u32,
    /// `/Prev` byte offset of the previous xref stream (left-justified in a
    /// [`PREV_FIELD_WIDTH`] field); `None` on the chain's final (main) stream.
    pub prev: Option<u64>,
    /// Additional entries from qpdf's trimmed source trailer. These are merged
    /// with `/Info`, `/Root`, and `/Size` and written in sorted key order after
    /// `/W`/`/Index`, before the generated `/ID`.
    pub trailer: Option<&'a Dictionary>,
    /// Trailer `/ID` as two raw byte strings, serialized as `<hex><hex>`.
    pub id: Option<(&'a [u8], &'a [u8])>,
}

/// Append two lowercase hex digits per byte of `bytes` to `out`.
fn push_hex(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0x0f) as usize]);
    }
}

/// Write the xref-stream object header and every dictionary key up to (but not
/// including) `/ID`, in qpdf's fixed order: `/Type /Length /Filter /DecodeParms
/// /W [/Index]`, then the sorted trimmed trailer entries (including generated
/// `/Root` and `/Size`, with `/Prev` immediately after `/Size`). The caller
/// appends `/ID` (concrete or inline-written) and the ` >>\nstream\n…` framing.
fn write_object_dict_prefix(
    out: &mut Vec<u8>,
    object: ObjectRef,
    dict: &XrefStreamDict,
    payload_len: usize,
) {
    out.extend_from_slice(format!("{} {} obj\n", object.number, object.generation).as_bytes());
    out.extend_from_slice(b"<< /Type /XRef");
    out.extend_from_slice(format!(" /Length {payload_len}").as_bytes());
    if dict.filtered {
        out.extend_from_slice(b" /Filter /FlateDecode /DecodeParms << /Columns ");
        out.extend_from_slice(columns(dict.widths).to_string().as_bytes());
        out.extend_from_slice(b" /Predictor 12 >>");
    }
    out.extend_from_slice(
        format!(
            " /W [ {} {} {} ]",
            dict.widths[0], dict.widths[1], dict.widths[2]
        )
        .as_bytes(),
    );
    if let Some((start, count)) = dict.index {
        out.extend_from_slice(format!(" /Index [ {start} {count} ]").as_bytes());
    }
    let mut trailer = dict.trailer.cloned().unwrap_or_default();
    if let Some(info) = dict.info {
        trailer.insert("Info", Object::Reference(info));
    }
    if let Some(root) = dict.root {
        trailer.insert("Root", Object::Reference(root));
    }
    trailer.insert("Size", Object::Integer(i64::from(dict.size)));
    for (key, value) in trailer.iter() {
        out.extend_from_slice(b" /");
        crate::object::write_name_escaped(out, key);
        out.push(b' ');
        value.write_pdf(out);
        if key == b"Size" {
            if let Some(prev) = dict.prev {
                out.extend_from_slice(format!(" /Prev {prev:<PREV_FIELD_WIDTH$}").as_bytes());
            }
        }
    }
}

/// Append the ` >>\nstream\n<payload>\nendstream\nendobj\n` framing that closes a
/// cross-reference stream object (the `\n` before `endstream` is qpdf's fixed
/// xref-stream framing, independent of the `NewlineBeforeEndstream` policy).
fn write_object_framing(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(b" >>\nstream\n");
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");
}

/// Write a complete cross-reference stream indirect object
/// (`<num> 0 obj … endobj\n`) to `out`, with `dict`'s keys in qpdf's fixed order
/// and `payload` as the already-encoded stream body.
pub(crate) fn write_object(
    out: &mut Vec<u8>,
    object: ObjectRef,
    dict: &XrefStreamDict,
    payload: &[u8],
) {
    write_object_dict_prefix(out, object, dict, payload.len());
    if let Some((id0, id1)) = dict.id {
        out.extend_from_slice(b" /ID [<");
        push_hex(out, id0);
        out.extend_from_slice(b"><");
        push_hex(out, id1);
        out.extend_from_slice(b">]");
    }
    write_object_framing(out, payload);
}

/// Like [`write_object`] but writes the trailer `/ID` via `id_writer` at its
/// fixed position (after `/Size`/`/Prev`), so a content-derived deterministic
/// `/ID` can be computed from the bytes written up to the array's `[`. The
/// `id_writer` must emit the full `[<hex0><hex1>]` array value. `dict.id` is
/// ignored. Used by the non-linearized generate writer for `--deterministic-id`
/// (which is not byte-parity with qpdf for xref-stream form, but must be
/// self-stable).
pub(crate) fn write_object_with_id_writer(
    out: &mut Vec<u8>,
    object: ObjectRef,
    dict: &XrefStreamDict,
    payload: &[u8],
    id_writer: &mut dyn FnMut(&mut Vec<u8>),
) {
    write_object_dict_prefix(out, object, dict, payload.len());
    out.extend_from_slice(b" /ID ");
    id_writer(out);
    write_object_framing(out, payload);
}

// ---------------------------------------------------------------------------
// First-pass region sizing (qpdf's two-pass writePad length-stabilisation).
//
// qpdf writes each linearized xref stream twice. The FIRST pass writes it
// uncompressed with a deliberately wide field-2 (forcing 4 bytes per offset),
// then pads the object to a fixed-width region with trailing spaces. The SECOND
// pass writes the real compressed stream and pads with spaces to the SAME region
// end, so the object that follows lands at a position independent of the
// compressed length. These helpers compute that fixed region size.
// ---------------------------------------------------------------------------

/// Worst-case padding qpdf reserves after a first-pass (uncompressed) xref
/// stream so the second-pass compressed stream always fits in the same region:
/// `16 + 5*ceil(xref_bytes / 16384)` (zlib's worst-case expansion plus slack).
/// Mirrors `QPDFWriter::calculateXrefStreamPadding`.
pub(crate) fn calculate_xref_stream_padding(xref_bytes: usize) -> usize {
    16 + 5 * xref_bytes.div_ceil(16384)
}

/// qpdf's first-pass `/W` widths: field 2 is forced wide enough for any offset
/// in the first 4 GB (`max_offset = 1 << 25` ⇒ 4 bytes) so the reserved region
/// is an upper bound on the second pass; field 3 sizes the object-stream index.
/// Mirrors `QPDFWriter::writeXRefStream`'s pass-1 field sizing.
///
/// When `max_ostream_index == 0` (no ObjStm members or only single-member
/// containers where the index is always 0), field 3 is 0 — matching qpdf's
/// behaviour of omitting the generation/index column when all values are 0.
pub(crate) fn first_pass_widths(
    max_id: u32,
    max_ostream_index: u64,
    hint_length: u64,
) -> XrefWidths {
    let f1 = bytes_needed((1u64 << 25) + hint_length).max(bytes_needed(u64::from(max_id)));
    let f3 = if max_ostream_index > 0 {
        bytes_needed(max_ostream_index)
    } else {
        0
    };
    [1, f1, f3]
}

/// PNG-Up-predicted (uncompressed) payload length for `n_entries` rows: each row
/// is one filter-tag byte plus `Σ/W` (`/Columns`) data bytes.
fn first_pass_payload_len(n_entries: usize, widths: XrefWidths, filtered: bool) -> usize {
    let row_width = columns(widths) + usize::from(filtered);
    row_width * n_entries
}

/// Byte length of the fixed region qpdf reserves for a first-pass xref stream:
/// the uncompressed object's own byte length plus
/// [`calculate_xref_stream_padding`]. The caller writes the second-pass
/// compressed object and space-pads it to this length so the next object's
/// offset is pinned. `dict.widths` must be the first-pass (wide) widths; the
/// `/Prev` and `/ID` values are width-only placeholders here.
pub(crate) fn first_pass_region_len(
    object: ObjectRef,
    dict: &XrefStreamDict,
    n_entries: usize,
) -> usize {
    let payload_len = first_pass_payload_len(n_entries, dict.widths, dict.filtered);
    let mut buf = Vec::new();
    write_object(&mut buf, object, dict, &vec![0u8; payload_len]);
    buf.len() + calculate_xref_stream_padding(buf.len())
}

/// qpdf's second-pass `/W` widths for a stream: field 2 holds `max_offset +
/// hint_length` (or the largest object number), field 3 the global maximum
/// object-stream member index. `hint_length` is 0 for the main (second-half)
/// stream and `/H[1]` for the first-page stream (mirrors `writeXRefStream`).
///
/// When `max_ostream_index == 0` (no ObjStm members, or only single-member
/// containers where every index is 0), field 3 is 0 — matching qpdf's
/// behaviour of omitting the generation/index column when all values are 0.
pub(crate) fn second_pass_widths(
    max_offset: u64,
    hint_length: u64,
    max_id: u32,
    max_ostream_index: u64,
) -> XrefWidths {
    let f1 = bytes_needed(max_offset + hint_length).max(bytes_needed(u64::from(max_id)));
    let f3 = if max_ostream_index > 0 {
        bytes_needed(max_ostream_index)
    } else {
        0
    };
    [1, f1, f3]
}

/// Build the cross-reference stream entries for object numbers
/// `start .. start + count` from the offset and compressed-member maps.
///
/// Object 0 is the free-list head (type 0, all-zero — qpdf writes generation 0,
/// not 65535, because the narrow field-3 cannot hold 65535). A number present in
/// `offs` is uncompressed (type 1, byte offset); one present in `member_new` is
/// compressed (type 2, container + index). Any gap falls back to a free entry.
pub(crate) fn build_entries(
    offs: &BTreeMap<u32, usize>,
    member_new: &BTreeMap<u32, (u32, u32)>,
    start: u32,
    count: u32,
) -> Vec<XrefStreamEntry> {
    (start..start + count)
        .map(|number| {
            if number != 0 {
                if let Some(&off) = offs.get(&number) {
                    return XrefStreamEntry {
                        entry_type: 1,
                        field2: off as u64,
                        field3: 0,
                    };
                }
                if let Some(&(container, index)) = member_new.get(&number) {
                    return XrefStreamEntry {
                        entry_type: 2,
                        field2: u64::from(container),
                        field3: u64::from(index),
                    };
                }
            }
            XrefStreamEntry {
                entry_type: 0,
                field2: 0,
                field3: 0,
            }
        })
        .collect()
}

/// Maximum byte offset among a stream's entries (field 2 of its type-1 rows);
/// type-2 rows carry small container numbers, so this is the file-offset
/// magnitude that sizes field 2.
pub(crate) fn max_entry_offset(entries: &[XrefStreamEntry]) -> u64 {
    entries
        .iter()
        .filter(|e| e.entry_type == 1)
        .map(|e| e.field2)
        .max()
        .unwrap_or(0)
}

/// Encode a cross-reference stream object and pad it with trailing spaces to
/// exactly `region_len` bytes (qpdf's pass-2 `writePad`), so the next object
/// lands at a fixed offset regardless of the compressed length.
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if the encoded object already exceeds
/// `region_len` (the reserved region was sized too small — a writer bug).
pub(crate) fn write_padded_region(
    object: ObjectRef,
    dict: &XrefStreamDict,
    payload: &[u8],
    region_len: usize,
) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(region_len);
    write_object(&mut buf, object, dict, payload);
    if buf.len() > region_len {
        return Err(crate::Error::Unsupported(format!(
            "linearized xref stream object ({} bytes) exceeds its reserved region \
             ({region_len} bytes)",
            buf.len()
        )));
    }
    buf.resize(region_len, b' ');
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Object map decoded from the qpdf 11.9.0 golden
    // (`--linearize --object-streams=generate --deterministic-id` of
    // tests/fixtures/compat/three-page.pdf). See the module doc for the shape.

    /// First-half xref stream (obj 7) entries: objects 6..=16. Objects 6-12 are
    /// uncompressed (type 1, byte offsets); 13-16 are members of the ObjStm
    /// container obj 12 (type 2).
    fn three_page_obj7_entries() -> Vec<XrefStreamEntry> {
        let t1 = |off: u64| XrefStreamEntry {
            entry_type: 1,
            field2: off,
            field3: 0,
        };
        let t2 = |idx: u64| XrefStreamEntry {
            entry_type: 2,
            field2: 12,
            field3: idx,
        };
        vec![
            t1(15),
            t1(216),
            t1(608),
            t1(677),
            t1(807),
            t1(1000),
            t1(1153),
            t2(0),
            t2(1),
            t2(2),
            t2(3),
        ]
    }

    /// Main (second-half) xref stream (obj 5) entries: objects 0..=5. Object 0
    /// is the free head; 1-5 are uncompressed (type 1, byte offsets).
    fn three_page_obj5_entries() -> Vec<XrefStreamEntry> {
        let t1 = |off: u64| XrefStreamEntry {
            entry_type: 1,
            field2: off,
            field3: 0,
        };
        vec![
            XrefStreamEntry {
                entry_type: 0,
                field2: 0,
                field3: 0,
            },
            t1(1540),
            t1(1731),
            t1(1883),
            t1(2074),
            t1(2226),
        ]
    }

    const ID0: [u8; 16] = [
        0x31, 0x96, 0x4d, 0xf6, 0xe5, 0xb2, 0x21, 0x18, 0x59, 0xe4, 0xac, 0x9d, 0x6e, 0x86, 0x2d,
        0x1a,
    ];
    const ID1: [u8; 16] = [
        0x60, 0x0e, 0x73, 0x37, 0x15, 0x98, 0x32, 0xba, 0x5a, 0xec, 0x69, 0x83, 0x87, 0xe2, 0xb7,
        0x95,
    ];

    #[test]
    fn calculate_padding_matches_qpdf() {
        // 16 + 5*ceil(n/16384): one 16K block for any small xref stream.
        assert_eq!(calculate_xref_stream_padding(0), 16);
        assert_eq!(calculate_xref_stream_padding(370), 21);
        assert_eq!(calculate_xref_stream_padding(16384), 21);
        assert_eq!(calculate_xref_stream_padding(16385), 26);
    }

    #[test]
    fn first_pass_widths_force_wide_field2() {
        // 1<<25 dominates field 2 (4 bytes); field 3 sizes the objstm index.
        assert_eq!(first_pass_widths(16, 3, 130), [1, 4, 1]);
        // When max_ostream_index is 0 there are no ObjStm members, so W[2] = 0
        // (matching qpdf which omits f3 when no compressed entries exist).
        assert_eq!(first_pass_widths(16, 0, 0), [1, 4, 0]);
    }

    /// The first-pass region size pins where the object after the first-half
    /// xref lands. qpdf 11.9.0 three-page golden: first-half xref (obj 7) at
    /// offset 216, catalog (obj 8) at 608, with a trailing newline outside the
    /// region — so the region is `608 - 216 - 1 = 391` bytes (a 370-byte
    /// uncompressed pass-1 object + 21 bytes of padding).
    #[test]
    fn first_pass_region_matches_three_page_golden() {
        let widths = first_pass_widths(16, 3, 130);
        assert_eq!(first_pass_payload_len(11, widths, true), 77);
        let dict = XrefStreamDict {
            filtered: true,
            widths,
            index: Some((6, 11)),
            info: Some(ObjectRef::new(15, 0)),
            root: Some(ObjectRef::new(8, 0)),
            size: 17,
            // `/Prev` and `/ID` are space-/fixed-width fields, so only their
            // widths (not values) affect the region size.
            prev: Some(2356),
            trailer: None,
            id: Some((&ID0, &ID1)),
        };
        assert_eq!(first_pass_region_len(ObjectRef::new(7, 0), &dict, 11), 391);
    }

    #[test]
    fn second_pass_widths_match_three_page() {
        // first-half stream: max entry offset 1153 + hint 130 = 1283 -> 2 bytes;
        // objstm index 3 -> 1 byte; max id 16 -> 1 byte. => [1 2 1].
        assert_eq!(second_pass_widths(1153, 130, 16, 3), [1, 2, 1]);
        // main stream: max entry offset 2226, hint 0 -> 2 bytes => [1 2 1].
        assert_eq!(second_pass_widths(2226, 0, 16, 3), [1, 2, 1]);
        // a >64 KB offset widens field 2 to 3 bytes.
        assert_eq!(second_pass_widths(70_000, 0, 16, 3), [1, 3, 1]);
    }

    #[test]
    fn build_entries_reproduce_golden_object_maps() {
        // First half (objs 6..16): obj6..12 uncompressed, obj13..16 in container 12.
        let mut offs = BTreeMap::new();
        for (n, off) in [
            (6, 15),
            (7, 216),
            (8, 608),
            (9, 677),
            (10, 807),
            (11, 1000),
            (12, 1153),
        ] {
            offs.insert(n, off);
        }
        let mut members = BTreeMap::new();
        for (n, idx) in [(13, 0u32), (14, 1), (15, 2), (16, 3)] {
            members.insert(n, (12u32, idx));
        }
        assert_eq!(
            build_entries(&offs, &members, 6, 11),
            three_page_obj7_entries()
        );

        // Second half (objs 0..6): obj0 free, obj1..5 uncompressed.
        let mut offs2 = BTreeMap::new();
        for (n, off) in [(1, 1540), (2, 1731), (3, 1883), (4, 2074), (5, 2226)] {
            offs2.insert(n, off);
        }
        assert_eq!(
            build_entries(&offs2, &BTreeMap::new(), 0, 6),
            three_page_obj5_entries()
        );
    }

    #[test]
    fn build_entries_fills_gaps_with_free_entries() {
        // A non-zero number that is neither in `offs` nor `member_new` falls
        // back to a free (type-0) entry. Defensive: a well-formed linearized
        // layout has no such gap, but the encoder must not emit a stale offset.
        let mut offs = BTreeMap::new();
        offs.insert(1u32, 100usize);
        let entries = build_entries(&offs, &BTreeMap::new(), 1, 3);
        assert_eq!(
            entries,
            vec![
                XrefStreamEntry {
                    entry_type: 1,
                    field2: 100,
                    field3: 0
                },
                XrefStreamEntry {
                    entry_type: 0,
                    field2: 0,
                    field3: 0
                },
                XrefStreamEntry {
                    entry_type: 0,
                    field2: 0,
                    field3: 0
                },
            ]
        );
    }

    #[test]
    fn max_entry_offset_ignores_compressed_rows() {
        let entries = three_page_obj7_entries();
        // The container-number field2 of the type-2 rows (12) must not be
        // mistaken for an offset; the max offset is obj12's 1153.
        assert_eq!(max_entry_offset(&entries), 1153);
        assert_eq!(max_entry_offset(&[]), 0);
    }

    #[test]
    fn write_padded_region_pads_to_length() {
        let dict = XrefStreamDict {
            filtered: true,
            widths: [1, 2, 1],
            index: None,
            info: None,
            root: None,
            size: 6,
            prev: None,
            trailer: None,
            id: Some((&ID0, &ID1)),
        };
        let region = write_padded_region(ObjectRef::new(5, 0), &dict, b"PAYLOAD", 400).unwrap();
        assert_eq!(region.len(), 400);
        // The object bytes are intact, followed by ASCII-space padding.
        assert!(region.starts_with(b"5 0 obj\n<< /Type /XRef /Length 7"));
        assert!(region.ends_with(b"   "));
        assert!(region[region.len() - 1] == b' ');
    }

    #[test]
    fn write_padded_region_rejects_oversized_object() {
        let dict = XrefStreamDict {
            filtered: true,
            widths: [1, 2, 1],
            index: None,
            info: None,
            root: None,
            size: 6,
            prev: None,
            trailer: None,
            id: Some((&ID0, &ID1)),
        };
        // A 10-byte region cannot hold the object; the writer must error rather
        // than silently overflow the reserved region.
        let err = write_padded_region(ObjectRef::new(5, 0), &dict, b"PAYLOAD", 10).unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(_)));
    }

    #[test]
    fn bytes_needed_spans_byte_boundaries() {
        assert_eq!(bytes_needed(0), 1);
        assert_eq!(bytes_needed(255), 1);
        assert_eq!(bytes_needed(256), 2);
        assert_eq!(bytes_needed(65_535), 2);
        assert_eq!(bytes_needed(65_536), 3);
    }

    #[test]
    fn build_rows_packs_big_endian_fields() {
        let entries = [
            XrefStreamEntry {
                entry_type: 1,
                field2: 0x0102,
                field3: 0,
            },
            XrefStreamEntry {
                entry_type: 2,
                field2: 12,
                field3: 3,
            },
        ];
        // /W [1 2 1] -> 4 bytes per row.
        assert_eq!(
            build_rows(&entries, [1, 2, 1]),
            vec![0x01, 0x01, 0x02, 0x00, 0x02, 0x00, 0x0c, 0x03]
        );
    }

    /// build_rows + PNG-Up predictor reproduce qpdf's exact pre-compression
    /// bytes (the 55-byte obj7 / 30-byte obj5 predicted streams decoded from the
    /// golden). Backend-independent — no Flate involved.
    #[test]
    fn predictor_reproduces_golden_pre_flate_bytes() {
        let obj7_predicted = "0201000f00020000c900020002880002000045000200018200020000c1\
             0002000199000201fc8b00020000000102000000010200000001";
        let rows = build_rows(&three_page_obj7_entries(), [1, 2, 1]);
        assert_eq!(hex(&png_up_predict(&rows, 4)), obj7_predicted);

        let obj5_predicted = "02000000000201060400020000bf000200019800020001bf000200009800";
        let rows5 = build_rows(&three_page_obj5_entries(), [1, 2, 1]);
        assert_eq!(hex(&png_up_predict(&rows5, 4)), obj5_predicted);
    }

    #[test]
    fn encode_payload_round_trips_through_inflate() {
        // Backend-independent sanity: whatever the deflate flavour, the payload
        // inflates back to the predicted rows.
        let rows = build_rows(&three_page_obj5_entries(), [1, 2, 1]);
        let predicted = png_up_predict(&rows, 4);
        let payload = encode_payload(&three_page_obj5_entries(), [1, 2, 1]);
        let inflated = inflate(&payload);
        assert_eq!(inflated, predicted);
    }

    /// The stream dictionary is emitted in qpdf's fixed key order, including the
    /// `/Prev` fixed-width field and the conditional `/Index` / `/Info` / `/Root`
    /// keys. Backend-independent (only `/Length` depends on the payload).
    #[test]
    fn dict_key_order_matches_qpdf() {
        let mut out = Vec::new();
        write_object(
            &mut out,
            ObjectRef::new(7, 0),
            &XrefStreamDict {
                filtered: true,
                widths: [1, 2, 1],
                index: Some((6, 11)),
                info: Some(ObjectRef::new(15, 0)),
                root: Some(ObjectRef::new(8, 0)),
                size: 17,
                prev: Some(2226),
                trailer: None,
                id: Some((&ID0, &ID1)),
            },
            b"PAYLOAD",
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with(
            "7 0 obj\n<< /Type /XRef /Length 7 /Filter /FlateDecode \
             /DecodeParms << /Columns 4 /Predictor 12 >> /W [ 1 2 1 ] \
             /Index [ 6 11 ] /Info 15 0 R /Root 8 0 R /Size 17 /Prev 2226"
        ));
        // /Prev value left-justified in a 21-char field (17 padding spaces),
        // then the standard inter-key separator: 18 spaces before /ID.
        let after_prev = &text[text.find("/Prev 2226").unwrap() + "/Prev 2226".len()..];
        let spaces = after_prev.chars().take_while(|c| *c == ' ').count();
        assert_eq!(spaces, 18);
        assert!(after_prev[spaces..].starts_with("/ID [<"));
        assert!(text.ends_with(" >>\nstream\nPAYLOAD\nendstream\nendobj\n"));
    }

    #[test]
    fn trimmed_trailer_extras_are_sorted_before_generated_id() {
        let mut trailer = Dictionary::new();
        trailer.insert("Info", Object::Dictionary(Dictionary::new()));
        trailer.insert("Foo", Object::Integer(7));
        let mut out = Vec::new();
        write_object(
            &mut out,
            ObjectRef::new(7, 0),
            &XrefStreamDict {
                filtered: true,
                widths: [1, 2, 1],
                index: None,
                info: None,
                root: Some(ObjectRef::new(1, 0)),
                size: 8,
                prev: None,
                trailer: Some(&trailer),
                id: Some((&ID0, &ID1)),
            },
            b"X",
        );
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("/W [ 1 2 1 ] /Foo 7 /Info << >> /Root 1 0 R /Size 8 /ID [<"),
            "trimmed trailer entries must retain qpdf's sorted writeTrailer order: {text}"
        );
    }

    /// The main xref stream omits /Index, /Info, /Root, and /Prev.
    #[test]
    fn main_xref_dict_omits_chain_and_root_keys() {
        let mut out = Vec::new();
        write_object(
            &mut out,
            ObjectRef::new(5, 0),
            &XrefStreamDict {
                filtered: true,
                widths: [1, 2, 1],
                index: None,
                info: None,
                root: None,
                size: 6,
                prev: None,
                trailer: None,
                id: Some((&ID0, &ID1)),
            },
            b"X",
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("/W [ 1 2 1 ] /Size 6 /ID [<"));
        assert!(!text.contains("/Index"));
        assert!(!text.contains("/Root"));
        assert!(!text.contains("/Prev"));
    }

    // ---- byte-identity vs qpdf 11.9.0 golden (zlib backend only) ----

    #[cfg(feature = "qpdf-zlib-compat")]
    const GOLDEN_OBJ7: &[u8] = b"7 0 obj\n<< /Type /XRef /Length 43 /Filter /FlateDecode \
        /DecodeParms << /Columns 4 /Predictor 12 >> /W [ 1 2 1 ] /Index [ 6 11 ] \
        /Info 15 0 R /Root 8 0 R /Size 17 /Prev 2226                  \
        /ID [<31964df6e5b2211859e4ac9d6e862d1a><600e7337159832ba5aec698387e2b795>] >>\n\
        stream\nx\x9ccbd\xe0g`b`8\t$\x98:@,W \xc1\xd8\x04b\x1d\x04\xb1f201\xfe\xe9\x06q\x19\x18\
        \x11\x04\x00\x98\xa4\x05(\nendstream\nendobj\n";

    #[cfg(feature = "qpdf-zlib-compat")]
    const GOLDEN_OBJ5: &[u8] = b"5 0 obj\n<< /Type /XRef /Length 31 /Filter /FlateDecode \
        /DecodeParms << /Columns 4 /Predictor 12 >> /W [ 1 2 1 ] /Size 6 \
        /ID [<31964df6e5b2211859e4ac9d6e862d1a><600e7337159832ba5aec698387e2b795>] >>\n\
        stream\nx\x9ccb\x00\x02&F6\x16\x06&\x06\x86\xfd@\x82q\x06\x88\x00\xb1\x18f0\x00\x00\
        \x1c7\x02\xc8\nendstream\nendobj\n";

    #[cfg(feature = "qpdf-zlib-compat")]
    #[test]
    fn first_half_xref_object_is_byte_identical_to_qpdf() {
        let payload = encode_payload(&three_page_obj7_entries(), [1, 2, 1]);
        let mut out = Vec::new();
        write_object(
            &mut out,
            ObjectRef::new(7, 0),
            &XrefStreamDict {
                filtered: true,
                widths: [1, 2, 1],
                index: Some((6, 11)),
                info: Some(ObjectRef::new(15, 0)),
                root: Some(ObjectRef::new(8, 0)),
                size: 17,
                prev: Some(2226),
                trailer: None,
                id: Some((&ID0, &ID1)),
            },
            &payload,
        );
        assert_eq!(out, GOLDEN_OBJ7);
    }

    #[cfg(feature = "qpdf-zlib-compat")]
    #[test]
    fn main_xref_object_is_byte_identical_to_qpdf() {
        let payload = encode_payload(&three_page_obj5_entries(), [1, 2, 1]);
        let mut out = Vec::new();
        write_object(
            &mut out,
            ObjectRef::new(5, 0),
            &XrefStreamDict {
                filtered: true,
                widths: [1, 2, 1],
                index: None,
                info: None,
                root: None,
                size: 6,
                prev: None,
                trailer: None,
                id: Some((&ID0, &ID1)),
            },
            &payload,
        );
        assert_eq!(out, GOLDEN_OBJ5);
    }

    // ---- test helpers ----

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    fn inflate(data: &[u8]) -> Vec<u8> {
        use flate2::read::ZlibDecoder;
        use std::io::Read as _;
        let mut out = Vec::new();
        ZlibDecoder::new(data)
            .read_to_end(&mut out)
            .expect("valid zlib payload");
        out
    }
}
