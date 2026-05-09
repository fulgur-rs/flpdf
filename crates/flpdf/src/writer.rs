use crate::parser::Parser;
use crate::{filters, Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, Write};

pub fn write_pdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, mut out: W) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut bytes = pdf.source_bytes()?;
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }

    let source_offsets = build_source_offsets(pdf.source_xref_offsets());
    let object_count = resolve_object_count(pdf.trailer().get("Size"), &source_offsets);
    let (touched_object_refs, touched_objstm_members) = collect_touched_object_refs(pdf);
    let mut xref_offsets = write_incremental_objects(&mut bytes, pdf, &touched_object_refs)?;
    let rewritten_stream_offsets =
        write_updated_object_streams(&mut bytes, pdf, &touched_objstm_members)?;
    xref_offsets.extend(rewritten_stream_offsets);
    let final_offsets = merge_source_and_touched_offsets(&source_offsets, &xref_offsets);

    let xref_offset = write_incremental_xref(&mut bytes, &final_offsets)?;
    write_incremental_trailer(
        &mut bytes,
        pdf,
        &root_ref,
        object_count,
        pdf.previous_xref_offset(),
        xref_offset,
    )?;

    out.write_all(&bytes)?;
    Ok(())
}

type RewrittenObjStmMembers = BTreeMap<ObjectRef, BTreeSet<ObjectRef>>;

fn collect_touched_object_refs<R: Read + Seek>(
    pdf: &Pdf<R>,
) -> (Vec<ObjectRef>, RewrittenObjStmMembers) {
    let mut touched = BTreeSet::new();
    let mut objstm_touched: BTreeMap<ObjectRef, BTreeSet<ObjectRef>> = BTreeMap::new();

    for object_ref in pdf.resolved_object_refs() {
        if let Some((stream_ref, _index)) = pdf.compressed_parent(object_ref) {
            objstm_touched
                .entry(stream_ref)
                .or_default()
                .insert(object_ref);
            continue;
        }

        touched.insert(object_ref);
    }

    (touched.into_iter().collect(), objstm_touched)
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
) -> BTreeMap<u32, (u16, usize)> {
    let mut merged = source_offsets.clone();
    for (number, (generation, offset)) in touched_offsets {
        merged.insert(*number, (*generation, *offset));
    }
    merged
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
    source_offsets: &BTreeMap<u32, (u16, usize)>,
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
    source_offsets: &BTreeMap<u32, (u16, usize)>,
) -> Result<usize> {
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n");
    bytes.extend_from_slice(b"0000000000 65535 f \n");

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
            let (generation, offset) = source_offsets.get(&object_number).ok_or_else(|| {
                crate::Error::Unsupported(
                    "incremental xref subsection is missing object entry".to_string(),
                )
            })?;

            bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes())
        }

        i += 1;
    }

    Ok(xref_offset)
}

fn write_incremental_trailer<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &Pdf<R>,
    root_ref: &ObjectRef,
    object_count: usize,
    previous_xref_offset: u64,
    xref_offset: usize,
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

    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(bytes);
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    Ok(())
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

pub fn write_qdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, mut out: W) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut object_refs = pdf.object_refs();
    object_refs.sort_by_key(|object_ref| (object_ref.number, object_ref.generation));

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.7\n");

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
