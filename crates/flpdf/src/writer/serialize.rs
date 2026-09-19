//! Shared writer serialization primitives.
//!
//! qpdf correspondence: QPDFWriter.cc shared object, stream, trailer, and xref serialization primitives.
//!
use super::{
    object_streams,
    output::{write_decimal_u64, write_object_ref, OutputSink},
    CompressStreams, NewlineBeforeEndstream, ObjectWriterEmission,
};
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
    let data = stream.get_raw_stream_data()?;
    super::output::with_buffer_sink(out, |out| {
        stream.write_stream_body(out, false)?;
        write_stream_payload(out, &data, policy)
    })
}

/// Emit stream framing after its dictionary has already been written.
pub(crate) fn write_stream_payload(
    out: &mut OutputSink<'_>,
    data: &[u8],
    policy: NewlineBeforeEndstream,
) -> crate::Result<()> {
    write_stream_payload_with_qdf(out, data, policy, false)
}

/// Emit stream framing with qpdf's QDF-specific conditional newline rule.
pub(crate) fn write_stream_payload_with_qdf(
    out: &mut OutputSink<'_>,
    data: &[u8],
    policy: NewlineBeforeEndstream,
    qdf_mode: bool,
) -> crate::Result<()> {
    out.write_bytes(b"\nstream\n")?;
    let payload_result = out.write_bytes(data);
    // QPDFWriter's stream-data PipelinePopper finishes the active stream
    // segment even while unwinding from a payload write error. The final
    // output position and deterministic-ID digest survive this boundary.
    let finish_result = out.finish_segment();
    payload_result?;
    finish_result?;
    if framing_adds_newline_with_qdf(data, policy, qdf_mode) {
        out.write_bytes(b"\n")?;
    }
    out.write_bytes(b"endstream")
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
    out: &mut OutputSink<'_>,
    body: object_streams::ObjStmBody,
    compress: CompressStreams,
    policy: NewlineBeforeEndstream,
    extends: Option<crate::ObjectRef>,
) -> crate::Result<()> {
    let first_offset = body.first_offset;
    let n_members = body.n_members;
    let body_bytes = body.bytes;
    let data = match compress {
        CompressStreams::Yes => {
            let encoded = crate::stream_filter::encode_flate(&body_bytes)?;
            drop(body_bytes);
            encoded
        }
        CompressStreams::No => body_bytes,
    };
    out.write_bytes(b"<< /Type /ObjStm /Length ")?;
    write_decimal_u64(out, data.len() as u64)?;
    if matches!(compress, CompressStreams::Yes) {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" /N ")?;
    write_decimal_u64(out, n_members as u64)?;
    out.write_bytes(b" /First ")?;
    write_decimal_u64(out, first_offset as u64)?;
    if let Some(extends) = extends {
        out.write_bytes(b" /Extends ")?;
        write_object_ref(out, extends)?; // cov:ignore: LLVM maps the covered compact ObjStm /Extends write continuation to this line
    }
    out.write_bytes(b" >>")?;
    write_stream_payload(out, &data, policy)
}

/// Encrypted sibling of [`write_objstm_stream_with_extends`]. The ObjStm body
/// remains the one qpdf-required container buffer; encoded bytes are consumed
/// directly by the encryption stage and never copied into a document buffer.
pub(crate) fn write_encrypted_objstm_stream_with_extends(
    out: &mut OutputSink<'_>,
    body: object_streams::ObjStmBody,
    compress: CompressStreams,
    policy: NewlineBeforeEndstream,
    extends: Option<crate::ObjectRef>,
    object_ref: crate::ObjectRef,
    context: &crate::writer::EncryptionContext,
) -> crate::Result<()> {
    let first_offset = body.first_offset;
    let n_members = body.n_members;
    let body_bytes = body.bytes;
    let data = match compress {
        CompressStreams::Yes => {
            let encoded = crate::stream_filter::encode_flate(&body_bytes)?;
            drop(body_bytes);
            encoded
        }
        CompressStreams::No => body_bytes,
    };
    let mut stream_length = data.len();
    crate::writer::adjust_aes_stream_length(&mut stream_length, context, true)?;
    out.write_bytes(b"<< /Type /ObjStm /Length ")?;
    write_decimal_u64(out, stream_length as u64)?;
    if matches!(compress, CompressStreams::Yes) {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" /N ")?;
    write_decimal_u64(out, n_members as u64)?;
    out.write_bytes(b" /First ")?;
    write_decimal_u64(out, first_offset as u64)?;
    if let Some(extends) = extends {
        out.write_bytes(b" /Extends ")?;
        write_object_ref(out, extends)?; // cov:ignore: LLVM maps the covered encrypted compact ObjStm /Extends write continuation to this line
    }
    out.write_bytes(b" >>")?;
    crate::writer::write_stream_payload_with_pipeline(
        out, &data, policy, object_ref, context, true, None,
    )?;
    Ok(())
}

/// Emit an object-stream container in qpdf QDF layout. QDF keeps the body
/// uncompressed, writes the structural dictionary one entry per line, and
/// applies the QDF stream framing rule (`QPDFWriter.cc:1620-1775`).
pub(crate) fn write_objstm_stream_with_extends_qdf(
    out: &mut OutputSink<'_>,
    body: object_streams::ObjStmBody,
    extends: Option<crate::ObjectRef>,
    first_offset: usize,
    newline_before_endstream: NewlineBeforeEndstream,
) -> crate::Result<()> {
    let n_members = body.n_members;
    let data = body.bytes;
    out.write_bytes(b"<<\n  /Type /ObjStm\n")?;
    out.write_bytes(b"  /Length ")?;
    write_decimal_u64(out, data.len() as u64)?;
    out.write_bytes(b"\n  /N ")?;
    write_decimal_u64(out, n_members as u64)?;
    out.write_bytes(b"\n  /First ")?;
    write_decimal_u64(out, first_offset as u64)?;
    out.write_bytes(b"\n")?;
    if let Some(extends) = extends {
        out.write_bytes(b"  /Extends ")?;
        write_object_ref(out, extends)?;
        out.write_bytes(b"\n")?; // cov:ignore: LLVM maps the covered QDF ObjStm /Extends write continuation to this line
    }
    out.write_bytes(b">>")?;
    write_stream_payload_with_qdf(out, &data, newline_before_endstream, true)
}

/// QDF-formatted encrypted ObjStm using the same one-container buffer and
/// stream-segment finish boundary as ordinary encrypted streams.
pub(crate) fn write_encrypted_objstm_stream_with_extends_qdf(
    out: &mut OutputSink<'_>,
    body: object_streams::ObjStmBody,
    extends: Option<crate::ObjectRef>,
    first_offset: usize,
    newline_before_endstream: NewlineBeforeEndstream,
    object_ref: crate::ObjectRef,
    context: &crate::writer::EncryptionContext,
) -> crate::Result<()> {
    let n_members = body.n_members;
    let data = body.bytes;
    let mut stream_length = data.len();
    crate::writer::adjust_aes_stream_length(&mut stream_length, context, true)?;
    out.write_bytes(b"<<\n  /Type /ObjStm\n")?;
    out.write_bytes(b"  /Length ")?;
    write_decimal_u64(out, stream_length as u64)?;
    out.write_bytes(b"\n  /N ")?;
    write_decimal_u64(out, n_members as u64)?;
    out.write_bytes(b"\n  /First ")?;
    write_decimal_u64(out, first_offset as u64)?;
    out.write_bytes(b"\n")?;
    if let Some(extends) = extends {
        out.write_bytes(b"  /Extends ")?;
        write_object_ref(out, extends)?;
        out.write_bytes(b"\n")?; // cov:ignore: LLVM maps the covered encrypted QDF ObjStm /Extends write continuation to this line
    }
    out.write_bytes(b">>")?;
    crate::writer::write_stream_payload_with_pipeline_qdf(
        out,
        &data,
        newline_before_endstream,
        true,
        object_ref,
        context,
        true,
        None,
    )?; // cov:ignore: LLVM maps the covered encrypted QDF ObjStm payload call continuation to this line
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

    use crate::writer::output::{decimal_u64_len, write_decimal_u64, write_object_ref, OutputSink};
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

    /// Encode the payload for qpdf's stream-compression policy.
    ///
    /// `filtered` is the effective `compress_streams && !qdf_mode` decision
    /// that is also reflected in the emitted dictionary. A linearization
    /// pass-1 call sets `skip_compression`: qpdf still emits the PNG predictor
    /// and advertises `/FlateDecode`, but deliberately leaves the predictor
    /// bytes uncompressed while it sizes the fixed region. Keeping that
    /// policy in the shared owner prevents plain and linearized callers from
    /// growing subtly different codec branches.
    pub(crate) fn encode_payload_for_policy(
        entries: &[XrefStreamEntry],
        widths: XrefWidths,
        filtered: bool,
        skip_compression: bool,
    ) -> Result<Vec<u8>> {
        if !filtered {
            encode_payload_raw(entries, widths)
        } else if skip_compression {
            encode_payload_uncompressed(entries, widths)
        } else {
            encode_payload(entries, widths)
        }
    }

    #[cfg(test)]
    #[test]
    fn dictionary_key_preserves_qpdf_first_byte() {
        let mut out = Vec::new();
        let mut sink = OutputSink::new(&mut out);
        write_qpdf_dictionary_key(&mut sink, b"/Canonical").unwrap();
        sink.write_bytes(b" ").unwrap();
        write_qpdf_dictionary_key(&mut sink, b"Raw").unwrap();
        drop(sink);
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
        /// Pre-serialized direct `/Root` value for the linearized/fallback
        /// callers below that use this struct's `canonical_entries` shape.
        /// The plain (non-linearized) writer's xref-stream route reaches its
        /// live direct `/Root` through
        /// `crate::writer::object::write_trailer_with_ref_map_and_kind_and_direct_root`
        /// instead (D14: the classic table and xref-stream routes share that
        /// one trailer owner rather than each holding a live-handle field
        /// here).
        pub root_value: Option<&'a [u8]>,
        /// `/Size` — the highest object number plus one.
        pub size: u32,
        /// `/Prev` byte offset of the previous xref stream (left-justified in a
        /// [`PREV_FIELD_WIDTH`] field); `None` on the chain's final (main) stream.
        pub prev: Option<u64>,
        /// Pre-serialized trailer entries from a live ObjectHandle trailer.
        /// Keys are decoded and values are already serialized with the
        /// writer reference map, so this struct does not itself hold a live
        /// trailer handle: the linearized writer builds these bytes in a
        /// separate two-pass pipeline before reaching this dictionary.
        pub canonical_entries: Option<&'a [(Vec<u8>, Vec<u8>)]>,
        /// Trailer `/ID` as two raw byte strings, serialized as `<hex><hex>`.
        pub id: Option<(&'a [u8], &'a [u8])>,
        /// Trailer `/Encrypt` reference. qpdf emits this after `/ID` on the
        /// first-page xref stream, but omits it from the main linearization
        /// xref stream (`t_lin_second`).
        pub encrypt: Option<ObjectRef>,
    }

    /// Append two lowercase hex digits per byte of `bytes` to `out`.
    fn push_hex(out: &mut OutputSink<'_>, bytes: &[u8]) -> Result<()> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for &b in bytes {
            out.write_bytes(&[HEX[(b >> 4) as usize], HEX[(b & 0x0f) as usize]])?;
        }
        Ok(())
    }

    /// Write the fixed xref-stream dictionary header — the object header line
    /// through `/W [...]` and the optional `/Index [...]` — in qpdf's fixed
    /// key order: `/Type /Length /Filter /DecodeParms /W [/Index]`
    /// (`QPDFWriter::writeXRefStream`, `libqpdf/QPDFWriter.cc:2465-2481`).
    ///
    /// This is exactly the part of the xref-stream dictionary that precedes
    /// qpdf's `writeTrailer(which, size, xref_stream=true, ...)` call
    /// (`QPDFWriter.cc:2481`): the caller supplies the trailer keys, `/ID`,
    /// `/Encrypt`, and the closing `>>` through that single trailer owner
    /// (`crate::writer::object::write_trailer_with_ref_map_and_kind`/
    /// `_and_direct_root`), then appends the stream framing. QDF uses qpdf's
    /// newline-plus-two-space layout.
    pub(crate) fn write_xref_stream_dict_header(
        out: &mut OutputSink<'_>,
        object: ObjectRef,
        filtered: bool,
        widths: XrefWidths,
        index: Option<(u32, u32)>,
        payload_len: usize,
        qdf: bool,
    ) -> Result<()> {
        write_decimal_u64(out, u64::from(object.number))?;
        out.write_bytes(b" ")?;
        write_decimal_u64(out, u64::from(object.generation))?;
        out.write_bytes(b" obj\n")?;
        if qdf {
            out.write_bytes(b"<<\n  /Type /XRef")?;
            out.write_bytes(b"\n  /Length ")?;
            write_decimal_u64(out, payload_len as u64)?;
        } else {
            out.write_bytes(b"<< /Type /XRef")?;
            out.write_bytes(b" /Length ")?;
            write_decimal_u64(out, payload_len as u64)?;
        }
        if filtered {
            if qdf {
                // cov:ignore-start: qpdf never filters a QDF structural stream
                out.write_bytes(b"\n  /Filter /FlateDecode /DecodeParms << /Columns ")?;
                // cov:ignore-end
            } else {
                out.write_bytes(b" /Filter /FlateDecode /DecodeParms << /Columns ")?;
            }
            write_decimal_u64(out, columns(widths) as u64)?;
            out.write_bytes(b" /Predictor 12 >>")?;
        }
        if qdf {
            out.write_bytes(b"\n  /W [ ")?;
        } else {
            out.write_bytes(b" /W [ ")?;
        }
        write_decimal_u64(out, u64::from(widths[0]))?;
        out.write_bytes(b" ")?;
        write_decimal_u64(out, u64::from(widths[1]))?;
        out.write_bytes(b" ")?;
        write_decimal_u64(out, u64::from(widths[2]))?;
        out.write_bytes(b" ]")?; // cov:ignore: LLVM maps the covered xref width write continuation to this line
        if let Some((start, count)) = index {
            out.write_bytes(b" /Index [ ")?;
            write_decimal_u64(out, u64::from(start))?;
            out.write_bytes(b" ")?;
            write_decimal_u64(out, u64::from(count))?;
            out.write_bytes(b" ]")?;
        }
        Ok(())
    }

    /// Write the xref-stream object header and every dictionary key up to (but not
    /// including) `/ID`, in qpdf's fixed order: `/Type /Length /Filter /DecodeParms
    /// /W [/Index]`, then the sorted trimmed trailer entries (including generated
    /// `/Root` and `/Size`, with `/Prev` immediately after `/Size`). The caller
    /// appends `/ID` (concrete or inline-written) and the stream framing.
    ///
    /// The canonical (pre-serialized `canonical_entries`) shape below is the
    /// linearization writer's route, which assembles trailer bytes in a
    /// separate two-pass pipeline that never holds a live `ObjectHandle`
    /// trailer at this call site. The plain (non-linearized) writer's
    /// xref-stream route instead calls [`write_xref_stream_dict_header`]
    /// directly and reaches the trailer through
    /// `crate::writer::object::write_trailer_with_ref_map_and_kind`/
    /// `_and_direct_root` with `xref_stream: true` — the same owner the
    /// classic table route uses — so this function only ever needs the
    /// canonical form.
    fn write_object_dict_prefix(
        out: &mut OutputSink<'_>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        payload_len: usize,
        qdf: bool,
    ) -> Result<()> {
        write_xref_stream_dict_header(
            out,
            object,
            dict.filtered,
            dict.widths,
            dict.index,
            payload_len,
            qdf,
        )?; // cov:ignore: covered multiline call; LLVM attributes this terminator to the call setup
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
            write_xref_dictionary_entry_prefix(out, qdf, &key)?;
            out.write_bytes(&value)?;
            if key == b"/Size" {
                if let Some(prev) = dict.prev {
                    out.write_bytes(b" /Prev ")?;
                    write_decimal_u64(out, prev)?;
                    push_spaces(out, PREV_FIELD_WIDTH.saturating_sub(decimal_u64_len(prev)))?;
                }
            }
        }
        Ok(())
    }

    fn write_xref_dictionary_entry_prefix(
        out: &mut OutputSink<'_>,
        qdf: bool,
        key: &[u8],
    ) -> Result<()> {
        if qdf {
            out.write_bytes(b"\n  ")?;
        } else {
            out.write_bytes(b" ")?;
        }
        write_qpdf_dictionary_key(out, key)?;
        out.write_bytes(b" ")
    }

    fn push_spaces(out: &mut OutputSink<'_>, count: usize) -> Result<()> {
        for _ in 0..count {
            out.write_bytes(b" ")?;
        }
        Ok(())
    }

    fn write_qpdf_dictionary_key(out: &mut OutputSink<'_>, key: &[u8]) -> Result<()> {
        if let Some(key) = key.strip_prefix(b"/") {
            out.write_bytes(b"/")?;
            crate::pdf_syntax::write_name_escaped(out, key)?;
        } else {
            crate::pdf_syntax::write_name_escaped(out, key)?;
        }
        Ok(())
    }

    /// Append the framing that closes a cross-reference stream object. QDF adds
    /// the extra newline after `endobj`, as `closeObject` does in qpdf.
    fn write_object_framing(out: &mut OutputSink<'_>, payload: &[u8], qdf: bool) -> Result<()> {
        if qdf {
            out.write_bytes(b"\nstream\n")?;
        } else {
            out.write_bytes(b" >>\nstream\n")?;
        }
        out.write_bytes(payload)?;
        out.write_bytes(b"\nendstream\nendobj\n")?;
        if qdf {
            out.write_bytes(b"\n")?;
        }
        Ok(())
    }

    fn write_object_internal(
        out: &mut OutputSink<'_>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        payload: &[u8],
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    ) -> Result<Option<std::ops::Range<usize>>> {
        write_object_dict_prefix(out, object, dict, payload.len(), qdf)?;
        let (id_range, has_id) = if let Some(id_writer) = id_writer {
            if qdf {
                out.write_bytes(b"\n  /ID ")?;
            } else {
                out.write_bytes(b" /ID ")?;
            }
            id_writer(out)?;
            (None, true)
        } else if let Some((id0, id1)) = dict.id {
            if qdf {
                out.write_bytes(b"\n  /ID ")?;
            } else {
                out.write_bytes(b" /ID ")?;
            }
            let id_start = usize::try_from(out.position()).map_err(|_| {
                // cov:ignore-start: the xref stream is emitted into the same usize-sized process memory as its output sink.
                crate::Error::Unsupported("xref stream ID offset exceeds usize range".to_string())
                // cov:ignore-end
            })?; // cov:ignore: LLVM maps the covered xref ID-start conversion continuation to this line
            out.write_bytes(b"[<")?;
            push_hex(out, id0)?;
            out.write_bytes(b"><")?;
            push_hex(out, id1)?;
            out.write_bytes(b">]")?;
            let id_end = usize::try_from(out.position()).map_err(|_| {
                // cov:ignore-start: the xref stream is emitted into the same usize-sized process memory as its output sink.
                crate::Error::Unsupported("xref stream ID offset exceeds usize range".to_string())
                // cov:ignore-end
            })?; // cov:ignore: LLVM maps the covered xref ID-end conversion continuation to this line
            (Some(id_start..id_end), true)
        } else {
            (None, false)
        };
        if let Some(encrypt) = dict.encrypt {
            if qdf && !has_id {
                // cov:ignore-start: qpdf xref streams always emit /ID before /Encrypt
                out.write_bytes(b"\n  /Encrypt ")?;
                // cov:ignore-end
            } else {
                out.write_bytes(b" /Encrypt ")?;
            }
            write_object_ref(out, encrypt)?;
        }
        if qdf {
            out.write_bytes(b"\n>>")?;
        }
        write_object_framing(out, payload, qdf)?;
        Ok(id_range)
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
        out: &mut OutputSink<'_>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        payload: &[u8],
    ) -> Result<Option<std::ops::Range<usize>>> {
        write_object_internal(out, object, dict, payload, false, None)
    }

    /// Write one prepared xref-stream object through the shared dictionary and
    /// framing owner. The returned `space_before_zero` is qpdf's
    /// `xref_offset - 1` value used by linearization's `/T` calculation.
    pub(crate) fn write_xref_stream(
        out: &mut OutputSink<'_>,
        object: ObjectRef,
        dict: &XrefStreamDict,
        layout: &XrefStreamLayout,
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter<'_>>,
    ) -> Result<(usize, Option<std::ops::Range<usize>>)> {
        // cov:ignore-start: xref stream output positions are backed by the same usize-sized process memory as the sink.
        let xref_offset = usize::try_from(out.position()).map_err(|_| {
            crate::Error::Unsupported("xref stream offset exceeds usize range".to_string())
        })?;
        // cov:ignore-end
        let id_range = write_object_internal(out, object, dict, &layout.payload, qdf, id_writer)?;
        Ok((xref_offset.saturating_sub(1), id_range))
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
        let mut sink = OutputSink::new(&mut buf);
        write_object(&mut sink, object, dict, &vec![0u8; payload_len])
            .expect("writing a first-pass xref stream to Vec cannot fail");
        drop(sink);
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

    /// Build a stream range after registering the stream's own type-1 entry.
    ///
    /// qpdf stores `xref_id` in its live xref map *before* it serializes any
    /// rows (`QPDFWriter.cc:2411-2415`). This helper performs the same mutation
    /// on the caller's live offset map before building the requested range. A
    /// type-1 row always carries field 3 zero; only type-2 rows use that field
    /// for an object-stream member index.
    pub(crate) fn build_entries_with_self(
        offs: &mut BTreeMap<u32, usize>,
        member_new: &BTreeMap<u32, (u32, u32)>,
        start: u32,
        count: u32,
        xref_id: u32,
        xref_offset: usize,
    ) -> Vec<XrefStreamEntry> {
        // This mutation is the Rust equivalent of qpdf's
        // `m->xref[xref_id] = QPDFXRefEntry(...)` before the row loop
        // (`QPDFWriter.cc:2411-2415`). The range may intentionally omit the
        // stream object; qpdf still registers it in the live map and simply
        // does not visit it while iterating `first..=last`.
        offs.insert(xref_id, xref_offset);
        build_entries(offs, member_new, start, count)
    }

    /// Apply qpdf's linearization hint-offset correction to type-1 rows.
    ///
    /// The first-half `writeXRefStream` receives `hint_id`, `hint_offset`, and
    /// `hint_length`; every type-1 offset at or after the hint object is
    /// shifted by the final hint object's length, except the hint row itself
    /// (`QPDFWriter.cc:2432-2436`). Callers that already hold physical
    /// post-hint offsets pass `None` instead of applying this correction twice.
    pub(crate) fn apply_hint_offset(
        entries: &mut [XrefStreamEntry],
        first: u32,
        hint_id: u32,
        hint_offset: u64,
        hint_length: u64,
    ) -> Result<()> {
        if hint_length == 0 {
            return Ok(());
        }
        for (index, entry) in entries.iter_mut().enumerate() {
            let number = first
                // cov:ignore-start: a slice cannot contain more than u32::MAX rows on supported targets.
                .checked_add(u32::try_from(index).map_err(|_| {
                    crate::Error::Unsupported("xref stream row index overflows u32".into())
                })?)
                // cov:ignore-end
                .ok_or_else(|| crate::Error::Unsupported("xref stream row overflows u32".into()))?;
            if entry.entry_type == 1 && number != hint_id && entry.field2 >= hint_offset {
                entry.field2 = entry.field2.checked_add(hint_length).ok_or_else(|| {
                    crate::Error::Unsupported("xref stream hint offset overflows u64".into())
                })?;
            }
        }
        Ok(())
    }

    /// Maximum offset in an encoded object-number range, used for qpdf's
    /// caller-supplied `max_offset` argument when the range is assembled from
    /// a live offset map rather than a completed row vector.
    #[allow(dead_code)]
    pub(crate) fn max_offset_for_range(
        offsets: &BTreeMap<u32, usize>,
        start: u32,
        count: u32,
    ) -> u64 {
        let end = start.saturating_add(count);
        offsets
            .iter()
            .filter(|&(&number, _)| number >= start && number < end)
            .map(|(_, &offset)| offset as u64)
            .max()
            .unwrap_or(0)
    }

    /// Prepared layout and payload for one qpdf `writeXRefStream` call.
    pub(crate) struct XrefStreamLayout {
        pub widths: XrefWidths,
        pub payload: Vec<u8>,
    }

    /// Build rows, field widths, and payload under one shared xref-stream
    /// policy. Consumer-specific code supplies only the range, offset maps,
    /// and pass-1 wide-field flag; self-entry registration and the raw versus
    /// predictor/Flate choice stay in this owner.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_xref_stream(
        offsets: &mut BTreeMap<u32, usize>,
        member_new: &BTreeMap<u32, (u32, u32)>,
        start: u32,
        count: u32,
        xref_id: u32,
        xref_offset: usize,
        max_offset: u64,
        max_id: u32,
        max_ostream_index: u64,
        hint_length: u64,
        hint: Option<(u32, u64, u64)>,
        filtered: bool,
        skip_compression: bool,
        force_wide_field2: bool,
    ) -> Result<XrefStreamLayout> {
        let mut entries =
            build_entries_with_self(offsets, member_new, start, count, xref_id, xref_offset);
        if let Some((hint_id, hint_offset, hint_length)) = hint {
            apply_hint_offset(&mut entries, start, hint_id, hint_offset, hint_length)?;
        }
        let widths = if force_wide_field2 {
            first_pass_widths(max_id, max_ostream_index, hint_length)
        } else {
            second_pass_widths(max_offset, hint_length, max_id, max_ostream_index)
        };
        let payload = encode_payload_for_policy(&entries, widths, filtered, skip_compression)?;
        Ok(XrefStreamLayout { widths, payload })
    }

    /// Prepare a linearization first-page xref stream from the writer-owned
    /// physical offset map without cloning that map into a virtual-offset
    /// table. qpdf keeps one xref map and applies the hint correction while it
    /// encodes rows (`QPDFWriter.cc:2437-2454`); the caller has already applied
    /// that correction to the writer-owned physical map, so this helper only
    /// materializes the bounded xref entry payload.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_xref_stream_from_physical_offsets(
        offsets: &mut BTreeMap<u32, usize>,
        member_new: &BTreeMap<u32, (u32, u32)>,
        start: u32,
        count: u32,
        xref_id: u32,
        xref_offset: usize,
        max_id: u32,
        max_ostream_index: u64,
        filtered: bool,
    ) -> Result<XrefStreamLayout> {
        let entries =
            build_entries_with_self(offsets, member_new, start, count, xref_id, xref_offset);
        let max_physical_offset = entries
            .iter()
            .filter(|entry| entry.entry_type == 1)
            .map(|entry| entry.field2)
            .max()
            .unwrap_or(0);
        let widths = second_pass_widths(max_physical_offset, 0, max_id, max_ostream_index);
        let payload = encode_payload_for_policy(&entries, widths, filtered, false)?;
        Ok(XrefStreamLayout { widths, payload })
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
        layout: &XrefStreamLayout,
        region_len: usize,
    ) -> Result<(Vec<u8>, Option<std::ops::Range<usize>>)> {
        let mut buf = Vec::with_capacity(region_len);
        let mut sink = OutputSink::new(&mut buf);
        let (_, id_range) = write_xref_stream(&mut sink, object, dict, layout, false, None)?;
        drop(sink);
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

    #[cfg(test)]
    mod tests {
        use super::*;

        fn base_dict<'a>() -> XrefStreamDict<'a> {
            XrefStreamDict {
                filtered: false,
                widths: [1, 1, 0],
                index: None,
                info: None,
                root: None,
                root_value: None,
                size: 2,
                prev: None,
                canonical_entries: None,
                id: None,
                encrypt: None,
            }
        }

        #[test]
        fn build_entries_with_self_registers_a_zero_generation_type1_row() {
            let mut offsets = BTreeMap::new();
            offsets.insert(1, 12usize);
            let mut members = BTreeMap::new();
            members.insert(2, (4, 7));

            let entries = build_entries_with_self(&mut offsets, &members, 0, 3, 1, 99);

            assert_eq!(offsets.get(&1), Some(&99));
            assert_eq!(entries[0].entry_type, 0);
            assert_eq!(entries[1].entry_type, 1);
            assert_eq!(entries[1].field2, 99);
            assert_eq!(entries[1].field3, 0);
            assert_eq!(entries[2].entry_type, 2);
            assert_eq!(entries[2].field2, 4);
            assert_eq!(entries[2].field3, 7);
        }

        #[test]
        fn build_entries_with_self_registers_even_when_the_range_omits_self() {
            let mut offsets = BTreeMap::new();
            let members = BTreeMap::new();

            let entries = build_entries_with_self(&mut offsets, &members, 0, 1, 1, 0);
            assert_eq!(offsets.get(&1), Some(&0));
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].entry_type, 0);
        }

        #[test]
        fn payload_policy_selects_raw_predictor_and_flate_paths() {
            let entries = [XrefStreamEntry {
                entry_type: 1,
                field2: 12,
                field3: 0,
            }];
            let widths = [1, 1, 1];

            let raw =
                encode_payload_for_policy(&entries, widths, false, false).expect("raw payload");
            let predictor = encode_payload_for_policy(&entries, widths, true, true)
                .expect("pass-1 predictor payload");
            let flate = encode_payload_for_policy(&entries, widths, true, false)
                .expect("compressed payload");

            assert_eq!(raw.len(), 3);
            assert_eq!(predictor.len(), 4);
            assert_ne!(flate, predictor);
        }

        #[test]
        fn hint_offsets_and_explicit_max_offset_follow_qpdf_layout_inputs() {
            let mut entries = [
                XrefStreamEntry {
                    entry_type: 1,
                    field2: 49,
                    field3: 0,
                },
                XrefStreamEntry {
                    entry_type: 1,
                    field2: 50,
                    field3: 0,
                },
                XrefStreamEntry {
                    entry_type: 1,
                    field2: 60,
                    field3: 0,
                },
            ];
            apply_hint_offset(&mut entries, 0, 1, 50, 5).expect("hint correction");
            assert_eq!(entries[0].field2, 49);
            assert_eq!(entries[1].field2, 50);
            assert_eq!(entries[2].field2, 65);
            apply_hint_offset(&mut entries, 0, 1, 50, 0).expect("zero-length hint is a no-op");

            let mut overflow = [XrefStreamEntry {
                entry_type: 1,
                field2: u64::MAX,
                field3: 0,
            }];
            let error = apply_hint_offset(&mut overflow, 0, 99, 0, 1)
                .expect_err("hint addition must not wrap");
            assert!(
                matches!(error, crate::Error::Unsupported(message) if message.contains("overflows"))
            );

            let mut offsets = BTreeMap::new();
            offsets.insert(1, 65_536usize);
            let layout = prepare_xref_stream(
                &mut offsets,
                &BTreeMap::new(),
                0,
                2,
                0,
                0,
                65_536,
                0,
                0,
                0,
                None,
                false,
                false,
                false,
            )
            .expect("explicit max offset");
            assert_eq!(layout.widths, [1, 3, 0]);
            assert_eq!(max_offset_for_range(&offsets, 0, 2), 65_536);
        }

        // D14: the plain (non-linearized) writer's xref-stream route no
        // longer holds a live ObjectHandle trailer in this dictionary — it
        // reaches trailer keys, /ID, /Encrypt, and the closing `>>` through
        // `crate::writer::object::write_trailer_with_ref_map_and_kind_and_direct_root`
        // instead, the same owner the classic table route uses. That
        // direct-root + qdf coverage now lives with its production caller:
        // `writer::plain::xref::tests::xref_stream_direct_root_shares_the_classic_trailer_owner`.
        #[test]
        fn xref_dictionary_covers_the_canonical_index_root_size_prev_and_encrypt_layout() {
            let canonical = XrefStreamDict {
                index: Some((3, 2)),
                info: Some(ObjectRef::new(7, 0)),
                root_value: Some(b"<< /Type /Catalog >>"),
                prev: Some(41),
                canonical_entries: Some(&[(b"/Custom".to_vec(), b"/Value".to_vec())]),
                encrypt: Some(ObjectRef::new(8, 0)),
                ..base_dict()
            };
            let mut output = Vec::new();
            crate::writer::output::with_buffer_sink(&mut output, |out| {
                write_object(out, ObjectRef::new(2, 0), &canonical, b"xref")
            })
            .expect("canonical xref dictionary");
            let text = String::from_utf8(output).unwrap();
            assert!(text.contains("/Index [ 3 2 ]"));
            assert!(text.contains("/Root << /Type /Catalog >>"));
            assert!(text.contains("/Size 2 /Prev 41"));
            assert!(text.contains("/Encrypt 8 0 R"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encryption::standard::ObjectKeyAlg;
    use crate::writer::{EncryptionContext, WriteCipher};

    fn body() -> object_streams::ObjStmBody {
        object_streams::ObjStmBody {
            bytes: b"1 0 value".to_vec(),
            first_offset: 4,
            n_members: 1,
        }
    }

    fn encryption_context() -> EncryptionContext {
        EncryptionContext {
            encrypt_dict: crate::ObjectHandle::dictionary(Vec::new()),
            file_key: vec![1; 5],
            cipher: WriteCipher::PerObject(ObjectKeyAlg::Rc4),
            encryption_v: 2,
            encryption_r: 3,
            encrypt_ref: crate::ObjectRef::new(99, 0),
            id0: b"id".to_vec(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        }
    }

    #[test]
    fn object_stream_extends_is_emitted_in_plain_encrypted_compact_and_qdf_routes() {
        let extends = Some(crate::ObjectRef::new(7, 2));
        let mut outputs = Vec::new();

        let mut compact = Vec::new();
        crate::writer::output::with_buffer_sink(&mut compact, |out| {
            write_objstm_stream_with_extends(
                out,
                body(),
                CompressStreams::No,
                NewlineBeforeEndstream::Never,
                extends,
            )
        })
        .expect("plain compact ObjStm");
        outputs.push(compact);

        let mut qdf = Vec::new();
        crate::writer::output::with_buffer_sink(&mut qdf, |out| {
            write_objstm_stream_with_extends_qdf(
                out,
                body(),
                extends,
                4,
                NewlineBeforeEndstream::Never,
            )
        })
        .expect("plain QDF ObjStm");
        outputs.push(qdf);

        let context = encryption_context();
        let mut encrypted = Vec::new();
        crate::writer::output::with_buffer_sink(&mut encrypted, |out| {
            write_encrypted_objstm_stream_with_extends(
                out,
                body(),
                CompressStreams::No,
                NewlineBeforeEndstream::Never,
                extends,
                crate::ObjectRef::new(3, 0),
                &context,
            )
        })
        .expect("encrypted compact ObjStm");
        outputs.push(encrypted);

        let mut encrypted_qdf = Vec::new();
        crate::writer::output::with_buffer_sink(&mut encrypted_qdf, |out| {
            write_encrypted_objstm_stream_with_extends_qdf(
                out,
                body(),
                extends,
                4,
                NewlineBeforeEndstream::Never,
                crate::ObjectRef::new(3, 0),
                &context,
            )
        })
        .expect("encrypted QDF ObjStm");
        outputs.push(encrypted_qdf);

        for output in outputs {
            assert!(output
                .windows(b"/Extends 7 2 R".len())
                .any(|window| window == b"/Extends 7 2 R"));
            assert!(output
                .windows(b"endstream".len())
                .any(|window| window == b"endstream"));
        }
    }
}
