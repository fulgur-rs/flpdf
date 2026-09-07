//! qpdf correspondence: QPDFWriter.cc standard write pipeline split across plain writer modules.
use std::io::{Read, Seek, Write};

use crate::writer::plain::xref::{IdPlan, TrailerPlan};
use crate::writer::ObjectWriterEmission;
use crate::writer::WriterOptions;
use crate::writer::WriterResult;
use crate::{ObjectRef, ObjectStreamMode, Pdf, XrefForm};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) mod body;
pub(crate) mod plan;
pub(crate) mod xref;

pub(crate) fn write_plain<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
) -> crate::Result<WriterResult> {
    // The live Disable queue preserves the mutation/progress timing contract
    // for the ordinary unnormalized route. QDF and page-content normalization
    // need the planned writer's second dimension (QDF framing or normalized
    // stream buffers), while the selected object-stream mode remains Disable.
    if options.object_streams == ObjectStreamMode::Disable
        && !options.qdf
        && !options.content_normalization
    {
        return write_plain_live_disable(pdf, out, options, generated_id);
    }
    let plan = plan::PlainWritePlan::build_with_generated_id(pdf, options, generated_id)?;
    crate::writer::configure_progress_for_pdf(pdf, options, 0, false)?; // cov:ignore: a pre-emission object-enumeration failure is surfaced by the underlying writer validation
    write_planned(pdf, out, options, &plan)
}

fn write_plain_live_disable<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
) -> crate::Result<WriterResult> {
    let source_root = pdf.root_ref();
    let direct_root = if source_root.is_none() {
        Some(pdf.root_handle()?)
    } else {
        None
    };
    let removed_refs: BTreeSet<ObjectRef> = pdf.deleted_object_refs().into_iter().collect();
    let source_id0 = plan::live_source_id0(pdf)?;
    let source_version = pdf.version().to_string();
    let source_extension_level = pdf.adobe_extension_level()?.unwrap_or(0);
    let (effective_version, final_extension_level) = crate::writer::effective_pdf_version_and_ext(
        &source_version,
        source_extension_level,
        options,
        false,
        false,
    );
    let version = effective_version.to_string();
    crate::writer::configure_progress_for_pdf(pdf, options, 0, false)?;
    let body = body::emit_live_disable(
        pdf,
        options,
        &version,
        final_extension_level,
        source_root,
        removed_refs.clone(),
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
    let max_output = body.layout.uncompressed.keys().copied().max().unwrap_or(0);
    let trailer_size = usize::try_from(max_output)
        .ok()
        .and_then(|size| size.checked_add(1))
        // cov:ignore-start: the in-memory body map cannot exceed the platform usize domain.
        .ok_or_else(|| {
            crate::Error::Unsupported("plain live writer /Size overflows usize".into())
        })?;
    // cov:ignore-end
    let deterministic_id = crate::writer::uses_deterministic_id(options);
    let generated_id = if deterministic_id {
        None
    } else {
        Some(generated_id.cloned().unwrap_or_else(|| {
            // cov:ignore-start: PdfWriter prepares this identifier before the plain route.
            crate::writer::generate_id_handle(source_id0.as_deref(), options.static_id)
            // cov:ignore-end
        })) // cov:ignore: LLVM attributes the prepared-id fallback terminator to closure cleanup.
    };
    let trailer_handle = crate::writer::build_writer_trailer_handle(
        pdf,
        trailer_size,
        root,
        direct_root_output.as_ref(),
        options,
        None,
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
    let map: HashMap<ObjectRef, ObjectRef> = old_to_new.iter().map(|(&a, &b)| (a, b)).collect();
    let trailer = TrailerPlan {
        form: XrefForm::Table,
        canonical_entries: plan::canonical_trailer_entries(pdf, &map, &removed_refs)?,
        root,
        direct_root: None,
        id,
        encrypt: trailer_handle.try_get_key(b"/Encrypt")?.object_ref(),
        structural_filtered: false,
        qdf: false,
    };
    let mut bytes = body.bytes;
    let written_xref = xref::append_xref_and_trailer_with_handle(
        &mut bytes,
        &body.layout,
        &trailer,
        &trailer_handle,
        &map,
        &removed_refs,
    )?; // cov:ignore: LLVM attributes xref append's call terminator to callback cleanup.
    out.write_all(&bytes)?;
    let old_to_new = map
        .into_iter()
        .filter(|(_, output)| body.layout.uncompressed.contains_key(&output.number))
        .collect();
    Ok(WriterResult::new(old_to_new, written_xref))
}

fn write_planned<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
    plan: &plan::PlainWritePlan,
) -> crate::Result<WriterResult> {
    plan.validate()?;
    let (mut bytes, layout) = body::emit_bodies(pdf, options, plan)?;
    let written_xref = xref::append_xref_and_trailer_with_handle(
        &mut bytes,
        &layout,
        &plan.trailer,
        &plan.trailer_handle,
        &plan.old_to_new,
        &plan.removed_refs,
    )?; // cov:ignore: validated plain body/trailer consumer; LLVM maps this multiline call continuation to a zero-count terminator
    out.write_all(&bytes)?;
    let old_to_new = plan
        .old_to_new
        .iter()
        .filter(|(_, output)| {
            layout.uncompressed.contains_key(&output.number)
                || layout.compressed.contains_key(&output.number)
        })
        .map(|(&source, &output)| (source, ObjectRef::new(output.number, 0)))
        .collect::<BTreeMap<ObjectRef, ObjectRef>>();
    Ok(WriterResult::new(old_to_new, written_xref))
}

pub(crate) fn eligible(
    pdf_is_encrypted: bool,
    options: &WriterOptions,
    mode: ObjectStreamMode,
) -> bool {
    // QDF and content normalization alter stream serialization, but qpdf
    // dispatches the selected object-stream mode independently
    // (`QPDFWriter.cc:2038-2140`). The planned writer owns both dimensions.
    mode == options.object_streams
        && !options.pclm
        && options.extra_header_text.is_empty()
        && options.encrypt.is_none()
        && options.copy_encryption.is_none()
        && !pdf_is_encrypted
}
