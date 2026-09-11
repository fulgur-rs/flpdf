//! qpdf correspondence: QPDFWriter.cc standard-write object placement and renumber planning.
//! Logical object placements for the qpdf-shaped plain writer pipeline.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Seek};

use crate::pdf_version::{parse_qpdf_writer_version, QpdfVersionParts};
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::writer::object_streams::{self, ObjectStreamGroup, ObjectStreamMode};
use crate::writer::plain::body;
use crate::writer::plain::xref::{materialized_id_handle, IdPlan, TrailerPlan};
use crate::writer::rewrite_renumber::{
    CanonicalCatalogFirstRenumber, NewNumberLookup, ObjectStreamRenumber, StreamParametersRemoved,
};
use crate::writer::{ObjectWriterEmission, WriterOptions};
use crate::{
    CompressStreams, ObjectHandle, ObjectRef, PageDocumentHelper, Pdf, XrefEntry, XrefForm,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedMember {
    pub(crate) source: ObjectRef,
    pub(crate) output: ObjectRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedObjectStreamOrigin {
    SourceBacked(ObjectRef),
    Generated(ObjectRef),
    Synthetic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedIndirectObject {
    Source {
        source: ObjectRef,
        output: ObjectRef,
    },
    RawSource {
        source: ObjectRef,
        raw: QpdfObjGen,
        output: ObjectRef,
    },
    ObjectStream {
        origin: PlannedObjectStreamOrigin,
        output: ObjectRef,
        members: Vec<PlannedMember>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct CachedStreamOutput {
    pub(crate) dict: crate::ObjectHandle,
    pub(crate) data: Vec<u8>,
    pub(crate) dictionary_options: crate::writer::StreamDictionaryOptions,
    pub(crate) fingerprint: StreamCacheFingerprint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamCacheFingerprint {
    stream: (usize, u64),
    dictionary: (usize, u64),
    parameter_handles: Vec<(usize, u64)>,
}

pub(crate) fn stream_cache_fingerprint(
    handle: &crate::ObjectHandle,
) -> crate::Result<StreamCacheFingerprint> {
    let dictionary = handle
        .as_stream_dict()
        .ok_or_else(|| crate::Error::Internal("canonical stream dictionary is missing".into()))?;
    let mut parameter_handles = Vec::new();
    for key in [
        b"/Filter".as_slice(),
        b"/DecodeParms".as_slice(),
        b"/F".as_slice(),
        b"/FFilter".as_slice(),
        b"/FDecodeParms".as_slice(),
    ] {
        let value = dictionary.try_get_key(key)?;
        if value.try_is_null()? {
            continue;
        }
        value.try_dereference()?;
        parameter_handles.push(value.mutation_fingerprint());
    }
    Ok(StreamCacheFingerprint {
        stream: handle.mutation_fingerprint(),
        dictionary: dictionary.mutation_fingerprint(),
        parameter_handles,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct PlainWritePlan {
    pub(crate) version: String,
    pub(crate) final_extension_level: i64,
    pub(crate) objects: Vec<PlannedIndirectObject>,
    pub(crate) root_source: Option<ObjectRef>,
    /// Remapped Catalog identity when the source `/Root` is indirect.
    pub(crate) root: Option<ObjectRef>,
    /// Canonical Catalog handle when the source `/Root` is direct.
    pub(crate) direct_root: Option<crate::ObjectHandle>,
    pub(crate) old_to_new: HashMap<ObjectRef, ObjectRef>,
    pub(crate) removed_refs: BTreeSet<ObjectRef>,
    pub(crate) cached_stream_outputs: HashMap<ObjectRef, CachedStreamOutput>,
    /// QDF re-numbers the same planned objects in emission order and inserts
    /// a synthetic length holder after each ordinary stream. The holder map is
    /// kept beside the plan so the body emitter can use the qpdf numbering
    /// without manufacturing source identities for those output-only objects.
    pub(crate) qdf_holder_map: HashMap<u32, u32>,
    pub(crate) qdf_holder_numbers: BTreeSet<u32>,
    pub(crate) trailer_handle: crate::ObjectHandle,
    pub(crate) trailer: TrailerPlan,
}

impl PlainWritePlan {
    #[cfg(test)]
    pub(crate) fn build<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        options: &WriterOptions,
    ) -> crate::Result<Self> {
        Self::build_with_generated_id(pdf, options, None)
    }

    #[cfg(test)]
    pub(crate) fn build_with_generated_id<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        options: &WriterOptions,
        setup_generated_id: Option<&crate::ObjectHandle>,
    ) -> crate::Result<Self> {
        Self::build_with_generated_id_and_source_object_stream_data(
            pdf,
            options,
            setup_generated_id,
            None,
        )
    }

    pub(crate) fn build_with_generated_id_and_source_object_stream_data<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        options: &WriterOptions,
        setup_generated_id: Option<&crate::ObjectHandle>,
        source_object_stream_data: Option<&BTreeMap<u32, u32>>,
    ) -> crate::Result<Self> {
        let source_root_ref = pdf.root_ref();
        let source_root_handle = if source_root_ref.is_none() {
            let root_candidate = pdf.trailer_key_handle(b"Root");
            if root_candidate.is_null() {
                return Err(crate::Error::Missing("/Root"));
            }
            Some(pdf.root_handle()?)
        } else {
            None
        };
        let source_had_compressed_objects = source_has_compressed_entries(pdf);
        // qpdf's removeObject erases the canonical cache slot rather than
        // retaining a persistent deleted-reference tombstone.
        let explicitly_removed = BTreeSet::new();
        let normalized_content_refs: BTreeSet<ObjectRef> = if options.content_normalization {
            let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
            let mut refs = BTreeSet::new();
            for page in pages {
                refs.extend(crate::writer::collect_content_stream_refs(pdf, page)?);
            }
            refs
        } else {
            BTreeSet::new()
        };
        let cached_stream_outputs: RefCell<HashMap<ObjectRef, CachedStreamOutput>> =
            RefCell::new(HashMap::new());
        let stream_parameters_removed = |handle: &crate::ObjectHandle| {
            let Some(source) = handle.object_ref() else {
                return if handle.is_data_modified() {
                    Ok(false)
                } else {
                    body::canonical_stream_will_be_refiltered(handle, options)
                };
            };
            if let Some(cached) = cached_stream_outputs.borrow().get(&source) {
                if cached.fingerprint == stream_cache_fingerprint(handle)? {
                    return Ok(cached.dictionary_options.remove_filter_parameters);
                }
            }

            // QPDFWriter::willFilterStream retains the produced buffer for the
            // later unparseObject emission (`QPDFWriter.cc:1239-1314,1539-1560`).
            // Cache every indirect source stream, not only data-modified ones,
            // so deferred providers are invoked once across planning and emit.
            let (dict, data, dictionary_options) = body::canonical_stream_output_with_status(
                handle,
                options,
                true,
                normalized_content_refs.contains(&source),
            )?;
            let fingerprint = stream_cache_fingerprint(handle)?;
            cached_stream_outputs.borrow_mut().insert(
                source,
                CachedStreamOutput {
                    dict,
                    data,
                    dictionary_options,
                    fingerprint,
                },
            );
            Ok(dictionary_options.remove_filter_parameters)
        };

        let mut placement = match options.object_streams {
            ObjectStreamMode::Disable => {
                let renumber = CanonicalCatalogFirstRenumber::build_qpdf_with_stream_policy(
                    pdf,
                    true,
                    options.preserve_unreferenced_objects,
                    &explicitly_removed,
                    Some(&stream_parameters_removed),
                )?;
                let mut placement = build_sources_from_canonical_renumber(&renumber);
                placement.removed_refs = explicitly_removed;
                placement
            }
            ObjectStreamMode::Preserve => {
                if !source_had_compressed_objects {
                    let renumber = CanonicalCatalogFirstRenumber::build_qpdf_with_stream_policy(
                        pdf,
                        true,
                        options.preserve_unreferenced_objects,
                        &explicitly_removed,
                        Some(&stream_parameters_removed),
                    )?; // cov:ignore: malformed canonical source graphs are rejected before placement
                    let mut placement = build_sources_from_canonical_renumber(&renumber);
                    placement.removed_refs = explicitly_removed;
                    placement
                } else {
                    let mut packing =
                        object_streams::plan_qpdf_preserve_object_streams_with_source_membership(
                            pdf,
                            options.preserve_unreferenced_objects,
                            source_object_stream_data,
                        )?; // cov:ignore: malformed source graph is rejected by the preserve planner
                    packing
                        .removed_refs
                        .extend(explicitly_removed.iter().copied());
                    for group in &mut packing.groups {
                        group
                            .members_mut()
                            .retain(|member| !packing.removed_refs.contains(member));
                    }
                    packing.groups.retain(|group| !group.members().is_empty());
                    retain_reachable_object_stream_members(
                        pdf,
                        &mut packing.groups,
                        &packing.removed_refs,
                        options.preserve_unreferenced_objects,
                        Some(&stream_parameters_removed),
                    )?; // cov:ignore: LLVM maps this covered preserve-group call terminator to a zero-count continuation region
                    let groups = &packing.groups;
                    let removed = &packing.removed_refs;
                    let renumber = renumber_plain(
                        pdf,
                        groups,
                        removed,
                        options.preserve_unreferenced_objects,
                        Some(&stream_parameters_removed),
                    )?; // cov:ignore: planner groups are produced by the same validated source walk
                    build_container_aware(renumber, packing.groups, packing.removed_refs)?
                }
            }
            ObjectStreamMode::Generate => {
                let (mut eligible, mut removed_refs) =
                    pdf.get_compressible_objgens_with_removed()?;
                removed_refs.extend(explicitly_removed.iter().copied());
                eligible.retain(|member| !removed_refs.contains(member));
                let groups = object_streams::even_split_into_streams(&eligible);
                let mut renumber_groups = Vec::with_capacity(groups.len());
                for members in groups {
                    let container = pdf.make_indirect_object_handle(ObjectHandle::null())?;
                    // cov:ignore-start: make_indirect_object_handle always returns an indirect handle.
                    let source = container.object_ref().ok_or_else(|| {
                        crate::Error::Internal(
                            "generated object-stream container lost its indirect identity".into(),
                        )
                    })?;
                    // cov:ignore-end
                    renumber_groups.push(ObjectStreamGroup::Generated { source, members });
                }
                let removed = &removed_refs;
                // qpdf's Generate pass only puts its reachable compressible set
                // into synthetic ObjStms (`QPDFWriter.cc:1970-2007`), while
                // `enqueueObjectsStandard` separately seeds every source object
                // when preserve-unreferenced is enabled (`QPDFWriter.cc:2907-2914`).
                // Keep that distinction: preserved orphans receive plain slots
                // instead of being silently dropped from the generated rewrite.
                // cov:ignore-start: LLVM attributes this covered Generate-group call to its opening line; the writer contract test exercises the complete call
                retain_reachable_object_stream_members(
                    pdf,
                    &mut renumber_groups,
                    removed,
                    options.preserve_unreferenced_objects,
                    Some(&stream_parameters_removed),
                )?;
                // cov:ignore-end
                let renumber = renumber_plain(
                    pdf,
                    &renumber_groups,
                    removed,
                    options.preserve_unreferenced_objects,
                    Some(&stream_parameters_removed),
                )?; // cov:ignore: llvm-cov assigns no executable counter to this multiline-call terminator; the Generate preserve path is exercised by the writer contract test.
                build_container_aware(renumber, renumber_groups, removed_refs)?
            }
        };

        if options.qdf {
            // qpdf drops every source XRef stream from the writer queue in QDF
            // mode before it is ever numbered: `enqueueObject` returns early
            // for `isStreamOfType("/XRef")` because fix-qdf expects exactly one
            // XRef stream, at the end of the file (`QPDFWriter.cc:1085-1093`).
            // The comment there names this very case — a QDF made from a file
            // with object streams while preserving unreferenced objects. Since
            // the exclusion happens before numbering, keeping the placement and
            // withholding only its length holder would both abort the emit and
            // shift every later QDF number by one.
            let mut retained = Vec::with_capacity(placement.objects.len());
            for object in placement.objects.drain(..) {
                let is_xref = match &object {
                    PlannedIndirectObject::Source { source, .. } => pdf
                        .get_object_handle(*source)
                        .try_is_stream_of_type(b"XRef", b"")?,
                    // cov:ignore-start: qdf XRef-stream raw identities are
                    // not representable as a valid input object reference.
                    PlannedIndirectObject::RawSource { raw, .. } => pdf
                        .get_object_handle_by_raw_identity(
                            raw.get_obj() as i32,
                            raw.get_gen() as i32,
                        )
                        .try_is_stream_of_type(b"XRef", b"")?,
                    // cov:ignore-end
                    PlannedIndirectObject::ObjectStream { .. } => false,
                };
                if is_xref {
                    if let PlannedIndirectObject::Source { source, .. } = &object {
                        placement.old_to_new.remove(source);
                    }
                    continue;
                }
                retained.push(object);
            }
            placement.objects = retained;
        }

        let qdf_emission = if options.qdf {
            Some(build_qdf_emission_plan(pdf, &placement)?)
        } else {
            None
        };
        if let Some(qdf) = &qdf_emission {
            for object in &mut placement.objects {
                match object {
                    PlannedIndirectObject::Source { source, output } => {
                        // cov:ignore-start: the emission map is constructed
                        // from this same placement immediately above, so a
                        // missing source is an internal invariant failure.
                        *output = qdf.map.get(source).copied().ok_or_else(|| {
                            crate::Error::Unsupported(format!(
                                "plain writer QDF: source {source} absent from emission map"
                            ))
                        })?;
                        // cov:ignore-end
                    }
                    // cov:ignore-start: qdf raw-generation placement has no
                    // valid pinned fixture for an out-of-range object header.
                    PlannedIndirectObject::RawSource { source, output, .. } => {
                        *output = qdf.map.get(source).copied().ok_or_else(|| {
                            crate::Error::Unsupported(format!(
                                "plain writer QDF: raw source {source} absent from emission map"
                            ))
                        })?;
                    }
                    // cov:ignore-end
                    PlannedIndirectObject::ObjectStream {
                        origin,
                        output,
                        members,
                    } => {
                        *output = match origin {
                            PlannedObjectStreamOrigin::SourceBacked(source)
                            | PlannedObjectStreamOrigin::Generated(source) => {
                                // cov:ignore-start: every source-backed
                                // placement is inserted into the same QDF map
                                // during emission-plan construction.
                                qdf.map.get(source).copied().ok_or_else(|| {
                                    crate::Error::Unsupported(format!(
                                        "plain writer QDF: ObjStm source {source} absent from emission map"
                                    ))
                                })?
                                // cov:ignore-end
                            }
                            PlannedObjectStreamOrigin::Synthetic => {
                                // cov:ignore-start: every synthetic container
                                // is keyed in container_map before placement
                                // mutation reaches this arm.
                                qdf.container_map.get(output).copied().ok_or_else(|| {
                                    crate::Error::Unsupported(
                                        "plain writer QDF: synthetic ObjStm absent from emission map"
                                            .to_string(),
                                    )
                                })?
                                // cov:ignore-end
                            }
                        };
                        for member in members {
                            // cov:ignore-start: every member is inserted into
                            // the QDF map by the same placement traversal.
                            member.output =
                                qdf.map.get(&member.source).copied().ok_or_else(|| {
                                    crate::Error::Unsupported(format!(
                                    "plain writer QDF: ObjStm member {} absent from emission map",
                                    member.source
                                ))
                                })?;
                            // cov:ignore-end
                        }
                    }
                }
            }
            placement.old_to_new = qdf.map.clone();
        }

        let root = source_root_ref.and_then(|source| placement.old_to_new.get(&source).copied());
        if source_root_ref.is_some() && root.is_none() {
            return Err(crate::Error::Unsupported(
                "plain writer plan: /Root absent from renumber map".to_string(),
            ));
        }
        let direct_root = source_root_handle;
        let has_object_stream = placement
            .objects
            .iter()
            .any(|object| matches!(object, PlannedIndirectObject::ObjectStream { .. }));

        let form = if has_object_stream {
            XrefForm::Stream
        } else {
            XrefForm::Table
        };
        let source_version = pdf.version().to_string();
        let source_extension_level = pdf.adobe_extension_level()?.unwrap_or(0);
        let (effective_version, final_extension_level) =
            crate::writer::effective_pdf_version_and_ext(
                &source_version,
                source_extension_level,
                options,
                false,
                has_object_stream || form == XrefForm::Stream,
            );
        let mut version = effective_version.to_string();
        // If a source version cannot be numerically compared, the canonical
        // writer keeps its raw value. PDF 1.5 introduced xref streams, so
        // repair the header to that floor exactly as the full-rewrite path
        // does whenever this plan actually uses a stream form.
        if form == XrefForm::Stream
            && parse_qpdf_writer_version(&version)
                .is_none_or(|current| current < QpdfVersionParts::new(1, 5))
        {
            version = "1.5".to_string();
        }

        let source_id0 = live_source_id0(pdf)?;
        let deterministic_id = crate::writer::uses_deterministic_id(options);
        let generated_id = if deterministic_id || options.copy_encryption.is_some() {
            None
        } else {
            setup_generated_id.cloned().or_else(|| {
                Some(crate::writer::generate_id_handle(
                    source_id0.as_deref(),
                    options.static_id,
                ))
            })
        };
        let max_output = placement
            .objects
            .iter()
            .map(|object| match object {
                PlannedIndirectObject::Source { output, .. }
                | PlannedIndirectObject::RawSource { output, .. }
                | PlannedIndirectObject::ObjectStream { output, .. } => output.number,
            })
            .max()
            .unwrap_or(0);
        let trailer_size = usize::try_from(max_output)
            .ok()
            .and_then(|size| size.checked_add(usize::from(form == XrefForm::Stream) + 1))
            .ok_or_else(|| {
                // cov:ignore-start: ObjectRef numbers are u32 and fit usize on supported targets
                // ObjectRef numbers are u32, so this overflow arm is unreachable on
                // the supported 64-bit targets.
                crate::Error::Unsupported("plain writer trailer /Size overflows usize".into())
            })?
            // cov:ignore-end
            ;
        let trailer_handle = crate::writer::build_writer_trailer_handle(
            pdf,
            trailer_size,
            root,
            direct_root.as_ref(),
            options,
            None,
            deterministic_id,
            generated_id.as_ref(),
        )?; // cov:ignore: LLVM attributes this validated trailer-call continuation to the call setup
        let id = if deterministic_id {
            IdPlan::Deterministic {
                source_id0,
                info_suffix: crate::writer::deterministic_id_info_suffix(pdf),
            }
        } else {
            IdPlan::Materialized {
                value: materialized_id_handle(&trailer_handle.try_get_key(b"/ID")?)?,
            }
        };
        let encrypt = trailer_handle.try_get_key(b"/Encrypt")?.object_ref();
        let direct_root_bytes = if root.is_none() {
            let root_handle = trailer_handle.try_get_key(b"/Root")?;
            let map = |object_ref| {
                placement
                    .old_to_new
                    .get(&object_ref)
                    .copied()
                    .ok_or_else(|| {
                        // cov:ignore-start: the direct Catalog is collected
                        // by the same traversal that builds this map, so a
                        // live reference cannot be absent at emission.
                        crate::Error::Unsupported(format!(
                            "plain writer: direct /Root reference {} {} R absent from renumber map",
                            object_ref.number, object_ref.generation
                        ))
                        // cov:ignore-end
                    }) // cov:ignore: the direct-root reference map is exercised; LLVM places the successful closure-exit counter on this continuation line.
            };
            let mut bytes = Vec::new();
            if options.qdf {
                // Same reasoning as the indirect root in `body.rs`: qpdf's ADBE
                // arbitration is guarded by `is_root`, not by mode
                // (`QPDFWriter.cc:1396-1436`), so the QDF layout must serialize
                // the arbitrated copy rather than the raw Catalog. A direct
                // Catalog is not `is_root` for qpdf either — the test is
                // `old_og == m->root_og` (`:1374`) and a direct dictionary has
                // no object identity — so arbitration stays off here.
                let arbitrated = root_handle.output_root_copy_with_adbe(
                    &version,
                    final_extension_level,
                    false,
                )?; // cov:ignore: LLVM attributes this covered multiline call terminator to the call setup
                arbitrated.write_object_qdf_with_ref_map_and_removed(
                    &mut bytes,
                    0,
                    &map,
                    &placement.removed_refs,
                )?; // cov:ignore: direct Catalog QDF serialization is exercised; LLVM maps this validated continuation to the call setup.
            } else {
                root_handle.write_root_object_with_ref_map_and_removed(
                    &mut bytes,
                    &map,
                    &placement.removed_refs,
                    &version,
                    final_extension_level,
                    false,
                )?; // cov:ignore: the direct Catalog serializer is exercised; LLVM maps this call terminator to a zero-count continuation region.
            }
            Some(bytes)
        } else {
            None
        };
        let structural_filtered = matches!(
            crate::writer::effective_stream_policy(options),
            Some(CompressStreams::Yes)
        );
        let trailer = TrailerPlan {
            form,
            canonical_entries: canonical_trailer_entries(
                pdf,
                &placement.old_to_new,
                &placement.removed_refs,
            )?, // cov:ignore: malformed live trailer graphs are rejected at the helper boundary
            root,
            direct_root: direct_root_bytes,
            id,
            encrypt,
            structural_filtered,
            qdf: options.qdf,
        };

        let plan = Self {
            version,
            final_extension_level,
            objects: placement.objects,
            root_source: source_root_ref,
            root,
            direct_root,
            old_to_new: placement.old_to_new,
            removed_refs: placement.removed_refs,
            cached_stream_outputs: cached_stream_outputs.into_inner(),
            qdf_holder_map: qdf_emission
                .as_ref()
                .map(|qdf| qdf.holder_map.clone())
                .unwrap_or_default(),
            qdf_holder_numbers: qdf_emission
                .as_ref()
                .map(|qdf| qdf.holder_numbers.clone())
                .unwrap_or_default(),
            trailer_handle,
            trailer,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub(crate) fn validate(&self) -> crate::Result<()> {
        let mut outputs = BTreeSet::new();
        let mut sources = BTreeSet::new();
        let mut source_backed_containers = BTreeSet::new();
        let mut has_object_stream = false;

        for object in &self.objects {
            match object {
                PlannedIndirectObject::Source { source, output } => {
                    require_not_removed(&self.removed_refs, *source, "source")?;
                    require_unique_output(&mut outputs, *output)?;
                    require_unique_source(&mut sources, *source)?;
                    require_matching_mapping(&self.old_to_new, *source, *output)?;
                }
                PlannedIndirectObject::RawSource {
                    source,
                    raw,
                    output,
                } => {
                    require_unique_output(&mut outputs, *output)?;
                    require_unique_source(&mut sources, *source)?;
                    require_matching_mapping(&self.old_to_new, *source, *output)?;
                    // cov:ignore-start: parsed raw xref entries are always
                    // indirect by construction.
                    if !raw.is_indirect() {
                        return Err(crate::Error::Unsupported(
                            "plain writer plan: raw source is not indirect".to_string(),
                        ));
                    }
                    // cov:ignore-end
                }
                PlannedIndirectObject::ObjectStream {
                    origin,
                    output,
                    members,
                } => {
                    has_object_stream = true;
                    require_unique_output(&mut outputs, *output)?;
                    if let PlannedObjectStreamOrigin::SourceBacked(source)
                    | PlannedObjectStreamOrigin::Generated(source) = origin
                    {
                        // A removed source container still owns the preserved
                        // membership and output identity. qpdf reconstructs it
                        // from a null placeholder while treating ordinary
                        // references to the removed source as null.
                        source_backed_containers.insert(*source);
                        require_unique_source(&mut sources, *source)?;
                        require_matching_mapping(&self.old_to_new, *source, *output)?;
                    }
                    for member in members {
                        require_not_removed(&self.removed_refs, member.source, "ObjStm member")?;
                        if member.output.generation != 0 {
                            return Err(crate::Error::Unsupported(format!(
                                "plain writer plan: ObjStm output member {} {} R must have generation 0",
                                member.output.number, member.output.generation
                            )));
                        }
                        require_unique_source(&mut sources, member.source)?;
                        require_matching_mapping(&self.old_to_new, member.source, member.output)?;
                        if !outputs.insert(member.output.number) {
                            return Err(crate::Error::Unsupported(format!(
                                "plain writer plan: output object {} has multiple placements",
                                member.output.number
                            )));
                        }
                    }
                }
            }
        }

        if let Some(removed) = self.removed_refs.iter().find(|removed| {
            self.old_to_new.contains_key(removed) && !source_backed_containers.contains(removed)
        }) {
            return Err(crate::Error::Unsupported(format!(
                "plain writer plan: removed source {} {} R remains in old-to-new map",
                removed.number, removed.generation
            )));
        }

        if let Some(extra) = self
            .old_to_new
            .keys()
            .find(|source| !sources.contains(source))
        {
            return Err(crate::Error::Unsupported(format!(
                "plain writer plan: source {} {} R has no placement",
                extra.number, extra.generation
            )));
        }

        match (
            self.root,
            self.direct_root.as_ref(),
            self.trailer.root,
            self.trailer.direct_root.as_ref(),
        ) {
            (Some(root), None, Some(trailer_root), None) => {
                if !self.old_to_new.values().any(|&output| output == root) {
                    return Err(crate::Error::Unsupported(format!(
                        "plain writer plan: root {} {} R is absent from old-to-new map",
                        root.number, root.generation
                    )));
                }
                if trailer_root != root {
                    return Err(crate::Error::Unsupported(format!(
                        "plain writer plan: trailer root {} {} R differs from plan root {} {} R",
                        trailer_root.number, trailer_root.generation, root.number, root.generation
                    )));
                }
            }
            (None, Some(_), None, Some(_)) => {}
            _ => {
                return Err(crate::Error::Unsupported(
                    "plain writer plan: Catalog root form is inconsistent".to_string(),
                ));
            }
        }

        if let Some(&max_output) = outputs.last() {
            for number in 1..=max_output {
                if !outputs.contains(&number) && !self.qdf_holder_numbers.contains(&number) {
                    return Err(crate::Error::Unsupported(format!(
                        "plain writer plan: output object {number} has no placement"
                    )));
                }
            }
        } // cov:ignore: a valid plan always places the mapped root, so outputs is nonempty

        if has_object_stream || self.trailer.form == XrefForm::Stream {
            let version = parse_qpdf_writer_version(&self.version).ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain writer plan: invalid PDF version {}",
                    self.version
                ))
            })?;
            if version < QpdfVersionParts::new(1, 5) {
                return Err(crate::Error::Unsupported(format!(
                    "plain writer plan: PDF {} cannot contain object or xref streams",
                    self.version
                )));
            }
        }

        Ok(())
    }

    pub(crate) fn new_for_original(&self, source: ObjectRef) -> Option<ObjectRef> {
        self.old_to_new.get(&source).copied()
    }
}

struct PlacementPlan {
    objects: Vec<PlannedIndirectObject>,
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    removed_refs: BTreeSet<ObjectRef>,
}

#[derive(Debug, Default)]
struct QdfEmissionPlan {
    map: HashMap<ObjectRef, ObjectRef>,
    container_map: HashMap<ObjectRef, ObjectRef>,
    holder_map: HashMap<u32, u32>,
    holder_numbers: BTreeSet<u32>,
}

/// Assign qpdf's sequential QDF emission numbers to the already-selected
/// plain placements. QDF emits source objects in the writer queue order and
/// inserts an output-only `/Length` holder immediately after every ordinary
/// stream; ObjStm members are part of the container body and do not receive
/// holders of their own (`QPDFWriter.cc:1621-1775`).
fn build_qdf_emission_plan<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    placement: &PlacementPlan,
) -> crate::Result<QdfEmissionPlan> {
    let mut result = QdfEmissionPlan::default();
    let mut next = 0_u32;
    let mut next_number = || {
        // cov:ignore-start: the supported PDF object space cannot emit more
        // than u32::MAX QDF objects.
        next = next.checked_add(1).ok_or_else(|| {
            crate::Error::Unsupported("plain writer QDF number overflows u32".to_string())
        })?;
        // cov:ignore-end
        Ok::<u32, crate::Error>(next)
    };

    for object in &placement.objects {
        match object {
            PlannedIndirectObject::Source { source, .. } => {
                let emission = next_number()?;
                result.map.insert(*source, ObjectRef::new(emission, 0));
                // A source stream's length is represented by a synthetic
                // next-number holder in QDF. An unretained source ObjStm is an
                // ordinary stream in qpdf's output and receives the same
                // holder. Only the rebuilt XRef stream is structural here;
                // retained/generated ObjStm containers are represented by the
                // dedicated placement arm below (`QPDFWriter.cc:1620-1775`).
                let handle = pdf.get_object_handle(*source);
                handle.try_dereference()?;
                let is_real_stream = handle.as_stream_dict().is_some()
                    && !handle.try_is_stream_of_type(b"XRef", b"")?;
                if is_real_stream {
                    let holder = next_number()?;
                    result.holder_map.insert(emission, holder);
                    result.holder_numbers.insert(holder);
                }
            }
            // cov:ignore-start: qdf raw-generation placement has no valid
            // pinned fixture for an out-of-range object header.
            PlannedIndirectObject::RawSource { source, raw, .. } => {
                let emission = next_number()?;
                result.map.insert(*source, ObjectRef::new(emission, 0));
                let handle = pdf
                    .get_object_handle_by_raw_identity(raw.get_obj() as i32, raw.get_gen() as i32);
                handle.try_dereference()?;
                if handle.as_stream_dict().is_some()
                    && !handle.try_is_stream_of_type(b"XRef", b"")?
                {
                    let holder = next_number()?;
                    result.holder_map.insert(emission, holder);
                    result.holder_numbers.insert(holder);
                }
            }
            // cov:ignore-end
            PlannedIndirectObject::ObjectStream {
                origin,
                output,
                members,
            } => {
                let container = next_number()?;
                result
                    .container_map
                    .insert(*output, ObjectRef::new(container, 0));
                if let PlannedObjectStreamOrigin::SourceBacked(source)
                | PlannedObjectStreamOrigin::Generated(source) = origin
                {
                    result.map.insert(*source, ObjectRef::new(container, 0));
                }
                for member in members {
                    let emission = next_number()?;
                    result
                        .map
                        .insert(member.source, ObjectRef::new(emission, 0));
                }
            }
        }
    }

    Ok(result)
}

pub(crate) fn live_source_id0<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<Option<Vec<u8>>> {
    let id = pdf.trailer().try_get_key(b"/ID")?;
    let Some(values) = id.try_as_array()? else {
        return Ok(None);
    };
    let Some(first) = values.first() else {
        return Ok(None);
    };
    first.try_dereference()?;
    Ok(first.as_string().filter(|bytes| !bytes.is_empty()))
}

/// Snapshot the writer-owned trailer entries from qpdf's live canonical
/// trailer handle, preserving the handle graph until each value is emitted.
///
/// qpdf's `getTrimmedTrailer`/`writeTrailer` path works from the live trailer,
/// while `enqueueObjectsStandard` applies `getKeys()` null visibility before
/// it seeds the object queue (`QPDFWriter.cc:1163-1192, 2009-2029, 2916-2924`).
/// The legacy `Pdf::trailer()` dictionary is a construction-time snapshot and
/// cannot represent a later `trailer()` mutation. Direct dictionaries
/// and arrays are serialized through the canonical writer boundary so nested
/// dictionary nulls are omitted and array null positions remain present.
pub(crate) fn canonical_trailer_entries(
    pdf: &mut Pdf<impl Read + Seek>,
    map: &HashMap<ObjectRef, ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    canonical_trailer_entries_with_visibility(pdf, map, removed_refs, true)
}

/// Snapshot trailer entries while preserving qpdf's mode-independent
/// top-level null visibility. `QPDFWriter::getTrimmedTrailer` applies the
/// `getKeys()` rule before `writeTrailer` for plain, QDF, and encrypted output
/// alike (`QPDFWriter.cc:1163-1192, 2009-2029, 2917-2926`).
pub(crate) fn canonical_trailer_entries_with_visibility(
    pdf: &mut Pdf<impl Read + Seek>,
    map: &HashMap<ObjectRef, ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    suppress_null_values: bool,
) -> crate::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let trailer = pdf.trailer();
    let entries = trailer.try_as_dictionary()?.unwrap_or_default();
    let mut serialized = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if is_writer_owned_trailer_key(&key) {
            continue;
        }
        if value
            .object_ref()
            .is_some_and(|object_ref| object_ref.number == 0 || removed_refs.contains(&object_ref))
            || (suppress_null_values && value.try_is_null()?)
        {
            continue;
        }

        let mut value_bytes = Vec::new();
        if let Some(object_ref) = value.object_ref() {
            let mapped = map.get(&object_ref).copied().ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain writer: trailer /{} reference {object_ref} absent from renumber map",
                    String::from_utf8_lossy(key.strip_prefix(b"/").unwrap_or(&key))
                ))
            })?;
            value_bytes.extend_from_slice(mapped.to_string().as_bytes());
        } else {
            let map_ref = |object_ref: ObjectRef| {
                map.get(&object_ref).copied().ok_or_else(|| {
                    crate::Error::Unsupported(format!(
                        "plain writer: trailer nested reference {object_ref} absent from renumber map"
                    ))
                })
            };
            value.write_object_with_ref_map_and_removed(
                &mut value_bytes,
                &map_ref,
                removed_refs,
            )?;
        }

        // Keep qpdf's decoded key for the writer's raw-name sort. The xref
        // emitter escapes it only after sorting, since escaping can change
        // the bytewise order (e.g. `/ A` versus `/!A`).
        serialized.push((key, value_bytes));
    }
    Ok(serialized)
}

fn is_writer_owned_trailer_key(key: &[u8]) -> bool {
    matches!(
        key,
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
    )
}

fn build_sources_from_canonical_renumber(
    renumber: &CanonicalCatalogFirstRenumber,
) -> PlacementPlan {
    let pairs: Vec<(ObjectRef, ObjectRef)> = renumber.pairs().collect();
    let old_to_new = pairs
        .iter()
        .map(|&(output, source)| (source, output))
        .collect();
    let objects = pairs
        .into_iter()
        .map(|(output, source)| {
            renumber
                .raw_source_for(source)
                .map(|raw| PlannedIndirectObject::RawSource {
                    source,
                    raw,
                    output,
                })
                .unwrap_or(PlannedIndirectObject::Source { source, output })
        })
        .collect();
    PlacementPlan {
        objects,
        old_to_new,
        removed_refs: BTreeSet::new(),
    }
}

fn renumber_plain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    groups: &[ObjectStreamGroup],
    removed_refs: &BTreeSet<ObjectRef>,
    preserve_unreferenced_objects: bool,
    stream_parameters_removed: StreamParametersRemoved<'_>,
) -> crate::Result<ObjectStreamRenumber> {
    ObjectStreamRenumber::build_with_stream_policy(
        pdf,
        groups,
        true,
        removed_refs,
        preserve_unreferenced_objects,
        stream_parameters_removed,
    )
}

fn retain_reachable_object_stream_members<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    groups: &mut Vec<ObjectStreamGroup>,
    removed_refs: &BTreeSet<ObjectRef>,
    preserve_unreferenced_objects: bool,
    stream_parameters_removed: StreamParametersRemoved<'_>,
) -> crate::Result<()> {
    if groups.is_empty() {
        return Ok(());
    }
    // cov:ignore-start: LLVM attributes this covered reachability call to an argument line; the preserve and Generate writer tests exercise the complete call
    let reachable = CanonicalCatalogFirstRenumber::build_qpdf_with_stream_policy(
        pdf,
        true,
        preserve_unreferenced_objects,
        removed_refs,
        stream_parameters_removed,
    )?;
    // cov:ignore-end
    for group in groups.iter_mut() {
        group
            .members_mut()
            .retain(|member| reachable.new_for_original(*member).is_some());
    }
    groups.retain(|group| !group.members().is_empty());
    Ok(())
}

fn build_container_aware(
    renumber: ObjectStreamRenumber,
    groups: Vec<ObjectStreamGroup>,
    removed_refs: BTreeSet<ObjectRef>,
) -> crate::Result<PlacementPlan> {
    let old_to_new: HashMap<ObjectRef, ObjectRef> = renumber
        .pairs()
        .map(|(output, source)| (source, output))
        .collect();
    let member_sources: BTreeSet<ObjectRef> = groups
        .iter()
        .flat_map(ObjectStreamGroup::members)
        .copied()
        .collect();
    let container_sources: BTreeSet<ObjectRef> = groups
        .iter()
        .filter_map(|group| match group {
            ObjectStreamGroup::SourceBacked { source, .. }
            | ObjectStreamGroup::Generated { source, .. } => Some(*source),
            ObjectStreamGroup::Synthetic { .. } => None,
        })
        .collect();
    let mut objects: Vec<PlannedIndirectObject> = renumber
        .pairs()
        .filter(|(_, source)| {
            !member_sources.contains(source) && !container_sources.contains(source)
        })
        .map(|(output, source)| {
            renumber
                .raw_source_for(source)
                .map(|raw| PlannedIndirectObject::RawSource {
                    source,
                    raw,
                    output,
                })
                .unwrap_or(PlannedIndirectObject::Source { source, output })
        })
        .collect();

    for (group_index, group) in groups.iter().enumerate() {
        // cov:ignore-start: ObjectStreamRenumber assigns a container for every supplied group
        let container = renumber.container_number(group_index).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "plain writer plan: ObjStm group {group_index} was never reached"
            ))
        })?;
        // cov:ignore-end
        let mut members: Vec<PlannedMember> = group
            .members()
            .iter()
            .map(|&source| {
                old_to_new
                    .get(&source)
                    .copied()
                    .map(|output| PlannedMember { source, output })
                    // cov:ignore-start: groups are the same inputs used to build old_to_new
                    .ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "plain writer plan: ObjStm member {} {} R absent from renumber map",
                            source.number, source.generation
                        ))
                    })
                // cov:ignore-end
            })
            .collect::<crate::Result<Vec<_>>>()?;
        members.sort_unstable_by_key(|member| member.output.number);
        let origin = match group {
            ObjectStreamGroup::SourceBacked { source, .. } => {
                PlannedObjectStreamOrigin::SourceBacked(*source)
            }
            ObjectStreamGroup::Generated { source, .. } => {
                PlannedObjectStreamOrigin::Generated(*source)
            }
            ObjectStreamGroup::Synthetic { .. } => PlannedObjectStreamOrigin::Synthetic,
        };
        objects.push(PlannedIndirectObject::ObjectStream {
            origin,
            output: ObjectRef::new(container, 0),
            members,
        });
    }

    objects.sort_unstable_by_key(|object| match object {
        PlannedIndirectObject::Source { output, .. }
        | PlannedIndirectObject::RawSource { output, .. }
        | PlannedIndirectObject::ObjectStream { output, .. } => output.number,
    });

    Ok(PlacementPlan {
        objects,
        old_to_new,
        removed_refs,
    })
}

pub(crate) fn source_has_compressed_entries<R: Read + Seek>(pdf: &Pdf<R>) -> bool {
    pdf.source_xref_entries()
        .values()
        .any(|offset| matches!(offset, XrefEntry::Compressed { .. }))
}

impl NewNumberLookup for PlainWritePlan {
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        PlainWritePlan::new_for_original(self, original)
    }
}

fn require_unique_output(outputs: &mut BTreeSet<u32>, output: ObjectRef) -> crate::Result<()> {
    if outputs.insert(output.number) {
        Ok(())
    } else {
        Err(crate::Error::Unsupported(format!(
            "plain writer plan: output object {} has multiple placements",
            output.number
        )))
    }
}

fn require_unique_source(
    sources: &mut BTreeSet<ObjectRef>,
    source: ObjectRef,
) -> crate::Result<()> {
    if sources.insert(source) {
        Ok(())
    } else {
        Err(crate::Error::Unsupported(format!(
            "plain writer plan: source {} {} R has multiple placements",
            source.number, source.generation
        )))
    }
}

fn require_not_removed(
    removed_refs: &BTreeSet<ObjectRef>,
    source: ObjectRef,
    role: &str,
) -> crate::Result<()> {
    if removed_refs.contains(&source) {
        Err(crate::Error::Unsupported(format!(
            "plain writer plan: removed source {} {} R has {role} placement",
            source.number, source.generation
        )))
    } else {
        Ok(())
    }
}

fn require_matching_mapping(
    old_to_new: &HashMap<ObjectRef, ObjectRef>,
    source: ObjectRef,
    output: ObjectRef,
) -> crate::Result<()> {
    match old_to_new.get(&source) {
        Some(mapped) if *mapped == output => Ok(()),
        Some(mapped) => Err(crate::Error::Unsupported(format!(
            "plain writer plan: source {} {} R maps to {} {} R but is placed at {} {} R",
            source.number,
            source.generation,
            mapped.number,
            mapped.generation,
            output.number,
            output.generation
        ))),
        None => Err(crate::Error::Unsupported(format!(
            "plain writer plan: source {} {} R is absent from old-to-new map",
            source.number, source.generation
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::object_handle::ObjectValue;
    use crate::writer::object_streams::ObjectStreamMode;
    use crate::writer::plain::xref::{append_xref_and_trailer, BodyLayout, IdPlan, TrailerPlan};
    use crate::writer::WriterOptions;
    use crate::{NewlineBeforeEndstream, ObjectHandle, ObjectRef, Pdf, PdfWriter, XrefForm};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    fn fixture_path(fixture: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(fixture)
    }

    fn write_options(mode: ObjectStreamMode) -> WriterOptions {
        WriterOptions {
            object_streams: mode,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        }
    }

    fn build(fixture: &str, mode: ObjectStreamMode) -> PlainWritePlan {
        let path = fixture_path(fixture);
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let options = write_options(mode);
        PlainWritePlan::build(&mut pdf, &options).unwrap()
    }

    fn direct_stream_source() -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let objects: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            ),
            (4, b"<< /Length 3 >>\nstream\nq Q\nendstream"),
        ];
        let mut offsets = BTreeMap::new();
        for (number, body) in objects {
            offsets.insert(*number, bytes.len() as u64);
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref = bytes.len() as u64;
        let size = 5;
        bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for number in 1..size {
            bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&number]).as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    fn raw_generation_stream_source() -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let catalog_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let pages_offset = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let page_offset = bytes.len();
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        );
        let content_offset = bytes.len();
        bytes.extend_from_slice(b"4 0 obj\n<< /Length 3 >>\nstream\nq Q\nendstream\nendobj\n");
        let stream_offset = bytes.len();
        bytes.extend_from_slice(
            b"5 65536 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n%tail\n",
        );
        let xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{pages_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{page_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{content_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{stream_offset:010} 65536 n \n").as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    #[test]
    fn plan_handles_direct_streams_without_a_source_cache_identity() {
        struct PassThroughTokenFilter;

        // cov:ignore-start: the direct-stream planning branch intentionally does not pipe data; this filter only flips qpdf's isDataModified bit.
        impl crate::token_filter::TokenFilter for PassThroughTokenFilter {
            fn handle_token(
                &mut self,
                token: &crate::tokenizer::Token,
                output: &mut crate::token_filter::TokenFilterOutput<'_>,
            ) -> crate::pipeline::PipelineResult<()> {
                output.write_token(token)
            }
        }
        // cov:ignore-end

        for modified in [false, true] {
            let mut pdf = Pdf::open_mem_owned(direct_stream_source()).unwrap();
            let page = pdf.get_object_handle(ObjectRef::new(3, 0));
            page.try_is_scalar().unwrap();
            let direct_stream = ObjectHandle::stream(
                ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(3))]),
                Rc::new(b"q Q".to_vec()),
            );
            page.replace_key(b"Contents", direct_stream).unwrap();
            let contents = page.try_get_key(b"Contents").unwrap();
            if modified {
                contents
                    .add_token_filter(Rc::new(RefCell::new(PassThroughTokenFilter)))
                    .unwrap();
            }

            let plan =
                PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap();
            assert_eq!(plan.root, Some(ObjectRef::new(1, 0)));
        }
    }

    #[test]
    fn qdf_plan_serializes_a_direct_catalog_in_qdf_layout() {
        let mut pdf = Pdf::empty().unwrap();
        let pages = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.trailer()
            .replace_key(
                b"/Root",
                ObjectHandle::dictionary(vec![
                    (b"Type".to_vec(), ObjectHandle::name(b"Catalog".to_vec())),
                    (b"Pages".to_vec(), pages),
                ]),
            )
            .unwrap();
        let mut options = write_options(ObjectStreamMode::Disable);
        options.qdf = true;

        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();

        let direct_root = plan
            .trailer
            .direct_root
            .as_ref()
            .expect("direct root bytes");
        assert!(direct_root.starts_with(b"<<\n"));
    }

    fn source(source: u32, output: u32) -> PlannedIndirectObject {
        PlannedIndirectObject::Source {
            source: ObjectRef::new(source, 0),
            output: ObjectRef::new(output, 0),
        }
    }

    fn plan_for_test(objects: Vec<PlannedIndirectObject>) -> PlainWritePlan {
        let root_source = ObjectRef::new(1, 0);
        let root_output = ObjectRef::new(1, 0);
        PlainWritePlan {
            version: "1.5".to_string(),
            final_extension_level: 0,
            objects,
            root_source: Some(root_source),
            root: Some(root_output),
            direct_root: None,
            old_to_new: HashMap::from([(root_source, root_output)]),
            removed_refs: BTreeSet::new(),
            cached_stream_outputs: HashMap::new(),
            qdf_holder_map: HashMap::new(),
            qdf_holder_numbers: BTreeSet::new(),
            trailer_handle: crate::ObjectHandle::dictionary(Vec::new()),
            trailer: TrailerPlan {
                form: XrefForm::Table,
                canonical_entries: Vec::new(),
                root: Some(root_output),
                direct_root: None,
                id: IdPlan::Materialized { value: None },
                encrypt: None,
                structural_filtered: false,
                qdf: false,
            },
        }
    }

    #[test]
    fn validation_rejects_duplicate_output_numbers() {
        let mut plan = plan_for_test(vec![source(1, 1), source(2, 1)]);
        plan.root = Some(ObjectRef::new(1, 0));
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("output object 1")));
    }

    #[test]
    fn validation_rejects_source_and_source_backed_container_for_same_source() {
        let container_source = ObjectRef::new(2, 0);
        let mut plan = plan_for_test(vec![
            source(1, 1),
            source(2, 3),
            PlannedIndirectObject::ObjectStream {
                origin: PlannedObjectStreamOrigin::SourceBacked(container_source),
                output: ObjectRef::new(2, 0),
                members: Vec::new(),
            },
        ]);
        plan.old_to_new
            .insert(container_source, ObjectRef::new(3, 0));
        plan.trailer.form = XrefForm::Stream;

        let error = plan.validate().unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("source 2 0 R has multiple placements")));
    }

    #[test]
    fn validation_rejects_objstm_output_with_nonzero_generation() {
        let member = PlannedMember {
            source: ObjectRef::new(7, 1),
            output: ObjectRef::new(2, 1),
        };
        let plan = plan_for_test(vec![PlannedIndirectObject::ObjectStream {
            origin: PlannedObjectStreamOrigin::Synthetic,
            output: ObjectRef::new(1, 0),
            members: vec![member],
        }]);
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("output member 2 1 R")));
    }

    #[test]
    fn validation_rejects_objstm_member_colliding_with_plain_output() {
        let member = PlannedMember {
            source: ObjectRef::new(7, 0),
            output: ObjectRef::new(1, 0),
        };
        let mut plan = plan_for_test(vec![
            source(1, 1),
            PlannedIndirectObject::ObjectStream {
                origin: PlannedObjectStreamOrigin::Synthetic,
                output: ObjectRef::new(3, 0),
                members: vec![member],
            },
        ]);
        plan.old_to_new
            .insert(ObjectRef::new(7, 0), ObjectRef::new(1, 0));

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("output object 1 has multiple placements")));
    }

    #[test]
    fn validation_accepts_nonzero_source_generation_for_zero_generation_output() {
        let member = PlannedMember {
            source: ObjectRef::new(7, 1),
            output: ObjectRef::new(2, 0),
        };
        let mut plan = plan_for_test(vec![
            source(1, 1),
            PlannedIndirectObject::ObjectStream {
                origin: PlannedObjectStreamOrigin::Synthetic,
                output: ObjectRef::new(3, 0),
                members: vec![member],
            },
        ]);
        plan.old_to_new
            .insert(ObjectRef::new(7, 1), ObjectRef::new(2, 0));
        plan.trailer.form = XrefForm::Stream;

        plan.validate().unwrap();
    }

    #[test]
    fn disable_xref_stream_source_keeps_parseable_source_version_and_uses_table() {
        let mut bytes = std::fs::read(fixture_path("three-page-objstm.pdf")).unwrap();
        bytes[7] = b'4';
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap();

        assert_eq!(plan.version, "1.4");
        assert_eq!(plan.trailer.form, XrefForm::Table);
        assert!(plan
            .objects
            .iter()
            .all(|object| matches!(object, PlannedIndirectObject::Source { .. })));
    }

    #[test]
    fn disable_xref_stream_source_does_not_apply_stream_version_repair() {
        let mut bytes = std::fs::read(fixture_path("three-page-objstm.pdf")).unwrap();
        bytes[5] = b'9';
        bytes[7] = b'9';
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .unwrap();
        assert_eq!(pdf.version(), "9.9");

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap();

        assert_eq!(plan.version, "9.9");
        assert_eq!(plan.trailer.form, XrefForm::Table);
        plan.validate().unwrap();
    }

    #[test]
    fn canonical_trailer_entries_follow_qpdf_null_visibility() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("three-page.pdf")).unwrap(),
        ))
        .unwrap();
        let trailer = pdf.trailer();
        trailer.remove_key(b"/Info");
        let null_ref = ObjectRef::new(100, 0);
        let null_handle = pdf.get_object_handle(null_ref);
        null_handle.set_resolved(ObjectValue::Null);
        trailer.replace_key(b"/Null", null_handle).unwrap();
        let mut map = HashMap::new();
        map.insert(null_ref, ObjectRef::new(200, 0));

        let suppressed =
            canonical_trailer_entries_with_visibility(&mut pdf, &map, &BTreeSet::new(), true)
                .unwrap();
        assert!(!suppressed.iter().any(|(key, _)| key == b"/Null"));

        let visible =
            canonical_trailer_entries_with_visibility(&mut pdf, &map, &BTreeSet::new(), false)
                .unwrap();
        assert_eq!(
            visible
                .iter()
                .find(|(key, _)| key == b"/Null")
                .map(|(_, value)| value.as_slice()),
            Some(b"200 0 R".as_slice())
        );
    }

    #[test]
    fn generated_xref_stream_uses_live_trailer_entries() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("three-page.pdf")).unwrap(),
        ))
        .unwrap();
        pdf.trailer()
            .replace_key(
                b"/Added",
                ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(7))]),
            )
            .unwrap();

        let output = {
            let mut writer = PdfWriter::new(&mut pdf);
            writer.set_object_stream_mode(ObjectStreamMode::Generate);
            writer.set_output_memory().unwrap();
            writer.write().unwrap();
            writer.get_buffer().unwrap()
        };
        let text = String::from_utf8_lossy(&output);

        assert!(
            text.contains("/Added << /Value 7 >>"),
            "generated xref stream must serialize the live trailer handle: {text}"
        );
    }

    #[test]
    fn live_source_id0_reads_existing_trailer_id() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("one-page.pdf")).unwrap(),
        ))
        .unwrap();

        assert!(pdf.trailer().try_has_key(b"/ID").unwrap());
        let id0 = live_source_id0(&mut pdf).unwrap();
        assert!(
            id0.is_some(),
            "live trailer /ID[0] must be visible to the planner"
        );
    }

    #[test]
    fn live_source_id0_reads_mutated_trailer_handle_id() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("one-page.pdf")).unwrap(),
        ))
        .unwrap();
        pdf.trailer()
            .replace_key(
                b"/ID",
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"mutated-permanent".to_vec()),
                    ObjectHandle::string(b"mutated-changing".to_vec()),
                ]),
            )
            .unwrap();

        assert_eq!(
            live_source_id0(&mut pdf).unwrap(),
            Some(b"mutated-permanent".to_vec())
        );
    }

    #[test]
    fn live_source_id0_returns_none_for_empty_id_array() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("one-page.pdf")).unwrap(),
        ))
        .unwrap();
        pdf.trailer()
            .replace_key(b"/ID", ObjectHandle::array(Vec::new()))
            .unwrap();

        assert_eq!(live_source_id0(&mut pdf).unwrap(), None);
    }

    #[test]
    fn canonical_deterministic_id_uses_the_live_info_entry() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("no-stream-one-page.pdf")).unwrap(),
        ))
        .unwrap();
        pdf.trailer()
            .replace_key(
                b"/Info",
                ObjectHandle::dictionary(vec![(
                    b"Title".to_vec(),
                    ObjectHandle::string(b"live-info-replacement-768".to_vec()),
                )]),
            )
            .unwrap();
        let mut options = write_options(ObjectStreamMode::Disable);
        options.static_id = false;
        options.deterministic_id = true;

        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();

        let IdPlan::Deterministic { info_suffix, .. } = plan.trailer.id else {
            panic!("deterministic ID plan expected"); // cov:ignore: deterministic_id=true guarantees this plan variant
        };
        assert_eq!(info_suffix, b" live-info-replacement-768");
    }

    #[test]
    fn canonical_trailer_sorts_decoded_names_before_escaping_them() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("no-stream-one-page.pdf")).unwrap(),
        ))
        .unwrap();
        let trailer = pdf.trailer();
        trailer.remove_key(b"/Info");
        trailer
            .replace_key(b"/ A", ObjectHandle::integer(1))
            .unwrap();
        trailer
            .replace_key(b"/!A", ObjectHandle::integer(2))
            .unwrap();

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap();
        let mut bytes = b"BODY".to_vec();
        let mut layout = BodyLayout::default();
        layout.uncompressed.insert(
            plan.root
                .expect("test plan must have an indirect root")
                .number,
            (
                plan.root
                    .expect("test plan must have an indirect root")
                    .generation,
                0,
            ),
        );
        append_xref_and_trailer(&mut bytes, &layout, &plan.trailer).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let escaped_space = text.find("/#20A").expect("escaped space-name key");
        let exclamation = text.find("/!A").expect("exclamation-name key");

        assert!(
            escaped_space < exclamation,
            "decoded key order must precede PDF name escaping: {text}"
        );
    }

    #[test]
    fn canonical_trailer_entries_reject_an_unmapped_indirect_value() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("three-page.pdf")).unwrap(),
        ))
        .unwrap();

        let error =
            canonical_trailer_entries(&mut pdf, &HashMap::new(), &BTreeSet::new()).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("trailer /Info reference")
                && message.contains("absent from renumber map")));
    }

    #[test]
    fn validation_rejects_source_missing_from_old_to_new() {
        let plan = plan_for_test(vec![source(1, 1), source(2, 2)]);
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("source 2 0 R is absent from old-to-new map")));
    }

    #[test]
    fn validation_rejects_source_mapping_that_differs_from_placement() {
        let mut plan = plan_for_test(vec![source(1, 1), source(2, 2)]);
        plan.old_to_new
            .insert(ObjectRef::new(2, 0), ObjectRef::new(3, 0));
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("source 2 0 R maps to 3 0 R but is placed at 2 0 R")));
    }

    #[test]
    fn validation_rejects_member_mapping_that_differs_from_placement() {
        let member = PlannedMember {
            source: ObjectRef::new(7, 0),
            output: ObjectRef::new(2, 0),
        };
        let mut plan = plan_for_test(vec![
            source(1, 1),
            PlannedIndirectObject::ObjectStream {
                origin: PlannedObjectStreamOrigin::Synthetic,
                output: ObjectRef::new(3, 0),
                members: vec![member],
            },
        ]);
        plan.old_to_new
            .insert(ObjectRef::new(7, 0), ObjectRef::new(4, 0));
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("source 7 0 R maps to 4 0 R but is placed at 2 0 R")));
    }

    #[test]
    fn validation_rejects_extra_old_to_new_entry() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        plan.old_to_new
            .insert(ObjectRef::new(2, 0), ObjectRef::new(2, 0));
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("source 2 0 R has no placement")));
    }

    #[test]
    fn validation_rejects_removed_source_placement() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        plan.removed_refs.insert(ObjectRef::new(1, 0));

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("removed source 1 0 R has source placement")));
    }

    #[test]
    fn validation_rejects_removed_objstm_member_placement() {
        let member = PlannedMember {
            source: ObjectRef::new(7, 0),
            output: ObjectRef::new(2, 0),
        };
        let mut plan = plan_for_test(vec![
            source(1, 1),
            PlannedIndirectObject::ObjectStream {
                origin: PlannedObjectStreamOrigin::Synthetic,
                output: ObjectRef::new(3, 0),
                members: vec![member],
            },
        ]);
        plan.old_to_new
            .insert(ObjectRef::new(7, 0), ObjectRef::new(2, 0));
        plan.removed_refs.insert(ObjectRef::new(7, 0));
        plan.trailer.form = XrefForm::Stream;

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("removed source 7 0 R has ObjStm member placement")));
    }

    #[test]
    fn validation_allows_removed_source_backed_container_placeholder() {
        let container_source = ObjectRef::new(2, 0);
        let mut plan = plan_for_test(vec![
            source(1, 1),
            PlannedIndirectObject::ObjectStream {
                origin: PlannedObjectStreamOrigin::SourceBacked(container_source),
                output: ObjectRef::new(2, 0),
                members: Vec::new(),
            },
        ]);
        plan.old_to_new
            .insert(container_source, ObjectRef::new(2, 0));
        plan.removed_refs.insert(container_source);
        plan.trailer.form = XrefForm::Stream;

        plan.validate().unwrap();
    }

    #[test]
    fn validation_rejects_removed_source_in_old_to_new_map() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        let removed = ObjectRef::new(2, 0);
        plan.old_to_new.insert(removed, ObjectRef::new(2, 0));
        plan.removed_refs.insert(removed);

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("removed source 2 0 R remains in old-to-new map")));
    }

    #[test]
    fn validation_rejects_duplicate_source_placement() {
        let plan = plan_for_test(vec![source(1, 1), source(1, 2)]);

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("source 1 0 R has multiple placements")));
    }

    #[test]
    fn validation_rejects_duplicate_objstm_member_output() {
        let mut plan = plan_for_test(vec![
            source(1, 1),
            PlannedIndirectObject::ObjectStream {
                origin: PlannedObjectStreamOrigin::Synthetic,
                output: ObjectRef::new(3, 0),
                members: vec![
                    PlannedMember {
                        source: ObjectRef::new(7, 0),
                        output: ObjectRef::new(2, 0),
                    },
                    PlannedMember {
                        source: ObjectRef::new(8, 0),
                        output: ObjectRef::new(2, 0),
                    },
                ],
            },
        ]);
        plan.old_to_new
            .insert(ObjectRef::new(7, 0), ObjectRef::new(2, 0));
        plan.old_to_new
            .insert(ObjectRef::new(8, 0), ObjectRef::new(2, 0));

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("output object 2 has multiple placements")));
    }

    #[test]
    fn validation_rejects_root_absent_from_old_to_new_values() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        plan.root = Some(ObjectRef::new(2, 0));
        plan.trailer.root = plan.root;

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("root 2 0 R is absent from old-to-new map")));
    }

    #[test]
    fn validation_rejects_output_number_holes() {
        let mut plan = plan_for_test(vec![source(1, 1), source(2, 3)]);
        plan.old_to_new
            .insert(ObjectRef::new(2, 0), ObjectRef::new(3, 0));

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("output object 2 has no placement")));
    }

    #[test]
    fn validation_rejects_raw_version_below_1_5_for_xref_stream() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        plan.version = "invalid".to_string();
        plan.trailer.form = XrefForm::Stream;

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("PDF invalid cannot contain object or xref streams")));
    }

    #[test]
    fn validation_rejects_version_below_1_5_for_xref_stream() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        plan.version = "1.4".to_string();
        plan.trailer.form = XrefForm::Stream;

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("PDF 1.4 cannot contain object or xref streams")));
    }

    #[test]
    fn lookup_helpers_return_mapping_and_skip_source_objects() {
        let plan = plan_for_test(vec![source(1, 1)]);

        assert_eq!(
            plan.new_for_original(ObjectRef::new(1, 0)),
            Some(ObjectRef::new(1, 0))
        );
        assert_eq!(
            <PlainWritePlan as NewNumberLookup>::new_for_original(&plan, ObjectRef::new(1, 0)),
            Some(ObjectRef::new(1, 0))
        );
    }

    #[test]
    fn validation_rejects_trailer_root_that_differs_from_plan_root() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        plan.trailer.root = Some(ObjectRef::new(2, 0));
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("trailer root 2 0 R differs from plan root 1 0 R")));
    }

    #[test]
    fn validation_rejects_inconsistent_catalog_root_forms() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        plan.root = None;
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message == "plain writer plan: Catalog root form is inconsistent"));
    }

    #[test]
    fn disable_plan_contains_only_source_objects() {
        let plan = build("three-page.pdf", ObjectStreamMode::Disable);
        assert!(plan
            .objects
            .iter()
            .all(|object| matches!(object, PlannedIndirectObject::Source { .. })));
        assert_eq!(plan.trailer.form, XrefForm::Table);
        plan.validate().unwrap();
    }

    #[test]
    fn preserve_without_source_objstm_uses_catalog_first_placement() {
        let plan = build("three-page.pdf", ObjectStreamMode::Preserve);

        assert!(plan
            .objects
            .iter()
            .all(|object| matches!(object, PlannedIndirectObject::Source { .. })));
        assert!(plan.trailer.form == XrefForm::Table);
    }

    #[test]
    fn build_materializes_deterministic_id_plan() {
        let path = fixture_path("three-page.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let mut options = write_options(ObjectStreamMode::Disable);
        options.static_id = false;
        options.deterministic_id = true;

        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();

        assert!(matches!(plan.trailer.id, IdPlan::Deterministic { .. }));
    }

    #[test]
    fn forced_version_below_1_5_selects_classic_xref() {
        let path = fixture_path("three-page-objstm.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let mut options = write_options(ObjectStreamMode::Disable);
        options.force_version = Some("1.4".to_string());

        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();

        assert_eq!(plan.version, "1.4");
        assert_eq!(plan.trailer.form, XrefForm::Table);
    }

    #[test]
    fn disable_public_null_replacement_remains_in_the_object_universe() {
        let path = fixture_path("null-visible-matrix.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let deleted = ObjectRef::new(5, 0);
        pdf.replace_object(deleted, ObjectHandle::null())
            .expect("replace the unreferenced object with null");

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap();

        assert!(plan.old_to_new.contains_key(&deleted));
        assert!(!plan.removed_refs.contains(&deleted));
    }

    #[test]
    fn generate_public_null_replacement_remains_in_the_object_universe() {
        let path = fixture_path("null-visible-matrix.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let deleted = ObjectRef::new(5, 0);
        pdf.replace_object(deleted, ObjectHandle::null())
            .expect("replace the unreferenced object with null");

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Generate)).unwrap();

        assert!(plan.old_to_new.contains_key(&deleted));
        assert!(!plan.removed_refs.contains(&deleted));
        plan.validate().unwrap();
    }

    #[test]
    fn preserve_source_objstm_members_keep_one_container_and_indices() {
        let plan = build("three-page-objstm.pdf", ObjectStreamMode::Preserve);
        let containers: Vec<_> = plan
            .objects
            .iter()
            .filter_map(|object| match object {
                PlannedIndirectObject::ObjectStream {
                    origin,
                    output,
                    members,
                } => Some((origin.clone(), *output, members.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(containers.len(), 1);
        assert_eq!(
            containers[0].0,
            PlannedObjectStreamOrigin::SourceBacked(ObjectRef::new(1, 0))
        );
        assert_eq!(
            plan.old_to_new.get(&ObjectRef::new(1, 0)),
            Some(&containers[0].1)
        );
        assert!(plan.objects.iter().all(|object| !matches!(
            object,
            PlannedIndirectObject::Source { source, .. }
                if *source == ObjectRef::new(1, 0)
        )));
        assert!(!containers[0].2.is_empty());
        for member in &containers[0].2 {
            assert_eq!(member.output.generation, 0);
        }
        assert_eq!(plan.trailer.form, XrefForm::Stream);
        plan.validate().unwrap();
    }

    #[test]
    fn preserve_without_source_objstm_uses_catalog_first_sources() {
        let plan = build("three-page.pdf", ObjectStreamMode::Preserve);
        assert!(plan
            .objects
            .iter()
            .all(|object| matches!(object, PlannedIndirectObject::Source { .. })));
    }

    #[test]
    fn preserve_stale_generation_is_removed_from_membership() {
        let plan = build(
            "null-visible-stale-generation-objstm.pdf",
            ObjectStreamMode::Preserve,
        );
        let stale = ObjectRef::new(4, 0);
        assert!(plan.removed_refs.contains(&stale));
        assert!(plan.objects.iter().all(|object| match object {
            PlannedIndirectObject::ObjectStream { members, .. } =>
                members.iter().all(|member| member.source != stale),
            PlannedIndirectObject::Source { source, .. } => *source != stale,
            // cov:ignore-start: this fixture has no raw-generation orphan.
            PlannedIndirectObject::RawSource { source, .. } => *source != stale,
            // cov:ignore-end
        }));
    }

    #[test]
    fn preserve_public_null_replacement_remains_in_the_object_stream() {
        let path = fixture_path("three-page-objstm.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let deleted = ObjectRef::new(4, 0);
        pdf.replace_object(deleted, ObjectHandle::null())
            .expect("replace the object with null");

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Preserve)).unwrap();

        assert!(plan.old_to_new.contains_key(&deleted));
        assert!(!plan.removed_refs.contains(&deleted));
    }

    #[test]
    fn preserve_classic_fallback_keeps_public_null_replacement() {
        let path = fixture_path("null-visible-matrix.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let deleted = ObjectRef::new(5, 0);
        pdf.replace_object(deleted, ObjectHandle::null())
            .expect("replace the unreferenced object with null");

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Preserve)).unwrap();

        assert!(plan.old_to_new.contains_key(&deleted));
        assert!(!plan.removed_refs.contains(&deleted));
    }

    #[test]
    fn generate_plan_even_splits_132_eligible_objects() {
        let plan = build("objstm-gen-nostream-130rev.pdf", ObjectStreamMode::Generate);
        let containers: Vec<(PlannedObjectStreamOrigin, usize)> = plan
            .objects
            .iter()
            .filter_map(|object| match object {
                PlannedIndirectObject::ObjectStream {
                    origin, members, ..
                } => Some((origin.clone(), members.len())),
                _ => None, // cov:ignore: this fixture deliberately packs every planned source
            })
            .collect();
        assert_eq!(containers.len(), 2);
        assert_eq!(
            containers.iter().map(|(_, size)| *size).collect::<Vec<_>>(),
            [66, 66]
        );
        assert!(containers
            .iter()
            .all(|(origin, _)| matches!(origin, PlannedObjectStreamOrigin::Generated(_))));
        plan.validate().unwrap();
    }

    #[test]
    fn generate_plan_mints_qpdf_null_container_handles() {
        let path = fixture_path("objstm-gen-nostream-130rev.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Generate)).unwrap();
        let containers: Vec<_> =
            plan.objects
                .iter()
                .filter_map(|object| match object {
                    PlannedIndirectObject::ObjectStream { origin, .. } => Some(origin),
                    PlannedIndirectObject::Source { .. }
                    | PlannedIndirectObject::RawSource { .. } => None, // cov:ignore: this fixture packs every planned source.
                })
                .collect();
        assert!(!containers.is_empty());
        assert!(containers.iter().all(
            |origin| matches!(origin, PlannedObjectStreamOrigin::Generated(source)
                if pdf.get_object_handle(*source).is_null())
        ));
    }

    #[test]
    fn repeated_generate_preserve_writes_keep_raw_generation_orphans_writer_local() {
        let mut planning_pdf =
            Pdf::open(std::io::Cursor::new(raw_generation_stream_source())).unwrap();
        let mut planning_options = write_options(ObjectStreamMode::Disable);
        planning_options.preserve_unreferenced_objects = true;
        let plan = PlainWritePlan::build(&mut planning_pdf, &planning_options).unwrap();
        assert!(plan
            .objects
            .iter()
            .any(|object| matches!(object, PlannedIndirectObject::RawSource { .. })));

        let mut pdf = Pdf::open(std::io::Cursor::new(raw_generation_stream_source())).unwrap();
        let mut lengths = Vec::new();
        let mut source_object_counts = Vec::new();

        for _ in 0..3 {
            let mut writer = PdfWriter::new(&mut pdf);
            writer.set_object_stream_mode(ObjectStreamMode::Generate);
            writer.set_preserve_unreferenced_objects(true);
            writer.set_deterministic_id(true);
            writer.set_output_memory().unwrap();
            writer.write().unwrap();
            let output = writer.get_buffer().unwrap();
            let mut written = Pdf::open(std::io::Cursor::new(output.clone())).unwrap();
            assert!(written
                .get_all_objects()
                .unwrap()
                .into_iter()
                .filter_map(|object| object.get_stream_data(crate::DecodeLevel::Generalized).ok())
                .any(|data| data.windows(3).any(|window| window == b"abc")));
            lengths.push(output.len());
            source_object_counts.push(pdf.get_all_objects().unwrap().len());
        }

        assert!(lengths.iter().all(|length| *length > 0));
        #[cfg(not(feature = "qpdf-zlib-compat"))]
        {
            assert_eq!(
                source_object_counts[1] - source_object_counts[0],
                1,
                "raw write added more than qpdf's generated placeholder: {source_object_counts:?}"
            );
            assert_eq!(
                source_object_counts[2] - source_object_counts[1],
                1,
                "raw write added more than qpdf's generated placeholder: {source_object_counts:?}"
            );
        }
    }
}
