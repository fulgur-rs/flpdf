use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};

pub fn write_pdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, mut out: W) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut bytes = pdf.source_bytes()?;
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }

    let source_offsets = build_source_offsets(pdf.xref_offsets());
    let object_count = resolve_object_count(pdf.trailer().get("Size"), &source_offsets);

    let xref_offset = write_incremental_xref(&mut bytes, &source_offsets, object_count)?;
    write_incremental_trailer(
        &mut bytes,
        pdf,
        &root_ref,
        object_count,
        pdf.startxref(),
        xref_offset,
    )?;

    out.write_all(&bytes)?;
    Ok(())
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
    let declared = match declared_size {
        Some(crate::Object::Integer(size)) if *size > 0 => *size as usize,
        _ => 0,
    };

    declared.max(max_object_number.saturating_add(1)).max(1)
}

fn write_incremental_xref(
    bytes: &mut Vec<u8>,
    source_offsets: &BTreeMap<u32, (u16, usize)>,
    object_count: usize,
) -> Result<usize> {
    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_count).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");

    for number in 1..object_count {
        match source_offsets.get(&(number as u32)) {
            Some((generation, offset)) => {
                bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes())
            }
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
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
