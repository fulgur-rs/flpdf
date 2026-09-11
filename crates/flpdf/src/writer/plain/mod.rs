//! qpdf correspondence: QPDFWriter.cc standard write pipeline split across plain writer modules.
use std::io::{Read, Seek, Write};

use crate::writer::plain::xref::{IdPlan, TrailerPlan};
use crate::writer::ObjectWriterEmission;
use crate::writer::WriterOptions;
use crate::writer::WriterResult;
use crate::{CompressStreams, ObjectRef, ObjectStreamMode, Pdf, XrefForm};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) mod body;
pub(crate) mod plan;
pub(crate) mod xref;

pub(crate) fn write_plain<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
    source_object_stream_data: &BTreeMap<u32, u32>,
) -> crate::Result<WriterResult> {
    // The live queue preserves the mutation/progress timing contract for the
    // ordinary unnormalized route. QDF and page-content normalization need the
    // planned writer's second dimension (QDF framing or normalized stream
    // buffers), so both keep going through the plan-based path regardless of
    // object-stream mode.
    //
    // qpdf's Preserve mode keeps whatever object streams the source already
    // has. When there are none, `preserveObjectStreams` returns before it
    // builds any mapping (`QPDFWriter.cc:1941-1945`), so
    // `object_to_object_stream` stays empty and the walk is exactly Disable's.
    // When there are some, `:1955-1966` fills that map and `enqueueObject`
    // (`:1072-1141`) redirects a member's discovery to its container, numbering
    // every member the instant the container is first queued
    // (`assignCompressedObjectNumbers`, `:1057-1069`). Both are the same
    // `enqueueObject`/`writeStandard` live walk, just with container-aware
    // numbering in the second case, so Preserve routes through the live queue
    // either way. Generate does not: it packs fresh containers the live walk
    // cannot discover incrementally, so it keeps the plan-based path.
    let is_live_disable_shaped = matches!(
        options.object_streams,
        ObjectStreamMode::Disable | ObjectStreamMode::Preserve
    );
    if is_live_disable_shaped && !options.qdf && !options.content_normalization {
        return write_plain_live_disable(
            pdf,
            out,
            options,
            generated_id,
            source_object_stream_data,
        );
    }
    let plan = plan::PlainWritePlan::build_with_generated_id_and_source_object_stream_data(
        pdf,
        options,
        generated_id,
        Some(source_object_stream_data),
    )?;
    crate::writer::configure_progress_for_pdf(pdf, options, 0, false)?; // cov:ignore: a pre-emission object-enumeration failure is surfaced by the underlying writer validation
    write_planned(pdf, out, options, &plan)
}

fn write_plain_live_disable<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
    source_object_stream_data: &BTreeMap<u32, u32>,
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
    let mut removed_refs: BTreeSet<ObjectRef> = BTreeSet::new();
    let object_streams = if options.object_streams == ObjectStreamMode::Preserve {
        let packing =
            crate::writer::object_streams::plan_qpdf_preserve_object_streams_with_source_membership(
                pdf,
                options.preserve_unreferenced_objects,
                Some(source_object_stream_data),
            )?; // cov:ignore: malformed source graph is rejected by the preserve planner
        removed_refs.extend(packing.removed_refs);
        packing.groups
    } else {
        Vec::new()
    };
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
    let (effective_version, final_extension_level) = crate::writer::effective_pdf_version_and_ext(
        &source_version,
        source_extension_level,
        options,
        false,
        has_object_stream_hint,
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
        &object_streams,
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
    let max_output = u32::try_from(body.object_count).unwrap_or(u32::MAX);
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
    // Object streams require a cross-reference stream: a classic table has no
    // type-2 row shape (ISO 32000-1 7.5.7). qpdf decides this from the same
    // setup-time membership that set the version floor above
    // (`QPDFWriter.cc:3023-3031`), not from what the walk turned out to
    // reach, so a registered-but-unreached container still produces a
    // cross-reference stream with zero type-2 rows.
    let form = if has_object_stream_hint {
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
    );
    // `canonical_trailer_entries` deliberately omits `/Root`, and the
    // cross-reference stream serializer reads it only from `root` or
    // `direct_root`. The live route can now select `XrefForm::Stream`, so a
    // direct Catalog has to be serialized here too or the output loses its
    // `/Root` entirely. The classic-table form gets it from the trailer handle
    // above, which is why this stayed `None` while the route was table-only.
    let direct_root_bytes = direct_root_output
        .as_ref()
        .map(|arbitrated| {
            let map_ref = |object_ref: ObjectRef| {
                map.get(&object_ref).copied().ok_or_else(|| {
                    // cov:ignore-start: the direct Catalog is collected by the
                    // same walk that fills this map, so a live reference cannot
                    // be absent at emission.
                    crate::Error::Unsupported(format!(
                        "plain live writer: direct /Root reference {} {} R absent from renumber map",
                        object_ref.number, object_ref.generation
                    ))
                    // cov:ignore-end
                }) // cov:ignore: the direct-root reference map is exercised; LLVM places the successful closure-exit counter on this continuation line.
            };
            let mut bytes = Vec::new();
            arbitrated
                .write_object_with_ref_map_and_removed(&mut bytes, &map_ref, &removed_refs)
                .map(|()| bytes)
        })
        .transpose()?;
    let trailer = TrailerPlan {
        form,
        canonical_entries: plan::canonical_trailer_entries(pdf, &map, &removed_refs)?,
        root,
        direct_root: direct_root_bytes,
        id,
        encrypt: trailer_handle.try_get_key(b"/Encrypt")?.object_ref(),
        structural_filtered,
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
    // qpdf's `getRenumberedObjGen` returns `obj_renumber[og]` unfiltered
    // (`QPDFWriter.cc:2215-2219`), and `assignCompressedObjectNumbers`
    // (`:1057-1069`) writes an entry for every member of a container, so a
    // type-2 member has a renumbered identity just like an uncompressed
    // object. Keep both, matching the planned route below.
    let old_to_new = map
        .into_iter()
        .filter(|(_, output)| {
            body.layout.uncompressed.contains_key(&output.number)
                || body.layout.compressed.contains_key(&output.number)
        })
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
