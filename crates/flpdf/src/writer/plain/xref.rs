//! Classic and stream cross-reference emission.
//!
//! qpdf correspondence: QPDFWriter.cc classic and stream xref emission for the plain writer.
//!
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::writer::{
    object::{ObjectWriterEmission, TrailerKind},
    output::OutputSink,
    serialize::xref_stream,
    write_deterministic_id_inline,
};
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
    /// Remapped output reference for an indirect Catalog, when `/Root` is
    /// indirect in the source.
    pub(crate) root: Option<ObjectRef>,
    /// Live direct Catalog value for a direct source `/Root`.
    pub(crate) direct_root: Option<ObjectHandle>,
    pub(crate) id: IdPlan,
    pub(crate) encrypt: Option<ObjectRef>,
    pub(crate) structural_filtered: bool,
    /// Whether the enclosing writer is emitting qpdf's QDF layout.
    pub(crate) qdf: bool,
}

/// Stream a classic xref table or xref stream after an already-written body.
/// Trailer values stay in the live handle graph until this call, matching
/// qpdf's `writeTrailer`/`unparseChild` ownership boundary.
pub(crate) fn append_xref_and_trailer(
    out: &mut OutputSink<'_>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
    trailer_handle: &ObjectHandle,
    old_to_new: &HashMap<ObjectRef, ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    layout.validate()?;
    match trailer.form {
        XrefForm::Table => append_classic_xref_and_trailer(
            out,
            layout,
            trailer,
            trailer_handle,
            old_to_new,
            removed_refs,
        ),
        XrefForm::Stream => append_xref_stream_and_trailer(
            out,
            layout,
            trailer,
            trailer_handle,
            old_to_new,
            removed_refs,
        ),
    }
}

fn append_xref_stream_and_trailer(
    out: &mut OutputSink<'_>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
    trailer_handle: &ObjectHandle,
    old_to_new: &HashMap<ObjectRef, ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    // cov:ignore-start: the final sink position is backed by the same usize-sized process memory as the emitted output.
    let xref_offset = usize::try_from(out.position()).map_err(|_| {
        crate::Error::Unsupported("plain writer xref offset exceeds usize range".into())
    })?;
    // cov:ignore-end
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
    let members: BTreeMap<u32, (u32, u32)> = layout
        .compressed
        .iter()
        .map(|(&number, location)| (number, (location.container, location.index)))
        .collect();
    let max_member_index = members
        .values()
        .map(|&(_, index)| u64::from(index))
        .max()
        .unwrap_or(0);
    let stream_layout = xref_stream::prepare_xref_stream(
        &mut offsets,
        &members,
        0,
        size,
        xref_number,
        xref_offset,
        xref_offset as u64,
        xref_number,
        max_member_index,
        0,
        None,
        trailer.structural_filtered,
        false,
        false,
    )?; // cov:ignore: plain xref payload setup is a validated in-memory writer boundary.

    let map = |object_ref: ObjectRef| {
        old_to_new.get(&object_ref).copied().ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "plain writer: trailer reference {object_ref} absent from renumber map"
            ))
        })
    };
    let dictionary = xref_stream::XrefStreamDict {
        filtered: trailer.structural_filtered,
        widths: stream_layout.widths,
        index: None,
        info: None,
        root: trailer.root,
        root_value: None,
        live_root_value: trailer.direct_root.as_ref(),
        size,
        prev: None,
        canonical_entries: None,
        live_trailer: Some(trailer_handle),
        live_map: Some(&map),
        live_removed_refs: Some(removed_refs),
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
                xref_stream::write_xref_stream(
                    out,
                    xref_ref,
                    &dictionary,
                    &stream_layout,
                    true,
                    None,
                )?; // cov:ignore: LLVM maps the covered materialized QDF xref-stream call continuation to this line
            } else {
                xref_stream::write_xref_stream(
                    out,
                    xref_ref,
                    &dictionary,
                    &stream_layout,
                    false,
                    None,
                )?; // cov:ignore: LLVM maps the covered materialized compact xref-stream call continuation to this line
            }
        }
        IdPlan::Deterministic {
            source_id0,
            info_suffix,
        } => {
            let mut id_writer = |out: &mut crate::writer::output::OutputSink<'_>| {
                write_deterministic_id_inline(out, info_suffix, source_id0.as_deref())
            };
            if trailer.qdf {
                xref_stream::write_xref_stream(
                    out,
                    xref_ref,
                    &dictionary,
                    &stream_layout,
                    true,
                    Some(&mut id_writer),
                )?; // cov:ignore: LLVM maps the covered deterministic QDF xref-stream call continuation to this line
            } else {
                xref_stream::write_xref_stream(
                    out,
                    xref_ref,
                    &dictionary,
                    &stream_layout,
                    false,
                    Some(&mut id_writer),
                )?; // cov:ignore: LLVM maps the covered deterministic compact xref-stream call continuation to this line
            }
        }
    }
    out.write_bytes(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes())?;
    written_xref_stream(layout, xref_ref, xref_offset)
}

fn append_classic_xref_and_trailer(
    out: &mut OutputSink<'_>,
    layout: &BodyLayout,
    trailer: &TrailerPlan,
    trailer_handle: &ObjectHandle,
    old_to_new: &HashMap<ObjectRef, ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
    // cov:ignore-start: the final sink position is backed by the same usize-sized process memory as the emitted output.
    let xref_offset = usize::try_from(out.position()).map_err(|_| {
        crate::Error::Unsupported("plain writer xref offset exceeds usize range".into())
    })?;
    // cov:ignore-end
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

    let mut entries = BTreeMap::new();
    for (&number, &(_, offset)) in &layout.uncompressed {
        entries.insert(
            number,
            XrefEntry::Uncompressed {
                offset: offset as u64,
            },
        );
    }
    for (&number, location) in &layout.compressed {
        entries.insert(
            number,
            XrefEntry::Compressed {
                stream: location.container,
                index: location.index,
            },
        );
    }
    let _ = write_xref_table(out, 0, size - 1, &entries, false, 0, 0, 0)?;

    let map = |object_ref: ObjectRef| {
        old_to_new.get(&object_ref).copied().ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "plain writer: trailer reference {object_ref} absent from renumber map"
            ))
        })
    };
    match &trailer.id {
        IdPlan::Deterministic {
            source_id0,
            info_suffix,
        } => {
            let mut id_writer = |out: &mut crate::writer::output::OutputSink<'_>| {
                write_deterministic_id_inline(out, info_suffix, source_id0.as_deref())
            };
            if let Some(direct_root) = trailer.direct_root.as_ref() {
                crate::writer::object::write_trailer_with_ref_map_and_kind_and_direct_root(
                    trailer_handle,
                    out,
                    TrailerKind::Normal {
                        size: i64::from(size),
                    },
                    false,
                    trailer.qdf,
                    Some(&mut id_writer),
                    &map,
                    removed_refs,
                    true,
                    direct_root,
                )?; // cov:ignore: deterministic direct-root trailer serialization is covered by the dedicated writer differential; LLVM maps this continuation separately.
            } else {
                trailer_handle.write_trailer_with_ref_map_and_kind(
                    out,
                    TrailerKind::Normal {
                        size: i64::from(size),
                    },
                    false,
                    trailer.qdf,
                    Some(&mut id_writer),
                    &map,
                    removed_refs,
                    true,
                )?; // cov:ignore: deterministic ID writer call is covered; LLVM maps this multiline terminator to the call setup
            }
        }
        IdPlan::Materialized { .. } => {
            if let Some(direct_root) = trailer.direct_root.as_ref() {
                crate::writer::object::write_trailer_with_ref_map_and_kind_and_direct_root(
                    trailer_handle,
                    out,
                    TrailerKind::Normal {
                        size: i64::from(size),
                    },
                    false,
                    trailer.qdf,
                    None,
                    &map,
                    removed_refs,
                    true,
                    direct_root,
                )?; // cov:ignore: LLVM attributes the covered materialized direct-root trailer continuation separately.
            } else {
                trailer_handle.write_trailer_with_ref_map_and_kind(
                    out,
                    TrailerKind::Normal {
                        size: i64::from(size),
                    },
                    false,
                    trailer.qdf,
                    None,
                    &map,
                    removed_refs,
                    true,
                )?; // cov:ignore: materialized ID writer call is covered; LLVM maps this multiline terminator to the call setup
            }
        }
    }
    if trailer.qdf {
        out.write_bytes(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes())?;
    } else {
        out.write_bytes(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes())?;
    }
    written_xref_table(layout, size)
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

/// Write qpdf's classic xref table rows.
///
/// This is the direct Rust counterpart of
/// `QPDFWriter::writeXRefTable` (`libqpdf/QPDFWriter.cc:2335-2379`). The
/// nonzero rows require an uncompressed type-1 entry unless
/// `suppress_offsets` is active; qpdf's `getOffset()` throws
/// `"getOffset called for xref entry of type != 1"` for a missing, free, or
/// compressed entry, so all three cases use the same `Error::Internal` path.
/// Object generations are output as zero because qpdf's writer opens every
/// emitted object as generation zero.
#[allow(
    clippy::too_many_arguments,
    reason = "preserve QPDFWriter::writeXRefTable's full overload fields one-to-one"
)]
pub(crate) fn write_xref_table(
    out: &mut OutputSink<'_>,
    first: u32,
    last: u32,
    entries: &BTreeMap<u32, XrefEntry>,
    suppress_offsets: bool,
    hint_id: u32,
    hint_offset: u64,
    hint_length: u64,
) -> crate::Result<usize> {
    let count = last
        .checked_sub(first)
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| crate::Error::Internal("invalid xref table range".to_string()))?;
    // qpdf captures `space_before_zero` after writing `xref\n{first} {count}`
    // but *before* the header's trailing newline (`QPDFWriter.cc:2356-2360`),
    // so the returned offset identifies the whitespace immediately preceding
    // the object-0 row. A linearized `/T` consumer relies on that exact byte,
    // so the newline must be appended only after the snapshot.
    out.write_bytes(format!("xref\n{first} {count}").as_bytes())?;
    let space_before_zero = usize::try_from(out.position())
        .map_err(|_| crate::Error::Unsupported("xref table position exceeds usize range".into()))?;
    out.write_bytes(b"\n")?;
    for number in first..=last {
        if number == 0 {
            out.write_bytes(b"0000000000 65535 f \n")?;
            continue;
        }

        let mut offset = 0;
        if !suppress_offsets {
            offset = match entries.get(&number) {
                Some(XrefEntry::Uncompressed { offset }) => *offset,
                Some(XrefEntry::Free { .. }) | Some(XrefEntry::Compressed { .. }) | None => {
                    return Err(crate::Error::Internal(
                        "getOffset called for xref entry of type != 1".to_string(),
                    ));
                }
            };
            if hint_id != 0 && number != hint_id && offset >= hint_offset {
                offset = offset
                    .checked_add(hint_length)
                    .ok_or_else(|| crate::Error::Internal("xref offset overflow".to_string()))?;
            }
        }
        out.write_bytes(format!("{offset:010} 00000 n \n").as_bytes())?;
    }
    Ok(space_before_zero)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn append_bytes(
        bytes: &mut Vec<u8>,
        layout: &BodyLayout,
        trailer: &TrailerPlan,
    ) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
        let trailer_handle = ObjectHandle::dictionary(Vec::new());
        crate::writer::output::with_buffer_sink(bytes, |out| {
            append_xref_and_trailer(
                out,
                layout,
                trailer,
                &trailer_handle,
                &HashMap::new(),
                &BTreeSet::new(),
            )
        })
    }

    fn append_with_handle_bytes(
        bytes: &mut Vec<u8>,
        layout: &BodyLayout,
        trailer: &TrailerPlan,
        trailer_handle: &ObjectHandle,
        old_to_new: &HashMap<ObjectRef, ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
        crate::writer::output::with_buffer_sink(bytes, |out| {
            append_xref_and_trailer(
                out,
                layout,
                trailer,
                trailer_handle,
                old_to_new,
                removed_refs,
            )
        })
    }

    fn append_with_digest(
        bytes: &mut Vec<u8>,
        layout: &BodyLayout,
        trailer: &TrailerPlan,
    ) -> crate::Result<BTreeMap<ObjectRef, XrefEntry>> {
        let trailer_handle = ObjectHandle::dictionary(Vec::new());
        let mut sink = OutputSink::new(bytes);
        sink.begin_digest();
        sink.write_bytes(b"body")?;
        append_xref_and_trailer(
            &mut sink,
            layout,
            trailer,
            &trailer_handle,
            &HashMap::new(),
            &BTreeSet::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn write_table_bytes(
        bytes: &mut Vec<u8>,
        first: u32,
        last: u32,
        entries: &BTreeMap<u32, XrefEntry>,
        suppress_offsets: bool,
        hint_id: u32,
        hint_offset: u64,
        hint_length: u64,
    ) -> crate::Result<usize> {
        crate::writer::output::with_buffer_sink(bytes, |out| {
            write_xref_table(
                out,
                first,
                last,
                entries,
                suppress_offsets,
                hint_id,
                hint_offset,
                hint_length,
            )
        })
    }

    fn trailer() -> TrailerPlan {
        TrailerPlan {
            form: XrefForm::Table,
            root: None,
            direct_root: None,
            id: IdPlan::Materialized { value: None },
            encrypt: None,
            structural_filtered: false,
            qdf: false,
        }
    }

    #[test]
    fn materialized_id_handle_rejects_a_non_array_value() {
        let error = materialized_id_handle(&ObjectHandle::integer(1))
            .expect_err("non-array trailer IDs must be rejected");
        assert!(error.to_string().contains("must be an array"));
    }

    #[test]
    fn classic_trailer_uses_live_shared_owner_for_null_and_unknown_keys() {
        let trailer_handle = ObjectHandle::dictionary(vec![
            (b"/Info".to_vec(), ObjectHandle::integer(1)),
            (b"/Custom".to_vec(), ObjectHandle::name(b"Value".to_vec())),
            (b"/NullEntry".to_vec(), ObjectHandle::null()),
            (b"/Size".to_vec(), ObjectHandle::integer(99)),
            (
                b"/ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"id0".to_vec()),
                    ObjectHandle::string(b"id1".to_vec()),
                ]),
            ),
        ]);
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 12));
        let mut bytes = Vec::new();
        let map = HashMap::new();

        append_with_handle_bytes(
            &mut bytes,
            &layout,
            &trailer(),
            &trailer_handle,
            &map,
            &BTreeSet::new(),
        )
        .expect("live trailer owner emits classic output");

        let text = String::from_utf8(bytes).expect("classic output is UTF-8");
        assert!(
            text.contains("trailer << /Custom /Value /Info 1 /Size 2 /ID [<696430><696431>] >>"),
            "actual output: {text:?}"
        );
        assert!(!text.contains("/NullEntry"));
    }

    #[test]
    fn classic_trailer_keeps_writer_owned_root_when_the_source_slot_is_null() {
        // Regression: when the source Catalog is not object 1 and source object 1
        // is null/free/removed, the planner renumbers the Catalog onto output
        // object 1 and build_writer_trailer_handle installs that output-space
        // handle as /Root. The trailer owner must not drop /Root via source
        // null/removed filtering (qpdf's writeTrailer, QPDFWriter.cc:1160-1236,
        // always emits /Root); only non-/Root source entries are filtered.
        let trailer_handle = ObjectHandle::dictionary(vec![
            (b"/Root".to_vec(), ObjectHandle::null()),
            (b"/NullEntry".to_vec(), ObjectHandle::null()),
            (b"/Size".to_vec(), ObjectHandle::integer(2)),
        ]);
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 12));
        let mut bytes = Vec::new();
        let map = HashMap::new();

        append_with_handle_bytes(
            &mut bytes,
            &layout,
            &trailer(),
            &trailer_handle,
            &map,
            &BTreeSet::new(),
        )
        .expect("live trailer owner emits classic output");

        let text = String::from_utf8(bytes).expect("classic output is UTF-8");
        assert!(
            text.contains("/Root"),
            "writer-owned /Root must survive source null filtering: {text:?}"
        );
        assert!(
            !text.contains("/NullEntry"),
            "non-/Root source null entries are still filtered: {text:?}"
        );
    }

    #[test]
    fn classic_shared_owner_rejects_an_offset_that_cannot_fit_qpdf_xref() {
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 10_000_000_000));
        let trailer_handle =
            ObjectHandle::dictionary(vec![(b"/Size".to_vec(), ObjectHandle::integer(2))]);
        let error = append_with_handle_bytes(
            &mut Vec::new(),
            &layout,
            &trailer(),
            &trailer_handle,
            &HashMap::new(),
            &BTreeSet::new(),
        )
        .expect_err("ten-digit overflow must be rejected");
        assert!(
            matches!(error, crate::Error::Unsupported(message) if message.contains("ten digits"))
        );
    }

    #[test]
    fn classic_shared_owner_builds_then_rejects_a_compressed_row() {
        let mut layout = BodyLayout::default();
        layout.compressed.insert(
            1,
            CompressedLocation {
                container: 4,
                index: 0,
            },
        );
        let trailer_handle =
            ObjectHandle::dictionary(vec![(b"/Size".to_vec(), ObjectHandle::integer(2))]);
        let error = append_with_handle_bytes(
            &mut Vec::new(),
            &layout,
            &trailer(),
            &trailer_handle,
            &HashMap::new(),
            &BTreeSet::new(),
        )
        .expect_err("classic xref must reject a compressed row");
        assert!(matches!(error, crate::Error::Internal(message) if message.contains("getOffset")));
    }

    #[test]
    fn classic_shared_owner_reports_a_missing_trailer_reference_map_entry() {
        let pdf = crate::Pdf::empty().expect("empty PDF for trailer reference test");
        let custom = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(3))
            .expect("indirect custom trailer value");
        let trailer_handle = ObjectHandle::dictionary(vec![
            (b"/CustomRef".to_vec(), custom),
            (b"/Size".to_vec(), ObjectHandle::integer(2)),
        ]);
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 12));
        let error = append_with_handle_bytes(
            &mut Vec::new(),
            &layout,
            &trailer(),
            &trailer_handle,
            &HashMap::new(),
            &BTreeSet::new(),
        )
        .expect_err("missing trailer map entry must be reported");
        assert!(
            matches!(error, crate::Error::Unsupported(message) if message.contains("absent from renumber map"))
        );
    }

    #[test]
    fn classic_xref_missing_row_is_qpdf_logic_error_not_a_fake_free_row() {
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 12));
        layout.uncompressed.insert(3, (0, 34));
        let mut bytes = Vec::new();

        let error = append_bytes(&mut bytes, &layout, &trailer())
            .expect_err("a missing nonzero row must not be serialized as free");
        assert!(matches!(
            error,
            crate::Error::Internal(message)
                if message == "getOffset called for xref entry of type != 1"
        ));
    }

    #[test]
    fn classic_xref_type2_row_is_qpdf_logic_error() {
        let mut layout = BodyLayout::default();
        layout.compressed.insert(
            1,
            CompressedLocation {
                container: 4,
                index: 0,
            },
        );
        let mut bytes = Vec::new();

        let error = append_bytes(&mut bytes, &layout, &trailer())
            .expect_err("a classic table must reject a compressed xref entry");
        assert!(matches!(
            error,
            crate::Error::Internal(message)
                if message == "getOffset called for xref entry of type != 1"
        ));
    }

    #[test]
    fn classic_xref_rows_always_emit_generation_zero() {
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (7, 12));
        let mut bytes = Vec::new();

        append_bytes(&mut bytes, &layout, &trailer()).expect("valid xref");

        assert!(bytes
            .windows(b"0000000012 00000 n \n".len())
            .any(|window| { window == b"0000000012 00000 n \n" }));
        assert!(!bytes
            .windows(b"0000000012 00007 n \n".len())
            .any(|window| { window == b"0000000012 00007 n \n" }));
    }

    #[test]
    fn xref_stream_type1_rows_keep_zero_field3_and_width_from_member_indices() {
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (256, 12));
        layout.compressed.insert(
            2,
            CompressedLocation {
                container: 4,
                index: 12,
            },
        );
        let mut trailer = trailer();
        trailer.form = XrefForm::Stream;
        let mut bytes = Vec::new();

        append_bytes(&mut bytes, &layout, &trailer).expect("xref stream emits a type-1 row");

        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("/W [ 1 1 1 ]"),
            "type-1 generation must not widen the object-stream index field: {text:?}"
        );
        let stream = bytes
            .windows(b"\nstream\n".len())
            .position(|window| window == b"\nstream\n")
            .expect("xref stream framing")
            + b"\nstream\n".len();
        assert_eq!(&bytes[stream + 3..stream + 6], &[1, 12, 0]);
    }

    #[test]
    fn xref_stream_materialized_and_deterministic_ids_cover_qdf_and_compact_routes() {
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 12));

        for qdf in [false, true] {
            let mut materialized = trailer();
            materialized.form = XrefForm::Stream;
            materialized.qdf = qdf;
            materialized.id = IdPlan::Materialized {
                value: Some((b"permanent".to_vec(), b"changing".to_vec())),
            };
            let mut bytes = Vec::new();
            append_bytes(&mut bytes, &layout, &materialized).expect("materialized xref-stream ID");
            assert!(bytes
                .windows(b"/ID [<7065726d616e656e74><6368616e67696e67>]".len())
                .any(|window| { window == b"/ID [<7065726d616e656e74><6368616e67696e67>]" }));

            let mut deterministic = trailer();
            deterministic.form = XrefForm::Stream;
            deterministic.qdf = qdf;
            deterministic.id = IdPlan::Deterministic {
                source_id0: Some(b"source".to_vec()),
                info_suffix: b" info".to_vec(),
            };
            let mut bytes = Vec::new();
            append_with_digest(&mut bytes, &layout, &deterministic)
                .expect("deterministic xref-stream ID");
            assert!(bytes
                .windows(b"/ID [<736f75726365><".len())
                .any(|window| { window == b"/ID [<736f75726365><" }));
            assert!(bytes.ends_with(b"%%EOF\n"));
        }
    }

    #[test]
    fn xref_stream_reports_a_missing_live_trailer_reference_mapping() {
        let pdf = crate::Pdf::empty().expect("empty PDF for trailer reference test");
        let custom = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(3))
            .expect("indirect custom trailer value");
        let trailer_handle = ObjectHandle::dictionary(vec![(b"/CustomRef".to_vec(), custom)]);
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(1, (0, 12));
        let mut stream_trailer = trailer();
        stream_trailer.form = XrefForm::Stream;

        let error = append_with_handle_bytes(
            &mut Vec::new(),
            &layout,
            &stream_trailer,
            &trailer_handle,
            &HashMap::new(),
            &BTreeSet::new(),
        )
        .expect_err("missing xref-stream trailer map entry must be reported");

        assert!(
            matches!(&error, crate::Error::Unsupported(message) if message.contains("trailer /CustomRef reference") && message.contains("absent from renumber map")),
            "unexpected xref-stream mapping error: {error:?}"
        );
    }

    #[test]
    fn xref_stream_width_accounts_for_the_xref_object_number() {
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(255, (0, 12));
        let mut trailer = trailer();
        trailer.form = XrefForm::Stream;
        let mut bytes = Vec::new();

        append_bytes(&mut bytes, &layout, &trailer)
            .expect("xref stream emits the high-numbered range");

        assert!(String::from_utf8_lossy(&bytes).contains("/W [ 1 2 0 ]"));
    }

    #[test]
    fn classic_xref_returns_space_before_zero_at_the_header_newline() {
        // qpdf's writeXRefTable returns `space_before_zero`, captured before
        // the header's trailing newline (QPDFWriter.cc:2356-2360); a
        // linearized `/T` identifies that whitespace byte immediately before
        // the object-0 row, so the returned offset must point at the `\n`,
        // not the first digit of the row after it.
        let mut entries = BTreeMap::new();
        entries.insert(1, XrefEntry::Uncompressed { offset: 100 });
        let mut bytes = Vec::new();
        let space_before_zero =
            write_table_bytes(&mut bytes, 0, 1, &entries, false, 0, 0, 0).expect("table writes");
        assert_eq!(bytes[space_before_zero], b'\n');
        assert_eq!(
            &bytes[space_before_zero + 1..space_before_zero + 1 + b"0000000000 65535 f \n".len()],
            b"0000000000 65535 f \n"
        );
    }

    #[test]
    fn classic_xref_full_contract_supports_range_and_hint_adjustment() {
        let mut entries = BTreeMap::new();
        entries.insert(1, XrefEntry::Uncompressed { offset: 100 });
        entries.insert(2, XrefEntry::Uncompressed { offset: 200 });
        let mut bytes = Vec::new();

        write_table_bytes(&mut bytes, 1, 2, &entries, false, 2, 50, 7)
            .expect("range and hint-adjusted table");
        let text = String::from_utf8(bytes).expect("xref is ASCII");
        assert!(text.starts_with("xref\n1 2\n"));
        assert!(text.contains("0000000107 00000 n \n"));
        assert!(text.contains("0000000200 00000 n \n"));
        assert!(!text.contains("65535 f"));
    }

    #[test]
    fn classic_xref_suppress_offsets_does_not_resolve_rows() {
        let mut bytes = Vec::new();

        write_table_bytes(&mut bytes, 0, 2, &BTreeMap::new(), true, 0, 0, 0)
            .expect("suppressed pass-1 rows do not require offsets");
        let text = String::from_utf8(bytes).expect("xref is ASCII");
        assert!(text.contains("0000000000 65535 f \n"));
        assert_eq!(text.matches("0000000000 00000 n \n").count(), 2);
    }

    #[test]
    fn classic_xref_free_row_uses_the_qpdf_get_offset_error() {
        let mut entries = BTreeMap::new();
        entries.insert(1, XrefEntry::Free { next: 0 });
        let mut bytes = Vec::new();

        let error = write_table_bytes(&mut bytes, 1, 1, &entries, false, 0, 0, 0)
            .expect_err("free rows cannot be emitted as classic live rows");
        assert!(matches!(
            error,
            crate::Error::Internal(message)
                if message == "getOffset called for xref entry of type != 1"
        ));
    }

    #[test]
    fn classic_xref_rejects_invalid_ranges_and_offset_overflow() {
        let mut bytes = Vec::new();
        let error = write_table_bytes(&mut bytes, 2, 1, &BTreeMap::new(), false, 0, 0, 0)
            .expect_err("reversed ranges are invalid");
        assert!(
            matches!(error, crate::Error::Internal(message) if message == "invalid xref table range")
        );

        let mut entries = BTreeMap::new();
        entries.insert(1, XrefEntry::Uncompressed { offset: u64::MAX });
        let error = write_table_bytes(&mut bytes, 1, 1, &entries, false, 2, 0, 1)
            .expect_err("hint adjustment must reject offset overflow");
        assert!(
            matches!(error, crate::Error::Internal(message) if message == "xref offset overflow")
        );
    }
}
