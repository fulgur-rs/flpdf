//! qpdf correspondence: QPDFWriter.cc standard write pipeline split across plain writer modules.
use std::io::{Read, Seek, Write};

use crate::writer::plain::xref::{IdPlan, TrailerPlan};
use crate::writer::ObjectWriterEmission;
use crate::writer::WriterOptions;
use crate::writer::WriterResult;
use crate::{CompressStreams, ObjectHandle, ObjectRef, ObjectStreamMode, Pdf, XrefForm};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) mod body;
pub(crate) mod plan;
pub(crate) mod xref;

pub(crate) fn write_plain<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
    special_streams: Option<&crate::writer::SpecialStreams>,
    source_object_stream_data: &BTreeMap<u32, u32>,
) -> crate::Result<WriterResult> {
    // The live queue preserves the mutation/progress timing contract for both
    // ordinary output and the QDF/normalization variants whose object-stream
    // policy has no source containers to reconstruct. The latter uses the same
    // queue with QDF length-holder and page-content formatting state.
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
    if qdf_or_normalize_live_eligible(options, source_object_stream_data) {
        let (page_sequences, contents_sequences, content_container_sequences) =
            live_page_context(pdf, special_streams)?;
        return write_plain_live(
            pdf,
            out,
            options,
            generated_id,
            source_object_stream_data,
            page_sequences,
            contents_sequences,
            content_container_sequences,
        );
    }
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

/// Return whether the QDF/normalization route can use the live queue without
/// having to rebuild source or generated ObjStm containers. A source Preserve
/// map makes the container-membership boundary observable, and Generate has
/// fresh packing decisions; both remain on the plan-based consumer.
pub(crate) fn qdf_or_normalize_live_eligible(
    options: &WriterOptions,
    source_object_stream_data: &BTreeMap<u32, u32>,
) -> bool {
    (options.qdf || options.content_normalization)
        && matches!(
            options.object_streams,
            ObjectStreamMode::Disable | ObjectStreamMode::Preserve
        )
        && (options.object_streams == ObjectStreamMode::Disable
            || source_object_stream_data.is_empty())
}

#[allow(clippy::type_complexity)] // the three maps are distinct qpdf setup dimensions and are kept separate at this boundary
fn live_page_context<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    special_streams: Option<&crate::writer::SpecialStreams>,
) -> crate::Result<(
    BTreeMap<ObjectRef, usize>,
    BTreeMap<ObjectRef, usize>,
    BTreeMap<ObjectRef, usize>,
)> {
    if let Some(special_streams) = special_streams {
        let page_sequences = special_streams
            .page_seq
            .iter()
            .map(|(&object, &sequence)| (object, sequence as usize))
            .collect();
        let contents_sequences = special_streams
            .contents_seq
            .iter()
            .map(|(&object, &sequence)| (object, sequence as usize))
            .collect();
        let content_container_sequences = special_streams
            .content_container_seq
            .iter()
            .map(|(&object, &sequence)| (object, sequence as usize))
            .collect();
        Ok((
            page_sequences,
            contents_sequences,
            content_container_sequences,
        ))
    } else {
        // cov:ignore-start: PdfWriter always supplies initialize_special_streams state for this route
        let (page_sequences, contents_sequences) = body::qdf_page_context(pdf)?;
        Ok((page_sequences, contents_sequences, BTreeMap::new()))
        // cov:ignore-end
    }
}

/// Assign qpdf's late trailer-reference numbers after the body queue has
/// drained. Trailer values are emitted after `/Root` in qpdf's sorted-key
/// walk, so callers run this once on each side of the root/direct-root
/// emission boundary (`QPDFWriter.cc:1144-1157,1160-1236`).
pub(crate) fn extend_late_trailer_map<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    map: &mut HashMap<ObjectRef, ObjectRef>,
    mut next: u32,
    before_root: bool,
    qdf: bool,
) -> crate::Result<u32> {
    let entries = pdf.trailer().try_as_dictionary()?.unwrap_or_default();
    for (key, value) in entries {
        if key.as_slice() == b"/Root" || (key.as_slice() < b"/Root") != before_root {
            continue;
        }
        if matches!(
            key.as_slice(),
            b"/ID"
                | b"/Encrypt"
                | b"/Prev"
                | b"/Root"
                | b"/Size"
                | b"/Type"
                | b"/F"
                | b"/FFilter"
                | b"/FDecodeParms"
                | b"/W"
                | b"/Index"
                | b"/Length"
                | b"/Filter"
                | b"/DecodeParms"
                | b"/XRefStm"
        ) {
            continue;
        }
        if value.try_is_null()? {
            continue;
        }
        let mut references = Vec::new();
        crate::writer::rewrite_renumber::collect_canonical_enqueue_refs(
            pdf,
            &value,
            0,
            true,
            &mut references,
        )?; // cov:ignore: late trailer reference collection success is covered by the callback trailer test
        next = assign_late_references(pdf, map, references, next, qdf)?;
    }
    Ok(next)
}

fn assign_late_references<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    map: &mut HashMap<ObjectRef, ObjectRef>,
    references: Vec<ObjectRef>,
    mut next: u32,
    qdf: bool,
) -> crate::Result<u32> {
    for reference in references {
        if reference.number == 0 || map.contains_key(&reference) {
            continue;
        }
        let handle = pdf.get_object_handle(reference);
        if qdf && handle.try_is_stream_of_type(b"XRef", b"")? {
            map.insert(reference, ObjectRef::new(0, 0));
            continue;
        }
        let is_stream = qdf && handle.try_is_stream_of_type(b"", b"")?;
        map.insert(reference, ObjectRef::new(next, 0));
        let increment = if is_stream { 2 } else { 1 };
        next = next.checked_add(increment).ok_or_else(|| {
            // cov:ignore-start: the qpdf object-number domain cannot be exhausted by a supported in-memory PDF
            crate::Error::Unsupported("plain live writer: late trailer number overflow".into())
            // cov:ignore-end
        })?; // cov:ignore: checked late-trailer allocation cannot overflow a supported output
    }
    Ok(next)
}

fn write_plain_live_disable<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
    source_object_stream_data: &BTreeMap<u32, u32>,
) -> crate::Result<WriterResult> {
    write_plain_live(
        pdf,
        out,
        options,
        generated_id,
        source_object_stream_data,
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    )
}

#[allow(clippy::too_many_arguments)] // qpdf writer state and setup-derived page/content maps are explicit consumer inputs
fn write_plain_live<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
    generated_id: Option<&crate::ObjectHandle>,
    source_object_stream_data: &BTreeMap<u32, u32>,
    page_sequences: BTreeMap<ObjectRef, usize>,
    contents_sequences: BTreeMap<ObjectRef, usize>,
    content_container_sequences: BTreeMap<ObjectRef, usize>,
) -> crate::Result<WriterResult> {
    let source_root = pdf.root_ref();
    let direct_root = if source_root.is_none() {
        Some(pdf.root_handle()?)
    } else {
        None
    };
    let initial_id_present = pdf
        .trailer()
        .try_as_dictionary()?
        .is_some_and(|entries| entries.iter().any(|(key, _)| key.as_slice() == b"/ID"));
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
    let body = if options.qdf || options.content_normalization {
        body::emit_live_qdf_or_normalize(
            pdf,
            options,
            &version,
            final_extension_level,
            source_root,
            removed_refs.clone(),
            page_sequences,
            contents_sequences,
            content_container_sequences,
        )?
    } else {
        body::emit_live_disable(
            pdf,
            options,
            &version,
            final_extension_level,
            source_root,
            removed_refs.clone(),
            &object_streams,
        )?
    }; // cov:ignore: the selected live-body call terminator is covered by the corresponding route tests.
    let mut old_to_new = body.old_to_new;
    for ignored in body.ignored_refs {
        old_to_new.insert(ignored, ObjectRef::new(0, 0));
    }
    // qpdf generates the changing ID from the live trailer at writeTrailer
    // time. Reuse the setup-provided array only while its permanent ID still
    // matches the live trailer; a progress callback that replaces `/ID` must
    // be observed by this late construction boundary.
    let source_id0 = plan::live_source_id0(pdf)?;
    let live_id = pdf.trailer_key_handle(b"ID");
    let setup_id0 = generated_id
        .as_ref()
        .and_then(|id| id.as_array())
        .and_then(|values| values.first().and_then(ObjectHandle::as_string));
    let reuse_setup_id = generated_id.is_some()
        && if live_id.try_is_null()? {
            !initial_id_present
        } else {
            source_id0.is_some() && setup_id0.as_deref() == source_id0.as_deref()
        };
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
    } else if reuse_setup_id {
        generated_id.cloned()
    } else {
        Some(crate::writer::generate_id_handle(
            source_id0.as_deref(),
            options.static_id,
        ))
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
    let mut trailer_map: HashMap<ObjectRef, ObjectRef> =
        old_to_new.iter().map(|(&a, &b)| (a, b)).collect();
    let initial_late_trailer_number = u32::try_from(trailer_size).map_err(|_| {
        // cov:ignore-start: the body queue is bounded by the qpdf u32 object-number domain
        crate::Error::Unsupported("plain live writer: late trailer number overflows u32".into())
        // cov:ignore-end
    })?; // cov:ignore: checked body-derived late-trailer allocation cannot overflow a supported output
    let mut next_late_trailer_number = extend_late_trailer_map(
        pdf,
        &mut trailer_map,
        initial_late_trailer_number,
        true,
        options.qdf,
    )?; // cov:ignore: shared late-trailer success continuation is covered by the QDF/normalize live tests

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
    let direct_root_bytes = if let Some(arbitrated) = direct_root_output.as_ref() {
        let mut references = Vec::new();
        crate::writer::rewrite_renumber::collect_canonical_enqueue_refs(
            pdf,
            arbitrated,
            0,
            true,
            &mut references,
        )?; // cov:ignore: direct-root reference collection success is covered by the late direct-root test
        next_late_trailer_number = assign_late_references(
            pdf,
            &mut trailer_map,
            references,
            next_late_trailer_number,
            options.qdf,
        )?; // cov:ignore: direct-root late assignment success is covered by the QDF/normalize direct-root test
        let map_ref = |object_ref: ObjectRef| {
            trailer_map.get(&object_ref).copied().ok_or_else(|| {
                // cov:ignore-start: every direct-root reference is collected before this static map is constructed
                crate::Error::Unsupported(format!(
                    "plain live writer: direct /Root reference {object_ref} has no output number"
                ))
                // cov:ignore-end
            }) // cov:ignore: direct-root references were collected into trailer_map before serialization
        };
        let mut bytes = Vec::new();
        if options.qdf {
            arbitrated.write_object_qdf_with_ref_map_and_removed(
                &mut bytes,
                2,
                &map_ref,
                &removed_refs,
            )?; // cov:ignore: QDF direct-root serialization success is covered by the direct-root live test
        } else {
            arbitrated.write_object_with_ref_map_and_removed(
                &mut bytes,
                &map_ref,
                &removed_refs,
            )?; // cov:ignore: compact direct-root serialization success is covered by the direct-root live test
        }
        Some(bytes)
    } else {
        None
    };
    extend_late_trailer_map(
        pdf,
        &mut trailer_map,
        next_late_trailer_number,
        false,
        options.qdf,
    )?; // cov:ignore: shared late-trailer success continuation is covered by the QDF/normalize live tests
    let trailer = TrailerPlan {
        form,
        canonical_entries: plan::canonical_trailer_entries(pdf, &trailer_map, &removed_refs)?,
        root,
        direct_root: direct_root_bytes,
        id,
        encrypt: trailer_handle.try_get_key(b"/Encrypt")?.object_ref(),
        structural_filtered,
        qdf: options.qdf,
    };
    let mut bytes = body.bytes;
    let written_xref = xref::append_xref_and_trailer_with_handle(
        &mut bytes,
        &body.layout,
        &trailer,
        &trailer_handle,
        &trailer_map,
        &removed_refs,
    )?; // cov:ignore: LLVM attributes xref append's call terminator to callback cleanup.
    out.write_all(&bytes)?;
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
