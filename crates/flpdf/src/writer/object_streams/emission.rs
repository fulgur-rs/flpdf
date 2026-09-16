//! The emitter builds the pair table and member bodies, then wraps them in the
//! `/Type /ObjStm` stream container used by the writer.
//!
//! qpdf correspondence: QPDFWriter.cc object-stream body and container emission.
//!

use std::collections::HashSet;
use std::io;
use std::rc::Rc;

use crate::stream_filter::encode_flate;
use crate::writer::output::{OutputSink, OutputTarget};
use crate::ObjectHandle;
use crate::ObjectRef;
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

/// Serialise a list of live `(ObjectRef, ObjectHandle)` pairs into an ObjStm
/// body following ISO 32000-1 §7.5.7. The handles remain live until the
/// caller-owned serializer observes each member.
///
/// This inner function does the real work without touching a `Pdf` reader; it
/// exists primarily to make unit-testing Pdf-free.
/// Serialise ObjStm members directly from the canonical ObjectHandle graph.
///
/// The member pair table is still supplied in output-number order by the
/// planner, but each body is emitted from its live handle. This is the qpdf
/// writer boundary: arrays and dictionaries retain indirect child identity,
/// dictionary nulls are suppressed by the handle unparser, and no temporary
/// [`ObjectHandle`] tree is materialised merely to calculate the body offsets.
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
    emit_objstm_body_from_members(members, write_member, false)
}

/// Serialise one live ObjStm pass through a caller-owned counted sink. When
/// `retain_body` is false the pass uses a discard target, matching qpdf's
/// `Pl_Discard`; when it is true, it owns one persistent `Vec`-backed sink for
/// the complete body. The live writer invokes this once for each qpdf pass.
pub(crate) fn emit_objstm_body_from_handles_with_sink<F>(
    members: &[(ObjectRef, ObjectHandle)],
    write_member: &mut F,
    retain_body: bool,
) -> crate::Result<ObjStmBody>
where
    F: FnMut(&mut OutputSink<'_>, u32, ObjectRef, &ObjectHandle) -> crate::Result<()>,
{
    emit_objstm_body_from_members_with_sink(members, write_member, false, retain_body)
}

/// QDF-formatted sibling of [`emit_objstm_body_from_handles_with_sink`].
pub(crate) fn emit_objstm_body_from_handles_with_sink_qdf<F>(
    members: &[(ObjectRef, ObjectHandle)],
    write_member: &mut F,
    retain_body: bool,
) -> crate::Result<ObjStmBody>
where
    F: FnMut(&mut OutputSink<'_>, u32, ObjectRef, &ObjectHandle) -> crate::Result<()>,
{
    emit_objstm_body_from_members_with_sink(members, write_member, true, retain_body)
}

struct ObjStmDiscardTarget;

impl OutputTarget for ObjStmDiscardTarget {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }

    fn finish_segment(&mut self) -> crate::Result<()> {
        Ok(())
    }

    fn finish_document(&mut self) -> crate::Result<()> {
        Ok(())
    }
}

fn emit_objstm_body_from_members_with_sink<T, F>(
    members: &[(ObjectRef, T)],
    write_member: &mut F,
    qdf: bool,
    retain_body: bool,
) -> crate::Result<ObjStmBody>
where
    F: FnMut(&mut OutputSink<'_>, u32, ObjectRef, &T) -> crate::Result<()>,
{
    if members.is_empty() {
        return Ok(ObjStmBody {
            bytes: vec![],
            first_offset: 0,
            n_members: 0,
        });
    }

    let mut seen: HashSet<u32> = HashSet::with_capacity(members.len());
    for (obj_ref, _) in members {
        if !seen.insert(obj_ref.number) {
            return Err(crate::Error::Unsupported(format!(
                "duplicate member in ObjStm batch {}",
                obj_ref.number
            )));
        }
    }

    if !retain_body {
        let mut discard = ObjStmDiscardTarget;
        let mut sink = OutputSink::new(&mut discard);
        for (member_index, (object_ref, object)) in members.iter().enumerate() {
            let member_index = u32::try_from(member_index).map_err(|_| {
                // cov:ignore-start: a Vec cannot hold more than u32::MAX
                // members in supported targets.
                crate::Error::Unsupported("ObjStm member index overflows u32".to_string())
                // cov:ignore-end
            })?; // cov:ignore: member_index comes from a Vec and cannot exceed u32::MAX on supported targets
            write_member(&mut sink, member_index, *object_ref, object)?;
            sink.write_bytes(b"\n")?;
        }
        return Ok(ObjStmBody {
            bytes: Vec::new(),
            first_offset: 0,
            n_members: members.len(),
        });
    }

    let mut offsets: Vec<usize> = Vec::with_capacity(members.len());
    let mut objects_section = Vec::new();
    {
        let mut sink = OutputSink::new(&mut objects_section);
        for (member_index, (object_ref, object)) in members.iter().enumerate() {
            offsets.push(sink.position_usize()?);
            let member_index = u32::try_from(member_index).map_err(|_| {
                // cov:ignore-start: a Vec cannot hold more than u32::MAX
                // members in supported targets.
                crate::Error::Unsupported("ObjStm member index overflows u32".to_string())
                // cov:ignore-end
            })?; // cov:ignore: member_index comes from a Vec and cannot exceed u32::MAX on supported targets
            write_member(&mut sink, member_index, *object_ref, object)?;
            sink.write_bytes(b"\n")?;
        }
    }

    let mut pair_table: Vec<u8> = Vec::new();
    use std::io::Write as _;
    for (i, ((obj_ref, _), offset)) in members.iter().zip(offsets.iter()).enumerate() {
        if i > 0 {
            pair_table.push(if qdf { b'\n' } else { b' ' });
        }
        let _ = write!(pair_table, "{} {}", obj_ref.number, offset);
    }
    pair_table.push(b'\n');

    let first_offset = pair_table.len();
    let objects_len = objects_section.len();
    objects_section.reserve(first_offset);
    objects_section.resize(
        objects_len.checked_add(first_offset).ok_or_else(|| {
            // cov:ignore-start: both lengths come from the same supported Vec
            // allocation, so their sum cannot exceed usize in a constructible
            // ObjStm.
            crate::Error::Unsupported("ObjStm body length overflows usize".to_string())
            // cov:ignore-end
        })?, // cov:ignore: LLVM maps the covered ObjStm body resize continuation to this line
        0,
    );
    objects_section.copy_within(0..objects_len, first_offset);
    objects_section[..first_offset].copy_from_slice(&pair_table);

    Ok(ObjStmBody {
        bytes: objects_section,
        first_offset,
        n_members: members.len(),
    })
}

fn emit_objstm_body_from_members<T, F>(
    members: &[(ObjectRef, T)],
    write_member: &mut F,
    qdf: bool,
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
            pair_table.push(if qdf { b'\n' } else { b' ' });
        }
        // Write directly into `pair_table` to avoid a temporary `String`
        // allocation per member.
        let _ = write!(pair_table, "{} {}", obj_ref.number, offset);
    }
    pair_table.push(b'\n');

    let first_offset = pair_table.len();

    // Prefix the pair table in place. The member bodies and the pair table
    // share one final local allocation; inserting the small prefix with an
    // in-place shift avoids the second full-size `pair_table || objects`
    // allocation that a normal `extend_from_slice` would require.
    let objects_len = objects_section.len();
    objects_section.reserve(first_offset);
    objects_section.resize(
        objects_len.checked_add(first_offset).ok_or_else(|| {
            // cov:ignore-start: both lengths come from the same supported Vec allocation, so their sum cannot exceed usize in a constructible ObjStm.
            crate::Error::Unsupported("ObjStm body length overflows usize".to_string())
            // cov:ignore-end
        })?, // cov:ignore: LLVM maps the covered ObjStm body resize continuation to this line
        0,
    );
    objects_section.copy_within(0..objects_len, first_offset);
    objects_section[..first_offset].copy_from_slice(&pair_table);

    Ok(ObjStmBody {
        bytes: objects_section,
        first_offset,
        n_members: members.len(),
    })
}

// ── ObjStm stream wrapper ────────────────────────────────────────────────────

/// Consume an [`ObjStmBody`] and build the complete `/Type /ObjStm` stream
/// dictionary (ISO 32000-1 §7.5.7).
///
/// The returned stream handle is ready to be written as an indirect object.
/// Key order follows qpdf parity: `Type → N → First → Length → Filter`.
///
/// The `compress` parameter controls whether the body bytes are compressed with
/// FlateDecode (`CompressStreams::Yes`, the default) or emitted raw
/// (`CompressStreams::No`).  Passing the same [`crate::writer::CompressStreams`]
/// value that drives the surrounding full-rewrite loop ensures the ObjStm
/// container uses the same policy as every other stream in the document.
/// Build the synthetic ObjStm container as an ObjectHandle while retaining the
/// same reference-counted payload for the stream pipeline. The container has
/// no source object identity, but its dictionary is still emitted through the
/// same live-handle serializer as ordinary streams; `/Extends`, when present,
/// is already in output-number space and is therefore stored as a reference
/// token rather than a legacy `Object` value.
///
/// Taking ownership of the body lets the uncompressed path transfer its
/// allocation directly into the stream payload. The compressed path allocates
/// its encoded payload once; the returned handle and the caller share that
/// allocation, matching qpdf's `shared_ptr<Buffer>` ownership at
/// `QPDFWriter.cc:1636-1750`.
pub(crate) fn wrap_objstm_body_as_handle(
    body: ObjStmBody,
    compress: crate::writer::CompressStreams,
    extends: Option<crate::ObjectRef>,
) -> crate::Result<(ObjectHandle, Rc<Vec<u8>>)> {
    let first_offset = body.first_offset;
    let n_members = body.n_members;
    let (data, filter) = match compress {
        crate::writer::CompressStreams::Yes => (Rc::new(encode_flate(&body.bytes)?), true),
        crate::writer::CompressStreams::No => (Rc::new(body.bytes), false),
    };

    let mut entries = vec![
        (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
        (
            b"N".to_vec(),
            ObjectHandle::integer(i64::try_from(n_members).unwrap_or(i64::MAX)),
        ),
        (
            b"First".to_vec(),
            ObjectHandle::integer(i64::try_from(first_offset).unwrap_or(i64::MAX)),
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
            ObjectHandle::new_indirect_unresolved(extends, -1),
        ));
    }
    let handle = ObjectHandle::stream(ObjectHandle::dictionary(entries), Rc::clone(&data));
    Ok((handle, data))
}

#[cfg(test)]
mod final_handle_tests {
    use super::{
        emit_objstm_body_from_handles_with_sink, emit_objstm_body_from_members_with_sink,
        wrap_objstm_body_as_handle, ObjStmBody, ObjStmDiscardTarget,
    };
    use crate::writer::output::{OutputSink, OutputTarget};
    use crate::writer::CompressStreams;
    use crate::ObjectHandle;
    use crate::ObjectRef;

    #[test]
    fn sink_emitter_runs_two_passes_and_retains_only_the_second_body() {
        let members = [(ObjectRef::new(7, 0), ObjectHandle::integer(9))];
        let mut calls = 0;
        let mut write_member =
            |out: &mut OutputSink<'_>, _index: u32, _object: ObjectRef, _handle: &ObjectHandle| {
                calls += 1;
                out.write_bytes(b"9")
            };

        let first_pass =
            emit_objstm_body_from_handles_with_sink(&members, &mut write_member, false)
                .expect("sink-backed ObjStm first pass");
        let body = emit_objstm_body_from_handles_with_sink(&members, &mut write_member, true)
            .expect("sink-backed ObjStm second pass");

        assert_eq!(calls, 2);
        assert!(first_pass.bytes.is_empty());
        assert_eq!(body.bytes, b"7 0\n9\n");
        assert_eq!(body.first_offset, 4);
    }

    #[test]
    fn sink_emitter_handles_empty_and_duplicate_batches() {
        let mut write_member = |_out: &mut OutputSink<'_>,
                                _index: u32,
                                _object: ObjectRef,
                                _handle: &ObjectHandle| { Ok(()) };
        let empty = emit_objstm_body_from_handles_with_sink(&[], &mut write_member, false)
            .expect("empty ObjStm batch");
        assert_eq!(
            empty,
            ObjStmBody {
                bytes: Vec::new(),
                first_offset: 0,
                n_members: 0,
            }
        );

        let single = [(ObjectRef::new(8, 0), ObjectHandle::integer(3))];
        let body = emit_objstm_body_from_handles_with_sink(&single, &mut write_member, false)
            .expect("a non-empty ObjStm batch invokes its member callback");
        assert_eq!(body.n_members, 1);

        let duplicate = [
            (ObjectRef::new(7, 0), ObjectHandle::integer(1)),
            (ObjectRef::new(7, 0), ObjectHandle::integer(2)),
        ];
        let error = emit_objstm_body_from_handles_with_sink(&duplicate, &mut write_member, false)
            .expect_err("duplicate ObjStm members must be rejected");
        assert!(error
            .to_string()
            .contains("duplicate member in ObjStm batch 7"));
    }

    #[test]
    fn discard_target_forwards_the_output_target_lifecycle() {
        let mut target = ObjStmDiscardTarget;
        assert_eq!(
            target
                .write_chunk(b"discarded")
                .expect("discard target accepts bytes"),
            b"discarded".len()
        );
        target.finish_segment().expect("discard segment finish");
        target.finish_document().expect("discard document finish");

        let mut write_member =
            |_out: &mut OutputSink<'_>, _index: u32, _object: ObjectRef, _value: &u8| Ok(());
        let members = [(ObjectRef::new(1, 0), 1u8)];
        let body =
            emit_objstm_body_from_members_with_sink(&members, &mut write_member, false, true)
                .expect("generic sink emitter");
        assert_eq!(body.n_members, 1);
    }

    #[test]
    fn object_stream_wrapper_retains_an_extends_reference_handle() {
        let body = ObjStmBody {
            bytes: b"1 0\n7".to_vec(),
            first_offset: 4,
            n_members: 1,
        };
        let (stream, _) =
            wrap_objstm_body_as_handle(body, CompressStreams::No, Some(ObjectRef::new(9, 0)))
                .expect("object stream wrapper");
        assert_eq!(
            stream
                .as_stream_dict()
                .expect("stream dictionary")
                .try_get_key(b"/Extends")
                .expect("Extends key")
                .object_ref(),
            Some(ObjectRef::new(9, 0))
        );
    }

    #[test]
    fn object_stream_wrapper_shares_payload_with_returned_data() {
        for compress in [CompressStreams::No, CompressStreams::Yes] {
            let body = ObjStmBody {
                bytes: vec![b'x'; 4096],
                first_offset: 4,
                n_members: 1,
            };
            let (stream, data) =
                wrap_objstm_body_as_handle(body, compress, None).expect("object stream wrapper");
            let stored = stream.as_stream_data().expect("stream data");

            assert_eq!(
                stored.as_ref().as_ptr(),
                data.as_ptr(),
                "{compress:?} payload must have one allocation"
            );
        }
    }
}
