//! qpdf correspondence: QPDFWriter.cc plain object-body emission split from planning and xref output.
use std::io::{Read, Seek};
#[cfg(test)]
use std::rc::Rc;

use crate::rewrite_renumber::renumber_qpdf_refs_in_place_with_removed;
use crate::writer::object_streams;
use crate::writer::plain::plan::{PlainWritePlan, PlannedIndirectObject};
use crate::writer::plain::xref::{BodyLayout, CompressedLocation};
use crate::writer::WriterOptions;
use crate::writer::{
    reencode_stream_for_compress, serialize, write_reencoded_object, CompressStreams,
    StreamEncryptionOptions, QPDF_BINARY_MARKER,
};
use crate::{Object, ObjectHandle, Pdf};

/// Emit every body placement already chosen by `plan`.
///
/// This stage resolves, rewrites, re-encodes, and serializes planned objects.
/// Numbering, membership, trailer construction, and xref output remain the
/// responsibility of the plan and xref stages.
pub(crate) fn emit_bodies<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    plan: &PlainWritePlan,
) -> crate::Result<(Vec<u8>, BodyLayout)> {
    validate_objstm_member_bodies(pdf, plan)?;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{}\n", plan.version).as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);

    let mut layout = BodyLayout::default();
    let expected = plan.objects.len().max(1);
    let mut events = 0_usize;
    for planned in &plan.objects {
        match planned {
            PlannedIndirectObject::Source { source, output } => {
                let offset = bytes.len();
                bytes.extend_from_slice(
                    format!("{} {} obj\n", output.number, output.generation).as_bytes(),
                );
                if plan.canonical {
                    emit_canonical_source(pdf, options, plan, *source, &mut bytes)?;
                } else {
                    let mut object = pdf.resolve(*source)?;
                    renumber_qpdf_refs_in_place_with_removed(
                        pdf,
                        &mut object,
                        plan,
                        &plan.removed_refs,
                    )?; // cov:ignore: validated source placements have already passed the reference-map invariant
                    match object {
                        Object::Stream(stream) => {
                            let (reencoded, source_filter_is_lone_flate) =
                                reencode_stream_for_compress(
                                    stream,
                                    options,
                                    true,
                                    pdf.recovered_stream_eol(*source),
                                    false,
                                    false,
                                );
                            write_reencoded_object(
                                &mut bytes,
                                &reencoded,
                                source_filter_is_lone_flate,
                                options,
                                None,
                                *output,
                                StreamEncryptionOptions::new(None, true),
                            )?; // cov:ignore: no emitter means this validated stream serializer is infallible
                        }
                        other => other.write_pdf(&mut bytes),
                    }
                }
                bytes.extend_from_slice(b"\nendobj\n");
                layout
                    .uncompressed
                    .insert(output.number, (output.generation, offset));
            }
            PlannedIndirectObject::ObjectStream {
                origin,
                output,
                members,
            } => {
                let mut resolved = Vec::with_capacity(members.len());
                for member in members {
                    let mut object = pdf.resolve(member.source)?;
                    renumber_qpdf_refs_in_place_with_removed(
                        pdf,
                        &mut object,
                        plan,
                        &plan.removed_refs,
                    )?;
                    resolved.push((member.output, object));
                }
                let body = object_streams::emit_objstm_body_from_resolved(&resolved)?;
                let offset = bytes.len();
                bytes.extend_from_slice(
                    format!("{} {} obj\n", output.number, output.generation).as_bytes(),
                );
                let structural_compress = if plan.trailer.structural_filtered {
                    CompressStreams::Yes
                } else {
                    CompressStreams::No
                };
                let extends = match origin {
                    crate::writer::plain::plan::PlannedObjectStreamOrigin::SourceBacked(source) => {
                        let object = pdf.resolve(*source)?;
                        match object.as_stream() {
                            Some(stream) => match stream.dict.get("Extends") {
                                Some(Object::Reference(extends)) => Some(
                                    plan.old_to_new.get(extends).copied().ok_or_else(|| {
                                        crate::Error::Unsupported(format!(
                                            "plain writer: source ObjStm /Extends {} {} R is absent from renumber map",
                                            extends.number, extends.generation
                                        ))
                                    })?,
                                ),
                                _ => None,
                            },
                            // qpdf permits a null or otherwise non-stream source
                            // identity here as a placeholder for a reconstructed
                            // object stream. The rebuilt container still carries
                            // the surviving members, but has no /Extends key.
                            None => None,
                        }
                    }
                    crate::writer::plain::plan::PlannedObjectStreamOrigin::Synthetic => None,
                };
                serialize::write_objstm_stream_with_extends(
                    &mut bytes,
                    &body,
                    structural_compress,
                    options.newline_before_endstream,
                    extends,
                )?; // cov:ignore: error arm requires an in-memory zlib encoder failure
                bytes.extend_from_slice(b"\nendobj\n");
                layout
                    .uncompressed
                    .insert(output.number, (output.generation, offset));
                for (index, member) in members.iter().enumerate() {
                    layout.compressed.insert(
                        member.output.number,
                        CompressedLocation {
                            container: output.number,
                            index: u32::try_from(index).unwrap_or(u32::MAX),
                        },
                    );
                }
            }
        }
        crate::writer::report_progress_event(options, &mut events, expected);
    }

    Ok((bytes, layout))
}

fn emit_canonical_source<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    plan: &PlainWritePlan,
    source: crate::ObjectRef,
    bytes: &mut Vec<u8>,
) -> crate::Result<()> {
    let handle = pdf.get_object_handle(source);
    pdf.resolve_object_handle(&handle)?;
    let map = |object_ref| {
        plan.new_for_original(object_ref).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "plain writer: reference {} {} R absent from renumber map",
                object_ref.number, object_ref.generation
            ))
        })
    };

    if handle.as_stream_dict().is_some() {
        let (dict, data, refiltered) = canonical_stream_output(&handle, options)?;
        dict.unparse_stream_body_with_ref_map_and_removed(
            bytes,
            refiltered,
            &map,
            &plan.removed_refs,
        )?; // cov:ignore: LLVM attributes the validated success continuation to the call lines above
        serialize::write_stream_payload(bytes, &data, options.newline_before_endstream);
    } else {
        handle.unparse_object_with_ref_map_and_removed(bytes, &map, &plan.removed_refs)?;
    }
    Ok(())
}

fn canonical_stream_output(
    handle: &ObjectHandle,
    options: &WriterOptions,
) -> crate::Result<(ObjectHandle, Vec<u8>, bool)> {
    let stream_dict = handle
        .as_stream_dict()
        .ok_or_else(|| crate::Error::Internal("canonical stream dictionary is missing".into()))?;
    let source_has_lone_flate = canonical_is_lone_flate(&stream_dict)?;
    // QPDFWriter.cc:1251-1278 gives cleartext /Type /Metadata streams their
    // own policy: decode fully and emit without a filter, even when the global
    // writer policy would preserve or compress a lone-Flate source. The plain
    // route is unencrypted, so this exception always applies here.
    let is_metadata_stream = stream_dict.try_is_dictionary_of_type(b"Metadata", b"")?;
    let policy = if is_metadata_stream {
        Some(CompressStreams::No)
    } else {
        crate::writer::effective_stream_policy(options)
    };
    let decode_level = if is_metadata_stream {
        crate::writer::DecodeLevel::All
    } else {
        options.decode_level
    };
    let preserve_lone_flate = matches!(policy, Some(CompressStreams::Yes))
        && source_has_lone_flate
        && !handle.is_data_modified()
        && !options.recompress_flate
        && !options.content_normalization
        && !stream_dict.try_has_key(b"/F")?;
    let source_for_pipe = handle.clone();

    // QPDFWriter::willFilterStream starts with `isDataModified()` before it
    // considers the user compression policy (`QPDFWriter.cc:1234-1245`). A
    // token-filtered stream must therefore take the pipe path even under
    // Preserve mode; only an unmodified stream may be emitted verbatim.
    let (data, filtering_attempted) =
        if !handle.is_data_modified() && (policy.is_none() || preserve_lone_flate) {
            (
                source_for_pipe.get_raw_stream_data()?.as_ref().clone(),
                false,
            )
        } else {
            let mut buffer = crate::pipeline::buffer::Buffer::new("canonical writer stream", None);
            let mut filtering_attempted = false;
            let encode_flags = if matches!(policy, Some(CompressStreams::Yes)) {
                crate::object_handle::STREAM_ENCODE_COMPRESS
            } else {
                0
            };
            let success = source_for_pipe.pipe_stream_data(
                &mut buffer,
                &mut filtering_attempted,
                encode_flags,
                decode_level,
                true,
                true,
            )?; // cov:ignore: filter-pipeline failures are covered at the pipeline boundary, not by this validated emitter
            let data = if !success {
                // QPDFWriter retries a failed filter pipeline against a fresh raw
                // source (QPDFWriter.cc:1287-1314). The first pipeline may have
                // consumed or partially filled its destination, so do not emit
                // that buffer when filtering fails.
                filtering_attempted = false;
                source_for_pipe.get_raw_stream_data()?.as_ref().clone()
            } else {
                buffer.take_buffer()?.to_vec()
            };
            (data, filtering_attempted)
        };

    let mut entries = stream_dict.try_as_dictionary()?.unwrap_or_default();
    entries.remove(b"/Length".as_slice());
    if filtering_attempted {
        entries.retain(|key, _| {
            !matches!(
                key.as_slice(),
                b"/Filter" | b"/DecodeParms" | b"/F" | b"/FFilter" | b"/FDecodeParms"
            )
        });
        if matches!(policy, Some(CompressStreams::Yes)) {
            entries.insert(
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            );
        }
    }
    entries.insert(
        b"/Length".to_vec(),
        ObjectHandle::integer(i64::try_from(data.len()).unwrap_or(i64::MAX)),
    );
    let dict = ObjectHandle::dictionary(entries.into_iter().collect());
    let refiltered = filtering_attempted && matches!(policy, Some(CompressStreams::Yes));
    Ok((dict, data, refiltered))
}

fn canonical_is_lone_flate(dict: &ObjectHandle) -> crate::Result<bool> {
    let filter = dict.try_get_key(b"/Filter")?;
    if filter.try_is_null()? {
        return Ok(false);
    }
    if filter.try_is_name_and_equals(b"FlateDecode")? || filter.try_is_name_and_equals(b"Fl")? {
        return Ok(true);
    }
    Ok(false)
}

fn validate_objstm_member_bodies<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    plan: &PlainWritePlan,
) -> crate::Result<()> {
    if !plan
        .objects
        .iter()
        .any(|object| matches!(object, PlannedIndirectObject::ObjectStream { .. }))
    {
        return Ok(());
    }

    let context = object_streams::eligibility_context(pdf)?;
    for planned in &plan.objects {
        let PlannedIndirectObject::ObjectStream { members, .. } = planned else {
            continue;
        };
        for member in members {
            let is_signature = object_streams::is_qpdf_signature_dict(pdf, member.source)?;
            let violation = {
                let object = pdf.resolve_borrowed(member.source)?;
                planned_member_body_violation(member.source, member.output, object, &context)
            }
            .or(is_signature.then_some("signature dictionary"));
            if let Some(kind) = violation {
                return Err(crate::Error::Unsupported(format!(
                    "plain writer body invariant: source {} planned as ObjStm member {} \
                     resolves to forbidden {kind}",
                    member.source, member.output
                )));
            }
        }
    }
    Ok(())
}

fn planned_member_body_violation(
    source: crate::ObjectRef,
    output: crate::ObjectRef,
    object: &Object,
    context: &object_streams::EligibilityContext,
) -> Option<&'static str> {
    if output.generation != 0 {
        return Some("nonzero output generation");
    }
    if matches!(object, Object::Stream(_)) {
        return Some("stream body");
    }
    if let Some(dict) = object.as_dict() {
        match dict.get("Type") {
            Some(Object::Name(name)) if name.as_slice() == b"XRef" => {
                return Some("/Type /XRef dictionary");
            }
            Some(Object::Name(name)) if name.as_slice() == b"ObjStm" => {
                return Some("/Type /ObjStm dictionary");
            }
            _ => {}
        }
    }
    if context.encryption_ref == Some(source) {
        return Some("encryption dictionary");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::plain::plan::PlannedIndirectObject;
    use crate::{Dictionary, NewlineBeforeEndstream, ObjectRef, ObjectStreamMode, Stream};
    use std::cell::Cell;
    use std::io::Cursor;

    #[test]
    fn disable_emission_records_every_planned_source_offset() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let (_, layout) = emit_bodies(&mut pdf, &options, &plan).unwrap();
        assert!(layout.compressed.is_empty());
        assert_eq!(
            layout.uncompressed.len(),
            plan.objects
                .iter()
                .filter(|object| matches!(object, PlannedIndirectObject::Source { .. }))
                .count()
        );
    }

    #[test]
    fn object_stream_emission_records_container_and_member_locations() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        for compress_streams in [CompressStreams::Yes, CompressStreams::No] {
            let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
            let options = WriterOptions {
                object_streams: ObjectStreamMode::Generate,
                compress_streams,
                static_id: true,
                newline_before_endstream: NewlineBeforeEndstream::Never,
                ..WriterOptions::default()
            };
            let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
            let (bytes, layout) = emit_bodies(&mut pdf, &options, &plan).unwrap();
            layout.validate().unwrap();

            let mut planned_members = 0;
            for object in &plan.objects {
                match object {
                    PlannedIndirectObject::Source { output, .. } => {
                        let (_, offset) = layout.uncompressed[&output.number];
                        assert!(bytes[offset..].starts_with(
                            format!("{} {} obj\n", output.number, output.generation).as_bytes()
                        ));
                    }
                    PlannedIndirectObject::ObjectStream {
                        output, members, ..
                    } => {
                        let (_, offset) = layout.uncompressed[&output.number];
                        assert!(bytes[offset..].starts_with(
                            format!("{} {} obj\n", output.number, output.generation).as_bytes()
                        ));
                        for (index, member) in members.iter().enumerate() {
                            assert_eq!(
                                layout.compressed[&member.output.number],
                                CompressedLocation {
                                    container: output.number,
                                    index: u32::try_from(index).unwrap(),
                                }
                            );
                        }
                        planned_members += members.len();
                    }
                }
            }
            assert!(planned_members > 0);
            assert_eq!(layout.compressed.len(), planned_members);
        }
    }

    #[test]
    fn source_emission_propagates_reference_rewrite_failure() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        let mut plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        plan.old_to_new.remove(&crate::ObjectRef::new(2, 0));

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("reference 2 0 R absent from renumber map")));
    }

    #[test]
    fn canonical_emission_reuses_a_legacy_recovered_stream_eol() {
        let fixture =
            include_bytes!("../../../../../tests/fixtures/compat/null-length-framing-matrix.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        pdf.resolve(ObjectRef::new(5, 0))
            .expect("legacy resolution records the recovered framing EOL");
        assert_eq!(
            pdf.recovered_stream_eol(ObjectRef::new(5, 0)),
            Some(&b"\n"[..])
        );
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            stream_data: Some(crate::StreamDataMode::Preserve),
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };
        let (bytes, _) = pdf
            .with_plain_writer_stream_recovery(|pdf| {
                let plan = PlainWritePlan::build(pdf, &options)?;
                emit_bodies(pdf, &options, &plan)
            })
            .unwrap();
        assert!(bytes
            .windows(b"missing-lf".len())
            .any(|window| window == b"missing-lf"));
    }

    #[test]
    fn canonical_stream_output_does_not_duplicate_a_recovered_stream_eol() {
        let fixture =
            include_bytes!("../../../../../tests/fixtures/compat/null-length-framing-matrix.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        pdf.resolve(ObjectRef::new(5, 0))
            .expect("legacy resolution records the recovered framing EOL");
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            stream_data: Some(crate::StreamDataMode::Preserve),
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };

        let (_, data) = pdf
            .with_plain_writer_stream_recovery(|pdf| {
                let handle = pdf.get_object_handle(ObjectRef::new(5, 0));
                pdf.resolve_object_handle(&handle)?;
                let (_, data, _) = canonical_stream_output(&handle, &options)?;
                Ok((Vec::<u8>::new(), data))
            })
            .unwrap();

        assert_eq!(data, b"missing-lf\n");
    }

    #[test]
    fn canonical_stream_output_decodes_metadata_even_under_compress_policy() {
        let mut filter_dict = Dictionary::new();
        filter_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = crate::filters::encode_stream_data(&filter_dict, b"metadata")
            .expect("metadata payload must encode");
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"Type".to_vec(), ObjectHandle::name(b"Metadata".to_vec())),
                (
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ),
                (
                    b"Length".to_vec(),
                    ObjectHandle::integer(encoded.len() as i64),
                ),
            ]),
            Rc::new(encoded),
        );

        let options = WriterOptions::default();
        let (dict, data, refiltered) = canonical_stream_output(&stream, &options).unwrap();

        assert_eq!(data, b"metadata");
        assert!(!refiltered);
        assert!(!dict.try_has_key(b"/Filter").unwrap());
    }

    #[test]
    fn canonical_emission_turns_a_reference_to_a_removed_object_into_null() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let root = pdf.root_ref().unwrap();
        let removed = ObjectRef::new(100, 0);
        let stream_ref = ObjectRef::new(101, 0);
        pdf.set_object(removed, Object::Integer(7));
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Length", Object::Integer(3));
        stream_dict.insert("StreamRemovedDirect", Object::Reference(removed));
        pdf.set_object(
            stream_ref,
            Object::Stream(Stream {
                dict: stream_dict,
                data: b"abc".to_vec(),
            }),
        );
        let mut catalog = pdf.resolve(root).unwrap().into_dict().unwrap();
        catalog.insert("RemovedDirect", Object::Reference(removed));
        catalog.insert(
            "RemovedArray",
            Object::Array(vec![Object::Reference(removed)]),
        );
        catalog.insert("RemovedStream", Object::Reference(stream_ref));
        pdf.set_object(root, Object::Dictionary(catalog));
        pdf.delete_object(removed);

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let (bytes, _) = emit_bodies(&mut pdf, &options, &plan).unwrap();

        assert!(bytes
            .windows(b"/RemovedArray [ null ]".len())
            .any(|window| window == b"/RemovedArray [ null ]"));
        assert!(!bytes
            .windows(b"/RemovedDirect".len())
            .any(|window| window == b"/RemovedDirect"));
        assert!(!bytes
            .windows(b"/StreamRemovedDirect".len())
            .any(|window| window == b"/StreamRemovedDirect"));
    }

    #[test]
    fn canonical_stream_output_retries_with_the_raw_payload_after_a_source_decode_failure() {
        let mut bytes = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let bodies: [(u32, &[u8]); 4] = [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /MediaBox [0 0 10 10] >>",
            ),
            (
                4,
                b"<< /Length 3 /Filter /FlateDecode >>\nstream\nabc\nendstream",
            ),
        ];
        let mut offsets = [0_usize; 5];
        for (number, body) in bodies {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );

        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let stream = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve_object_handle(&stream).unwrap();
        let options = WriterOptions {
            recompress_flate: true,
            ..WriterOptions::default()
        };

        let (dict, data, refiltered) = canonical_stream_output(&stream, &options).unwrap();

        assert_eq!(data, b"abc");
        assert!(!refiltered);
        assert!(dict
            .get_key(b"/Filter")
            .try_is_name_and_equals(b"FlateDecode")
            .unwrap());
    }

    #[test]
    fn canonical_stream_output_marks_the_first_provider_attempt_retryable() {
        let pdf = Pdf::empty().unwrap();
        let stream = pdf.new_stream().unwrap();
        let retry_seen = Rc::new(Cell::new(false));
        let retry_seen_in_callback = Rc::clone(&retry_seen);

        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, _suppress_warnings, will_retry| {
                    retry_seen_in_callback.set(will_retry);
                    pipeline
                        .write(b"provider bytes")
                        .map_err(crate::Error::from)?;
                    pipeline.finish().map_err(crate::Error::from)?;
                    Ok(true)
                },
                None,
                None,
            )
            .unwrap();

        let options = WriterOptions {
            compress_streams: CompressStreams::Yes,
            ..WriterOptions::default()
        };
        let (_, data, refiltered) = canonical_stream_output(&stream, &options).unwrap();

        assert!(retry_seen.get());
        assert!(refiltered);
        assert!(!data.is_empty());
    }

    #[test]
    fn canonical_stream_output_recognizes_a_lone_flate_abbreviation() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), ObjectHandle::name(b"Fl".to_vec())),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);

        assert!(canonical_is_lone_flate(&dict).unwrap());

        let stream = ObjectHandle::stream(dict, Rc::new(b"abc".to_vec()));
        let options = WriterOptions {
            compress_streams: CompressStreams::Yes,
            ..WriterOptions::default()
        };

        let (_, data, refiltered) = canonical_stream_output(&stream, &options).unwrap();

        assert_eq!(data, b"abc");
        assert!(!refiltered);
    }

    #[test]
    fn disable_emission_serializes_reachable_source_objstm_container() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page-objstm.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let root = pdf.root_ref().unwrap();
        let mut catalog = pdf.resolve(root).unwrap().into_dict().unwrap();
        let source_objstm = ObjectRef::new(1, 0);
        catalog.insert("ReachableStructural", Object::Reference(source_objstm));
        pdf.set_object(root, Object::Dictionary(catalog));
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let output_objstm = plan.new_for_original(source_objstm).unwrap();

        let (bytes, layout) = emit_bodies(&mut pdf, &options, &plan).unwrap();

        assert!(layout.uncompressed.contains_key(&output_objstm.number));
        assert!(bytes
            .windows(b"/Type /ObjStm".len())
            .any(|window| window == b"/Type /ObjStm"));
    }

    #[test]
    fn object_stream_emission_propagates_reference_rewrite_failure() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let mut plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        plan.old_to_new.remove(&crate::ObjectRef::new(2, 0));

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("reference 2 0 R absent from renumber map")));
    }

    #[test]
    fn source_objstm_extends_must_be_present_in_the_renumber_map() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page-objstm.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Preserve,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let mut candidates = vec![PlannedIndirectObject::Source {
            source: ObjectRef::new(0, 0),
            output: ObjectRef::new(0, 0),
        }];
        candidates.extend(plan.objects.iter().cloned());
        let container_source = candidates
            .iter()
            .find_map(|object| match object {
                PlannedIndirectObject::ObjectStream {
                    origin:
                        crate::writer::plain::plan::PlannedObjectStreamOrigin::SourceBacked(source),
                    ..
                } => Some(*source),
                PlannedIndirectObject::Source { .. }
                | PlannedIndirectObject::ObjectStream {
                    origin: crate::writer::plain::plan::PlannedObjectStreamOrigin::Synthetic,
                    ..
                } => None,
            })
            .expect("preserve plan must retain a source-backed ObjStm");

        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        dict.insert("N", Object::Integer(0));
        dict.insert("First", Object::Integer(0));
        dict.insert("Length", Object::Integer(0));
        dict.insert("Extends", Object::Reference(ObjectRef::new(99_999, 0)));
        pdf.set_object(
            container_source,
            Object::Stream(Stream {
                dict,
                data: Vec::new(),
            }),
        );

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("/Extends 99999 0 R is absent from renumber map")));
    }

    fn assert_invalid_planned_member_is_rejected(invalid: Object, expected_kind: &str) {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let members: Vec<_> = plan
            .objects
            .iter()
            .filter_map(|object| match object {
                PlannedIndirectObject::ObjectStream { members, .. } => Some(members),
                PlannedIndirectObject::Source { .. } => None,
            })
            .flatten()
            .cloned()
            .collect();
        let member = members
            .into_iter()
            .find(|member| Some(member.source) != pdf.root_ref())
            .unwrap();
        pdf.set_object(member.source, invalid);

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("plain writer body invariant")
                && message.contains(&member.source.to_string())
                && message.contains(expected_kind)));
    }

    #[test]
    fn invalid_planned_objstm_stream_member_is_rejected() {
        let mut dict = Dictionary::new();
        dict.insert("Length", Object::Integer(0));
        assert_invalid_planned_member_is_rejected(
            Object::Stream(Stream {
                dict,
                data: Vec::new(),
            }),
            "stream",
        );
    }

    #[test]
    fn invalid_planned_objstm_xref_dictionary_member_is_rejected() {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"XRef".to_vec()));
        assert_invalid_planned_member_is_rejected(Object::Dictionary(dict), "/Type /XRef");
    }

    #[test]
    fn invalid_planned_nested_objstm_dictionary_member_is_rejected() {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        assert_invalid_planned_member_is_rejected(Object::Dictionary(dict), "/Type /ObjStm");
    }

    #[test]
    fn invalid_planned_signature_dictionary_member_is_rejected() {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"Sig".to_vec()));
        dict.insert("ByteRange", Object::Array(Vec::new()));
        dict.insert("Contents", Object::String(vec![0]));
        assert_invalid_planned_member_is_rejected(Object::Dictionary(dict), "signature");
    }

    #[test]
    fn planned_member_identity_invariants_are_classified() {
        let source = crate::ObjectRef::new(7, 0);
        let object = Object::Null;
        let ordinary = object_streams::EligibilityContext {
            encryption_ref: None,
        };
        assert_eq!(
            planned_member_body_violation(source, crate::ObjectRef::new(2, 1), &object, &ordinary,),
            Some("nonzero output generation")
        );

        let encrypted = object_streams::EligibilityContext {
            encryption_ref: Some(source),
        };
        assert_eq!(
            planned_member_body_violation(source, crate::ObjectRef::new(2, 0), &object, &encrypted,),
            Some("encryption dictionary")
        );
    }
}
