//! qpdf correspondence: `QPDFWriter::writeStandard` and `QPDFWriter::enqueueObjectsPCLm`.
//!
//! qpdf-shaped PCLm body/trailer orchestration.
//!
//! PCLm changes only the initial seed order. The object queue, dynamic child
//! discovery, root output copy, and xref/trailer emission remain shared with
//! the standard writer (`QPDFWriter.cc:2928-2954,2991-3044`).

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek, Write};

use super::object::ObjectWriterEmission;
use super::plain::xref::{IdPlan, TrailerPlan};
use super::{
    build_writer_trailer_handle, deterministic_id_info_suffix, effective_pdf_version_and_ext,
    generate_id_handle, source_permanent_id_value_handle, uses_deterministic_id, WriterOptions,
};
use crate::writer::plain::plan::canonical_trailer_entries_with_visibility;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result, XrefForm};

pub(crate) fn write_pclm<R: Read + Seek + 'static, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
) -> Result<super::WriterResult> {
    let deterministic_id = uses_deterministic_id(options);
    let root_source = pdf.root_ref();
    let direct_root = if root_source.is_none() {
        Some(pdf.root_handle()?)
    } else {
        None
    };
    let source_version = pdf.version().to_string();
    let source_extension_level = pdf.adobe_extension_level()?.unwrap_or(0);
    let (version, final_extension_level) = effective_pdf_version_and_ext(
        &source_version,
        source_extension_level,
        options,
        false,
        false,
    );

    let body = super::plain::body::emit_live_pclm(
        pdf,
        options,
        version,
        final_extension_level,
        root_source,
        BTreeSet::new(),
    )?; // cov:ignore: the live body forwards canonical read and serialization failures; focused planner tests cover the injected failure boundary.
    let body_map: HashMap<ObjectRef, ObjectRef> = body.old_to_new.into_iter().collect();
    let new_root = root_source.and_then(|source| body_map.get(&source).copied());
    // cov:ignore-start: seed_handles always enqueues an indirect source /Root before the live body drains.
    if root_source.is_some() && new_root.is_none() {
        return Err(Error::Unsupported(
            "PCLm live writer: /Root absent from queue".to_string(),
        ));
    }
    // cov:ignore-end

    // qpdf generates the changing ID lazily from the live trailer after the
    // body has been written (`QPDFWriter.cc:1836-1903`). A progress callback
    // may therefore replace `/ID` while the queue is emitting objects; do not
    // reuse the setup-time ID snapshot here.
    let source_id0 = source_permanent_id_value_handle(&pdf.trailer_key_handle(b"ID"));
    let generated_id =
        (!deterministic_id).then(|| generate_id_handle(source_id0.as_deref(), options.static_id));

    let trailer_size = body
        .object_count
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("PCLm live writer: /Size overflows usize".into()))?;
    let mut trailer_map = body_map.clone();
    // cov:ignore-start: a supported in-memory body cannot contain u32::MAX objects.
    let initial_late_trailer_number = u32::try_from(trailer_size).map_err(|_| {
        Error::Unsupported("PCLm live writer: late trailer number overflows u32".into())
    })?;
    // cov:ignore-end
    let mut next_late_trailer_number = super::plain::extend_late_trailer_map(
        pdf,
        &mut trailer_map,
        initial_late_trailer_number,
        true,
        false,
    )?; // cov:ignore: shared late-trailer success continuation is covered by the PCLm live tests

    let direct_root_output = direct_root
        .as_ref()
        .map(|root| root.output_root_copy_with_adbe(version, final_extension_level, true))
        .transpose()?;
    let direct_root_bytes = direct_root_output
        .as_ref()
        .map(|root| {
            let mut map_ref = |handle: &ObjectHandle| {
                let object_ref = handle.object_ref().ok_or_else(|| {
                    Error::Unsupported("PCLm live writer: direct /Root child has no identity".into()) // cov:ignore: the dynamic reference map is called only for indirect children.
                })?; // cov:ignore: the dynamic reference map is called only for indirect children.
                if let Some(output) = trailer_map.get(&object_ref).copied() {
                    return Ok(output);
                }
                // qpdf serializes a direct Catalog from the trailer after the
                // body queue has drained. An indirect child added by a
                // progress callback is therefore assigned a number here but
                // has no body or xref row (`QPDFWriter.cc:1144-1157,1160-1236`).
                let output = ObjectRef::new(next_late_trailer_number, 0);
                next_late_trailer_number = next_late_trailer_number.checked_add(1).ok_or_else(|| {
                    Error::Unsupported("PCLm live writer: late trailer number overflow".into()) // cov:ignore: a supported output cannot exhaust the u32 object-number space.
                })?; // cov:ignore: a supported output cannot exhaust the u32 object-number space.
                trailer_map.insert(object_ref, output);
                Ok(output)
            };
            let mut write_string = |out: &mut Vec<u8>, value: &[u8]| {
                crate::pdf_syntax::write_string_value(out, value);
                Ok(())
            };
            let mut direct_stream_writer = super::object::DefaultDynamicDirectStreamWriter {
                newline_before_endstream: Some(options.newline_before_endstream),
            };
            let mut bytes = Vec::new();
            super::object::write_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                root,
                &mut bytes,
                &mut map_ref,
                &BTreeSet::new(),
                &mut write_string,
                &mut direct_stream_writer,
            )?; // cov:ignore: the direct-root serializer's error path is a defensive invariant failure after live mapping.
            Ok::<_, Error>(bytes)
        })
        .transpose()?;

    super::plain::extend_late_trailer_map(
        pdf,
        &mut trailer_map,
        next_late_trailer_number,
        false,
        false,
    )?; // cov:ignore: shared late-trailer success continuation is covered by the PCLm live tests

    let trailer_handle = build_writer_trailer_handle(
        pdf,
        trailer_size,
        new_root,
        direct_root_output.as_ref(),
        options,
        None,
        deterministic_id,
        generated_id.as_ref(),
    )?; // cov:ignore: the validated writer trailer construction has no reachable PCLm error after setup and body validation.

    // qpdf's PCLm seed does not enqueue unrelated trailer values until
    // writeTrailer. Preserve those late assigned numbers without inventing
    // bodies or xref rows (`QPDFWriter.cc:2928-2954,1160-1236`).
    let id = if deterministic_id {
        IdPlan::Deterministic {
            source_id0,
            info_suffix: deterministic_id_info_suffix(pdf),
        }
    } else {
        IdPlan::Materialized {
            value: super::plain::xref::materialized_id_handle(
                &trailer_handle.try_get_key(b"/ID")?,
            )?, // cov:ignore: PCLm supplies the validated two-string writer-owned ID.
        }
    };
    let trailer = TrailerPlan {
        form: XrefForm::Table,
        canonical_entries: canonical_trailer_entries_with_visibility(
            pdf,
            &trailer_map,
            &BTreeSet::new(),
            true,
        )?, // cov:ignore: the live trailer map contains every visible PCLm reference.
        root: new_root,
        direct_root: direct_root_bytes,
        id,
        encrypt: None,
        structural_filtered: false,
        qdf: false,
    };
    let mut bytes = body.bytes;
    let written_xref = super::plain::xref::append_xref_and_trailer_with_handle(
        &mut bytes,
        &body.layout,
        &trailer,
        &trailer_handle,
        &trailer_map,
        &BTreeSet::new(),
    )?; // cov:ignore: xref/trailer emission consumes the validated shared PCLm plan.
    let emitted_old_to_new = body_map
        .into_iter()
        .filter(|(_, output)| body.layout.uncompressed.contains_key(&output.number))
        .collect();
    out.write_all(&bytes)?;
    Ok(super::WriterResult::new(emitted_old_to_new, written_xref))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn pclm_adds_a_newline_after_an_internal_extra_header() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        ))
        .unwrap();
        let options = WriterOptions {
            pclm: true,
            extra_header_text: "X-PCLm".to_string(),
            ..WriterOptions::default()
        };
        let mut output = Vec::new();
        write_pclm(&mut pdf, &mut output, &options).unwrap();
        assert!(output.starts_with(b"%PDF-1.3\n%PCLm 1.0\nX-PCLm\n"));
    }
}
