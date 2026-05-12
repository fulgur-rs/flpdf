use crate::parser::Parser;
use crate::{filters, Dictionary, Object, ObjectRef, Pdf, Result, XrefForm, XrefOffset};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, Write};

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

    /// When `true`, decode every stream through its filter pipeline and re-emit
    /// the document end-to-end with a single `/FlateDecode` filter applied to
    /// each stream.  The output contains no `/Prev` chain, no `ObjStm`, and no
    /// xref-stream-only keys.
    ///
    /// When `false` (the default) the existing incremental-update write path is
    /// used, preserving the source bytes verbatim.
    pub full_rewrite: bool,
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
            if values.len() == 2 && matches!(values[0], Object::String(_)) =>
        {
            values[0].clone()
        }
        _ => pi_id.clone(),
    };
    trailer.insert("ID", Object::Array(vec![first_id, pi_id]));
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
/// # Scope limitations (TODO)
///
/// - **ObjStm dissolve**: Object streams are dissolved — members are emitted as
///   ordinary indirect objects.  There is currently no merging of existing
///   ObjStm containers back into the regular sequence; they are simply skipped.
///   A dedicated "renumber + pack into ObjStm" pass (flpdf-9hc.20.13) is a
///   future concern.
///
/// - **Encrypted documents**: `/Encrypt` in the trailer is not supported.
///   The function returns `Err(Unsupported)` when encryption is detected so
///   that callers do not silently produce a corrupt output.
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
fn write_pdf_full_rewrite<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    // Reject encrypted documents — decoding encrypted streams without the
    // encryption key would produce garbage.  (Scope limitation; NOTES.)
    if pdf.trailer().get("Encrypt").is_some() {
        return Err(crate::Error::Unsupported(
            "full-rewrite mode does not support encrypted documents".to_string(),
        ));
    }

    let version = effective_pdf_version(pdf.version(), options, false).to_owned();

    let mut object_refs = pdf.object_refs();
    object_refs.sort_by_key(|r| (r.number, r.generation));

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);

    let mut offsets = BTreeMap::<u32, (u16, usize)>::new();

    for object_ref in &object_refs {
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
            let reencoded = reencode_stream_flate(&stream);
            reencoded.write_pdf(&mut bytes);
        } else {
            object.write_pdf(&mut bytes);
        }

        bytes.extend_from_slice(b"\nendobj\n");
        offsets.insert(object_ref.number, (object_ref.generation, emit_offset));
    }

    // Build xref / trailer matching the input's xref form.
    let xref_offset = bytes.len();
    let object_count = offsets
        .keys()
        .next_back()
        .copied()
        .unwrap_or(0)
        .saturating_add(1) as usize;

    match pdf.last_xref_form() {
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
            trailer.insert("Size", Object::Integer(object_count as i64));
            trailer.insert("Root", Object::Reference(root_ref));
            if options.static_id {
                apply_static_id(&mut trailer);
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
                match offsets.get(&number) {
                    Some(&(gen, off)) => {
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
                    }
                    None => {
                        // Gap in emitted objects — emit as a free entry so the
                        // xref stream covers `[0, /Size)` contiguously.
                        xref_entries.insert(number, (0, XrefOffset::Free { next: 0 }));
                    }
                }
            }
            xref_entries.insert(
                xref_object_number,
                (
                    0,
                    XrefOffset::Offset(u64::try_from(xref_offset).map_err(|_| {
                        crate::Error::Unsupported(
                            "xref stream offset does not fit u64".to_string(),
                        )
                    })?),
                ),
            );

            // The entries are now contiguous `[0, final_object_count)`, so a
            // single Index range suffices.
            let ranges: Vec<(u32, u32)> = vec![(0, final_object_count as u32)];

            let stream_data = build_xref_stream_bytes(&xref_entries, &ranges)?;

            let mut index_array = Vec::with_capacity(ranges.len() * 2);
            for &(start, count) in &ranges {
                index_array.push(Object::Integer(i64::from(start)));
                index_array.push(Object::Integer(i64::from(count)));
            }

            let mut xref_dict = pdf.trailer().clone();
            strip_incremental_trailer_keys(&mut xref_dict);
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
            xref_dict.insert(
                "Length",
                Object::Integer(i64::try_from(stream_data.len()).map_err(|_| {
                    crate::Error::Unsupported("xref stream /Length does not fit i64".to_string())
                })?),
            );
            if options.static_id {
                apply_static_id(&mut xref_dict);
            }

            let xref_stream = Object::Stream(crate::Stream::new(xref_dict, stream_data));
            bytes.extend_from_slice(format!("{xref_object_number} 0 obj\n").as_bytes());
            xref_stream.write_pdf(&mut bytes);
            bytes.extend_from_slice(b"\nendobj\n");
            bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        }
    }

    out.write_all(&bytes)?;
    Ok(())
}

/// Decode a stream's filter chain and re-encode with a single `/FlateDecode`
/// filter.  On any error (unsupported filter, corrupt data, etc.) the original
/// stream object is returned unchanged so the caller can still emit a readable
/// — if not fully normalized — PDF.
fn reencode_stream_flate(stream: &crate::Stream) -> Object {
    // Decode the stream through whatever filters are declared in its dict.
    let decoded = match filters::decode_stream_data(&stream.dict, &stream.data) {
        Ok(d) => d,
        Err(_) => {
            // Decode failure: emit the stream with its original dict/data.
            return Object::Stream(stream.clone());
        }
    };

    // Re-encode with a minimal FlateDecode dict.  If encoding fails (which
    // should be vanishingly rare for in-memory zlib), keep the original
    // stream verbatim — declaring /FlateDecode on uncompressed bytes would
    // produce an unreadable PDF.
    let mut encode_dict = Dictionary::new();
    encode_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let encoded = match filters::encode_stream_data(&encode_dict, &decoded) {
        Ok(e) => e,
        Err(_) => return Object::Stream(stream.clone()),
    };

    // Build a new stream dict: copy everything from the original except the
    // filter-related keys (which we replace) and Length (which we update).
    let mut new_dict = stream.dict.clone();
    new_dict.remove("Filter");
    new_dict.remove("DecodeParms");
    new_dict.remove("FFilter");
    new_dict.remove("FDecodeParms");

    // Always apply FlateDecode — even if the encoded result is larger than the
    // raw data (which can happen for small streams).  This guarantees that the
    // output always has a single well-known filter regardless of stream size.
    new_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    new_dict.insert(
        "Length",
        Object::Integer(i64::try_from(encoded.len()).unwrap_or(i64::MAX)),
    );
    Object::Stream(crate::Stream::new(new_dict, encoded))
}
