//! qpdf correspondence: QPDFWriter.cc object-stream body and container emission.
//! The emitter builds the pair table and member bodies, then wraps them in the
//! `/Type /ObjStm` stream container used by the writer.

use std::collections::HashSet;

use crate::object::ObjectRef;
#[cfg(test)]
use crate::object::{Dictionary, Object};
#[cfg(test)]
use crate::writer::ObjectWriterEmission;
use crate::ObjectHandle;
// ── ObjStm body emitter ───────────────────────────────────────────────────────

/// The serialised body of an ObjStm (ISO 32000-1 §7.5.7).
///
/// Contains the raw pair table concatenated with the objects section.
/// Compression (FlateDecode) and the stream dictionary wrapping are handled
/// by a subsequent step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjStmBody {
    /// Raw concatenation: pair table || objects section.  To be deflate-wrapped later.
    pub bytes: Vec<u8>,
    /// Offset within `bytes` where the first object body starts.  Matches /First.
    pub first_offset: usize,
    /// Number of members.  Matches /N.
    pub n_members: usize,
}

/// Serialise a list of pre-resolved `(ObjectRef, Object)` pairs into an ObjStm
/// body following ISO 32000-1 §7.5.7.
///
/// This inner function does the real work without touching a `Pdf` reader; it
/// exists primarily to make unit-testing Pdf-free.
#[cfg(test)]
pub(crate) fn emit_objstm_body_from_resolved(
    members: &[(ObjectRef, Object)],
) -> crate::Result<ObjStmBody> {
    emit_objstm_body_from_resolved_with_writer(
        members,
        &mut |out, _member_index, _object_ref, object| {
            object.write_pdf(out);
            Ok(())
        },
    )
}

/// Serialize pre-resolved ObjStm members while delegating each complete member
/// body to `write_member`.
///
/// `member_index` is the member's zero-based index in the object stream. The
/// encrypted full-rewrite writer passes it to `EncryptedStringEmitter` so the
/// callback serializer runs without installing an individual object data key;
/// the enclosing ObjStm stream remains the sole encryption boundary.
#[cfg(test)]
pub(crate) fn emit_objstm_body_from_resolved_with_writer<F>(
    members: &[(ObjectRef, Object)],
    write_member: &mut F,
) -> crate::Result<ObjStmBody>
where
    F: FnMut(&mut Vec<u8>, u32, ObjectRef, &Object) -> crate::Result<()>,
{
    emit_objstm_body_from_members(members, write_member)
}

/// Serialise ObjStm members directly from the canonical ObjectHandle graph.
///
/// The member pair table is still supplied in output-number order by the
/// planner, but each body is emitted from its live handle. This is the qpdf
/// writer boundary: arrays and dictionaries retain indirect child identity,
/// dictionary nulls are suppressed by the handle unparser, and no temporary
/// [`ObjectHandle`] tree is materialised merely to calculate the body offsets.
#[cfg(test)]
pub(crate) fn emit_objstm_body_from_handles(
    members: &[(ObjectRef, ObjectHandle)],
) -> crate::Result<ObjStmBody> {
    emit_objstm_body_from_handles_with_writer(
        members,
        &mut |out, _member_index, _object_ref, handle| handle.write_object(out),
    )
}

/// Handle-backed ObjStm body emission with a caller-owned member serializer.
///
/// The callback is used for the encrypted full-rewrite route, where qpdf's
/// per-object data-key scope is applied while the ObjectHandle walker writes
/// strings. The callback receives the same two-pass member index used by the
/// ObjStm pair table and may therefore preserve qpdf's encryption boundary
/// without materialising a legacy object tree.
pub(crate) fn emit_objstm_body_from_handles_with_writer<F>(
    members: &[(ObjectRef, ObjectHandle)],
    write_member: &mut F,
) -> crate::Result<ObjStmBody>
where
    F: FnMut(&mut Vec<u8>, u32, ObjectRef, &ObjectHandle) -> crate::Result<()>,
{
    emit_objstm_body_from_members(members, write_member)
}

fn emit_objstm_body_from_members<T, F>(
    members: &[(ObjectRef, T)],
    write_member: &mut F,
) -> crate::Result<ObjStmBody>
where
    F: FnMut(&mut Vec<u8>, u32, ObjectRef, &T) -> crate::Result<()>,
{
    if members.is_empty() {
        return Ok(ObjStmBody {
            bytes: vec![],
            first_offset: 0,
            n_members: 0,
        });
    }

    // Duplicate detection — fail fast before producing any output.
    let mut seen: HashSet<u32> = HashSet::with_capacity(members.len());
    for (obj_ref, _) in members {
        if !seen.insert(obj_ref.number) {
            return Err(crate::Error::Unsupported(format!(
                "duplicate member in ObjStm batch {}",
                obj_ref.number
            )));
        }
    }

    // Build the objects section and record per-member offsets.
    let mut objects_section: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(members.len());

    for (member_index, (object_ref, object)) in members.iter().enumerate() {
        offsets.push(objects_section.len());
        // cov:ignore-start: a Vec cannot hold more than u32::MAX members in supported targets.
        let member_index = u32::try_from(member_index).map_err(|_| {
            crate::Error::Unsupported("ObjStm member index overflows u32".to_string())
        })?;
        // cov:ignore-end
        write_member(&mut objects_section, member_index, *object_ref, object)?;
        // Append exactly one newline after each object body (write_pdf has no trailing LF).
        objects_section.push(b'\n');
    }

    // Build the pair table: `<number> <offset>` for each member, all
    // space-separated on a single line with one trailing newline before the
    // objects section — qpdf 11.9.0's `/Type /ObjStm` layout (a newline after
    // each pair, as flpdf used to emit, is valid PDF but not byte-identical).
    let mut pair_table: Vec<u8> = Vec::new();
    use std::io::Write as _;
    for (i, ((obj_ref, _), offset)) in members.iter().zip(offsets.iter()).enumerate() {
        if i > 0 {
            pair_table.push(b' ');
        }
        // Write directly into `pair_table` to avoid a temporary `String`
        // allocation per member.
        let _ = write!(pair_table, "{} {}", obj_ref.number, offset);
    }
    pair_table.push(b'\n');

    let first_offset = pair_table.len();

    // Concatenate: pair table || objects section.
    let mut bytes = pair_table;
    bytes.extend_from_slice(&objects_section);

    Ok(ObjStmBody {
        bytes,
        first_offset,
        n_members: members.len(),
    })
}

// ── ObjStm stream wrapper ────────────────────────────────────────────────────

/// Wrap an [`ObjStmBody`] and build the complete `/Type /ObjStm` stream
/// dictionary (ISO 32000-1 §7.5.7).
///
/// The returned [`crate::Stream`] is ready to be written as an indirect object.
/// Key order follows qpdf parity: `Type → N → First → Length → Filter`.
///
/// The `compress` parameter controls whether the body bytes are compressed with
/// FlateDecode (`CompressStreams::Yes`, the default) or emitted raw
/// (`CompressStreams::No`).  Passing the same [`crate::writer::CompressStreams`]
/// value that drives the surrounding full-rewrite loop ensures the ObjStm
/// container uses the same policy as every other stream in the document.
#[cfg(test)]
pub(crate) fn wrap_objstm_body(
    body: &ObjStmBody,
    compress: crate::writer::CompressStreams,
) -> crate::Result<crate::Stream> {
    match compress {
        crate::writer::CompressStreams::Yes => {
            // Build a temporary encode dict with /Filter /FlateDecode.
            let mut encode_dict = Dictionary::new();
            encode_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));

            // Compress the body bytes via the existing helper.
            let encoded = crate::filters::encode_stream_data(&encode_dict, &body.bytes)?;

            // Build the final stream dictionary in qpdf-compatible key order.
            let mut dict = Dictionary::new();
            dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
            dict.insert("N", Object::Integer(body.n_members as i64));
            dict.insert("First", Object::Integer(body.first_offset as i64));
            dict.insert("Length", Object::Integer(encoded.len() as i64));
            dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));

            Ok(crate::Stream {
                dict,
                data: encoded,
            })
        }
        crate::writer::CompressStreams::No => {
            // Emit raw (uncompressed) body bytes without any /Filter.
            let mut dict = Dictionary::new();
            dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
            dict.insert("N", Object::Integer(body.n_members as i64));
            dict.insert("First", Object::Integer(body.first_offset as i64));
            dict.insert("Length", Object::Integer(body.bytes.len() as i64));
            // No /Filter key — body is raw plaintext.

            Ok(crate::Stream {
                dict,
                data: body.bytes.clone(),
            })
        }
    }
}

/// Build the synthetic ObjStm container as an ObjectHandle while retaining
/// the raw payload separately for the stream pipeline. The container has no
/// source object identity, but its dictionary is still emitted through the
/// same live-handle serializer as ordinary streams; `/Extends`, when present,
/// is already in output-number space and is therefore stored as a reference
/// token rather than a legacy `Object` value.
pub(crate) fn wrap_objstm_body_as_handle(
    body: &ObjStmBody,
    compress: crate::writer::CompressStreams,
    extends: Option<crate::ObjectRef>,
) -> crate::Result<(ObjectHandle, Vec<u8>)> {
    let (data, filter) = match compress {
        crate::writer::CompressStreams::Yes => {
            let encode_dict = ObjectHandle::dictionary(vec![(
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            )]);
            (
                crate::filters::encode_stream_data_from_handle(&encode_dict, &body.bytes)?,
                true,
            )
        }
        crate::writer::CompressStreams::No => (body.bytes.clone(), false),
    };

    let mut entries = vec![
        (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
        (
            b"N".to_vec(),
            ObjectHandle::integer(i64::try_from(body.n_members).unwrap_or(i64::MAX)),
        ),
        (
            b"First".to_vec(),
            ObjectHandle::integer(i64::try_from(body.first_offset).unwrap_or(i64::MAX)),
        ),
        (
            b"Length".to_vec(),
            ObjectHandle::integer(i64::try_from(data.len()).unwrap_or(i64::MAX)),
        ),
    ];
    if filter {
        entries.push((
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        ));
    }
    if let Some(extends) = extends {
        entries.push((
            b"Extends".to_vec(),
            ObjectHandle::from_value(crate::object_handle::ObjectValue::Reference(extends)),
        ));
    }
    let handle = ObjectHandle::stream(
        ObjectHandle::dictionary(entries),
        std::rc::Rc::new(data.clone()),
    );
    Ok((handle, data))
}
