//! qpdf correspondence: QPDFWriter.cc shared object, stream, trailer, and xref serialization primitives.
use super::{object_streams, CompressStreams, NewlineBeforeEndstream, ObjectWriterEmission};
use crate::ObjectHandle;

/// Write a PDF stream to `out`, applying the [`NewlineBeforeEndstream`] policy.
///
/// The emitted layout is
///
/// ```text
/// <stream-dict>\nstream\n<payload><EOL>endstream
/// ```
///
/// where `<payload>` is the raw `stream.data` byte sequence and `<EOL>` is:
///
/// - [`NewlineBeforeEndstream::Yes`]: always one `b'\n'`; and
/// - [`NewlineBeforeEndstream::Never`]: no byte, except that QDF callers use
///   qpdf's separate `qdf_mode && last_char != '\n'` rule in the internal
///   framing helper.
///
/// # `/Length` invariant
///
/// This helper emits the stream dictionary unchanged. Callers must set
/// `/Length` to `stream.data.len()`: the raw payload byte count only. Any
/// framing LF inserted before `endstream` is not part of `/Length`.
pub fn write_stream_to_buf(
    out: &mut Vec<u8>,
    stream: &ObjectHandle,
    policy: NewlineBeforeEndstream,
) -> crate::Result<()> {
    stream.write_stream_body(out, false)?;
    let data = stream.get_raw_stream_data()?;
    write_stream_payload(out, &data, policy);
    Ok(())
}

/// Emit stream framing after its dictionary has already been written.
pub(crate) fn write_stream_payload(out: &mut Vec<u8>, data: &[u8], policy: NewlineBeforeEndstream) {
    write_stream_payload_with_qdf(out, data, policy, false);
}

/// Emit stream framing with qpdf's QDF-specific conditional newline rule.
pub(crate) fn write_stream_payload_with_qdf(
    out: &mut Vec<u8>,
    data: &[u8],
    policy: NewlineBeforeEndstream,
    qdf_mode: bool,
) {
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(data);
    if framing_adds_newline_with_qdf(data, policy, qdf_mode) {
        out.push(b'\n');
    }
    out.extend_from_slice(b"endstream");
}

/// Whether stream framing adds one LF, including qpdf's QDF-only rule.
pub(crate) fn framing_adds_newline_with_qdf(
    data: &[u8],
    policy: NewlineBeforeEndstream,
    qdf_mode: bool,
) -> bool {
    match policy {
        NewlineBeforeEndstream::Yes => true,
        NewlineBeforeEndstream::Never => qdf_mode && data.last() != Some(&b'\n'),
    }
}

/// Serialize an object-stream container, preserving qpdf's source `/Extends`
/// edge when this is a source-backed Preserve group.
pub(crate) fn write_objstm_stream_with_extends(
    out: &mut Vec<u8>,
    body: &object_streams::ObjStmBody,
    compress: CompressStreams,
    policy: NewlineBeforeEndstream,
    extends: Option<crate::ObjectRef>,
) -> crate::Result<()> {
    let (_, data) = object_streams::wrap_objstm_body_as_handle(body, compress, extends)?;
    out.extend_from_slice(b"<< /Type /ObjStm /Length ");
    out.extend_from_slice(data.len().to_string().as_bytes());
    if matches!(compress, CompressStreams::Yes) {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(
        format!(" /N {} /First {}", body.n_members, body.first_offset).as_bytes(),
    );
    if let Some(extends) = extends {
        out.extend_from_slice(
            format!(" /Extends {} {} R", extends.number, extends.generation).as_bytes(),
        );
    }
    out.extend_from_slice(b" >>");
    write_stream_payload(out, &data, policy);
    Ok(())
}

pub(crate) mod xref_stream {
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

    use crate::pipeline::buffer::Buffer;
    use crate::pipeline::flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE};
    use crate::pipeline::png_filter::{PngFilter, PngFilterAction};
    use crate::pipeline::{Pipeline, PipelineError, PipelineResult};

    use crate::ObjectRef;
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

    const XREF_BUFFER_ID: &str = "xref stream";
    const XREF_FLATE_ID: &str = "compress xref";
    const XREF_PNG_ID: &str = "pngify xref";

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

    /// Write the low `width` big-endian bytes of `value` to the active qpdf pipeline.
    fn write_be(pipeline: &mut dyn Pipeline, value: u64, width: u8) -> PipelineResult<()> {
        if width as usize > std::mem::size_of::<u64>() {
            return Err(PipelineError::logic(
                "QPDFWriter::writeBinary called with too many bytes",
            ));
        }
        let bytes = value.to_be_bytes();
        pipeline.write(&bytes[bytes.len() - width as usize..])
    }

    /// Write each `/W`-formatted entry directly to the active pipeline.
    fn write_entries(
        pipeline: &mut dyn Pipeline,
        entries: &[XrefStreamEntry],
        widths: XrefWidths,
    ) -> PipelineResult<()> {
        for entry in entries {
            write_be(pipeline, u64::from(entry.entry_type), widths[0])?;
            write_be(pipeline, entry.field2, widths[1])?;
            write_be(pipeline, entry.field3, widths[2])?;
        }
        Ok(())
    }

    fn predictor_columns(widths: XrefWidths) -> u32 {
        u32::from(widths[0]) + u32::from(widths[1]) + u32::from(widths[2])
    }

    /// Encode through qpdf's `Pl_PNGFilter("pngify xref")` and
    /// `Pl_Flate("compress xref")` stages into `Pl_Buffer("xref stream")`.
    pub(crate) fn encode_payload(
        entries: &[XrefStreamEntry],
        widths: XrefWidths,
    ) -> Result<Vec<u8>> {
        let columns = predictor_columns(widths);
        let mut sink = Buffer::new(XREF_BUFFER_ID, None);
        {
            let mut flate = Flate::new(
                XREF_FLATE_ID,
                &mut sink,
                FlateAction::Deflate,
                DEFAULT_OUT_BUFFER_SIZE,
            )?; // cov:ignore: fixed nonzero qpdf output buffer cannot fail construction
            let mut predictor = PngFilter::new(
                XREF_PNG_ID,
                &mut flate,
                PngFilterAction::Encode,
                columns,
                1,
                8,
            )?;
            write_entries(&mut predictor, entries, widths)?;
            predictor.finish()?;
        }
        Ok(sink.take_buffer()?)
    }

    /// Encode the unfiltered cross-reference payload used when qpdf's global
    /// stream-compression policy is disabled.
    pub(crate) fn encode_payload_raw(
        entries: &[XrefStreamEntry],
        widths: XrefWidths,
    ) -> Result<Vec<u8>> {
        let mut sink = Buffer::new(XREF_BUFFER_ID, None);
        write_entries(&mut sink, entries, widths)?;
        sink.finish()?;
        Ok(sink.take_buffer()?)
    }

    /// PNG-Up-predicted rows WITHOUT Flate — qpdf's pass-1 (`skip_compression`) xref
    /// stream payload. qpdf still declares `/Filter /FlateDecode` on the pass-1
    /// object (an invalid but throwaway buffer used only to size the region and seed
    /// the deterministic `/ID`), so the payload is the predictor output alone.
    pub(crate) fn encode_payload_uncompressed(
        entries: &[XrefStreamEntry],
        widths: XrefWidths,
    ) -> Result<Vec<u8>> {
        let columns = predictor_columns(widths);
        let mut sink = Buffer::new(XREF_BUFFER_ID, None);
        {
            let mut predictor = PngFilter::new(
                XREF_PNG_ID,
                &mut sink,
                PngFilterAction::Encode,
                columns,
                1,
                8,
            )?;
            write_entries(&mut predictor, entries, widths)?;
            predictor.finish()?;
        }
        Ok(sink.take_buffer()?)
    }

    #[cfg(test)]
    #[test]
    fn dictionary_key_preserves_qpdf_first_byte() {
        let mut out = Vec::new();
        write_qpdf_dictionary_key(&mut out, b"/Canonical");
        out.push(b' ');
        write_qpdf_dictionary_key(&mut out, b"Raw");
        assert_eq!(out, b"/Canonical Raw");
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
        /// Serialized direct `/Root` value, when the source trailer carries an
        /// inline Catalog rather than an indirect object.
        pub root_value: Option<&'a [u8]>,
        /// `/Size` — the highest object number plus one.
        pub size: u32,
        /// `/Prev` byte offset of the previous xref stream (left-justified in a
        /// [`PREV_FIELD_WIDTH`] field); `None` on the chain's final (main) stream.
        pub prev: Option<u64>,
        /// Canonical entries from a live ObjectHandle trailer. Keys are decoded
        /// and values are already serialized with the writer reference map, so
        /// the xref route does not reconstruct trailer data from a stale
        pub canonical_entries: Option<&'a [(Vec<u8>, Vec<u8>)]>,
        /// Trailer `/ID` as two raw byte strings, serialized as `<hex><hex>`.
        pub id: Option<(&'a [u8], &'a [u8])>,
        /// Trailer `/Encrypt` reference. qpdf emits this after `/ID` on the
        /// first-page xref stream, but omits it from the main linearization
        /// xref stream (`t_lin_second`).
        pub encrypt: Option<ObjectRef>,
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
    /// appends `/ID` (concrete or inline-written) and the stream framing. QDF uses
    /// qpdf's newline-plus-two-space layout; `/Index` remains on the `/W` line,
    /// matching `QPDFWriter::writeXRefStream`.
    fn write_object_dict_prefix(
        out: &mut Vec<u8>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        payload_len: usize,
        qdf: bool,
    ) {
        out.extend_from_slice(format!("{} {} obj\n", object.number, object.generation).as_bytes());
        if qdf {
            out.extend_from_slice(b"<<\n  /Type /XRef");
            out.extend_from_slice(format!("\n  /Length {payload_len}").as_bytes());
        } else {
            out.extend_from_slice(b"<< /Type /XRef");
            out.extend_from_slice(format!(" /Length {payload_len}").as_bytes());
        }
        if dict.filtered {
            if qdf {
                // cov:ignore-start: qpdf never filters a QDF structural stream
                out.extend_from_slice(b"\n  /Filter /FlateDecode /DecodeParms << /Columns ");
                // cov:ignore-end
            } else {
                out.extend_from_slice(b" /Filter /FlateDecode /DecodeParms << /Columns ");
            }
            out.extend_from_slice(columns(dict.widths).to_string().as_bytes());
            out.extend_from_slice(b" /Predictor 12 >>");
        }
        if qdf {
            out.extend_from_slice(b"\n  /W [ ");
        } else {
            out.extend_from_slice(b" /W [ ");
        }
        out.extend_from_slice(
            format!("{} {} {} ]", dict.widths[0], dict.widths[1], dict.widths[2]).as_bytes(),
        );
        if let Some((start, count)) = dict.index {
            out.extend_from_slice(format!(" /Index [ {start} {count} ]").as_bytes());
        }
        let mut entries = dict
            .canonical_entries
            .map_or_else(Vec::new, ToOwned::to_owned);
        if let Some(info) = dict.info {
            entries.push((
                b"/Info".to_vec(),
                format!("{} {} R", info.number, info.generation).into_bytes(),
            ));
        }
        if let Some(root) = dict.root {
            entries.push((
                b"/Root".to_vec(),
                format!("{} {} R", root.number, root.generation).into_bytes(),
            ));
        } else if let Some(root) = dict.root_value {
            entries.push((b"/Root".to_vec(), root.to_vec()));
        }
        entries.push((b"/Size".to_vec(), dict.size.to_string().into_bytes()));
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (key, value) in entries {
            if qdf {
                // cov:ignore-start: QDF xref emission normally supplies canonical trailer entries
                out.extend_from_slice(b"\n  ");
                // cov:ignore-end
            } else {
                out.push(b' ');
            }
            write_qpdf_dictionary_key(out, &key);
            out.push(b' ');
            out.extend_from_slice(&value);
            if key == b"/Size" {
                if let Some(prev) = dict.prev {
                    out.extend_from_slice(format!(" /Prev {prev:<PREV_FIELD_WIDTH$}").as_bytes());
                }
            }
        }
    }

    fn write_qpdf_dictionary_key(out: &mut Vec<u8>, key: &[u8]) {
        if let Some(key) = key.strip_prefix(b"/") {
            out.push(b'/');
            crate::pdf_syntax::write_name_escaped(out, key);
        } else {
            crate::pdf_syntax::write_name_escaped(out, key);
        }
    }

    /// Append the framing that closes a cross-reference stream object. QDF adds
    /// the extra newline after `endobj`, as `closeObject` does in qpdf.
    fn write_object_framing(out: &mut Vec<u8>, payload: &[u8], qdf: bool) {
        if qdf {
            out.extend_from_slice(b"\nstream\n");
        } else {
            out.extend_from_slice(b" >>\nstream\n");
        }
        out.extend_from_slice(payload);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        if qdf {
            out.push(b'\n');
        }
    }

    fn write_object_internal(
        out: &mut Vec<u8>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        payload: &[u8],
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    ) -> Option<std::ops::Range<usize>> {
        write_object_dict_prefix(out, object, dict, payload.len(), qdf);
        let (id_range, has_id) = if let Some(id_writer) = id_writer {
            if qdf {
                out.extend_from_slice(b"\n  /ID ");
            } else {
                out.extend_from_slice(b" /ID ");
            }
            id_writer(out);
            (None, true)
        } else if let Some((id0, id1)) = dict.id {
            if qdf {
                out.extend_from_slice(b"\n  /ID ");
            } else {
                out.extend_from_slice(b" /ID ");
            }
            let id_start = out.len();
            out.push(b'[');
            out.push(b'<');
            push_hex(out, id0);
            out.extend_from_slice(b"><");
            push_hex(out, id1);
            out.extend_from_slice(b">]");
            (Some(id_start..out.len()), true)
        } else {
            (None, false)
        };
        if let Some(encrypt) = dict.encrypt {
            if qdf && !has_id {
                // cov:ignore-start: qpdf xref streams always emit /ID before /Encrypt
                out.extend_from_slice(b"\n  /Encrypt ");
                // cov:ignore-end
            } else {
                out.extend_from_slice(b" /Encrypt ");
            }
            out.extend_from_slice(
                format!("{} {} R", encrypt.number, encrypt.generation).as_bytes(),
            );
        }
        if qdf {
            out.extend_from_slice(b"\n>>");
        }
        write_object_framing(out, payload, qdf);
        id_range
    }

    /// Write a complete cross-reference stream indirect object
    /// (`<num> 0 obj … endobj\n`) to `out`, with `dict`'s keys in qpdf's fixed order
    /// and `payload` as the already-encoded stream body.
    ///
    /// Returns the byte range of the emitted `/ID [<hex0><hex1>]` array token
    /// within `out` (`None` when `dict.id` is absent), so a deterministic-`/ID`
    /// back-patch can target that exact span instead of scanning a whole
    /// section — a section that may also carry a live trailer's arbitrary
    /// custom entries, whose serialized bytes are not guaranteed to avoid the
    /// placeholder's fixed byte pattern.
    pub(crate) fn write_object(
        out: &mut Vec<u8>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        payload: &[u8],
    ) -> Option<std::ops::Range<usize>> {
        write_object_internal(out, object, dict, payload, false, None)
    }

    /// QDF-formatted variant of [`write_object`].
    pub(crate) fn write_object_qdf(
        out: &mut Vec<u8>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        payload: &[u8],
    ) -> Option<std::ops::Range<usize>> {
        write_object_internal(out, object, dict, payload, true, None)
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
        write_object_internal(out, object, dict, payload, false, Some(id_writer));
    }

    /// QDF-formatted variant of [`write_object_with_id_writer`].
    pub(crate) fn write_object_with_id_writer_qdf(
        out: &mut Vec<u8>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        payload: &[u8],
        id_writer: &mut dyn FnMut(&mut Vec<u8>),
    ) {
        write_object_internal(out, object, dict, payload, true, Some(id_writer));
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
    /// Returns the padded object bytes together with the `/ID` array token's
    /// byte range within them (see [`write_object`]), so a caller that
    /// back-patches a deterministic `/ID` placeholder later can target that
    /// exact span rather than the whole region (which also carries this
    /// object's stream payload and, on the first-page xref stream, arbitrary
    /// custom trailer entries).
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
    ) -> Result<(Vec<u8>, Option<std::ops::Range<usize>>)> {
        let mut buf = Vec::with_capacity(region_len);
        let id_range = write_object(&mut buf, object, dict, payload);
        if buf.len() > region_len {
            return Err(crate::Error::Unsupported(format!(
                "linearized xref stream object ({} bytes) exceeds its reserved region \
             ({region_len} bytes)",
                buf.len()
            )));
        }
        buf.resize(region_len, b' ');
        Ok((buf, id_range))
    }
}
