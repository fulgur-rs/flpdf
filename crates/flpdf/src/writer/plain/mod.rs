//! qpdf correspondence: QPDFWriter.cc standard write pipeline split across plain writer modules.
use std::io::{Read, Seek, Write};

use crate::writer::WriterOptions;
use crate::writer::WriterResult;
use crate::{ObjectRef, ObjectStreamMode, Pdf};
use std::collections::BTreeMap;

pub(crate) mod body;
pub(crate) mod plan;
pub(crate) mod xref;

pub(crate) fn write_plain<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriterOptions,
) -> crate::Result<WriterResult> {
    let plan = plan::PlainWritePlan::build(pdf, options)?;
    crate::writer::configure_progress_for_pdf(
        pdf,
        options,
        plan.generated_object_stream_count(),
        false,
    )?; // cov:ignore: a pre-emission object-enumeration failure is surfaced by the underlying writer validation
    write_planned(pdf, out, options, &plan)
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
    mode == options.object_streams
        && !options.qdf
        && !options.pclm
        && options.extra_header_text.is_empty()
        && options.encrypt.is_none()
        && options.copy_encryption.is_none()
        && !options.content_normalization
        && !pdf_is_encrypted
}
