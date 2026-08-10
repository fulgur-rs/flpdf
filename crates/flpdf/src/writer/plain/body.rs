//! qpdf correspondence: QPDFWriter.cc plain object-body emission split from planning and xref output.
use std::io::{Read, Seek};

use crate::rewrite_renumber::renumber_qpdf_refs_in_place_with_removed;
use crate::writer::object_streams;
use crate::writer::plain::plan::{PlainWritePlan, PlannedIndirectObject};
use crate::writer::plain::xref::{BodyLayout, CompressedLocation};
use crate::writer::{
    reencode_stream_for_compress, serialize, write_reencoded_object, CompressStreams,
    QPDF_BINARY_MARKER,
};
use crate::{Object, Pdf, WriteOptions};

/// Emit every body placement already chosen by `plan`.
///
/// This stage resolves, rewrites, re-encodes, and serializes planned objects.
/// Numbering, membership, trailer construction, and xref output remain the
/// responsibility of the plan and xref stages.
pub(crate) fn emit_bodies<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriteOptions,
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
                let mut object = pdf.resolve(*source)?;
                let offset = bytes.len();
                bytes.extend_from_slice(
                    format!("{} {} obj\n", output.number, output.generation).as_bytes(),
                );
                renumber_qpdf_refs_in_place_with_removed(
                    pdf,
                    &mut object,
                    plan,
                    &plan.removed_refs,
                )?;
                match object {
                    Object::Stream(stream) => {
                        let (reencoded, source_filter_is_lone_flate) = reencode_stream_for_compress(
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
                        )?; // cov:ignore: no emitter means this validated stream serializer is infallible
                    }
                    other => other.write_pdf(&mut bytes),
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
                        let Some(stream) = object.as_stream() else {
                            return Err(crate::Error::Unsupported(format!(
                                "plain writer: source ObjStm {} {} R is not a stream",
                                source.number, source.generation
                            )));
                        };
                        match stream.dict.get("Extends") {
                            Some(Object::Reference(extends)) => Some(
                                plan.old_to_new.get(extends).copied().ok_or_else(|| {
                                    crate::Error::Unsupported(format!(
                                        "plain writer: source ObjStm /Extends {} {} R is absent from renumber map",
                                        extends.number, extends.generation
                                    ))
                                })?,
                            ),
                            _ => None,
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
    use std::io::Cursor;

    #[test]
    fn disable_emission_records_every_planned_source_offset() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriteOptions {
            full_rewrite: true,
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriteOptions::default()
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
            let options = WriteOptions {
                full_rewrite: true,
                object_streams: ObjectStreamMode::Generate,
                compress_streams,
                static_id: true,
                newline_before_endstream: NewlineBeforeEndstream::Never,
                ..WriteOptions::default()
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
        let options = WriteOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriteOptions::default()
        };
        let mut plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        plan.old_to_new.remove(&crate::ObjectRef::new(2, 0));

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("reference 2 0 R absent from renumber map")));
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
        let options = WriteOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriteOptions::default()
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
        let options = WriteOptions {
            object_streams: ObjectStreamMode::Generate,
            ..WriteOptions::default()
        };
        let mut plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        plan.old_to_new.remove(&crate::ObjectRef::new(2, 0));

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("reference 2 0 R absent from renumber map")));
    }

    fn assert_invalid_planned_member_is_rejected(invalid: Object, expected_kind: &str) {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriteOptions {
            object_streams: ObjectStreamMode::Generate,
            ..WriteOptions::default()
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
