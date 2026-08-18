//! qpdf correspondence: QPDFWriter.cc standard-write object placement and renumber planning.
//! Logical object placements for the qpdf-shaped plain writer pipeline.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek};

use crate::pdf_version::{parse_pdf_version, PDF_1_5};
use crate::rewrite_renumber::{
    CanonicalCatalogFirstRenumber, NewNumberLookup, ObjectStreamRenumber,
};
use crate::writer::object_streams::{self, ObjectStreamGroup, ObjectStreamMode};
use crate::writer::plain::xref::{materialized_id_handle, IdPlan, TrailerPlan};
use crate::writer::WriterOptions;
use crate::{CompressStreams, ObjectRef, Pdf, XrefEntry, XrefForm};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedMember {
    pub(crate) source: ObjectRef,
    pub(crate) output: ObjectRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedObjectStreamOrigin {
    SourceBacked(ObjectRef),
    Synthetic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedIndirectObject {
    Source {
        source: ObjectRef,
        output: ObjectRef,
    },
    ObjectStream {
        origin: PlannedObjectStreamOrigin,
        output: ObjectRef,
        members: Vec<PlannedMember>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct PlainWritePlan {
    pub(crate) version: String,
    pub(crate) objects: Vec<PlannedIndirectObject>,
    pub(crate) root: ObjectRef,
    pub(crate) old_to_new: HashMap<ObjectRef, ObjectRef>,
    pub(crate) removed_refs: BTreeSet<ObjectRef>,
    pub(crate) trailer: TrailerPlan,
}

impl PlainWritePlan {
    pub(crate) fn generated_object_stream_count(&self) -> usize {
        self.objects
            .iter()
            .filter(|object| {
                matches!(
                    object,
                    PlannedIndirectObject::ObjectStream {
                        origin: PlannedObjectStreamOrigin::Synthetic,
                        ..
                    }
                )
            })
            .count()
    }

    pub(crate) fn build<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        options: &WriterOptions,
    ) -> crate::Result<Self> {
        let source_root = pdf.root_ref().ok_or(crate::Error::Missing("/Root"))?;
        let source_had_compressed_objects = source_has_compressed_entries(pdf);
        let explicitly_removed: BTreeSet<ObjectRef> =
            pdf.deleted_object_refs().into_iter().collect();

        let placement = match options.object_streams {
            ObjectStreamMode::Disable => {
                let renumber = CanonicalCatalogFirstRenumber::build_qpdf(
                    pdf,
                    true,
                    options.preserve_unreferenced_objects,
                    &explicitly_removed,
                )?;
                let mut placement = build_sources_from_pairs(renumber.pairs());
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
                    let mut placement = build_sources_from_pairs(renumber.pairs());
                    placement.removed_refs = explicitly_removed;
                    placement
                } else {
                    let mut packing =
                        object_streams::plan_qpdf_preserve_object_streams_with_unreferenced(
                            pdf,
                            options.preserve_unreferenced_objects,
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
                let mut compressible = object_streams::compressible_objgens_qpdf_plan(pdf)?;
                compressible
                    .removed_refs
                    .extend(explicitly_removed.iter().copied());
                compressible
                    .eligible
                    .retain(|member| !compressible.removed_refs.contains(member));
                let groups = object_streams::even_split_into_streams(&compressible.eligible);
                let renumber_groups: Vec<ObjectStreamGroup> = groups
                    .iter()
                    .cloned()
                    .map(|members| ObjectStreamGroup::Synthetic { members })
                    .collect();
                let removed = &compressible.removed_refs;
                let renumber = renumber_plain(pdf, &renumber_groups, removed, false)?;
                build_container_aware(renumber, renumber_groups, compressible.removed_refs)?
            }
        };

        let root = placement
            .old_to_new
            .get(&source_root)
            .copied()
            .ok_or_else(|| {
                crate::Error::Unsupported(
                    "plain writer plan: /Root absent from renumber map".to_string(),
                )
            })?;
        let has_object_stream = placement
            .objects
            .iter()
            .any(|object| matches!(object, PlannedIndirectObject::ObjectStream { .. }));

        let form = if has_object_stream {
            XrefForm::Stream
        } else {
            XrefForm::Table
        };
        let mut version = crate::writer::effective_pdf_version(
            pdf.version(),
            options,
            false,
            has_object_stream || form == XrefForm::Stream,
        )
        .to_string();
        // `effective_pdf_version` returns an unparseable source version verbatim,
        // so a malformed header such as `%PDF-x.y` would survive into a plan that
        // `validate` then rejects. PDF 1.5 introduced xref streams, so repair the
        // header to that floor exactly as the full-rewrite path does, keeping an
        // input the previous route rewrote successfully out of the error arm.
        if form == XrefForm::Stream
            && parse_pdf_version(&version).is_none_or(|current| current < PDF_1_5)
        {
            version = "1.5".to_string();
        }

        let source_id0 = live_source_id0(pdf)?;
        let generated_id = if options.deterministic_id || options.copy_encryption.is_some() {
            None
        } else {
            Some(crate::writer::generate_id_handle(
                source_id0.as_deref(),
                options.static_id,
            ))
        };
        let max_output = placement
            .objects
            .iter()
            .map(|object| match object {
                PlannedIndirectObject::Source { output, .. }
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
            pdf.last_xref_form() == XrefForm::Stream,
            form == XrefForm::Stream,
            trailer_size,
            root,
            options,
            None,
            options.deterministic_id,
            generated_id.as_ref(),
        )?; // cov:ignore: LLVM attributes this validated trailer-call continuation to the call setup
        let id = if options.deterministic_id {
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
            id,
            encrypt,
            structural_filtered,
        };

        let plan = Self {
            version,
            objects: placement.objects,
            root,
            old_to_new: placement.old_to_new,
            removed_refs: placement.removed_refs,
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
                PlannedIndirectObject::ObjectStream {
                    origin,
                    output,
                    members,
                } => {
                    has_object_stream = true;
                    require_unique_output(&mut outputs, *output)?;
                    if let PlannedObjectStreamOrigin::SourceBacked(source) = origin {
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

        if !self.old_to_new.values().any(|&output| output == self.root) {
            return Err(crate::Error::Unsupported(format!(
                "plain writer plan: root {} {} R is absent from old-to-new map",
                self.root.number, self.root.generation
            )));
        }

        if self.trailer.root != self.root {
            return Err(crate::Error::Unsupported(format!(
                "plain writer plan: trailer root {} {} R differs from plan root {} {} R",
                self.trailer.root.number,
                self.trailer.root.generation,
                self.root.number,
                self.root.generation
            )));
        }

        if let Some(&max_output) = outputs.last() {
            for number in 1..=max_output {
                if !outputs.contains(&number) {
                    return Err(crate::Error::Unsupported(format!(
                        "plain writer plan: output object {number} has no placement"
                    )));
                }
            }
        } // cov:ignore: a valid plan always places the mapped root, so outputs is nonempty

        if has_object_stream || self.trailer.form == XrefForm::Stream {
            let version = parse_pdf_version(&self.version).ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain writer plan: invalid PDF version {}",
                    self.version
                ))
            })?;
            if version < PDF_1_5 {
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

fn live_source_id0<R: Read + Seek>(pdf: &mut Pdf<R>) -> crate::Result<Option<Vec<u8>>> {
    let id = pdf.trailer_handle().try_get_key(b"/ID")?;
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
/// cannot represent a later `trailer_handle()` mutation. Direct dictionaries
/// and arrays are serialized through the canonical writer boundary so nested
/// dictionary nulls are omitted and array null positions remain present.
pub(crate) fn canonical_trailer_entries(
    pdf: &mut Pdf<impl Read + Seek>,
    map: &HashMap<ObjectRef, ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    canonical_trailer_entries_with_visibility(pdf, map, removed_refs, true)
}

/// Snapshot trailer entries while preserving qpdf's mode-dependent top-level
/// null visibility. The ordinary plain route suppresses null trailer values,
/// while the specialized encrypted writer passes `false`, matching
/// `QPDFWriter::writeTrailer` after `getTrimmedTrailer` (`QPDFWriter.cc:2009-
/// 2027, 2917-2926`).
pub(crate) fn canonical_trailer_entries_with_visibility(
    pdf: &mut Pdf<impl Read + Seek>,
    map: &HashMap<ObjectRef, ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    suppress_null_values: bool,
) -> crate::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let trailer = pdf.trailer_handle();
    let entries = trailer.try_as_dictionary()?.unwrap_or_default();
    let mut serialized = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if is_writer_owned_trailer_key(&key) {
            continue;
        }
        if value
            .object_ref()
            .is_some_and(|object_ref| object_ref.number == 0 || removed_refs.contains(&object_ref))
            || value.as_reference().is_some_and(|object_ref| {
                object_ref.number == 0 || removed_refs.contains(&object_ref)
            })
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
            value.unparse_object_with_ref_map_and_removed(
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

fn build_sources_from_pairs(
    pairs: impl IntoIterator<Item = (ObjectRef, ObjectRef)>,
) -> PlacementPlan {
    let pairs: Vec<(ObjectRef, ObjectRef)> = pairs.into_iter().collect();
    let old_to_new = pairs
        .iter()
        .map(|&(output, source)| (source, output))
        .collect();
    let objects = pairs
        .into_iter()
        .map(|(output, source)| PlannedIndirectObject::Source { source, output })
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
) -> crate::Result<ObjectStreamRenumber> {
    if preserve_unreferenced_objects {
        ObjectStreamRenumber::build_preserving_unreferenced(pdf, groups, true, removed_refs)
    } else {
        ObjectStreamRenumber::build(pdf, groups, true, removed_refs)
    }
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
            ObjectStreamGroup::SourceBacked { source, .. } => Some(*source),
            ObjectStreamGroup::Synthetic { .. } => None,
        })
        .collect();
    let mut objects: Vec<PlannedIndirectObject> = old_to_new
        .iter()
        .filter(|(source, _)| {
            !member_sources.contains(source) && !container_sources.contains(source)
        })
        .map(|(&source, &output)| PlannedIndirectObject::Source { source, output })
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

    fn source(source: u32, output: u32) -> PlannedIndirectObject {
        PlannedIndirectObject::Source {
            source: ObjectRef::new(source, 0),
            output: ObjectRef::new(output, 0),
        }
    }

    fn resolved_reference<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        object_ref: ObjectRef,
        target: ObjectRef,
    ) -> ObjectHandle {
        let handle = pdf.get_object_handle(object_ref);
        handle.set_resolved(ObjectValue::Reference(target));
        handle
    }

    fn plan_for_test(objects: Vec<PlannedIndirectObject>) -> PlainWritePlan {
        let root_source = ObjectRef::new(1, 0);
        let root_output = ObjectRef::new(1, 0);
        PlainWritePlan {
            version: "1.5".to_string(),
            objects,
            root: root_output,
            old_to_new: HashMap::from([(root_source, root_output)]),
            removed_refs: BTreeSet::new(),
            trailer: TrailerPlan {
                form: XrefForm::Table,
                canonical_entries: Vec::new(),
                root: root_output,
                id: IdPlan::Materialized { value: None },
                encrypt: None,
                structural_filtered: false,
            },
        }
    }

    #[test]
    fn validation_rejects_duplicate_output_numbers() {
        let mut plan = plan_for_test(vec![source(1, 1), source(2, 1)]);
        plan.root = ObjectRef::new(1, 0);
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
        bytes[5] = b'x';
        bytes[7] = b'y';
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .unwrap();
        assert_eq!(pdf.version(), "x.y");

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap();

        assert_eq!(plan.version, "x.y");
        assert_eq!(plan.trailer.form, XrefForm::Table);
        plan.validate().unwrap();
    }

    #[test]
    fn canonical_rewrite_uses_live_trailer_and_suppresses_nested_null_dictionary_entries() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("three-page.pdf")).unwrap(),
        ))
        .unwrap();
        pdf.delete_object(ObjectRef::new(999, 0));
        let zero = resolved_reference(&mut pdf, ObjectRef::new(100, 0), ObjectRef::new(0, 0));
        let removed = resolved_reference(&mut pdf, ObjectRef::new(101, 0), ObjectRef::new(999, 0));
        pdf.trailer_handle().replace_key(b"/Zero", zero).unwrap();
        pdf.trailer_handle()
            .replace_key(b"/Removed", removed)
            .unwrap();
        let missing = pdf.get_object_handle(ObjectRef::new(999, 0));
        let added = ObjectHandle::dictionary(vec![
            (b"Gone".to_vec(), missing),
            (
                b"Array".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::null()]),
            ),
        ]);
        pdf.trailer_handle().replace_key(b"/Added", added).unwrap();

        let output = {
            let mut writer = PdfWriter::new(&mut pdf);
            writer.set_object_stream_mode(ObjectStreamMode::Disable);
            writer.set_output_memory().unwrap();
            writer.write().unwrap();
            writer.get_buffer().unwrap()
        };
        let text = String::from_utf8_lossy(&output);

        assert!(text.contains("/Added << /Array [ null ] >>"));
        assert!(!text.contains("/Gone"));
        assert!(!text.contains("/Zero"));
        assert!(!text.contains("/Removed"));
    }

    #[test]
    fn canonical_trailer_entries_follow_qpdf_null_visibility() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("three-page.pdf")).unwrap(),
        ))
        .unwrap();
        let trailer = pdf.trailer_handle();
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
        pdf.trailer_handle()
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

        assert!(pdf.trailer().get("ID").is_some());
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
        pdf.trailer_handle()
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
        pdf.trailer_handle()
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
        pdf.trailer_handle()
            .replace_key(
                b"/Info",
                ObjectHandle::dictionary(vec![(
                    b"Title".to_vec(),
                    ObjectHandle::string(b"live-info-replacement-768".to_vec()),
                )]),
            )
            .unwrap();
        let mut options = write_options(ObjectStreamMode::Disable);
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
        let trailer = pdf.trailer_handle();
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
        layout
            .uncompressed
            .insert(plan.root.number, (plan.root.generation, 0));
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
    fn canonical_trailer_entries_reject_an_unmapped_nested_value() {
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path("three-page.pdf")).unwrap(),
        ))
        .unwrap();
        let trailer = pdf.trailer_handle();
        trailer.remove_key(b"/Info");
        let nested_ref =
            resolved_reference(&mut pdf, ObjectRef::new(1000, 0), ObjectRef::new(999, 0));
        trailer
            .replace_key(
                b"/Nested",
                ObjectHandle::dictionary(vec![(b"Bare".to_vec(), nested_ref)]),
            )
            .unwrap();

        let error =
            canonical_trailer_entries(&mut pdf, &HashMap::new(), &BTreeSet::new()).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("trailer nested reference 1000 0 R")
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
        plan.root = ObjectRef::new(2, 0);
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
    fn validation_rejects_invalid_version_for_xref_stream() {
        let mut plan = plan_for_test(vec![source(1, 1)]);
        plan.version = "invalid".to_string();
        plan.trailer.form = XrefForm::Stream;

        let err = plan.validate().unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("invalid PDF version invalid")));
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
        plan.trailer.root = ObjectRef::new(2, 0);
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("trailer root 2 0 R differs from plan root 1 0 R")));
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
    fn build_rejects_explicitly_deleted_root_before_placement() {
        let path = fixture_path("three-page.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let root = pdf.root_ref().unwrap();
        pdf.delete_object(root);

        let err =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("/Root absent from renumber map")));
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
    fn disable_explicit_deletion_is_excluded_before_placement() {
        let path = fixture_path("null-visible-matrix.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let deleted = ObjectRef::new(5, 0);
        pdf.delete_object(deleted);

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap();

        assert!(plan.removed_refs.contains(&deleted));
        assert!(!plan.old_to_new.contains_key(&deleted));
        assert!(plan.objects.iter().all(
            |object| matches!(object, PlannedIndirectObject::Source { source, .. }
                if *source != deleted)
        ));
    }

    #[test]
    fn generate_explicit_deletion_is_excluded_before_placement() {
        let path = fixture_path("null-visible-matrix.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let deleted = ObjectRef::new(5, 0);
        pdf.delete_object(deleted);

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Generate)).unwrap();

        assert!(plan.removed_refs.contains(&deleted));
        assert!(!plan.old_to_new.contains_key(&deleted));
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
        }));
    }

    #[test]
    fn preserve_explicit_deletion_is_removed_from_membership() {
        let path = fixture_path("three-page-objstm.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let deleted = ObjectRef::new(4, 0);
        pdf.delete_object(deleted);

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Preserve)).unwrap();

        assert!(plan.removed_refs.contains(&deleted));
        assert!(plan.objects.iter().all(|object| match object {
            PlannedIndirectObject::ObjectStream { members, .. } =>
                members.iter().all(|member| member.source != deleted),
            PlannedIndirectObject::Source { source, .. } => *source != deleted,
        }));
    }

    #[test]
    fn preserve_classic_fallback_excludes_explicit_deletion_before_placement() {
        let path = fixture_path("null-visible-matrix.pdf");
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let deleted = ObjectRef::new(5, 0);
        pdf.delete_object(deleted);

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Preserve)).unwrap();

        assert!(plan.removed_refs.contains(&deleted));
        assert!(!plan.old_to_new.contains_key(&deleted));
        assert!(plan.objects.iter().all(
            |object| matches!(object, PlannedIndirectObject::Source { source, .. }
                if *source != deleted)
        ));
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
        assert_eq!(
            containers,
            vec![
                (PlannedObjectStreamOrigin::Synthetic, 66),
                (PlannedObjectStreamOrigin::Synthetic, 66),
            ]
        );
        plan.validate().unwrap();
    }
}
