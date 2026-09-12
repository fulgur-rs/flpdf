//! qpdf correspondence: QPDFWriter.cc standard write pipeline split across plain writer modules.
use std::io::{Read, Seek};

use crate::writer::output::OutputSink;
use crate::writer::plain::xref::{IdPlan, TrailerPlan};
use crate::writer::ObjectWriterEmission;
use crate::writer::WriterOptions;
use crate::writer::WriterResult;
use crate::{CompressStreams, ObjectRef, ObjectStreamMode, Pdf, XrefForm};
use std::collections::{BTreeMap, HashMap};

pub(crate) mod body;
pub(crate) mod plan;
pub(crate) mod xref;

pub(crate) fn write_plain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    out: &mut OutputSink<'_>,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
    encryption_parameters: Option<crate::writer::EncryptionParameters>,
    source_object_stream_data: &BTreeMap<u32, u32>,
    special_streams: Option<&crate::writer::SpecialStreams>,
    generated_compressible: Option<&crate::writer::object_streams::CompressiblePlan>,
    generated_object_stream_sources: &[ObjectRef],
) -> crate::Result<WriterResult> {
    let _ = (generated_compressible, generated_object_stream_sources);
    // qpdf's writeStandard uses one enqueueObject/object_queue walk for every
    // non-linearized object-stream and QDF combination. Membership is fixed
    // during setup; numbering and surviving stream-child discovery remain
    // emission-time responsibilities of that one live queue.
    write_plain_live(
        pdf,
        out,
        options,
        generated_id,
        encryption_parameters,
        source_object_stream_data,
        special_streams,
    )
}

fn write_plain_live<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    out: &mut OutputSink<'_>,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
    encryption_parameters: Option<crate::writer::EncryptionParameters>,
    source_object_stream_data: &BTreeMap<u32, u32>,
    special_streams: Option<&crate::writer::SpecialStreams>,
) -> crate::Result<WriterResult> {
    let source_root = pdf.root_ref();
    let direct_root = if source_root.is_none() {
        Some(pdf.root_handle()?)
    } else {
        None
    };
    // qpdf's removeObject erases the only document cache slot and turns
    // retained aliases into direct null; it does not leave a tombstone for a
    // later writer pass. Keep the writer's operation-local set empty here.
    let object_stream_plan =
        plan::build_live_object_stream_plan(pdf, options, source_object_stream_data)?;
    let removed_refs = object_stream_plan.removed_refs;
    let object_streams = object_stream_plan.groups;
    // qpdf gates both the 1.5 version floor and the cross-reference form on the
    // same setup-time map, `object_stream_to_objects`
    // (`QPDFWriter.cc:2172-2173` and `:3023-3031`), which
    // `preserveObjectStreams` fills before the write pass
    // (`:1955-1966`) and which live reachability never revisits. Derive both
    // from one quantity here too: Preserve's source-container groups after the
    // removed-ref filter. Splitting them -- floor from this set, form from the
    // post-walk layout -- lets a container that is registered but never
    // reached declare 1.5 while emitting a classic table, which qpdf never
    // does.
    let has_object_stream_hint = !object_streams.is_empty();
    let source_id0 = plan::live_source_id0(pdf)?;
    let source_version = pdf.version().to_string();
    let source_extension_level = pdf.adobe_extension_level()?.unwrap_or(0);
    let (effective_version, final_extension_level) =
        crate::writer::effective_pdf_version_and_ext_with_encryption(
            &source_version,
            source_extension_level,
            options,
            false,
            has_object_stream_hint,
            encryption_parameters.as_ref(),
        );
    let version = effective_version.to_string();
    crate::writer::configure_progress_for_pdf(pdf, options, 0, false)?;
    let deterministic_id = crate::writer::uses_deterministic_id(options);
    if deterministic_id {
        // QPDFWriter::writeStandard installs Pl_MD5 before writeHeader and
        // leaves it active until the inline /ID writer reaches its opening
        // bracket. The xref payload is written only after that cutoff.
        out.begin_digest();
    }
    // Object/string/stream encryption needs the file-key state while the live
    // queue is still assigning numbers. The /Encrypt object itself is emitted
    // only after that queue, so zero is a non-body sentinel for this temporary
    // context; the final context below receives qpdf's actual next object ID.
    let body_encryption_context = encryption_parameters
        .clone()
        .map(|parameters| parameters.into_context(ObjectRef::new(0, 0)));
    let empty_content_containers = std::collections::BTreeSet::new();
    let content_container_refs = special_streams
        .map(|streams| &streams.content_container_refs)
        .unwrap_or(&empty_content_containers);
    let mut body = body::emit_live(
        pdf,
        out,
        options,
        &version,
        final_extension_level,
        source_root,
        removed_refs.clone(),
        &object_streams,
        body_encryption_context.as_ref(),
        content_container_refs,
    )?; // cov:ignore: LLVM attributes the live-body call terminator to closure cleanup.
    let old_to_new = body.old_to_new;
    let root = source_root.and_then(|source| old_to_new.get(&source).copied());
    if source_root.is_some() && root.is_none() {
        // cov:ignore-start: root is seeded before emission; this guards only a violated queue invariant.
        return Err(crate::Error::Unsupported(
            "plain live writer: /Root absent from queue".to_string(),
        ));
        // cov:ignore-end
    }
    let direct_root_output = direct_root
        .as_ref()
        .map(|root| root.output_root_copy_with_adbe(&version, final_extension_level, false))
        .transpose()?;
    // `old_to_new` numbers every live object (uncompressed and compressed)
    // through one counter (`LiveQueue::enqueue_handle`), so it is always a
    // bijection onto `1..=old_to_new.len()`; a compressed member's output
    // number can exceed every uncompressed number, so `/Size` cannot be
    // derived from the uncompressed map alone once Preserve has live
    // compressed content.
    let mut max_output = u32::try_from(body.object_count).unwrap_or(u32::MAX);
    let encryption_context = if let Some(parameters) = encryption_parameters {
        let encrypt_number = max_output.checked_add(1).ok_or_else(|| {
            crate::Error::Unsupported("plain live writer /Encrypt number overflows u32".into())
        })?;
        let context = parameters.into_context(ObjectRef::new(encrypt_number, 0));
        let offset = usize::try_from(out.position()).map_err(|_| {
            crate::Error::Unsupported("plain live writer output position exceeds usize".into())
        })?;
        out.write_bytes(format!("{encrypt_number} 0 obj\n").as_bytes())?;
        crate::writer::encrypted_strings::write_encryption_dictionary_handle(
            out,
            &context.encrypt_dict_handle(),
        )?;
        out.write_bytes(b"\nendobj\n")?;
        if options.qdf {
            out.write_bytes(b"\n")?;
        }
        body.layout.uncompressed.insert(encrypt_number, (0, offset));
        max_output = encrypt_number;
        Some(context)
    } else {
        None
    };
    let trailer_size = usize::try_from(max_output)
        .ok()
        .and_then(|size| size.checked_add(1))
        // cov:ignore-start: the in-memory body map cannot exceed the platform usize domain.
        .ok_or_else(|| {
            crate::Error::Unsupported("plain live writer /Size overflows usize".into())
        })?;
    // cov:ignore-end
    let generated_id = if deterministic_id {
        None
    } else {
        Some(generated_id.cloned().unwrap_or_else(|| {
            crate::writer::generate_id_handle(source_id0.as_deref(), options.static_id)
        }))
    };
    let trailer_handle = crate::writer::build_writer_trailer_handle(
        pdf,
        trailer_size,
        root,
        direct_root_output.as_ref(),
        options,
        encryption_context.as_ref(),
        deterministic_id,
        generated_id.as_ref(),
    )?; // cov:ignore: LLVM attributes trailer construction's call terminator to callback cleanup.
    let id = if deterministic_id {
        IdPlan::Deterministic {
            source_id0,
            info_suffix: crate::writer::deterministic_id_info_suffix(pdf),
        }
    } else {
        IdPlan::Materialized {
            value: xref::materialized_id_handle(&trailer_handle.try_get_key(b"/ID")?)?,
        }
    };
    let mut trailer_map: HashMap<ObjectRef, ObjectRef> =
        old_to_new.iter().map(|(&a, &b)| (a, b)).collect();

    // Object streams require a cross-reference stream: a classic table has no
    // type-2 row shape (ISO 32000-1 7.5.7). qpdf decides this from the same
    // setup-time membership that set the version floor above
    // (`QPDFWriter.cc:3023-3031`), not from what the walk turned out to
    // reach, so a registered-but-unreached container still produces a
    // cross-reference stream with zero type-2 rows.
    let form = if has_object_stream_hint || (!options.qdf && pdf.last_xref_form == XrefForm::Stream)
    {
        XrefForm::Stream
    } else {
        XrefForm::Table
    };
    // Mirrors `plan.rs`'s `structural_filtered` derivation: the xref stream's
    // own `/Filter /FlateDecode` + PNG `/Predictor 12` framing follows the
    // same effective stream-compression policy as every other stream, not a
    // hardcoded choice (`QPDFWriter.cc` writes the xref stream through the
    // same `Pl_Flate` pipeline as any other filtered stream).
    let structural_filtered = matches!(
        crate::writer::effective_stream_policy(options),
        Some(CompressStreams::Yes)
    ) && !options.qdf;
    let trailer = TrailerPlan {
        form,
        root,
        direct_root: direct_root_output,
        id,
        encrypt: trailer_handle.try_get_key(b"/Encrypt")?.object_ref(),
        structural_filtered,
        qdf: options.qdf,
    };
    let written_xref = xref::append_xref_and_trailer(
        out,
        &body.layout,
        &trailer,
        &trailer_handle,
        &trailer_map,
        &removed_refs,
    )?; // cov:ignore: LLVM attributes xref append's call terminator to callback cleanup.
        // qpdf's `getRenumberedObjGen` returns `obj_renumber[og]` unfiltered
        // (`QPDFWriter.cc:2215-2219`), and `assignCompressedObjectNumbers`
        // (`:1057-1069`) writes an entry for every member of a container, so a
        // type-2 member has a renumbered identity just like an uncompressed
        // object. Keep both, matching the planned route below.
    let old_to_new = trailer_map
        .into_iter()
        .filter(|(_, output)| {
            body.layout.uncompressed.contains_key(&output.number)
                || body.layout.compressed.contains_key(&output.number)
        })
        .collect();
    Ok(WriterResult::new(old_to_new, written_xref))
}

pub(crate) fn eligible(
    pdf_is_encrypted: bool,
    options: &WriterOptions,
    mode: ObjectStreamMode,
) -> bool {
    mode == options.object_streams
        && !options.pclm
        && options.extra_header_text.is_empty()
        && options.encrypt.is_none()
        && options.copy_encryption.is_none()
        && !pdf_is_encrypted
}
