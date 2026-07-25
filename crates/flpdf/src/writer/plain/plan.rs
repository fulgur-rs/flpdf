//! Logical object placements for the qpdf-shaped plain writer pipeline.

// Production routing moves to this plan in a later layer. Layer 3 builds and
// validates the plan in shadow tests without replacing the legacy emitters.
#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Seek};

use crate::rewrite_renumber::{CatalogFirstRenumber, GenerateRenumber, NewNumberLookup};
use crate::writer::object_streams::{self, ObjectStreamMode};
use crate::writer::plain::xref::{CompressedLocation, IdPlan, TrailerPlan};
use crate::{CompressStreams, Object, ObjectRef, Pdf, WriteOptions, XrefForm, XrefOffset};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedMember {
    pub(crate) source: ObjectRef,
    pub(crate) output: ObjectRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedIndirectObject {
    Source {
        source: ObjectRef,
        output: ObjectRef,
    },
    ObjectStream {
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
    pub(crate) fn build<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        options: &WriteOptions,
    ) -> crate::Result<Self> {
        let source_root = pdf.root_ref().ok_or(crate::Error::Missing("/Root"))?;
        let source_had_compressed_objects = source_has_compressed_entries(pdf);

        let placement = match options.object_streams {
            ObjectStreamMode::Disable => {
                let renumber = CatalogFirstRenumber::build_qpdf(pdf, true)?;
                build_sources_from_catalog_first(renumber)
            }
            ObjectStreamMode::Preserve => {
                let packing = object_streams::plan_qpdf_preserve_object_streams(pdf)?;
                if packing.batches.is_empty() && !source_had_compressed_objects {
                    let renumber = CatalogFirstRenumber::build_qpdf(pdf, true)?;
                    build_sources_from_catalog_first(renumber)
                } else {
                    let renumber = GenerateRenumber::build(
                        pdf,
                        &packing.batches,
                        true,
                        &packing.removed_refs,
                    )?; // cov:ignore: qpdf packing and GenerateRenumber share the same validated inputs
                    build_container_aware(renumber, packing.batches, packing.removed_refs)?
                }
            }
            ObjectStreamMode::Generate => {
                let compressible = object_streams::compressible_objgens_qpdf_plan(pdf)?;
                let groups = object_streams::even_split_into_streams(&compressible.eligible);
                let renumber =
                    GenerateRenumber::build(pdf, &groups, true, &compressible.removed_refs)?;
                build_container_aware(renumber, groups, compressible.removed_refs)?
            }
        };

        let root = placement
            .old_to_new
            .get(&source_root)
            .copied()
            // cov:ignore-start: both renumberers seed traversal from the source root
            .ok_or_else(|| {
                crate::Error::Unsupported(
                    "plain writer plan: /Root absent from renumber map".to_string(),
                )
            })?; // cov:ignore-end
        let has_object_stream = placement
            .objects
            .iter()
            .any(|object| matches!(object, PlannedIndirectObject::ObjectStream { .. }));

        let form = if crate::writer::force_version_below_1_5(options) {
            XrefForm::Table
        } else if options.object_streams == ObjectStreamMode::Generate
            || !placement.groups.is_empty()
        {
            XrefForm::Stream
        } else if options.object_streams == ObjectStreamMode::Preserve
            && source_had_compressed_objects
        {
            XrefForm::Table // cov:ignore: requires a compressed source whose qpdf packing is intentionally empty
        } else {
            pdf.last_xref_form()
        };
        let version = crate::writer::effective_pdf_version(
            pdf.version(),
            options,
            false,
            has_object_stream || form == XrefForm::Stream,
        )
        .to_string();

        let mut dictionary = pdf.trailer().clone();
        crate::writer::strip_incremental_trailer_keys(&mut dictionary);
        crate::writer::remap_qpdf_trailer_refs_with_removed(
            pdf,
            &mut dictionary,
            &placement.old_to_new,
            &placement.removed_refs,
        )?; // cov:ignore: remap failure requires a malformed trailer rejected before plain planning
        dictionary.insert("Root", Object::Reference(root));
        crate::writer::apply_encrypt_trailer_entries(
            &mut dictionary,
            pdf,
            options,
            None,
            options.deterministic_id,
        );
        let id = if options.deterministic_id {
            IdPlan::Deterministic {
                source_id0: crate::writer::source_permanent_id(pdf.trailer()),
                info_suffix: crate::writer::deterministic_id_info_suffix(pdf),
            }
        } else {
            IdPlan::Materialized
        };
        let structural_filtered = matches!(
            crate::writer::effective_stream_policy(options),
            Some(CompressStreams::Yes)
        );
        let trailer = TrailerPlan {
            form,
            dictionary,
            root,
            id,
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
        let mut has_object_stream = false;

        for object in &self.objects {
            match object {
                PlannedIndirectObject::Source { source, output } => {
                    require_unique_output(&mut outputs, *output)?;
                    require_unique_source(&mut sources, *source)?;
                    require_matching_mapping(&self.old_to_new, *source, *output)?;
                }
                PlannedIndirectObject::ObjectStream { output, members } => {
                    has_object_stream = true;
                    require_unique_output(&mut outputs, *output)?;
                    for member in members {
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
            let version = crate::writer::parse_pdf_version(&self.version).ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain writer plan: invalid PDF version {}",
                    self.version
                ))
            })?;
            if version < (1, 5) {
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

    pub(crate) fn compressed_location(&self, output: ObjectRef) -> Option<CompressedLocation> {
        self.objects.iter().find_map(|object| match object {
            PlannedIndirectObject::ObjectStream {
                output: container,
                members,
            } => members
                .iter()
                .position(|member| member.output == output)
                .and_then(|index| {
                    u32::try_from(index).ok().map(|index| CompressedLocation {
                        container: container.number,
                        index,
                    })
                }),
            PlannedIndirectObject::Source { .. } => None,
        })
    }
}

struct PlacementPlan {
    objects: Vec<PlannedIndirectObject>,
    old_to_new: HashMap<ObjectRef, ObjectRef>,
    groups: Vec<Vec<ObjectRef>>,
    removed_refs: BTreeSet<ObjectRef>,
}

fn build_sources_from_catalog_first(renumber: CatalogFirstRenumber) -> PlacementPlan {
    let pairs: Vec<(ObjectRef, ObjectRef)> = renumber.pairs().collect();
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
        groups: Vec::new(),
        removed_refs: BTreeSet::new(),
    }
}

fn build_container_aware(
    renumber: GenerateRenumber,
    groups: Vec<Vec<ObjectRef>>,
    removed_refs: BTreeSet<ObjectRef>,
) -> crate::Result<PlacementPlan> {
    let old_to_new: HashMap<ObjectRef, ObjectRef> = renumber
        .pairs()
        .map(|(output, source)| (source, output))
        .collect();
    let member_sources: BTreeSet<ObjectRef> = groups.iter().flatten().copied().collect();
    let mut objects: Vec<PlannedIndirectObject> = old_to_new
        .iter()
        .filter(|(source, _)| !member_sources.contains(source))
        .map(|(&source, &output)| PlannedIndirectObject::Source { source, output })
        .collect();

    for (group_index, group) in groups.iter().enumerate() {
        // cov:ignore-start: GenerateRenumber assigns a container for every supplied group
        let container = renumber.container_number(group_index).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "plain writer plan: ObjStm group {group_index} was never reached"
            ))
        })?;
        // cov:ignore-end
        let mut members: Vec<PlannedMember> = group
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
        objects.push(PlannedIndirectObject::ObjectStream {
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
        groups,
        removed_refs,
    })
}

fn source_has_compressed_entries<R: Read + Seek>(pdf: &Pdf<R>) -> bool {
    pdf.source_xref_entries()
        .values()
        .any(|offset| matches!(offset, XrefOffset::Compressed { .. }))
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
    use crate::writer::plain::xref::{IdPlan, TrailerPlan};
    use crate::{
        Dictionary, NewlineBeforeEndstream, ObjectRef, Pdf, WriteOptions, XrefForm, XrefOffset,
    };

    fn fixture_path(fixture: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(fixture)
    }

    fn write_options(mode: ObjectStreamMode) -> WriteOptions {
        WriteOptions {
            full_rewrite: true,
            object_streams: mode,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriteOptions::default()
        }
    }

    fn build(fixture: &str, mode: ObjectStreamMode) -> PlainWritePlan {
        let path = fixture_path(fixture);
        let mut pdf =
            Pdf::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let options = write_options(mode);
        PlainWritePlan::build(&mut pdf, &options).unwrap()
    }

    fn assert_plan_matches_legacy_xref(fixture: &str, mode: ObjectStreamMode) {
        let plan = build(fixture, mode);

        let mut source = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(fixture_path(fixture)).unwrap(),
        ))
        .unwrap();
        let mut output = Vec::new();
        crate::write_pdf_with_options(&mut source, &mut output, &write_options(mode)).unwrap();

        let legacy = Pdf::open(std::io::Cursor::new(output)).unwrap();
        assert_eq!(legacy.root_ref(), Some(plan.root));
        assert_eq!(legacy.version(), plan.version);
        assert_eq!(legacy.last_xref_form(), plan.trailer.form);
        let entries = legacy.source_xref_entries();
        let mut planned_active: BTreeSet<u32> = plan
            .objects
            .iter()
            .flat_map(|object| match object {
                PlannedIndirectObject::Source { output, .. } => vec![output.number],
                PlannedIndirectObject::ObjectStream { output, members } => {
                    std::iter::once(output.number)
                        .chain(members.iter().map(|member| member.output.number))
                        .collect()
                }
            })
            .collect();
        if plan.trailer.form == XrefForm::Stream {
            let xref_number = planned_active
                .last()
                .copied()
                .unwrap_or(0)
                .checked_add(1)
                .unwrap();
            planned_active.insert(xref_number);
        }
        let legacy_active: BTreeSet<u32> = entries
            .iter()
            .filter_map(|(reference, offset)| {
                matches!(
                    offset,
                    XrefOffset::Offset(_) | XrefOffset::Compressed { .. }
                )
                .then_some(reference.number)
            })
            .collect();
        assert_eq!(legacy_active, planned_active);

        for object in &plan.objects {
            match object {
                PlannedIndirectObject::Source { output, .. } => {
                    assert!(
                        matches!(entries.get(output), Some(XrefOffset::Offset(_))),
                        "planned source {output} is not uncompressed in legacy output"
                    );
                }
                PlannedIndirectObject::ObjectStream { output, members } => {
                    assert!(
                        matches!(entries.get(output), Some(XrefOffset::Offset(_))),
                        "planned ObjStm {output} is not uncompressed in legacy output"
                    );
                    for (index, member) in members.iter().enumerate() {
                        let index = u32::try_from(index).unwrap();
                        assert_eq!(
                            plan.compressed_location(member.output),
                            Some(CompressedLocation {
                                container: output.number,
                                index,
                            })
                        );
                        assert_eq!(
                            entries.get(&member.output),
                            Some(&XrefOffset::Compressed {
                                stream: output.number,
                                index,
                            }),
                            "planned member {} has a different legacy location",
                            member.output
                        );
                    }
                }
            }
        }
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
            objects,
            root: root_output,
            old_to_new: HashMap::from([(root_source, root_output)]),
            removed_refs: BTreeSet::new(),
            trailer: TrailerPlan {
                form: XrefForm::Table,
                dictionary: Dictionary::new(),
                root: root_output,
                id: IdPlan::Materialized,
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
    fn validation_rejects_objstm_output_with_nonzero_generation() {
        let member = PlannedMember {
            source: ObjectRef::new(7, 1),
            output: ObjectRef::new(2, 1),
        };
        let plan = plan_for_test(vec![PlannedIndirectObject::ObjectStream {
            output: ObjectRef::new(1, 0),
            members: vec![member],
        }]);
        let err = plan.validate().unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(ref message)
            if message.contains("output member 2 1 R")));
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
    fn build_floors_xref_stream_output_to_pdf_1_5() {
        let mut bytes = std::fs::read(fixture_path("three-page-objstm.pdf")).unwrap();
        bytes[7] = b'4';
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

        let plan =
            PlainWritePlan::build(&mut pdf, &write_options(ObjectStreamMode::Disable)).unwrap();

        assert_eq!(plan.version, "1.5");
        assert_eq!(plan.trailer.form, XrefForm::Stream);
        assert!(plan
            .objects
            .iter()
            .all(|object| matches!(object, PlannedIndirectObject::Source { .. })));
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
        assert_eq!(plan.compressed_location(ObjectRef::new(1, 0)), None);
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
    fn preserve_plan_keeps_source_objstm_members_together() {
        let plan = build("three-page-objstm.pdf", ObjectStreamMode::Preserve);
        assert!(plan.objects.iter().any(
            |object| matches!(object, PlannedIndirectObject::ObjectStream { members, .. }
                if !members.is_empty())
        ));
        assert_eq!(plan.trailer.form, XrefForm::Stream);
        plan.validate().unwrap();
    }

    #[test]
    fn generate_plan_even_splits_132_eligible_objects() {
        let plan = build("objstm-gen-nostream-130rev.pdf", ObjectStreamMode::Generate);
        let sizes: Vec<usize> = plan
            .objects
            .iter()
            .filter_map(|object| match object {
                PlannedIndirectObject::ObjectStream { members, .. } => Some(members.len()),
                _ => None, // cov:ignore: this fixture deliberately packs every planned source
            })
            .collect();
        assert_eq!(sizes, vec![66, 66]);
        plan.validate().unwrap();
    }

    #[test]
    fn disable_plan_matches_legacy_xref() {
        assert_plan_matches_legacy_xref("three-page.pdf", ObjectStreamMode::Disable);
    }

    #[test]
    fn preserve_plan_matches_legacy_xref() {
        assert_plan_matches_legacy_xref("three-page-objstm.pdf", ObjectStreamMode::Preserve);
    }

    #[test]
    fn generate_plan_matches_legacy_xref() {
        assert_plan_matches_legacy_xref(
            "objstm-gen-nostream-130rev.pdf",
            ObjectStreamMode::Generate,
        );
    }
}
