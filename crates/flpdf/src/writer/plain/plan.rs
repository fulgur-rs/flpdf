//! Plan logical object placement for ordinary PDF rewrites.
//!
//! qpdf correspondence: QPDFWriter.cc standard-write object placement and renumber planning.
//!

#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

#[cfg(test)]
use crate::pdf_version::{parse_qpdf_writer_version, QpdfVersionParts};
#[cfg(test)]
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::writer::object_streams::{self, ObjectStreamGroup, ObjectStreamMode};
#[cfg(test)]
use crate::writer::plain::xref::{materialized_id_handle, IdPlan, TrailerPlan};
#[cfg(test)]
use crate::writer::rewrite_renumber::{
    CanonicalCatalogFirstRenumber, NewNumberLookup, ObjectStreamRenumber,
};
use crate::writer::WriterOptions;
#[cfg(test)]
use crate::{CompressStreams, XrefForm};
use crate::{ObjectHandle, ObjectRef, Pdf};

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedMember {
    pub(crate) source: ObjectRef,
    pub(crate) output: ObjectRef,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedObjectStreamOrigin {
    SourceBacked(ObjectRef),
    Generated(ObjectRef),
    Synthetic,
}

#[cfg(test)]
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

/// Object-stream membership fixed during qpdf writer setup, before the live
/// standard-write queue starts assigning output numbers.
pub(crate) struct LiveObjectStreamPlan {
    pub(crate) groups: Vec<ObjectStreamGroup>,
    pub(crate) removed_refs: BTreeSet<ObjectRef>,
}

/// Build only the object-stream membership qpdf decides in `doWriteSetup`.
/// Reachability, stream dictionary visibility, child discovery, and output
/// numbering remain emission-time responsibilities of the live queue.
pub(crate) fn build_live_object_stream_plan<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    source_object_stream_data: &BTreeMap<u32, u32>,
    generated_compressible: Option<&object_streams::CompressiblePlan>,
    generated_object_stream_sources: &[ObjectRef],
) -> crate::Result<LiveObjectStreamPlan> {
    let mut plan = match options.object_streams {
        ObjectStreamMode::Disable => LiveObjectStreamPlan {
            groups: Vec::new(),
            removed_refs: BTreeSet::new(),
        },
        ObjectStreamMode::Preserve => {
            let plan = object_streams::plan_qpdf_preserve_object_streams_with_source_membership(
                pdf,
                options.preserve_unreferenced_objects,
                Some(source_object_stream_data),
            )?; // cov:ignore: LLVM maps the covered Preserve planning continuation to the call opening line
            LiveObjectStreamPlan {
                groups: plan.groups,
                removed_refs: plan.removed_refs,
            }
        }
        ObjectStreamMode::Generate => {
            // qpdf fixes Generate membership in `doWriteSetup`, before
            // `prepareFileForWrite` runs (`QPDFWriter.cc:2125-2139,2195`).
            // Recomputing it here would observe the post-preparation graph and
            // drop objects such as an indirect `/Extensions` dictionary that
            // preparation has since directized, so the setup snapshot is the
            // only accepted source of membership on this route.
            let compressible = generated_compressible
                .ok_or_else(|| {
                    crate::Error::Internal(
                        "Generate object-stream planning requires the writer setup snapshot".into(),
                    )
                })?
                .clone();
            let mut eligible = compressible.eligible;
            let removed_refs = compressible.removed_refs;
            eligible.retain(|member| !removed_refs.contains(member));
            let batches = object_streams::even_split_into_streams(&eligible);
            let mut groups = Vec::with_capacity(batches.len());
            for (index, members) in batches.into_iter().enumerate() {
                let source = if let Some(&source) = generated_object_stream_sources.get(index) {
                    source
                } else {
                    let container = pdf.make_indirect_object_handle(ObjectHandle::null())?;
                    container.object_ref().ok_or_else(|| {
                        // cov:ignore-start: make_indirect_object_handle always returns an indirect handle.
                        crate::Error::Internal(
                            "generated object-stream container lost its indirect identity".into(),
                        )
                        // cov:ignore-end
                    })? // cov:ignore: LLVM maps the covered generated-container identity continuation to this line
                };
                groups.push(ObjectStreamGroup::Generated { source, members });
            }
            LiveObjectStreamPlan {
                groups,
                removed_refs,
            }
        }
    };

    // QPDFWriter applies output-sensitive exclusions after Preserve/Generate
    // membership is built but before constructing the reverse container map
    // (`QPDFWriter.cc:2141-2173`). The plain pipeline never linearizes, so
    // only the encrypted-Catalog exclusion applies here. Route it through the
    // same `filter_objstm_batches_for_output` the specialized and linearized
    // Preserve/Generate routes already share, instead of a plain-local retain
    // (D23), so the three routes cannot drift on `QPDFWriter.cc:2141-2173`.
    let output_encrypted = options.encrypt.is_some() || options.copy_encryption.is_some();
    let mut origin_by_source: BTreeMap<ObjectRef, bool> = BTreeMap::new();
    let mut batches: Vec<Vec<ObjectRef>> = Vec::with_capacity(plan.groups.len());
    let mut source_containers: Vec<Option<ObjectRef>> = Vec::with_capacity(plan.groups.len());
    for group in plan.groups.drain(..) {
        let (source, members, is_generated) = match group {
            ObjectStreamGroup::SourceBacked { source, members } => (source, members, false),
            ObjectStreamGroup::Generated { source, members } => (source, members, true),
        };
        origin_by_source.insert(source, is_generated);
        source_containers.push(Some(source));
        batches.push(members);
    }
    object_streams::filter_objstm_batches_for_output(
        pdf,
        &mut batches,
        &mut source_containers,
        false,
        output_encrypted,
    )?; // cov:ignore: LLVM attributes this covered multiline exclusion terminator to the call setup
    plan.groups = batches
        .into_iter()
        .zip(source_containers)
        .map(|(members, source)| {
            let source =
                source.expect("plain writer ObjStm group must retain its source after filtering");
            let is_generated = origin_by_source.get(&source).copied().unwrap_or_else(|| {
                // cov:ignore-start: filter_objstm_batches_for_output only drops entries, it
                // never introduces a source this loop did not already record above.
                unreachable!("plain writer ObjStm filter surfaced an untracked source container")
                // cov:ignore-end
            });
            if is_generated {
                ObjectStreamGroup::Generated { source, members }
            } else {
                ObjectStreamGroup::SourceBacked { source, members }
            }
        })
        .collect();

    Ok(plan)
}

#[cfg(test)]
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
    pub(crate) qdf_holder_numbers: BTreeSet<u32>,
    pub(crate) trailer: TrailerPlan,
}

#[cfg(test)]
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
        let mut source_object_stream_data = BTreeMap::new();
        if options.object_streams == ObjectStreamMode::Preserve {
            // Keep the test-only convenience wrapper on the same canonical
            // D9 source-membership owner as production setup. It must not
            // rederive the empty/non-empty decision from raw xref entries.
            pdf.get_object_stream_data(&mut source_object_stream_data);
        }
        Self::build_with_generated_id_and_source_object_stream_data(
            pdf,
            options,
            setup_generated_id,
            &source_object_stream_data,
            None,
            &[],
        )
    }

    pub(crate) fn build_with_generated_id_and_source_object_stream_data<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        options: &WriterOptions,
        setup_generated_id: Option<&crate::ObjectHandle>,
        source_object_stream_data: &BTreeMap<u32, u32>,
        generated_compressible: Option<&object_streams::CompressiblePlan>,
        generated_object_stream_sources: &[ObjectRef],
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
        let source_had_compressed_objects = !source_object_stream_data.is_empty();
        // qpdf's removeObject erases the canonical cache slot rather than
        // retaining a persistent deleted-reference tombstone.
        let explicitly_removed = BTreeSet::new();
        let mut placement = match options.object_streams {
            ObjectStreamMode::Disable => {
                let renumber = CanonicalCatalogFirstRenumber::build_qpdf(
                    pdf,
                    true,
                    options.preserve_unreferenced_objects,
                    &explicitly_removed,
                )?;
                let mut placement = build_sources_from_canonical_renumber(&renumber);
                placement.removed_refs = explicitly_removed;
                placement
            }
            ObjectStreamMode::Preserve => {
                if !source_had_compressed_objects {
                    let renumber = CanonicalCatalogFirstRenumber::build_qpdf(
                        pdf,
                        true,
                        options.preserve_unreferenced_objects,
                        &explicitly_removed,
                    )?; // cov:ignore: malformed canonical source graphs are rejected before placement
                    let mut placement = build_sources_from_canonical_renumber(&renumber);
                    placement.removed_refs = explicitly_removed;
                    placement
                } else {
                    let mut packing =
                        object_streams::plan_qpdf_preserve_object_streams_with_source_membership(
                            pdf,
                            options.preserve_unreferenced_objects,
                            Some(source_object_stream_data),
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
                    )?; // cov:ignore: LLVM maps this covered preserve-group call terminator to a zero-count continuation region
                    let groups = &packing.groups;
                    let removed = &packing.removed_refs;
                    let renumber = renumber_plain(
                        pdf,
                        groups,
                        removed,
                        options.preserve_unreferenced_objects,
                    )?; // cov:ignore: planner groups are produced by the same validated source walk
                    build_container_aware(renumber, packing.groups, packing.removed_refs)?
                }
            }
            ObjectStreamMode::Generate => {
                let compressible = if let Some(snapshot) = generated_compressible {
                    snapshot.clone()
                } else {
                    object_streams::compressible_objgens_qpdf_plan(pdf)?
                };
                let mut eligible = compressible.eligible;
                let mut removed_refs = compressible.removed_refs;
                removed_refs.extend(explicitly_removed.iter().copied());
                eligible.retain(|member| !removed_refs.contains(member));
                let groups = object_streams::even_split_into_streams(&eligible);
                let mut renumber_groups = Vec::with_capacity(groups.len());
                for (group_index, members) in groups.into_iter().enumerate() {
                    let source =
                        if let Some(&source) = generated_object_stream_sources.get(group_index) {
                            source
                        } else {
                            let container = pdf.make_indirect_object_handle(ObjectHandle::null())?;
                            // cov:ignore-start: make_indirect_object_handle always returns an indirect handle.
                            let source = container.object_ref().ok_or_else(|| {
                                crate::Error::Internal(
                                    "generated object-stream container lost its indirect identity"
                                        .into(),
                                )
                            })?;
                            // cov:ignore-end
                            source
                        };
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
                )?;
                // cov:ignore-end
                let renumber = renumber_plain(
                    pdf,
                    &renumber_groups,
                    removed,
                    options.preserve_unreferenced_objects,
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
                        .get_object_handle_by_raw_identity(raw.get_obj(), raw.get_gen())
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
        let structural_filtered = matches!(
            crate::writer::effective_stream_policy(options),
            Some(CompressStreams::Yes)
        );
        let trailer = TrailerPlan {
            form,
            root,
            direct_root: direct_root.clone(),
            id,
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
            qdf_holder_numbers: qdf_emission
                .as_ref()
                .map(|qdf| qdf.holder_numbers.clone())
                .unwrap_or_default(),
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

#[cfg(test)]
struct PlacementPlan {
    objects: Vec<PlannedIndirectObject>,
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    removed_refs: BTreeSet<ObjectRef>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct QdfEmissionPlan {
    map: HashMap<ObjectRef, ObjectRef>,
    container_map: HashMap<ObjectRef, ObjectRef>,
    holder_numbers: BTreeSet<u32>,
}

/// Assign qpdf's sequential QDF emission numbers to the already-selected
/// plain placements. QDF emits source objects in the writer queue order and
/// inserts an output-only `/Length` holder immediately after every ordinary
/// stream; ObjStm members are part of the container body and do not receive
/// holders of their own (`QPDFWriter.cc:1621-1775`).
#[cfg(test)]
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
                    result.holder_numbers.insert(holder);
                }
            }
            // cov:ignore-start: qdf raw-generation placement has no valid
            // pinned fixture for an out-of-range object header.
            PlannedIndirectObject::RawSource { source, raw, .. } => {
                let emission = next_number()?;
                result.map.insert(*source, ObjectRef::new(emission, 0));
                let handle = pdf.get_object_handle_by_raw_identity(raw.get_obj(), raw.get_gen());
                handle.try_dereference()?;
                if handle.as_stream_dict().is_some()
                    && !handle.try_is_stream_of_type(b"XRef", b"")?
                {
                    let holder = next_number()?;
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

#[cfg(test)]
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

#[cfg(test)]
fn renumber_plain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    groups: &[ObjectStreamGroup],
    removed_refs: &BTreeSet<ObjectRef>,
    preserve_unreferenced_objects: bool,
) -> crate::Result<ObjectStreamRenumber> {
    ObjectStreamRenumber::build(
        pdf,
        groups,
        true,
        removed_refs,
        preserve_unreferenced_objects,
    )
}

#[cfg(test)]
fn retain_reachable_object_stream_members<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    groups: &mut Vec<ObjectStreamGroup>,
    removed_refs: &BTreeSet<ObjectRef>,
    preserve_unreferenced_objects: bool,
) -> crate::Result<()> {
    if groups.is_empty() {
        return Ok(());
    }
    // cov:ignore-start: LLVM attributes this covered reachability call to an argument line; the preserve and Generate writer tests exercise the complete call
    let reachable = CanonicalCatalogFirstRenumber::build_qpdf(
        pdf,
        true,
        preserve_unreferenced_objects,
        removed_refs,
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

#[cfg(test)]
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
        .map(|group| match group {
            ObjectStreamGroup::SourceBacked { source, .. }
            | ObjectStreamGroup::Generated { source, .. } => *source,
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

#[cfg(test)]
impl NewNumberLookup for PlainWritePlan {
    fn new_for_original(&self, original: ObjectRef) -> Option<ObjectRef> {
        PlainWritePlan::new_for_original(self, original)
    }
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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
    fn plan_handles_direct_modified_streams_without_payload_execution() {
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
    fn non_linearized_plan_does_not_invoke_a_stream_provider() {
        let mut pdf = Pdf::empty().unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let calls_for_provider = Rc::clone(&calls);
        let stream = pdf.new_stream().unwrap();
        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, suppress_warnings, will_retry| {
                    calls_for_provider
                        .borrow_mut()
                        .push((suppress_warnings, will_retry));
                    if will_retry {
                        return Ok(false);
                    }
                    pipeline.write(b"planned-at-emission")?;
                    pipeline.finish()?;
                    Ok(true)
                },
                Some(ObjectHandle::null()),
                Some(ObjectHandle::null()),
            )
            .unwrap();
        pdf.root_handle()
            .unwrap()
            .replace_key(b"/Deferred", stream)
            .unwrap();
        let options = write_options(ObjectStreamMode::Disable);

        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        assert!(calls.borrow().is_empty());

        let mut output = Vec::new();
        crate::writer::output::with_buffer_sink(&mut output, |out| {
            crate::writer::plain::body::emit_bodies(&mut pdf, out, &options, &plan)
        })
        .unwrap();
        assert_eq!(*calls.borrow(), vec![(false, true), (false, false)]);
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
            .expect("direct root handle");
        assert!(direct_root.as_dictionary().is_some());
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
            qdf_holder_numbers: BTreeSet::new(),
            trailer: TrailerPlan {
                form: XrefForm::Table,
                root: Some(root_output),
                direct_root: None,
                id: IdPlan::Materialized { value: None },
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
        let trailer_handle = ObjectHandle::dictionary(vec![
            (b"/ A".to_vec(), ObjectHandle::integer(1)),
            (b"/!A".to_vec(), ObjectHandle::integer(2)),
        ]);
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
        crate::writer::output::with_buffer_sink(&mut bytes, |out| {
            append_xref_and_trailer(
                out,
                &layout,
                &plan.trailer,
                &trailer_handle,
                &HashMap::new(),
                &BTreeSet::new(),
            )
        })
        .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let escaped_space = text.find("/#20A").expect("escaped space-name key");
        let exclamation = text.find("/!A").expect("exclamation-name key");

        assert!(
            escaped_space < exclamation,
            "decoded key order must precede PDF name escaping: {text}"
        );
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
    fn live_generate_setup_mints_qpdf_null_container_handles() {
        let path = fixture_path("objstm-gen-nostream-130rev.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let options = write_options(ObjectStreamMode::Generate);
        // Generate membership now always arrives as the writer's setup-time
        // snapshot; only the container identities are left for the planner to
        // mint when the caller supplies none.
        let compressible = object_streams::compressible_objgens_qpdf_plan(&mut pdf).unwrap();
        let plan = build_live_object_stream_plan(
            &mut pdf,
            &options,
            &BTreeMap::new(),
            Some(&compressible),
            &[],
        )
        .unwrap();

        assert!(!plan.groups.is_empty());
        assert!(plan.groups.iter().all(|group| {
            matches!(group, ObjectStreamGroup::Generated { source, .. }
                if pdf.get_object_handle(*source).is_null())
        }));
    }

    /// Build the live object-stream membership for `fixture` under `mode`,
    /// optionally with `--encrypt` set, returning the resulting groups.
    fn build_live_plan_with_encryption(
        fixture: &str,
        mode: ObjectStreamMode,
        encrypted: bool,
    ) -> (Pdf<std::io::BufReader<std::fs::File>>, LiveObjectStreamPlan) {
        let path = fixture_path(fixture);
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let mut source_object_stream_data = BTreeMap::new();
        if mode == ObjectStreamMode::Preserve {
            pdf.get_object_stream_data(&mut source_object_stream_data);
        }
        let compressible = if mode == ObjectStreamMode::Generate {
            Some(object_streams::compressible_objgens_qpdf_plan(&mut pdf).unwrap())
        } else {
            None
        };
        let mut options = write_options(mode);
        if encrypted {
            options.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
                Vec::new(),
                Vec::new(),
            ));
        }
        let plan = build_live_object_stream_plan(
            &mut pdf,
            &options,
            &source_object_stream_data,
            compressible.as_ref(),
            &[],
        )
        .unwrap();
        (pdf, plan)
    }

    /// qpdf keeps the root Catalog outside every ObjStm once encryption is
    /// requested (`QPDFWriter.cc:2141-2158`), but only then — Preserve mode's
    /// unencrypted membership packs the Catalog into its source container.
    /// Exercises the D23 unification (`filter_objstm_batches_for_output`) on
    /// the SourceBacked branch: the root leaves its container, every other
    /// member (and the container's own source identity) survives, and the
    /// group keeps its `SourceBacked` origin.
    #[test]
    fn live_preserve_setup_excludes_root_from_objstm_only_when_encrypted() {
        let (pdf, plan) = build_live_plan_with_encryption(
            "three-page-objstm.pdf",
            ObjectStreamMode::Preserve,
            false,
        );
        let root = pdf.root_ref().expect("fixture has an indirect /Root");
        assert!(
            plan.groups
                .iter()
                .any(|group| group.members().contains(&root)),
            "unencrypted Preserve baseline must still pack the Catalog into its source ObjStm"
        );
        let unencrypted_member_count: usize =
            plan.groups.iter().map(|group| group.members().len()).sum();

        let (pdf, plan) = build_live_plan_with_encryption(
            "three-page-objstm.pdf",
            ObjectStreamMode::Preserve,
            true,
        );
        let root = pdf.root_ref().expect("fixture has an indirect /Root");
        assert!(
            plan.groups
                .iter()
                .all(|group| !group.members().contains(&root)),
            "encrypted Preserve output must exclude the Catalog from every ObjStm"
        );
        assert!(
            plan.groups
                .iter()
                .all(|group| matches!(group, ObjectStreamGroup::SourceBacked { .. })),
            "excluding the Catalog must not change surviving groups' SourceBacked origin"
        );
        let encrypted_member_count: usize =
            plan.groups.iter().map(|group| group.members().len()).sum();
        assert_eq!(
            encrypted_member_count + 1,
            unencrypted_member_count,
            "only the Catalog itself should be removed by the encrypted exclusion"
        );
    }

    /// Same qpdf exclusion (`QPDFWriter.cc:2141-2158`) exercised on the
    /// `Generated` branch of the D23 unification: a freshly minted Generate
    /// container must also lose the Catalog once encrypted, while keeping its
    /// `Generated` origin.
    #[test]
    fn live_generate_setup_excludes_root_from_objstm_only_when_encrypted() {
        let (pdf, plan) = build_live_plan_with_encryption(
            "three-page-objstm.pdf",
            ObjectStreamMode::Generate,
            false,
        );
        let root = pdf.root_ref().expect("fixture has an indirect /Root");
        assert!(
            plan.groups
                .iter()
                .any(|group| group.members().contains(&root)),
            "unencrypted Generate baseline must still pack the Catalog into a generated ObjStm"
        );

        let (pdf, plan) = build_live_plan_with_encryption(
            "three-page-objstm.pdf",
            ObjectStreamMode::Generate,
            true,
        );
        let root = pdf.root_ref().expect("fixture has an indirect /Root");
        assert!(
            plan.groups
                .iter()
                .all(|group| !group.members().contains(&root)),
            "encrypted Generate output must exclude the Catalog from every ObjStm"
        );
        assert!(
            plan.groups
                .iter()
                .all(|group| matches!(group, ObjectStreamGroup::Generated { .. })),
            "excluding the Catalog must not change surviving groups' Generated origin"
        );
    }

    #[test]
    fn live_preserve_setup_keeps_source_backed_object_stream_membership() {
        let path = fixture_path("three-page-objstm.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let options = write_options(ObjectStreamMode::Preserve);
        let mut source_object_stream_data = BTreeMap::new();
        pdf.get_object_stream_data(&mut source_object_stream_data);

        let plan = build_live_object_stream_plan(
            &mut pdf,
            &options,
            &source_object_stream_data,
            None,
            &[],
        )
        .expect("live Preserve membership setup");

        assert!(plan.groups.iter().any(|group| {
            matches!(group, ObjectStreamGroup::SourceBacked { members, .. } if !members.is_empty())
        }));
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
