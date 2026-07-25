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
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{}\n", plan.version).as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);

    let mut layout = BodyLayout::default();
    for planned in &plan.objects {
        match planned {
            PlannedIndirectObject::Source { source, output } => {
                let offset = bytes.len();
                bytes.extend_from_slice(
                    format!("{} {} obj\n", output.number, output.generation).as_bytes(),
                );
                let mut object = pdf.resolve(*source)?;
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
                        );
                        write_reencoded_object(
                            &mut bytes,
                            &reencoded,
                            source_filter_is_lone_flate,
                            options,
                        );
                    }
                    other => other.write_pdf(&mut bytes),
                }
                bytes.extend_from_slice(b"\nendobj\n");
                layout
                    .uncompressed
                    .insert(output.number, (output.generation, offset));
            }
            PlannedIndirectObject::ObjectStream { output, members } => {
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
                serialize::write_objstm_stream(
                    &mut bytes,
                    &body,
                    structural_compress,
                    options.newline_before_endstream,
                )?;
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
    }

    Ok((bytes, layout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::plain::plan::PlannedIndirectObject;
    use crate::{NewlineBeforeEndstream, ObjectStreamMode};

    #[test]
    fn disable_emission_records_every_planned_source_offset() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open_mem(fixture).unwrap();
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
            let mut pdf = Pdf::open_mem(fixture).unwrap();
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
                    PlannedIndirectObject::ObjectStream { output, members } => {
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
}
