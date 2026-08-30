//! qpdf correspondence: QPDFWriter.cc classic and stream xref emission for the plain writer.
use std::collections::BTreeMap;

use crate::writer::{serialize::xref_stream, write_deterministic_id_inline};
use crate::{ObjectHandle, ObjectRef, XrefEntry, XrefForm};

/// Location of an object encoded inside an object-stream container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompressedLocation {
    pub(crate) container: u32,
    pub(crate) index: u32,
}

/// Physical locations of the objects already written into a plain PDF body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BodyLayout {
    pub(crate) uncompressed: BTreeMap<u32, (u16, usize)>,
    pub(crate) compressed: BTreeMap<u32, CompressedLocation>,
}

impl BodyLayout {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        for number in self.uncompressed.keys() {
            if self.compressed.contains_key(number) {
                return Err(crate::Error::Unsupported(format!(
                    "plain writer layout: object {number} is both uncompressed and compressed"
                )));
            }
        }
        Ok(())
    }

    fn max_number(&self) -> u32 {
        self.uncompressed
            .keys()
            .chain(self.compressed.keys())
            .copied()
            .max()
            .unwrap_or(0)
    }
}

/// How the trailer `/ID` is provided while its bytes are assembled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IdPlan {
    Materialized {
        value: Option<(Vec<u8>, Vec<u8>)>,
    },
    Deterministic {
        source_id0: Option<Vec<u8>>,
        info_suffix: Vec<u8>,
    },
}

/// Inputs needed to assemble an xref section after a plain body has been emitted.
#[derive(Clone, Debug)]
pub(crate) struct TrailerPlan {
    pub(crate) form: XrefForm,
    /// Canonical trailer entries from the live ObjectHandle graph. Keys remain
    /// decoded until emission so qpdf's raw-name sort is preserved.
    pub(crate) canonical_entries: Vec<(Vec<u8>, Vec<u8>)>,
    /// Remapped output reference for an indirect Catalog, when `/Root` is
    /// indirect in the source.
    pub(crate) root: Option<ObjectRef>,
    /// Serialized direct Catalog value for a direct source `/Root`.
    pub(crate) direct_root: Option<Vec<u8>>,
    pub(crate) id: IdPlan,
    pub(crate) encrypt: Option<ObjectRef>,
    pub(crate) structural_filtered: bool,
    /// Whether the enclosing writer is emitting qpdf's QDF layout.
    pub(crate) qdf: bool,
}

/// Append a classic xref table or xref stream for an already-written body.
pub(crate) fn append_xref_and_trailer(
    bytes: &mut Vec<u8>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    layout.validate()?;

    match trailer.form {
        XrefForm::Table if layout.compressed.is_empty() => {
            append_classic_xref_and_trailer(bytes, layout, trailer)
        }
        XrefForm::Table => Err(crate::Error::Unsupported(
            "plain writer classic xref cannot represent compressed objects".into(),
        )),
        XrefForm::Stream => append_xref_stream_and_trailer(bytes, layout, trailer),
    }
}

fn append_xref_stream_and_trailer(
    bytes: &mut Vec<u8>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    let xref_offset = bytes.len();
    let max_number = layout.max_number();
    let xref_number = max_number.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported("plain writer xref object number overflows u32".into())
    })?;
    let size = xref_number
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("plain writer /Size overflows u32".into()))?;

    let mut offsets: BTreeMap<u32, usize> = layout
        .uncompressed
        .iter()
        .map(|(&number, &(_, offset))| (number, offset))
        .collect();
    offsets.insert(xref_number, xref_offset);
    let members: BTreeMap<u32, (u32, u32)> = layout
        .compressed
        .iter()
        .map(|(&number, location)| (number, (location.container, location.index)))
        .collect();
    let mut entries = xref_stream::build_entries(&offsets, &members, 0, size);
    for (&number, &(generation, _)) in &layout.uncompressed {
        entries[number as usize].field3 = u64::from(generation);
    }
    let max_generation = layout
        .uncompressed
        .values()
        .map(|&(generation, _)| u64::from(generation))
        .max()
        .unwrap_or(0);
    let max_member_index = members
        .values()
        .map(|&(_, index)| u64::from(index))
        .max()
        .unwrap_or(0);
    let widths = xref_stream::second_pass_widths(
        xref_stream::max_entry_offset(&entries),
        0,
        max_number,
        max_generation.max(max_member_index),
    );
    let payload = if trailer.structural_filtered {
        xref_stream::encode_payload(&entries, widths)?
    } else {
        xref_stream::encode_payload_raw(&entries, widths)?
    };

    let dictionary = xref_stream::XrefStreamDict {
        filtered: trailer.structural_filtered,
        widths,
        index: None,
        info: None,
        root: trailer.root,
        root_value: trailer.direct_root.as_deref(),
        size,
        prev: None,
        canonical_entries: Some(&trailer.canonical_entries),
        id: None,
        encrypt: trailer.encrypt,
    };
    let xref_ref = ObjectRef::new(xref_number, 0);
    match &trailer.id {
        IdPlan::Materialized { value } => {
            let id = value
                .as_ref()
                .map(|(id0, id1)| (id0.as_slice(), id1.as_slice()));
            let dictionary = xref_stream::XrefStreamDict { id, ..dictionary };
            if trailer.qdf {
                xref_stream::write_object_qdf(bytes, xref_ref, &dictionary, &payload);
            } else {
                xref_stream::write_object(bytes, xref_ref, &dictionary, &payload);
            }
        }
        IdPlan::Deterministic {
            source_id0,
            info_suffix,
        } => {
            let mut id_writer = |out: &mut Vec<u8>| {
                write_deterministic_id_inline(out, info_suffix, source_id0.as_deref())
            };
            if trailer.qdf {
                xref_stream::write_object_with_id_writer_qdf(
                    bytes,
                    xref_ref,
                    &dictionary,
                    &payload,
                    &mut id_writer,
                );
            } else {
                xref_stream::write_object_with_id_writer(
                    bytes,
                    xref_ref,
                    &dictionary,
                    &payload,
                    &mut id_writer,
                );
            }
        }
    }
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    written_xref_stream(layout, xref_ref, xref_offset)
}

/// Read the writer-owned `/ID` value from the canonical handle graph.
///
/// qpdf's trailer writer accepts an absent `/ID`, but when an identifier is
/// present it must be exactly an array of two string values before the xref
/// layer can emit it (`QPDFWriter.cc:1160-1236`). The handle is resolved
/// lazily so indirect `/ID` values follow the same route as every other
/// writer trailer entry.
pub(crate) fn materialized_id_handle(
    id: &ObjectHandle,
) -> crate::Result<Option<(Vec<u8>, Vec<u8>)>> {
    if id.try_is_null()? {
        return Ok(None);
    }
    let Some(values) = id.try_as_array()? else {
        return Err(crate::Error::Unsupported(
            "plain writer materialized /ID must be an array".into(),
        ));
    };
    let [id0, id1] = values.as_slice() else {
        return Err(crate::Error::Unsupported(
            "plain writer materialized /ID must contain two strings".into(),
        ));
    };
    id0.try_dereference()?;
    id1.try_dereference()?;
    match (id0.as_string(), id1.as_string()) {
        (Some(id0), Some(id1)) => Ok(Some((id0, id1))),
        _ => Err(crate::Error::Unsupported(
            "plain writer materialized /ID must contain two strings".into(),
        )),
    }
}

fn append_classic_xref_and_trailer(
    bytes: &mut Vec<u8>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    let xref_offset = bytes.len();
    let size = layout
        .max_number()
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("plain writer /Size overflows u32".into()))?;
    if layout
        .uncompressed
        .values()
        .any(|&(_, offset)| offset as u64 >= 10_000_000_000)
    {
        return Err(crate::Error::Unsupported(
            "plain writer classic xref offset exceeds ten digits".into(),
        ));
    }

    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..size {
        if let Some(&(generation, offset)) = layout.uncompressed.get(&number) {
            bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        } else {
            bytes.extend_from_slice(b"0000000000 00000 f \n");
        }
    }

    bytes.extend_from_slice(b"trailer ");
    write_canonical_classic_trailer(bytes, trailer, size, &trailer.canonical_entries);
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    written_xref_table(layout, size)
}

fn write_canonical_classic_trailer(
    bytes: &mut Vec<u8>,
    trailer: &TrailerPlan,
    size: u32,
    canonical: &[(Vec<u8>, Vec<u8>)],
) {
    let mut entries = canonical.to_vec();
    if let Some(root) = trailer.root {
        entries.push((
            b"/Root".to_vec(),
            format!("{} {} R", root.number, root.generation).into_bytes(),
        ));
    }
    if let Some(root) = &trailer.direct_root {
        entries.push((b"/Root".to_vec(), root.clone()));
    }
    entries.push((b"/Size".to_vec(), size.to_string().into_bytes()));
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    bytes.extend_from_slice(b"<<");
    for (key, value) in entries {
        bytes.push(b' ');
        write_qpdf_dictionary_key(bytes, &key);
        bytes.push(b' ');
        bytes.extend_from_slice(&value);
    }

    if let IdPlan::Materialized {
        value: Some((id0, id1)),
    } = &trailer.id
    {
        bytes.extend_from_slice(b" /ID [<");
        write_hex(bytes, id0);
        bytes.extend_from_slice(b"><");
        write_hex(bytes, id1);
        bytes.extend_from_slice(b">]");
    } else if let IdPlan::Deterministic {
        source_id0,
        info_suffix,
    } = &trailer.id
    {
        bytes.extend_from_slice(b" /ID ");
        write_deterministic_id_inline(bytes, info_suffix, source_id0.as_deref());
    }

    if let Some(encrypt) = trailer.encrypt {
        bytes.extend_from_slice(b" /Encrypt ");
        bytes.extend_from_slice(format!("{} {} R", encrypt.number, encrypt.generation).as_bytes());
    }
    bytes.extend_from_slice(b" >>");
}

/// Emit a qpdf dictionary key without changing its first byte.
/// `QPDF_Name::normalizeName` (`libqpdf/QPDF_Name.cc:27-50`) preserves a raw
/// slashless key such as `Array1`, while canonical keys already carry `/`.
fn write_qpdf_dictionary_key(out: &mut Vec<u8>, key: &[u8]) {
    if let Some(key) = key.strip_prefix(b"/") {
        out.push(b'/');
        crate::pdf_syntax::write_name_escaped(out, key);
    } else {
        crate::pdf_syntax::write_name_escaped(out, key);
    }
}

fn write_hex(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 0x0f) as usize]);
    }
}

fn written_xref_table(
    layout: &BodyLayout,
    size: u32,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    let mut result = BTreeMap::new();
    for number in 1..size {
        if let Some(&(_generation, offset)) = layout.uncompressed.get(&number) {
            result.insert(
                ObjectRef::new(number, 0),
                XrefEntry::Uncompressed {
                    // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                    // on every supported target.
                    offset: u64::try_from(offset).map_err(|_| {
                        crate::Error::Unsupported("xref offset does not fit u64".to_string())
                    })?,
                    // cov:ignore-end
                },
            );
        }
    }
    Ok(result)
}

fn written_xref_stream(
    layout: &BodyLayout,
    xref_ref: ObjectRef,
    xref_offset: usize,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    let size = xref_ref
        .number
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("plain writer /Size overflows u32".into()))?;
    let mut result = BTreeMap::new();
    for number in 1..size {
        if number == xref_ref.number {
            result.insert(
                ObjectRef::new(xref_ref.number, 0),
                XrefEntry::Uncompressed {
                    // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                    // on every supported target.
                    offset: u64::try_from(xref_offset).map_err(|_| {
                        crate::Error::Unsupported("xref offset does not fit u64".to_string())
                    })?,
                    // cov:ignore-end
                },
            );
        } else if let Some(&(_generation, offset)) = layout.uncompressed.get(&number) {
            result.insert(
                ObjectRef::new(number, 0),
                XrefEntry::Uncompressed {
                    // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                    // on every supported target.
                    offset: u64::try_from(offset).map_err(|_| {
                        crate::Error::Unsupported("xref offset does not fit u64".to_string())
                    })?,
                    // cov:ignore-end
                },
            );
        } else if let Some(location) = layout.compressed.get(&number) {
            result.insert(
                ObjectRef::new(number, 0),
                XrefEntry::Compressed {
                    stream: location.container,
                    index: location.index,
                },
            );
        }
    }
    Ok(result)
}
