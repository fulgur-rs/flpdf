#[path = "writer/object_streams.rs"]
pub(crate) mod object_streams;
pub use object_streams::ObjectStreamMode;

use crate::parser::Parser;
use crate::{filters, Dictionary, Object, ObjectRef, Pdf, Result, XrefForm, XrefOffset};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, Write};

/// Controls whether the full-rewrite path applies FlateDecode compression to
/// output streams.
///
/// # Byte-vs-observable policy
///
/// flpdf uses zlib (via the `flate2` crate) with `Compression::default()`,
/// which selects a different compression level and block layout than qpdf's
/// internal zlib build.  As a result, **flpdf's FlateDecode output is
/// observably equivalent to qpdf's (same decoded bytes) but will not be
/// byte-identical**.  The acceptance criterion for this toggle is round-trip
/// correctness (decoded bytes match), not byte-identical agreement with qpdf.
///
/// This tradeoff is intentional and documented here to avoid spending time
/// chasing byte-level zlib parity, which would require re-implementing qpdf's
/// exact compression parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressStreams {
    /// Apply FlateDecode to every output stream that does not already carry a
    /// filter chain flpdf cannot re-encode (e.g. DCTDecode, JPXDecode).
    ///
    /// For the full-rewrite path this means: decode the source stream through
    /// its declared filter pipeline and re-emit the result with a single
    /// `/FlateDecode` filter.  Streams whose decode or re-encode fails are
    /// emitted verbatim (the fallback preserves readability).
    ///
    /// This is the default — matching qpdf's behaviour for a plain
    /// `qpdf in.pdf out.pdf` invocation.
    #[default]
    Yes,
    /// Emit every output stream without any FlateDecode compression.
    ///
    /// For the full-rewrite path: decode the source stream and write the raw
    /// bytes without any `/Filter`.  Streams whose decode fails (e.g. because
    /// the declared filter is `DCTDecode` / `JPXDecode` and the image data is
    /// opaque to flpdf) are passed through verbatim — their original `/Filter`
    /// chain is preserved so the output remains readable.
    No,
}

/// Controls whether a newline is explicitly inserted immediately before the
/// `endstream` keyword.
///
/// ISO 32000-1 §7.3.8.1 requires an end-of-line marker before `endstream`.
/// qpdf exposes this as `--newline-before-endstream=y/n`.  The default is
/// [`NewlineBeforeEndstream::Yes`], matching qpdf's default behaviour.
///
/// # Byte-level semantics
///
/// When `Yes`: exactly one `b'\n'` is written immediately before `endstream`,
/// regardless of whether the stream payload already ends with a newline.  The
/// `/Length` dictionary entry is **not** modified — it continues to reflect the
/// raw payload length only, not the extra newline byte (matching qpdf parity).
///
/// When `No`: no newline is written before `endstream` unless the payload does
/// not already end with a newline character (`\n` or `\r`), in which case a
/// single `b'\n'` is appended to maintain ISO 32000-1 parseability.  The
/// `/Length` value is likewise unmodified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewlineBeforeEndstream {
    /// Explicitly write exactly one `b'\n'` before `endstream` (default).
    ///
    /// This guarantees the invariant required by ISO 32000-1 §7.3.8.1 and
    /// matches qpdf's `--newline-before-endstream=y` behaviour.
    #[default]
    Yes,
    /// Do not add an extra newline before `endstream`.
    ///
    /// If the stream payload already ends with `\n` or `\r`, `endstream` is
    /// written immediately after the payload (adjacency).  If the payload does
    /// not end with a newline, a single `b'\n'` is inserted to preserve
    /// parseability — matching qpdf's `--newline-before-endstream=n` behaviour.
    No,
}

/// Options controlling [`write_pdf_with_options`].
///
/// Constructed via `Default::default()` or struct literal. The struct is
/// `#[non_exhaustive]` so additional fields can be added without breaking
/// existing callers.
#[non_exhaustive]
#[derive(Debug, Default, Clone)]
pub struct WriteOptions {
    /// Override the trailer `/ID`'s second element (the changing identifier)
    /// with qpdf's static-id constant — the first 32 hex digits of π. The
    /// first element (the permanent identifier) is preserved from the input
    /// trailer when present; if absent, both elements are set to the constant.
    /// Mirrors `qpdf --static-id` and is intended for byte-identical testing.
    pub static_id: bool,

    /// Enforce a minimum PDF version in the output header.
    ///
    /// The effective version is `max(source_version, min_version)`.  Only
    /// applies to the new-generation write paths (`write_qdf`, linearize);
    /// the incremental-update path (`write_pdf`) leaves the source header
    /// untouched.  Format: `"1.3"`, `"1.7"`, etc.
    ///
    /// Mirrors `qpdf --min-version`.
    pub min_version: Option<String>,

    /// Force the output PDF version header to exactly this value, ignoring the
    /// source version and the linearize floor.
    ///
    /// Mirrors `qpdf --force-version`.
    pub force_version: Option<String>,

    /// When `true`, suppress the `%% Original object ID: N M` comments that the
    /// QDF writer would otherwise emit before each object.
    ///
    /// Mirrors `qpdf --no-original-object-ids`. qpdf's own help: *"Omit
    /// comments in a QDF file indicating the object ID an object had in the
    /// original file."* Observed against qpdf 11.9.0, this flag affects **only**
    /// QDF output (`qpdf --qdf` vs `qpdf --qdf --no-original-object-ids`); JSON
    /// v1 and v2 output are byte-identical with or without it, so this field is
    /// intentionally **not** wired into any JSON path.
    ///
    /// `flpdf`'s current [`write_qdf`] does not yet emit the `%% Original
    /// object ID` comments at all (the QDF-comment body is tracked under epic
    /// `flpdf-9hc.6`). This flag is therefore plumbed through `WriteOptions`
    /// for acceptance and forward-compatibility; the eventual QDF-comment
    /// emitter will read this bool to decide whether to skip the annotation.
    /// With no emission point today, the default (`false`) and the flag both
    /// produce byte-identical QDF output.
    pub no_original_object_ids: bool,

    /// When `true`, decode every stream through its filter pipeline and re-emit
    /// the document end-to-end with a single `/FlateDecode` filter applied to
    /// each stream.  The output contains no `/Prev` chain, no `ObjStm`, and no
    /// xref-stream-only keys.
    ///
    /// When `false` (the default) the existing incremental-update write path is
    /// used, preserving the source bytes verbatim.
    pub full_rewrite: bool,

    /// Object stream emission policy for the output.
    ///
    /// Mirrors `qpdf --object-streams=preserve|disable|generate`. Defaults to
    /// [`ObjectStreamMode::Preserve`], matching qpdf's behaviour for a plain
    /// `qpdf in.pdf out.pdf` invocation.
    ///
    /// Only consulted by writer paths that emit ObjStms; the incremental copy
    /// path that simply appends to the source bytes ignores it.
    pub object_streams: ObjectStreamMode,

    /// Stream compression policy for the full-rewrite path.
    ///
    /// [`CompressStreams::Yes`] (the default) decodes each stream and
    /// re-encodes it with a single `/FlateDecode` filter, matching qpdf's
    /// default behaviour.  [`CompressStreams::No`] decodes each stream and
    /// emits the raw bytes without any filter; streams that cannot be decoded
    /// (e.g. `DCTDecode`/`JPXDecode` image data) are passed through verbatim.
    ///
    /// Only consulted by the full-rewrite path (`full_rewrite = true`);
    /// it governs regular indirect streams, ObjStm containers, and the
    /// xref stream alike. The incremental-update path and `write_qdf`
    /// are unaffected.
    pub compress_streams: CompressStreams,

    /// Whether to insert a newline immediately before each `endstream` keyword.
    ///
    /// ISO 32000-1 §7.3.8.1 requires an end-of-line marker before `endstream`.
    /// [`NewlineBeforeEndstream::Yes`] (the default) always writes exactly one
    /// `b'\n'` before `endstream`, matching qpdf's `--newline-before-endstream=y`
    /// behaviour.  [`NewlineBeforeEndstream::No`] omits the extra newline when
    /// the stream payload already ends with a newline character.
    ///
    /// The `/Length` value in the stream dictionary is **not** affected by this
    /// setting — it always reflects the raw payload byte count only.
    ///
    /// Applied to every stream in the full-rewrite output.  The incremental-update
    /// path emits streams verbatim from the input and is unaffected.
    pub newline_before_endstream: NewlineBeforeEndstream,

    /// Emit the document in QDF (Query Data Format) mode.
    ///
    /// When `true` (and `full_rewrite` is also `true`), every stream that uses a
    /// "safe text" filter chain — [`FlateDecode`], [`LZWDecode`], [`ASCIIHexDecode`],
    /// [`ASCII85Decode`], [`RunLengthDecode`] — is fully decoded and written as raw
    /// bytes.  The `/Filter` and `/DecodeParms` entries are removed from the stream
    /// dictionary and `/Length` is updated to the decoded byte count, making the
    /// stream data human-readable in a text editor.
    ///
    /// Image/binary codecs that flpdf cannot decompress — `DCTDecode`, `JBIG2Decode`,
    /// `JPXDecode`, `CCITTFaxDecode` — and any unknown filter are left **untouched**:
    /// the compressed bytes and the original `/Filter` chain are preserved verbatim.
    /// This matches qpdf's own QDF behaviour.
    ///
    /// When `true`, this setting takes precedence over [`compress_streams`] for the
    /// per-object stream emission: the stream is always emitted decompressed regardless
    /// of the `compress_streams` value.  The xref stream and ObjStm containers are
    /// governed solely by `compress_streams` and are not affected by this flag
    /// (QDF-specific xref/ObjStm behaviour is handled by later epic layers).
    ///
    /// The CLI flag `--qdf` is wired up in epic layer 6.8; until then this field
    /// is the library-level entry point.  Test via
    /// `WriteOptions { qdf: true, full_rewrite: true, .. }`.
    ///
    /// [`FlateDecode`]: https://pdf.pizza/spec/7.4.4
    /// [`LZWDecode`]: https://pdf.pizza/spec/7.4.4
    /// [`ASCIIHexDecode`]: https://pdf.pizza/spec/7.4.2
    /// [`ASCII85Decode`]: https://pdf.pizza/spec/7.4.3
    /// [`RunLengthDecode`]: https://pdf.pizza/spec/7.4.5
    /// [`compress_streams`]: WriteOptions::compress_streams
    pub qdf: bool,
}

/// Parse a PDF version string of the form `"M.m"` into `(major, minor)`.
///
/// Returns `None` for any string that does not match `digit+ '.' digit+`.
/// Only `1.x` documents are common in practice; `2.0` uses the same syntax.
pub fn parse_pdf_version(v: &str) -> Option<(u8, u8)> {
    let (major, minor) = v.split_once('.')?;
    let major: u8 = major.parse().ok()?;
    let minor: u8 = minor.parse().ok()?;
    Some((major, minor))
}

/// Compute the effective PDF version to write given the source version, the
/// caller-supplied options, and whether the output is linearized.
///
/// Rule (mirrors qpdf):
/// 1. If `options.force_version` is set, use it verbatim.
/// 2. Otherwise start from `max(source, min_version_option)`.
/// 3. If `linearize` is true, apply an additional `max(…, "1.2")` floor
///    (linearized PDFs require at least PDF 1.2).
///
/// If the version strings cannot be parsed the function falls back to the
/// `source` string unchanged (rather than panicking) so callers do not need to
/// validate before calling.
///
/// # `/Catalog /Version` reconciliation (qpdf semantics)
///
/// ISO 32000-1 §7.5.2 lets a `/Catalog /Version` entry override the header
/// when it is *higher*; readers compute the effective version as
/// `max(header, catalog)`. Empirically (verified against qpdf 11.x with
/// `qpdf --force-version` / `--min-version` on fixtures carrying a
/// `/Catalog /Version`), qpdf rewrites **only** the `%PDF-x.y` header line and
/// never strips, lowers, or otherwise touches `/Catalog /Version` — even when
/// it is higher than the chosen header. It also does **not** fold
/// `/Catalog /Version` into the source floor: the `--min-version` baseline is
/// the header version alone, not `max(header, catalog)`.
///
/// "Reconciled per qpdf semantics" therefore means *leave `/Catalog /Version`
/// alone* — `source` here is the header version and this function deliberately
/// does not read the Catalog. This keeps the implementation minimal and
/// byte-faithful to qpdf rather than guessing at a broader reconciliation.
pub fn effective_pdf_version<'a>(
    source: &'a str,
    options: &'a WriteOptions,
    linearize: bool,
) -> &'a str {
    // --force-version wins outright, but only when the value is a valid version string.
    // Silently ignore invalid values (same treatment as invalid min_version) so that
    // callers that cannot pre-validate do not produce a corrupted PDF header.
    if let Some(ref forced) = options.force_version {
        if parse_pdf_version(forced).is_some() {
            return forced.as_str();
        }
    }

    // Parse source; bail to source string on failure.
    let Some(mut best) = parse_pdf_version(source) else {
        return source;
    };

    // Apply --min-version floor.
    if let Some(ref min_v) = options.min_version {
        if let Some(min_parsed) = parse_pdf_version(min_v) {
            if min_parsed > best {
                best = min_parsed;
            }
        }
    }

    // Apply linearize floor (PDF spec requires >= 1.2).
    if linearize {
        let lin_floor = (1u8, 2u8);
        if lin_floor > best {
            best = lin_floor;
        }
    }

    // If best == source parsed, return the original source slice to avoid an
    // allocation.  Otherwise find which option string owns this version.
    if parse_pdf_version(source) == Some(best) {
        return source;
    }
    if let Some(ref min_v) = options.min_version {
        if parse_pdf_version(min_v) == Some(best) {
            return min_v.as_str();
        }
    }
    // Linearize floor "1.2" — only reached when best == (1,2) and neither
    // source nor min_version matched.
    "1.2"
}

/// Binary header marker emitted by qpdf on the second line of every output
/// PDF (immediately after the `%PDF-x.y` version line).  The four bytes are
/// all > 127, which signals to file-transfer tools that the file is binary,
/// as recommended by the PDF specification.  We fix these to qpdf's values so
/// that flpdf output is byte-identical to qpdf output for the header section.
///
/// Hex: `25 BF F7 A2 FE 0A`  →  `%` + four high bytes + newline.
const QPDF_BINARY_MARKER: &[u8] = b"%\xbf\xf7\xa2\xfe\n";

/// qpdf's static-id constant: the first 32 hex digits of π, encoded as 16 raw
/// bytes so the trailer emits `<31415926535897932384626433832795>`.
pub(crate) const QPDF_STATIC_ID: [u8; 16] = [
    0x31, 0x41, 0x59, 0x26, 0x53, 0x58, 0x97, 0x93, 0x23, 0x84, 0x62, 0x64, 0x33, 0x83, 0x27, 0x95,
];

/// Write `pdf` as an incrementally-updated revision (qpdf's default `--object-streams=preserve` mode).
///
/// The original bytes are copied to `out` unchanged, then a single update section is
/// appended for every object that has been mutated via [`Pdf::set_object`]. The xref
/// table form (table vs. stream) is preserved from the input, so the output remains
/// readable by older PDF consumers when the source used `xref` tables and stays
/// compact when the source used cross-reference streams.
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
pub fn write_pdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, out: W) -> Result<()> {
    write_pdf_with_options(pdf, out, &WriteOptions::default())
}

/// Like [`write_pdf`] but with caller-supplied [`WriteOptions`].
pub fn write_pdf_with_options<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriteOptions,
) -> Result<()> {
    if options.full_rewrite {
        return write_pdf_full_rewrite(pdf, out, options);
    }
    write_pdf_incremental(pdf, out, options)
}

fn write_pdf_incremental<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut bytes = pdf.source_bytes()?;
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }

    let source_offsets = build_source_offsets(pdf.source_xref_offsets());
    let source_xref_offsets = build_source_xref_offsets(pdf.source_xref_entries());
    let (touched_object_refs, deleted_object_refs, touched_objstm_members) =
        collect_touched_object_refs(pdf);
    let mut xref_offsets = write_incremental_objects(&mut bytes, pdf, &touched_object_refs)?;
    let deleted_table_entries = build_deleted_table_entries(pdf, &deleted_object_refs);
    let rewritten_stream_offsets =
        write_updated_object_streams(&mut bytes, pdf, &touched_objstm_members)?;
    xref_offsets.extend(rewritten_stream_offsets);
    let final_offsets =
        merge_source_and_touched_offsets(&source_offsets, &xref_offsets, &deleted_table_entries);
    let final_xref_offsets = merge_source_and_touched_offsets_for_xref_stream(
        &source_xref_offsets,
        &xref_offsets,
        &deleted_object_refs,
    );
    let mut object_count = match pdf.last_xref_form() {
        XrefForm::Table => resolve_object_count(pdf.trailer().get("Size"), &final_offsets),
        XrefForm::Stream => {
            resolve_xref_stream_object_count(pdf.trailer().get("Size"), &final_xref_offsets)
        }
    };

    let xref_offset = match pdf.last_xref_form() {
        XrefForm::Table => write_incremental_xref(&mut bytes, &final_offsets)?,
        XrefForm::Stream => {
            let xref_object_number = next_xref_stream_object_number(&final_xref_offsets)?;
            object_count = object_count.max(xref_object_number as usize + 1);
            let xref_offset = write_incremental_xref_stream(
                &mut bytes,
                pdf.trailer(),
                &final_xref_offsets,
                &root_ref,
                xref_object_number,
                object_count,
                pdf.previous_xref_offset(),
            )?;
            xref_offset
        }
    };
    write_incremental_trailer(
        &mut bytes,
        pdf,
        &root_ref,
        object_count,
        pdf.previous_xref_offset(),
        xref_offset,
        options,
    )?;

    out.write_all(&bytes)?;
    Ok(())
}

type RewrittenObjStmMembers = BTreeMap<ObjectRef, BTreeSet<ObjectRef>>;

fn collect_touched_object_refs<R: Read + Seek>(
    pdf: &Pdf<R>,
) -> (Vec<ObjectRef>, Vec<ObjectRef>, RewrittenObjStmMembers) {
    let mut touched = BTreeSet::new();
    let mut deleted = BTreeSet::new();
    let mut objstm_touched: BTreeMap<ObjectRef, BTreeSet<ObjectRef>> = BTreeMap::new();

    for object_ref in pdf.deleted_object_refs() {
        deleted.insert(object_ref);
    }

    for object_ref in pdf.resolved_object_refs() {
        if deleted.contains(&object_ref) {
            continue;
        }
        if let Some((stream_ref, _index)) = pdf.compressed_parent(object_ref) {
            objstm_touched
                .entry(stream_ref)
                .or_default()
                .insert(object_ref);
            continue;
        }

        touched.insert(object_ref);
    }

    (
        touched.into_iter().collect(),
        deleted.into_iter().collect(),
        objstm_touched,
    )
}

#[derive(Clone, Copy)]
enum XrefTableEntry {
    InUse { generation: u16, offset: usize },
    Free { generation: u16, next: u32 },
}

fn write_incremental_objects<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &mut Pdf<R>,
    touched_object_refs: &[ObjectRef],
) -> Result<BTreeMap<u32, (u16, usize)>> {
    let mut updated_offsets = BTreeMap::new();

    for object_ref in touched_object_refs {
        let object = pdf.resolve(*object_ref)?;
        let Some(offset) = write_object(bytes, *object_ref, &object)? else {
            continue;
        };
        updated_offsets.insert(object_ref.number, (object_ref.generation, offset));
    }

    Ok(updated_offsets)
}

fn write_updated_object_streams<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &mut Pdf<R>,
    members_by_stream: &RewrittenObjStmMembers,
) -> Result<BTreeMap<u32, (u16, usize)>> {
    let mut updated_offsets = BTreeMap::new();

    for (stream_ref, member_refs) in members_by_stream {
        let stream_object = pdf.resolve(*stream_ref)?;
        let Object::Stream(stream) = stream_object else {
            return Err(crate::Error::Unsupported(format!(
                "object {} is not an object stream",
                stream_ref.number
            )));
        };

        let (rebuilt_data, rebuilt_first) = rebuild_object_stream(&stream, member_refs, pdf)?;

        let mut stream_dict = stream.dict.clone();
        stream_dict.insert(
            "First",
            Object::Integer(i64::try_from(rebuilt_first).map_err(|_| {
                crate::Error::Unsupported("object stream /First too large".to_string())
            })?),
        );
        stream_dict.insert(
            "Length",
            Object::Integer(i64::try_from(rebuilt_data.len()).map_err(|_| {
                crate::Error::Unsupported("object stream /Length too large".to_string())
            })?),
        );

        let rebuilt_stream = Object::Stream(crate::Stream::new(stream_dict, rebuilt_data));
        let offset = write_object(bytes, *stream_ref, &rebuilt_stream)?
            .expect("writing object always returns an offset");
        updated_offsets.insert(stream_ref.number, (stream_ref.generation, offset));
    }

    Ok(updated_offsets)
}

fn rebuild_object_stream<R: Read + Seek>(
    stream: &crate::Stream,
    updated_members: &BTreeSet<ObjectRef>,
    pdf: &mut Pdf<R>,
) -> Result<(Vec<u8>, usize)> {
    let stream_data = filters::decode_stream_data(&stream.dict, &stream.data)?;

    let object_count = usize::try_from(parse_non_negative_i64(
        stream
            .dict
            .get("N")
            .ok_or(crate::Error::Missing("Object stream /N"))?,
        "Object stream /N",
    )?)
    .map_err(|_| crate::Error::Unsupported("Object stream /N does not fit usize".to_string()))?;

    let first = usize::try_from(parse_non_negative_i64(
        stream
            .dict
            .get("First")
            .ok_or(crate::Error::Missing("Object stream /First"))?,
        "Object stream /First",
    )?)
    .map_err(|_| {
        crate::Error::Unsupported("Object stream /First does not fit usize".to_string())
    })?;

    let mut header_parser = Parser::new(&stream_data);
    let mut members = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        let number = u32::try_from(parse_non_negative_i64_value(
            header_parser.integer_for_indirect()?,
            "object stream object number",
        )?)
        .map_err(|_| {
            crate::Error::Unsupported("object stream object number does not fit u32".to_string())
        })?;
        let offset = usize::try_from(parse_non_negative_i64_value(
            header_parser.integer_for_indirect()?,
            "object stream object offset",
        )?)
        .map_err(|_| {
            crate::Error::Unsupported("object stream object offset does not fit usize".to_string())
        })?;
        members.push((number, offset));
    }

    let mut decoded_members = Vec::with_capacity(object_count);
    for (index, (number, offset)) in members.iter().enumerate() {
        let start = first.checked_add(*offset).ok_or_else(|| {
            crate::Error::Unsupported("object stream offset overflow".to_string())
        })?;
        let end = if index + 1 < members.len() {
            first.checked_add(members[index + 1].1).ok_or_else(|| {
                crate::Error::Unsupported("object stream offset overflow".to_string())
            })?
        } else {
            stream_data.len()
        };

        if start > end || end > stream_data.len() {
            return Err(crate::Error::parse(
                0,
                "compressed object offset out of range",
            ));
        }

        let object_ref = ObjectRef::new(*number, 0);
        let object = if updated_members.contains(&object_ref) {
            pdf.resolve(object_ref)?
        } else {
            let mut parser = Parser::new(&stream_data[start..end]);
            parser.object()?
        };

        decoded_members.push((object_ref, object));
    }

    let mut header = Vec::new();
    let mut body = Vec::new();
    let mut object_offsets = Vec::with_capacity(decoded_members.len());

    for (_, object) in &decoded_members {
        object_offsets.push(body.len());
        object.write_pdf(&mut body);
        body.push(b'\n');
    }

    for ((object_ref, _), offset) in decoded_members.iter().zip(object_offsets.iter()) {
        header.extend_from_slice(format!("{} {} ", object_ref.number, offset).as_bytes());
    }

    let rebuilt_first = header.len();

    let mut rebuilt_data = header;
    rebuilt_data.extend_from_slice(&body);

    let encoded = filters::encode_stream_data(&stream.dict, &rebuilt_data)?;
    Ok((encoded, rebuilt_first))
}

fn parse_non_negative_i64(value: &crate::Object, context: &str) -> Result<i64> {
    let crate::Object::Integer(integer) = value else {
        return Err(crate::Error::parse(0, format!("{context} is not integer")));
    };
    if *integer < 0 {
        return Err(crate::Error::parse(0, format!("{context} is negative")));
    }
    Ok(*integer)
}

fn parse_non_negative_i64_value(value: i64, context: &str) -> Result<i64> {
    if value < 0 {
        return Err(crate::Error::parse(0, format!("{context} is negative")));
    }
    Ok(value)
}

fn write_object(
    bytes: &mut Vec<u8>,
    object_ref: ObjectRef,
    object: &Object,
) -> Result<Option<usize>> {
    let offset = bytes.len();
    bytes.extend_from_slice(
        format!("{} {} obj\n", object_ref.number, object_ref.generation).as_bytes(),
    );
    object.write_pdf(bytes);
    bytes.extend_from_slice(b"\nendobj\n");

    Ok(Some(offset))
}

fn merge_source_and_touched_offsets(
    source_offsets: &BTreeMap<u32, (u16, usize)>,
    touched_offsets: &BTreeMap<u32, (u16, usize)>,
    deleted_entries: &BTreeMap<u32, (u16, u32)>,
) -> BTreeMap<u32, XrefTableEntry> {
    let mut merged = source_offsets
        .iter()
        .map(|(number, (generation, offset))| {
            (
                *number,
                XrefTableEntry::InUse {
                    generation: *generation,
                    offset: *offset,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (number, (generation, offset)) in touched_offsets {
        merged.insert(
            *number,
            XrefTableEntry::InUse {
                generation: *generation,
                offset: *offset,
            },
        );
    }
    for (number, (generation, next)) in deleted_entries {
        merged.insert(
            *number,
            XrefTableEntry::Free {
                generation: *generation,
                next: *next,
            },
        );
    }
    merged
}

fn merge_source_and_touched_offsets_for_xref_stream(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    touched_offsets: &BTreeMap<u32, (u16, usize)>,
    deleted_object_refs: &[ObjectRef],
) -> BTreeMap<u32, (u16, XrefOffset)> {
    let mut merged = source_offsets.clone();
    for (number, (generation, offset)) in touched_offsets {
        merged.insert(*number, (*generation, XrefOffset::Offset(*offset as u64)));
    }
    for (number, (generation, next)) in build_deleted_entries(source_offsets, deleted_object_refs) {
        merged.insert(number, (generation, XrefOffset::Free { next }));
    }
    merged
}

fn build_deleted_table_entries<R: Read + Seek>(
    pdf: &Pdf<R>,
    deleted_object_refs: &[ObjectRef],
) -> BTreeMap<u32, (u16, u32)> {
    let source_offsets = build_source_xref_offsets(pdf.source_xref_entries());
    build_deleted_entries(&source_offsets, deleted_object_refs)
}

fn build_deleted_entries(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    deleted_object_refs: &[ObjectRef],
) -> BTreeMap<u32, (u16, u32)> {
    if deleted_object_refs.is_empty() {
        return BTreeMap::new();
    }

    let mut deleted_refs = deleted_object_refs.to_vec();
    deleted_refs.sort_by_key(|object_ref| object_ref.number);
    deleted_refs.dedup_by_key(|object_ref| object_ref.number);

    let mut entries = BTreeMap::new();
    let first_deleted = deleted_refs[0].number;
    entries.insert(0, (65535, first_deleted));

    deleted_refs
        .iter()
        .enumerate()
        .for_each(|(index, object_ref)| {
            let next = deleted_refs
                .get(index + 1)
                .map(|next_ref| next_ref.number)
                .unwrap_or_else(|| source_free_head(source_offsets));
            entries.insert(
                object_ref.number,
                (
                    incremented_generation(current_generation(source_offsets, *object_ref)),
                    next,
                ),
            );
        });

    entries
}

fn source_free_head(source_offsets: &BTreeMap<u32, (u16, XrefOffset)>) -> u32 {
    match source_offsets.get(&0) {
        Some((_, XrefOffset::Free { next })) => *next,
        _ => 0,
    }
}

fn current_generation(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    object_ref: ObjectRef,
) -> u16 {
    source_offsets
        .get(&object_ref.number)
        .map(|(generation, _)| *generation)
        .unwrap_or(object_ref.generation)
}

fn incremented_generation(generation: u16) -> u16 {
    generation.saturating_add(1)
}

fn next_xref_stream_object_number(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
) -> Result<u32> {
    source_offsets
        .keys()
        .copied()
        .next_back()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("xref object number does not fit u32".to_string()))
}

fn build_source_xref_offsets(
    source_offsets: BTreeMap<ObjectRef, XrefOffset>,
) -> BTreeMap<u32, (u16, XrefOffset)> {
    let mut offsets = BTreeMap::new();
    for (object_ref, xref_offset) in source_offsets {
        offsets.insert(object_ref.number, (object_ref.generation, xref_offset));
    }
    offsets
}

fn build_source_offsets(entries: Vec<(ObjectRef, u64)>) -> BTreeMap<u32, (u16, usize)> {
    let mut source_offsets = BTreeMap::new();

    for (object_ref, xref_offset) in entries {
        let next = source_offsets
            .get(&object_ref.number)
            .copied()
            .map(|(generation, _)| generation)
            .unwrap_or(0);

        if object_ref.generation >= next {
            source_offsets.insert(
                object_ref.number,
                (object_ref.generation, xref_offset as usize),
            );
        }
    }

    source_offsets
}

fn resolve_object_count(
    declared_size: Option<&crate::Object>,
    source_offsets: &BTreeMap<u32, XrefTableEntry>,
) -> usize {
    let max_object_number = source_offsets.keys().next_back().copied().unwrap_or(0) as usize;
    let declared = declared_size
        .and_then(|size| match size {
            crate::Object::Integer(value) => usize::try_from(*value).ok(),
            _ => None,
        })
        .unwrap_or(0);

    declared.max(max_object_number.saturating_add(1)).max(1)
}

fn resolve_xref_stream_object_count(
    declared_size: Option<&crate::Object>,
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
) -> usize {
    let max_object_number = source_offsets.keys().next_back().copied().unwrap_or(0) as usize;
    let declared = declared_size
        .and_then(|size| match size {
            crate::Object::Integer(value) => usize::try_from(*value).ok(),
            _ => None,
        })
        .unwrap_or(0);

    declared.max(max_object_number.saturating_add(1)).max(1)
}

fn write_incremental_xref(
    bytes: &mut Vec<u8>,
    source_offsets: &BTreeMap<u32, XrefTableEntry>,
) -> Result<usize> {
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n");
    let (free_generation, free_next) = match source_offsets.get(&0) {
        Some(XrefTableEntry::Free { generation, next }) => (*generation, *next),
        _ => (65535, 0),
    };
    bytes.extend_from_slice(format!("{free_next:010} {free_generation:05} f \n").as_bytes());

    let mut object_numbers: Vec<u32> = source_offsets.keys().copied().filter(|n| *n > 0).collect();
    if object_numbers.is_empty() {
        return Ok(xref_offset);
    }

    object_numbers.sort_unstable();

    let mut i = 0;
    while i < object_numbers.len() {
        let start = object_numbers[i];
        let mut end = start;

        while i + 1 < object_numbers.len() && object_numbers[i + 1] == end + 1 {
            i += 1;
            end = object_numbers[i];
        }

        let count = end - start + 1;
        bytes.extend_from_slice(format!("{} {}\n", start, count).as_bytes());
        for object_number in start..=end {
            let entry = source_offsets.get(&object_number).ok_or_else(|| {
                crate::Error::Unsupported(
                    "incremental xref subsection is missing object entry".to_string(),
                )
            })?;

            match entry {
                XrefTableEntry::InUse { generation, offset } => {
                    bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes())
                }
                XrefTableEntry::Free { generation, next } => {
                    bytes.extend_from_slice(format!("{next:010} {generation:05} f \n").as_bytes())
                }
            }
        }

        i += 1;
    }

    Ok(xref_offset)
}

fn write_incremental_xref_stream(
    bytes: &mut Vec<u8>,
    trailer: &Dictionary,
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    root_ref: &ObjectRef,
    xref_object_number: u32,
    object_count: usize,
    previous_xref_offset: u64,
) -> Result<usize> {
    let xref_offset = bytes.len();
    if source_offsets.contains_key(&xref_object_number) {
        return Err(crate::Error::Unsupported(format!(
            "xref stream object number {} already exists",
            xref_object_number
        )));
    }

    let mut offsets = source_offsets.clone();
    offsets.insert(0, (65535, XrefOffset::Free { next: 0 }));
    offsets.insert(
        xref_object_number,
        (
            0,
            XrefOffset::Offset(u64::try_from(xref_offset).map_err(|_| {
                crate::Error::Unsupported("xref stream object offset does not fit u64".to_string())
            })?),
        ),
    );

    let mut object_numbers: Vec<u32> = offsets.keys().copied().collect();
    object_numbers.sort_unstable();

    let mut ranges = Vec::new();
    let mut index = 0;
    while index < object_numbers.len() {
        let start = object_numbers[index];
        let mut end = start;
        while index + 1 < object_numbers.len() && object_numbers[index + 1] == end + 1 {
            index += 1;
            end = object_numbers[index];
        }

        ranges.push((start, end.saturating_sub(start).saturating_add(1)));
        index += 1;
    }

    let stream_data = build_xref_stream_bytes(&offsets, &ranges)?;

    let mut index_array = Vec::with_capacity(ranges.len() * 2);
    for (start, count) in &ranges {
        index_array.push(Object::Integer(i64::from(*start)));
        index_array.push(Object::Integer(i64::from(*count)));
    }

    let size = i64::try_from(object_count)
        .map_err(|_| crate::Error::Unsupported("xref stream /Size does not fit i64".to_string()))?;
    let mut stream_dict = trailer.clone();
    strip_incremental_trailer_keys(&mut stream_dict);
    stream_dict.insert("Type", Object::Name(b"XRef".to_vec()));
    stream_dict.insert("Size", Object::Integer(size));
    stream_dict.insert(
        "W",
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(8),
            Object::Integer(4),
        ]),
    );
    stream_dict.insert("Index", Object::Array(index_array));
    stream_dict.insert("Root", Object::Reference(*root_ref));
    stream_dict.insert(
        "Length",
        Object::Integer(i64::try_from(stream_data.len()).map_err(|_| {
            crate::Error::Unsupported("xref stream /Length does not fit i64".to_string())
        })?),
    );
    stream_dict.insert(
        "Prev",
        Object::Integer(previous_xref_offset.try_into().map_err(|_| {
            crate::Error::Unsupported("startxref offset does not fit i64".to_string())
        })?),
    );

    let xref_stream = Object::Stream(crate::Stream::new(stream_dict, stream_data));
    write_object(bytes, ObjectRef::new(xref_object_number, 0), &xref_stream)?;

    Ok(xref_offset)
}

fn build_xref_stream_bytes(
    offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    ranges: &[(u32, u32)],
) -> Result<Vec<u8>> {
    let mut stream_data = Vec::new();
    for &(start, count) in ranges {
        let end = start.checked_add(count).ok_or_else(|| {
            crate::Error::Unsupported("xref stream range does not fit u32".to_string())
        })?;
        for object_number in start..end {
            let (generation, xref_offset) = offsets.get(&object_number).ok_or_else(|| {
                crate::Error::Unsupported(
                    "incremental xref stream is missing object entry".to_string(),
                )
            })?;

            let object_type = match (object_number, xref_offset) {
                (0, _) | (_, XrefOffset::Free { .. }) => 0,
                (_, XrefOffset::Compressed { .. }) => 2,
                _ => 1,
            };

            stream_data.push(object_type);
            match xref_offset {
                XrefOffset::Free { next } => {
                    stream_data.extend_from_slice(&(u64::from(*next).to_be_bytes()[0..8]));
                    stream_data.extend_from_slice(&u32::from(*generation).to_be_bytes()[0..4]);
                }
                XrefOffset::Offset(offset) => {
                    stream_data.extend_from_slice(&((*offset).to_be_bytes()[0..8]));
                    stream_data.extend_from_slice(&u32::from(*generation).to_be_bytes()[0..4]);
                }
                XrefOffset::Compressed { stream, index } => {
                    stream_data.extend_from_slice(&(u64::from(*stream).to_be_bytes()[0..8]));
                    stream_data.extend_from_slice(&u32::to_be_bytes(*index)[0..4]);
                }
            }
        }
    }

    Ok(stream_data)
}

fn write_incremental_trailer<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &Pdf<R>,
    root_ref: &ObjectRef,
    object_count: usize,
    previous_xref_offset: u64,
    xref_offset: usize,
    options: &WriteOptions,
) -> Result<()> {
    let mut trailer = pdf.trailer().clone();
    strip_xref_stream_trailer_keys(&mut trailer);
    trailer.insert("Size", Object::Integer(object_count as i64));
    trailer.insert("Root", Object::Reference(*root_ref));
    trailer.insert(
        "Prev",
        Object::Integer(previous_xref_offset.try_into().map_err(|_| {
            crate::Error::Unsupported("startxref offset does not fit i64".to_string())
        })?),
    );
    if options.static_id {
        apply_static_id(&mut trailer);
    } else {
        apply_random_id(&mut trailer);
    }

    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(bytes);
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    Ok(())
}

/// Replace `trailer`'s `/ID` so the changing identifier (element 2) is qpdf's
/// static-id constant. The permanent identifier (element 1) is taken from the
/// existing `/ID` when its shape matches a 2-element array of strings; in any
/// other case (missing, wrong arity, wrong types) both elements fall back to
/// the constant — matching qpdf's behaviour on inputs without a usable `/ID`.
pub(crate) fn apply_static_id(trailer: &mut Dictionary) {
    let pi_id = Object::String(QPDF_STATIC_ID.to_vec());
    let first_id = match trailer.get("ID") {
        Some(Object::Array(values))
            if values.len() == 2
                && matches!(values[0], Object::String(_))
                && matches!(values[1], Object::String(_)) =>
        {
            values[0].clone()
        }
        _ => pi_id.clone(),
    };
    trailer.insert("ID", Object::Array(vec![first_id, pi_id]));
}

/// Generate a fresh 16-byte file identifier.
///
/// Mirrors qpdf's default-`/ID` algorithm in spirit: an MD5 digest seeded from
/// volatile per-invocation entropy (wall-clock nanoseconds, the process id, and
/// a strictly-monotonic process-global counter).  MD5 is already a direct
/// dependency, so no new crate is introduced.  The counter guarantees two calls
/// within the same nanosecond tick still produce distinct identifiers, which is
/// what makes "every save emits a different `/ID`" hold even for back-to-back
/// writes in a tight loop.
fn fresh_id_bytes() -> [u8; 16] {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    let mut hasher = md5::Md5::new();
    use md5::Digest as _;
    hasher.update(nanos.to_le_bytes());
    hasher.update(pid.to_le_bytes());
    hasher.update(seq.to_le_bytes());
    hasher.finalize().into()
}

/// Replace `trailer`'s `/ID` following the default (no-flag) identifier
/// strategy of ISO 32000-1 §14.4.
///
/// The permanent identifier (element 1) is preserved from the existing `/ID`
/// when its shape matches a 2-element array of strings — i.e. on a re-save of a
/// file that already carries a well-formed `/ID`.  In every other case
/// (missing, wrong arity, wrong element types — i.e. the first save by flpdf)
/// element 1 is freshly generated.  The changing identifier (element 2) is
/// **always** freshly generated, so the `/ID` varies on every save while the
/// permanent identifier remains stable across re-saves.  This matches qpdf's
/// default observable behaviour (random `/ID`s differ between runs).
pub(crate) fn random_id_array(source_id: Option<&Object>) -> Object {
    let first_id = match source_id {
        Some(Object::Array(values))
            if values.len() == 2
                && matches!(values[0], Object::String(_))
                && matches!(values[1], Object::String(_)) =>
        {
            values[0].clone()
        }
        _ => Object::String(fresh_id_bytes().to_vec()),
    };
    let second_id = Object::String(fresh_id_bytes().to_vec());
    Object::Array(vec![first_id, second_id])
}

/// Apply [`random_id_array`] to a trailer dictionary in place, reading the
/// existing `/ID` (if any) as the permanent-identifier source.
pub(crate) fn apply_random_id(trailer: &mut Dictionary) {
    let id = random_id_array(trailer.get("ID"));
    trailer.insert("ID", id);
}

fn strip_incremental_trailer_keys(trailer: &mut Dictionary) {
    strip_xref_stream_trailer_keys(trailer);
    trailer.remove("Prev");
}

fn strip_xref_stream_trailer_keys(trailer: &mut Dictionary) {
    let has_xref_stream_markers = matches!(trailer.get("Type"), Some(Object::Name(type_name)) if type_name.as_slice() == b"XRef")
        || trailer.get("XRefStm").is_some()
        || trailer.get("W").is_some()
        || trailer.get("Index").is_some();

    if !has_xref_stream_markers {
        return;
    }

    for key in [
        "Type",
        "F",
        "FFilter",
        "FDecodeParms",
        "W",
        "Index",
        "Length",
        "Filter",
        "DecodeParms",
        "XRefStm",
    ] {
        trailer.remove(key);
    }
}

/// Write `pdf` as a flat, qdf-style dump (qpdf's `--qdf` form).
///
/// Every known object is rewritten in `(number, generation)` order with no compression
/// applied, the cross-reference table is rebuilt as a classic `xref` section, and a
/// minimal trailer pointing at `/Root` is emitted. The result is intended for human
/// inspection, diffing, and reproducibility tests rather than smallest-on-disk output.
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`, and
/// [`crate::Error::Unsupported`] if the same object number appears more than once in
/// the cache (which would indicate a cross-reference table that we'd otherwise
/// misrender).
pub fn write_qdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, mut out: W) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut object_refs = pdf.object_refs();
    object_refs.sort_by_key(|object_ref| (object_ref.number, object_ref.generation));

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{}\n", pdf.version()).as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);

    let mut offsets = BTreeMap::<u32, (u16, usize)>::new();
    for object_ref in &object_refs {
        let object = pdf.resolve(*object_ref)?;
        if offsets
            .insert(object_ref.number, (object_ref.generation, bytes.len()))
            .is_some()
        {
            return Err(crate::Error::Unsupported(format!(
                "duplicate object number {} in xref table",
                object_ref.number
            )));
        }
        bytes.extend_from_slice(
            format!("{} {} obj\n", object_ref.number, object_ref.generation).as_bytes(),
        );
        object.write_pdf(&mut bytes);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    let object_count = object_refs
        .iter()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0)
        .saturating_add(1) as usize;

    bytes.extend_from_slice(format!("xref\n0 {}\n", object_count).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..object_count {
        match offsets.get(&(number as u32)) {
            Some((generation, offset)) => {
                bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes())
            }
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }

    let mut trailer = Dictionary::new();
    trailer.insert("Size", Object::Integer(object_count as i64));
    trailer.insert("Root", Object::Reference(root_ref));
    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(&mut bytes);
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());

    out.write_all(&bytes)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Full-rewrite path: decode+re-encode every stream, replaces incremental copy
// ---------------------------------------------------------------------------

/// Write `pdf` as a full non-incremental rewrite.
///
/// Every stream is decoded through its filter chain and re-encoded with a
/// single `/FlateDecode` filter.  The output has no `/Prev` chain and no
/// `ObjStm` container objects.  ObjStm member objects are emitted as ordinary
/// indirect objects.  XRef stream container objects are replaced by a freshly
/// rebuilt xref table (or xref stream, matching the input's form).
///
/// # Metadata preservation policy
///
/// The `/Info` dictionary (containing `/Producer`, `/CreationDate`, `/ModDate`,
/// `/Author`, `/Title`, `/Creator`, `/Keywords`, `/Subject`, `/Trapped`, etc.)
/// is preserved **verbatim** from the source document.  No fields are added,
/// removed, or rewritten — in particular no "modified by flpdf" suffix is
/// appended to `/Producer`.  This mirrors `qpdf`'s default behaviour
/// (`qpdf in.pdf out.pdf`) and is required for byte-identical round-trip tests.
///
/// # Scope limitations (TODO)
///
/// - **ObjStm dissolve**: Object streams are dissolved — members are emitted as
///   ordinary indirect objects.  There is currently no merging of existing
///   ObjStm containers back into the regular sequence; they are simply skipped.
///   A dedicated "renumber + pack into ObjStm" pass (flpdf-9hc.20.13) is a
///   future concern.
///
/// - **Encrypted documents**: authenticated inputs are rewritten as plaintext;
///   no encryption dictionary is emitted and no re-encryption is attempted.
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
fn write_pdf_full_rewrite<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
) -> Result<()> {
    // ── ObjStm-routing overview ───────────────────────────────────────────────
    // 1. Run the planner to decide which objects belong in ObjStm batches.
    // 2. Build a member→(container_num, index_in_batch) lookup.
    // 3. Allocate container object numbers above the highest input number.
    // 4. Emission loop: skip members (they'll be emitted via containers).
    // 5. After the loop: emit each container as a stream object.
    // 6. xref: for Stream form, insert Compressed entries for members.
    //    For Table form with non-empty batches: return Err (5.7 guard).
    // ─────────────────────────────────────────────────────────────────────────

    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut version = effective_pdf_version(pdf.version(), options, false).to_owned();

    // ── Step 1: run the ObjStm planner ───────────────────────────────────────
    let planner_config = object_streams::planner_config_from_options(options);
    let plan = object_streams::plan_object_streams(pdf, &planner_config)?;

    // Xref form selection: ObjStm-resident objects need type-2 xref entries,
    // which can only live in xref streams.  When the planner emits any batch
    // we therefore force-upgrade to `Stream` even if the source used a
    // classic xref table.  An empty plan respects the source form, so a
    // Disable-mode rewrite of a Table-form input still produces a classic
    // xref table.
    let effective_xref_form = if plan.batches.is_empty() {
        pdf.last_xref_form()
    } else {
        XrefForm::Stream
    };

    // PDF 1.5 introduced xref streams.  Bump the header floor to 1.5 whenever
    // the chosen xref form is `Stream`, overriding even an explicit
    // `--force-version` lower than 1.5.  This applies both when the source
    // was already a stream and when we just upgraded for ObjStm output.
    if matches!(effective_xref_form, XrefForm::Stream)
        && parse_pdf_version(&version).is_none_or(|v| v < (1, 5))
    {
        version = "1.5".to_string();
    }

    // ── Step 2 & 3: build member→batch lookup and allocate container numbers ─
    let mut object_refs = pdf.object_refs();
    object_refs.sort_by_key(|r| (r.number, r.generation));

    let existing_max: u32 = object_refs.iter().map(|r| r.number).max().unwrap_or(0);

    // Allocate a fresh object number for each container above existing_max.
    let container_refs: Vec<ObjectRef> = (1..=plan.batches.len())
        .map(|i| {
            existing_max
                .checked_add(i as u32)
                .map(|n| ObjectRef::new(n, 0))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            crate::Error::Unsupported(
                "full-rewrite: ObjStm container number overflows u32".to_string(),
            )
        })?;

    // member_to_batch: ObjectRef → (container_obj_num, index_in_batch)
    use std::collections::HashMap;
    let mut member_to_batch: HashMap<ObjectRef, (u32, u32)> = HashMap::new();
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_num = container_refs[batch_idx].number;
        for (idx_in_batch, &member_ref) in batch.iter().enumerate() {
            member_to_batch.insert(member_ref, (container_num, idx_in_batch as u32));
        }
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);
    if options.qdf {
        bytes.extend_from_slice(b"%QDF-1.0\n");
        bytes.extend_from_slice(b"\n");
    }

    let mut offsets = BTreeMap::<u32, (u16, usize)>::new();

    for object_ref in &object_refs {
        if Some(*object_ref) == pdf.encryption_ref() {
            continue;
        }

        // ── Step 4: skip members that will be routed into an ObjStm batch ───
        if member_to_batch.contains_key(object_ref) {
            continue;
        }

        // Resolve the object; propagate the error so callers see corrupt input
        // rather than getting a silent success with missing /Root descendants.
        let object = pdf.resolve(*object_ref)?;

        // Skip xref-stream container objects — we'll rebuild the xref from
        // scratch below.  Skip ObjStm container objects — their members have
        // already been resolved into the cache as individual objects and are
        // emitted in the main loop.
        if let Object::Stream(ref s) = object {
            let ty = s.dict.get("Type");
            let is_xref = matches!(ty, Some(Object::Name(n)) if n.as_slice() == b"XRef");
            let is_objstm = matches!(ty, Some(Object::Name(n)) if n.as_slice() == b"ObjStm");
            if is_xref || is_objstm {
                continue;
            }
        }

        // Duplicate detection (same contract as write_qdf).
        if offsets.contains_key(&object_ref.number) {
            return Err(crate::Error::Unsupported(format!(
                "duplicate object number {} in xref table",
                object_ref.number
            )));
        }

        let emit_offset = bytes.len();
        bytes.extend_from_slice(
            format!("{} {} obj\n", object_ref.number, object_ref.generation).as_bytes(),
        );

        if let Object::Stream(stream) = object {
            // QDF mode always decodes streams to raw bytes (CompressStreams::No).
            // For non-QDF full-rewrite the compress_streams option governs.
            let compress_policy = if options.qdf {
                CompressStreams::No
            } else {
                options.compress_streams
            };
            let reencoded = apply_stream_compress_policy(&stream, compress_policy);
            if let Object::Stream(ref s) = reencoded {
                write_stream_to_buf(&mut bytes, s, options.newline_before_endstream);
            } else {
                reencoded.write_pdf(&mut bytes);
            }
        } else {
            object.write_pdf(&mut bytes);
        }

        bytes.extend_from_slice(b"\nendobj\n");
        offsets.insert(object_ref.number, (object_ref.generation, emit_offset));
    }

    // ── Step 5: emit each ObjStm container ───────────────────────────────────
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_ref = container_refs[batch_idx];
        let body = object_streams::emit_objstm_body(pdf, batch)?;
        let stream = object_streams::wrap_objstm_body(&body, options.compress_streams)?;

        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{} 0 obj\n", container_ref.number).as_bytes());
        write_stream_to_buf(&mut bytes, &stream, options.newline_before_endstream);
        bytes.extend_from_slice(b"\nendobj\n");
        offsets.insert(container_ref.number, (0, emit_offset));
    }

    // Build xref / trailer matching the input's xref form.
    let xref_offset = bytes.len();
    // `object_count` is the smallest object number strictly greater than every
    // emitted one — i.e. the number we'll assign to a freshly created xref
    // stream object.  Using `saturating_add` here would silently fail when the
    // input's highest object number is `u32::MAX`: we'd reuse that exact
    // number for the xref stream and collide with an existing object.  Use
    // `checked_add` so the overflow surfaces as an explicit error instead.
    let max_object_number = offsets.keys().next_back().copied().unwrap_or(0);
    let object_count: usize = max_object_number
        .checked_add(1)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            crate::Error::Unsupported("full-rewrite: object count does not fit in u32".to_string())
        })?;

    match effective_xref_form {
        XrefForm::Table => {
            // Classic xref table.
            bytes.extend_from_slice(format!("xref\n0 {}\n", object_count).as_bytes());
            bytes.extend_from_slice(b"0000000000 65535 f \n");
            for number in 1..object_count {
                match offsets.get(&(number as u32)) {
                    Some((generation, offset)) => bytes
                        .extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes()),
                    None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
                }
            }

            // Trailer — start from the document trailer, strip incremental keys.
            let mut trailer = pdf.trailer().clone();
            strip_incremental_trailer_keys(&mut trailer);
            if pdf.is_encrypted() {
                trailer.remove("Encrypt");
            }
            trailer.insert("Size", Object::Integer(object_count as i64));
            trailer.insert("Root", Object::Reference(root_ref));
            if options.static_id {
                apply_static_id(&mut trailer);
            } else {
                apply_random_id(&mut trailer);
            }

            bytes.extend_from_slice(b"trailer\n");
            trailer.write_pdf(&mut bytes);
            bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        }

        XrefForm::Stream => {
            // Cross-reference stream — pick a fresh object number beyond all
            // emitted objects.
            let xref_object_number = object_count as u32;

            // Build the xref stream entries.  We include every object number
            // in `[0, xref_object_number]` so that `/Size` and `/Index` stay
            // consistent: object 0 and any gaps in the emitted-objects set are
            // emitted as free entries, and the xref stream object itself sits
            // at the end.  This produces a single contiguous /Index range and
            // matches the structure a reader expects from a non-incremental
            // xref stream.
            let final_object_count = (xref_object_number as usize).saturating_add(1);
            let mut xref_entries: BTreeMap<u32, (u16, XrefOffset)> = BTreeMap::new();
            xref_entries.insert(0, (65535, XrefOffset::Free { next: 0 }));
            for number in 1..xref_object_number {
                if let Some(&(gen, off)) = offsets.get(&number) {
                    // Regular indirect object (plain or ObjStm container).
                    xref_entries.insert(
                        number,
                        (
                            gen,
                            XrefOffset::Offset(u64::try_from(off).map_err(|_| {
                                crate::Error::Unsupported(
                                    "xref offset does not fit u64".to_string(),
                                )
                            })?),
                        ),
                    );
                } else if let Some(&(container_num, index_in_batch)) =
                    member_to_batch.get(&ObjectRef::new(number, 0))
                {
                    // Member object that lives inside an ObjStm container.
                    // Compressed members have implicit generation 0; the
                    // type-2 xref record carries the container's object number
                    // and the member's index within that container.
                    xref_entries.insert(
                        number,
                        (
                            0,
                            XrefOffset::Compressed {
                                stream: container_num,
                                index: index_in_batch,
                            },
                        ),
                    );
                } else {
                    // Gap in emitted objects — emit as a free entry so the
                    // xref stream covers `[0, /Size)` contiguously.
                    xref_entries.insert(number, (0, XrefOffset::Free { next: 0 }));
                }
            }
            xref_entries.insert(
                xref_object_number,
                (
                    0,
                    XrefOffset::Offset(u64::try_from(xref_offset).map_err(|_| {
                        crate::Error::Unsupported("xref stream offset does not fit u64".to_string())
                    })?),
                ),
            );

            // The entries are now contiguous `[0, final_object_count)`, so a
            // single Index range suffices.
            let ranges: Vec<(u32, u32)> = vec![(0, final_object_count as u32)];

            // Build the raw xref entry bytes (binary format: field widths
            // declared in /W).  This is structural PDF data, not a content
            // stream, so `apply_stream_compress_policy` is not used here.
            // Instead we apply the compress_streams toggle directly: when
            // `CompressStreams::Yes` the raw bytes are FlateDecode-compressed
            // (matching qpdf's default behaviour for xref streams); when
            // `CompressStreams::No` they are stored without any filter.
            let raw_xref_bytes = build_xref_stream_bytes(&xref_entries, &ranges)?;

            let mut index_array = Vec::with_capacity(ranges.len() * 2);
            for &(start, count) in &ranges {
                index_array.push(Object::Integer(i64::from(start)));
                index_array.push(Object::Integer(i64::from(count)));
            }

            let mut xref_dict = pdf.trailer().clone();
            strip_incremental_trailer_keys(&mut xref_dict);
            if pdf.is_encrypted() {
                xref_dict.remove("Encrypt");
            }
            // The trailer may carry filter keys from the input's xref stream
            // (e.g. /Filter /FlateDecode). We're emitting freshly built bytes
            // via `Stream::new`, so any stale filter declaration would make
            // readers attempt to decode the new bytes under the wrong codec.
            // We set /Filter (or omit it) based on compress_streams below.
            xref_dict.remove("Filter");
            xref_dict.remove("DecodeParms");
            xref_dict.remove("F");
            xref_dict.remove("FFilter");
            xref_dict.remove("FDecodeParms");
            xref_dict.insert("Type", Object::Name(b"XRef".to_vec()));
            xref_dict.insert("Size", Object::Integer(final_object_count as i64));
            xref_dict.insert(
                "W",
                Object::Array(vec![
                    Object::Integer(1),
                    Object::Integer(8),
                    Object::Integer(4),
                ]),
            );
            xref_dict.insert("Index", Object::Array(index_array));
            xref_dict.insert("Root", Object::Reference(root_ref));

            // Apply the compress_streams policy: FlateDecode-compress the xref
            // binary payload when Yes, or store raw bytes when No.
            let stream_data = match options.compress_streams {
                CompressStreams::Yes => {
                    let mut encode_dict = Dictionary::new();
                    encode_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
                    match filters::encode_stream_data(&encode_dict, &raw_xref_bytes) {
                        Ok(compressed) => {
                            xref_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
                            compressed
                        }
                        // Compression failure is essentially impossible for
                        // in-memory zlib, but fall back to raw bytes so we
                        // never emit an unreadable xref stream.
                        Err(_) => raw_xref_bytes,
                    }
                }
                CompressStreams::No => {
                    // No filter: store the structural binary directly.
                    raw_xref_bytes
                }
            };

            xref_dict.insert(
                "Length",
                Object::Integer(i64::try_from(stream_data.len()).map_err(|_| {
                    crate::Error::Unsupported("xref stream /Length does not fit i64".to_string())
                })?),
            );
            if options.static_id {
                apply_static_id(&mut xref_dict);
            } else {
                apply_random_id(&mut xref_dict);
            }

            let xref_stream = crate::Stream::new(xref_dict, stream_data);
            bytes.extend_from_slice(format!("{xref_object_number} 0 obj\n").as_bytes());
            write_stream_to_buf(&mut bytes, &xref_stream, options.newline_before_endstream);
            bytes.extend_from_slice(b"\nendobj\n");
            bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        }
    }

    out.write_all(&bytes)?;
    Ok(())
}

/// Apply the stream compression policy to a single stream object.
///
/// This is the choke-point for re-emitting **regular indirect stream
/// objects** in the full-rewrite path. The cross-reference stream and
/// object-stream (ObjStm) containers apply the same `CompressStreams`
/// policy on their own dedicated branches (the xref-stream branch below
/// and [`object_streams::wrap_objstm_body`]); they do not flow through
/// this function. (`write_qdf` is exempt — it is a debug-dump path that
/// emits source bytes verbatim by design and does not consult this function.)
///
/// # Policy: `CompressStreams::Yes` (default)
///
/// Decode the stream through its declared filter pipeline and re-encode with a
/// single `/FlateDecode` filter.  This matches qpdf's default passthrough mode.
///
/// Streams whose decode succeeds but re-encode fails (vanishingly rare for
/// in-memory zlib) are returned verbatim.
///
/// # Policy: `CompressStreams::No`
///
/// Decode the stream and emit the raw bytes without any `/Filter`.  The
/// filter-related keys (`/Filter`, `/DecodeParms`, `/F`, `/FFilter`,
/// `/FDecodeParms`) are stripped from the output dictionary.
///
/// # Fallback for unsupported / corrupt inputs
///
/// When `decode_stream_data` returns an error — e.g. because the declared
/// filter is `DCTDecode` or `JPXDecode` (image codecs not implemented by
/// flpdf) or because the stream data is corrupt — the original stream is
/// returned **unchanged** (dict + data verbatim).  This preserves readability:
/// a PDF reader that understands the codec can still decode the stream, and we
/// do not corrupt the data by emitting uninterpreted bytes under a wrong
/// (or missing) filter declaration.
///
/// # Byte-vs-observable note
///
/// For `CompressStreams::Yes`, flpdf's FlateDecode output uses
/// `flate2::Compression::default()`, which selects different compression
/// parameters than qpdf's internal zlib build.  The decoded bytes are
/// identical to qpdf's, but the raw compressed bytes differ.  This is
/// intentional: byte-identical agreement with qpdf is not a goal for this
/// toggle (the held-for-review zlib-parity decision, flpdf-9hc.12.5).
/// See [`CompressStreams`] for the full policy statement.
pub fn apply_stream_compress_policy(stream: &crate::Stream, policy: CompressStreams) -> Object {
    // Decode the stream through whatever filters are declared in its dict.
    let decoded = match filters::decode_stream_data(&stream.dict, &stream.data) {
        Ok(d) => d,
        Err(_) => {
            // Decode failure (unsupported codec or corrupt data): emit verbatim.
            // The original /Filter chain is preserved so downstream readers
            // (e.g. image renderers) can still interpret the stream correctly.
            return Object::Stream(stream.clone());
        }
    };

    // Build a new dict: strip all filter-related keys, update /Length.
    // `/F` carries an external-file reference for the stream data, so we
    // strip it as well — otherwise readers may try to load the old external
    // file instead of the new embedded stream we just produced.
    let mut new_dict = stream.dict.clone();
    new_dict.remove("Filter");
    new_dict.remove("DecodeParms");
    new_dict.remove("F");
    new_dict.remove("FFilter");
    new_dict.remove("FDecodeParms");

    match policy {
        CompressStreams::Yes => {
            // Re-encode with a minimal FlateDecode dict.  If encoding fails
            // (vanishingly rare for in-memory zlib), keep the original stream
            // verbatim — declaring /FlateDecode on uncompressed bytes would
            // produce an unreadable PDF.
            let mut encode_dict = Dictionary::new();
            encode_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
            let encoded = match filters::encode_stream_data(&encode_dict, &decoded) {
                Ok(e) => e,
                Err(_) => return Object::Stream(stream.clone()),
            };

            // Always apply FlateDecode — even if the encoded result is larger
            // than the raw data (which can happen for small streams).  This
            // guarantees a single well-known filter regardless of stream size.
            new_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
            new_dict.insert(
                "Length",
                Object::Integer(i64::try_from(encoded.len()).unwrap_or(i64::MAX)),
            );
            Object::Stream(crate::Stream::new(new_dict, encoded))
        }
        CompressStreams::No => {
            // Emit raw (decoded) bytes without any filter.
            new_dict.insert(
                "Length",
                Object::Integer(i64::try_from(decoded.len()).unwrap_or(i64::MAX)),
            );
            Object::Stream(crate::Stream::new(new_dict, decoded))
        }
    }
}

/// Write a PDF stream to `buf`, applying the [`NewlineBeforeEndstream`] policy.
///
/// This is the **single choke-point** through which all stream emission in the
/// full-rewrite writer paths flows.  It mirrors the layout that
/// [`Object::Stream::write_pdf`] produces, but gives the caller control over
/// the newline before `endstream`.
///
/// # Layout
///
/// ```text
/// <stream-dict>\nstream\n<payload><EOL>endstream
/// ```
///
/// where `<EOL>` is:
/// - `NewlineBeforeEndstream::Yes`: always `b'\n'` (one byte, unconditionally).
/// - `NewlineBeforeEndstream::No`: empty when payload ends with `\n` or `\r`;
///   otherwise `b'\n'` (one byte, for ISO 32000-1 parseability).
///
/// # /Length invariant
///
/// The helper **does not** modify the stream dictionary.  Callers are
/// responsible for setting `/Length` to `stream.data.len()` before calling
/// (i.e., the raw payload byte count, not including the EOL byte).
pub fn write_stream_to_buf(
    buf: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
) {
    stream.dict.write_pdf(buf);
    buf.extend_from_slice(b"\nstream\n");
    buf.extend_from_slice(&stream.data);

    match policy {
        NewlineBeforeEndstream::Yes => {
            // Always write exactly one newline before endstream.
            buf.push(b'\n');
        }
        NewlineBeforeEndstream::No => {
            // Only write a newline when the payload does not already end with one.
            let ends_with_eol = stream
                .data
                .last()
                .map(|&b| b == b'\n' || b == b'\r')
                .unwrap_or(false);
            if !ends_with_eol {
                buf.push(b'\n');
            }
        }
    }

    buf.extend_from_slice(b"endstream");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_array(d: &Dictionary) -> Vec<Object> {
        match d.get("ID") {
            Some(Object::Array(v)) => v.clone(),
            other => panic!("expected /ID array, got {other:?}"),
        }
    }

    #[test]
    fn apply_static_id_preserves_first_when_both_elements_are_strings() {
        let mut t = Dictionary::new();
        t.insert(
            "ID",
            Object::Array(vec![
                Object::String(b"permanent".to_vec()),
                Object::String(b"changing".to_vec()),
            ]),
        );
        apply_static_id(&mut t);
        let v = id_array(&t);
        assert_eq!(
            v[0],
            Object::String(b"permanent".to_vec()),
            "element 1 must be preserved when /ID is a 2-string array"
        );
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    #[test]
    fn apply_static_id_falls_back_when_second_element_is_not_a_string() {
        // `/ID [<valid> 123]` — arity 2 but element 2 is not a string.
        // Both elements must fall back to the constant (qpdf parity); the
        // old guard checked only element 1 and wrongly kept it.
        let mut t = Dictionary::new();
        t.insert(
            "ID",
            Object::Array(vec![
                Object::String(b"permanent".to_vec()),
                Object::Integer(123),
            ]),
        );
        apply_static_id(&mut t);
        let v = id_array(&t);
        assert_eq!(
            v[0],
            Object::String(QPDF_STATIC_ID.to_vec()),
            "malformed second element must force element 1 to the constant"
        );
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    #[test]
    fn apply_static_id_falls_back_when_id_is_missing() {
        let mut t = Dictionary::new();
        apply_static_id(&mut t);
        let v = id_array(&t);
        assert_eq!(v[0], Object::String(QPDF_STATIC_ID.to_vec()));
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    // --- Default (no-flag) /ID generation strategy (flpdf-9hc.13.2) ---------

    fn str_bytes(o: &Object) -> &[u8] {
        match o {
            Object::String(s) => s.as_slice(),
            other => panic!("expected /ID element to be a string, got {other:?}"),
        }
    }

    #[test]
    fn apply_random_id_first_save_generates_both_elements_fresh() {
        // No source /ID: both elements must be fresh 16-byte randoms — never
        // the π constant and never all-zero.
        let mut t = Dictionary::new();
        apply_random_id(&mut t);
        let v = id_array(&t);
        assert_eq!(v.len(), 2);
        let (e0, e1) = (str_bytes(&v[0]), str_bytes(&v[1]));
        assert_eq!(e0.len(), 16, "element 1 must be 16 bytes");
        assert_eq!(e1.len(), 16, "element 2 must be 16 bytes");
        assert_ne!(
            e0,
            &QPDF_STATIC_ID[..],
            "element 1 must not be the π constant"
        );
        assert_ne!(
            e1,
            &QPDF_STATIC_ID[..],
            "element 2 must not be the π constant"
        );
        assert!(e0.iter().any(|&b| b != 0), "element 1 must not be all-zero");
        assert!(e1.iter().any(|&b| b != 0), "element 2 must not be all-zero");
        // First save: the two elements are independently random.
        assert_ne!(
            e0, e1,
            "first-save elements must be independently generated"
        );
    }

    #[test]
    fn apply_random_id_varies_per_save() {
        // Two saves of the same (no-/ID) input yield different /ID arrays —
        // even back-to-back, thanks to the process-global counter.
        let mut a = Dictionary::new();
        let mut b = Dictionary::new();
        apply_random_id(&mut a);
        apply_random_id(&mut b);
        assert_ne!(id_array(&a), id_array(&b), "/ID must change on every save");
    }

    #[test]
    fn apply_random_id_re_save_preserves_element1_rotates_element2() {
        // First save (no source /ID): both fresh.
        let mut first = Dictionary::new();
        apply_random_id(&mut first);
        let v1 = id_array(&first);
        let perm = v1[0].clone();

        // Re-save: feed the well-formed 2-string /ID back in.  Element 1 must
        // be preserved verbatim (ISO 32000-1 §14.4); element 2 must rotate.
        let mut second = Dictionary::new();
        second.insert("ID", Object::Array(v1.clone()));
        apply_random_id(&mut second);
        let v2 = id_array(&second);

        assert_eq!(
            v2[0], perm,
            "element 1 (permanent id) must be preserved on re-save"
        );
        assert_ne!(
            v2[1], v1[1],
            "element 2 (changing id) must rotate on re-save"
        );
    }

    #[test]
    fn apply_random_id_regenerates_element1_when_source_id_is_malformed() {
        // Arity-2 array but element 2 is not a string → not a usable /ID, so
        // element 1 is treated as a first save and regenerated.
        let mut t = Dictionary::new();
        t.insert(
            "ID",
            Object::Array(vec![
                Object::String(b"would-be-permanent".to_vec()),
                Object::Integer(123),
            ]),
        );
        apply_random_id(&mut t);
        let v = id_array(&t);
        assert_ne!(
            v[0],
            Object::String(b"would-be-permanent".to_vec()),
            "malformed source /ID must not be trusted as permanent id"
        );
        assert_eq!(str_bytes(&v[0]).len(), 16);
        assert_eq!(str_bytes(&v[1]).len(), 16);
    }
}
