//! qpdf correspondence: QPDFWriter.cc object-stream planning and source-container preservation.
//! The planner chooses generated batches, reconstructs source-backed Preserve
//! groups, and applies writer reachability and output-placement policies.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

use super::eligibility::{
    compressible_objgens_qpdf_plan, eligibility_context, even_split_into_streams_with_cap,
    is_eligible_for_objstm_handle, EligibilityContext,
};
use crate::writer::WriterOptions;
use crate::ObjectRef;
use crate::XrefEntry;
// ── Packing planner types ────────────────────────────────────────────────────

/// Controls how the ObjStm packing planner groups objects into batches.
///
/// Mirrors `qpdf --object-streams=preserve|disable|generate`. The default,
/// `Preserve`, matches qpdf's behaviour for a plain `qpdf in.pdf out.pdf`
/// invocation: ObjStms present in the input are reused; their membership is
/// not repartitioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectStreamMode {
    /// Keep the original ObjStm membership from the source document.
    #[default]
    Preserve,
    /// Emit no ObjStms; all eligible objects become plain indirects.
    Disable,
    /// Pack eligible objects into fresh ObjStms (greedy with cap).
    Generate,
}

/// qpdf's default ObjStm batch size cap.
pub(crate) const DEFAULT_BATCH_SIZE_CAP: NonZeroUsize = match NonZeroUsize::new(100) {
    Some(n) => n,
    None => unreachable!(),
};

/// Configuration for the ObjStm packing planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannerConfig {
    pub mode: ObjectStreamMode,
    /// Maximum number of members per ObjStm batch. qpdf default is 100.
    pub batch_size_cap: NonZeroUsize,
    /// Whether qpdf should retain source objects that are not reachable from
    /// the trailer/root graph. This is a Preserve-only policy; Generate still
    /// takes its members from `getCompressibleObjGens`.
    pub preserve_unreferenced_objects: bool,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            mode: ObjectStreamMode::Preserve,
            batch_size_cap: DEFAULT_BATCH_SIZE_CAP,
            preserve_unreferenced_objects: false,
        }
    }
}

/// The output of the packing planner: an ordered list of batches,
/// each of which will become one ObjStm in the output.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PackingPlan {
    /// Each inner `Vec` is one ObjStm batch, members in deterministic order.
    pub batches: Vec<Vec<ObjectRef>>,
    /// Exact stale generations removed by qpdf's compressible-object walk.
    ///
    /// Standard enqueue does not remove these references. Generate and
    /// source-ObjStm Preserve carry them into their dedicated serializer so
    /// array occurrences become inline null and dictionary values disappear.
    pub removed_refs: BTreeSet<ObjectRef>,
}

/// One object-stream group in the qpdf-shaped plain-writer plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjectStreamGroup {
    /// A Preserve group reconstructed from this exact source ObjStm.
    SourceBacked {
        source: ObjectRef,
        members: Vec<ObjectRef>,
    },
    /// A Generate group whose container has no source identity.
    Synthetic { members: Vec<ObjectRef> },
}

impl ObjectStreamGroup {
    pub(crate) fn members(&self) -> &[ObjectRef] {
        match self {
            Self::SourceBacked { members, .. } | Self::Synthetic { members } => members,
        }
    }

    pub(crate) fn members_mut(&mut self) -> &mut Vec<ObjectRef> {
        match self {
            Self::SourceBacked { members, .. } | Self::Synthetic { members } => members,
        }
    }
}

/// Source-aware object-stream plan for the qpdf-shaped plain writer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ObjectStreamPlan {
    pub(crate) groups: Vec<ObjectStreamGroup>,
    pub(crate) removed_refs: BTreeSet<ObjectRef>,
}

/// Convert public [`WriterOptions`] into an internal
/// [`PlannerConfig`].  The conversion is direct: `WriterOptions.object_streams`
/// names the policy, and the planner's batch cap defaults to qpdf's value of
/// 100.  Future writer-side knobs (e.g. an explicit cap override) would be
/// threaded through this conversion.
///
/// QDF changes stream formatting and normalization, but does not override
/// [`WriterOptions::object_streams`]. This matches qpdf's `setQDFMode` and
/// `setObjectStreamMode` setters, which remain independent until the writer's
/// setup dispatches the selected object-stream mode.
pub(crate) fn planner_config_from_options(options: &WriterOptions) -> PlannerConfig {
    PlannerConfig {
        mode: options.object_streams,
        batch_size_cap: DEFAULT_BATCH_SIZE_CAP,
        preserve_unreferenced_objects: options.preserve_unreferenced_objects,
    }
}

// ── Packing planner ──────────────────────────────────────────────────────────

/// Decide how many ObjStms to emit and which objects belong in each.
///
/// - `Disable`  → returns an empty plan (zero batches).
/// - `Preserve` → reconstructs the source document's ObjStm grouping,
///   skipping ineligible members without applying Generate's 100-member cap.
/// - `Generate` → follows qpdf's compressible-object traversal and evenly
///   splits the result across the minimum number of streams allowed by the
///   member cap.
///
/// Plan object streams with an optional qpdf-reachable candidate set.
///
/// The specialized writer uses this only for Generate combined with
/// `preserveUnreferencedObjects`: qpdf's `generateObjectStreams` always takes
/// its members from `getCompressibleObjGens` and never lets the preserve flag
/// expand that set (`QPDFWriter.cc:1970-2006`). All `/Length` exclusions come
/// from that same qpdf-shaped reachable walk; Preserve with
/// `preserveUnreferencedObjects` deliberately has no such intersection
/// (`QPDFWriter.cc:1939-1967`).
pub(crate) fn plan_object_streams_with_reachability<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
    config: &PlannerConfig,
    reachable: Option<&BTreeSet<ObjectRef>>,
) -> crate::Result<PackingPlan> {
    if config.mode == ObjectStreamMode::Disable {
        return Ok(PackingPlan::default());
    }

    let ctx = eligibility_context(pdf)?;
    let length_exclusions =
        if config.mode == ObjectStreamMode::Preserve && config.preserve_unreferenced_objects {
            // QPDFWriter::preserveObjectStreams keeps every source member when
            // preserveUnreferencedObjects is set; it does not run the
            // getCompressibleObjGens intersection in that branch.
            BTreeSet::new()
        } else {
            compressible_objgens_qpdf_plan(pdf)?.indirect_objstm_length_refs
        };

    match config.mode {
        ObjectStreamMode::Disable => {
            unreachable!() // cov:ignore: the early Disable return makes this arm unreachable
        }
        ObjectStreamMode::Preserve => plan_preserve(pdf, &ctx, &length_exclusions),
        ObjectStreamMode::Generate => plan_generate(pdf, config, &length_exclusions, reachable),
    }
}

/// Apply qpdf's output-mode ObjStm exclusions after membership planning.
///
/// This mirrors QPDFWriter.cc:2141-2160: linearized output removes page
/// dictionaries and the root Catalog; encrypted output removes the root
/// Catalog. The input document's linearization state is deliberately not
/// consulted here.
pub(crate) fn filter_objstm_batches_for_output<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
    batches: &mut Vec<Vec<ObjectRef>>,
    output_linearized: bool,
    output_encrypted: bool,
) -> crate::Result<()> {
    let root = (output_linearized || output_encrypted)
        .then(|| pdf.root_ref())
        .flatten();
    let page_refs: BTreeSet<ObjectRef> = if output_linearized {
        crate::pages::page_refs(pdf)?.into_iter().collect()
    } else {
        BTreeSet::new()
    };

    for batch in batches.iter_mut() {
        batch.retain(|member| root != Some(*member) && !page_refs.contains(member));
    }
    batches.retain(|batch| !batch.is_empty());
    Ok(())
}

/// Reconstruct Preserve-mode source containers after filtering their members
/// through qpdf's compressible-object walk.
///
/// qpdf's `preserveObjectStreams` intersects the source object-to-container map
/// with `getCompressibleObjGens`. Container membership and source member order
/// are retained, and Preserve never applies Generate's 100-member cap. The
/// traversal's operation-specific stale-generation removals are returned with
/// the batches for the dedicated serializer.
/// Reconstruct source ObjStm grouping with qpdf's
/// `preserveUnreferencedObjects` policy. qpdf keeps every source ObjStm
/// member in the source container map in this mode; reachability filtering is
/// applied only when the setting is disabled.
pub(crate) fn plan_qpdf_preserve_object_streams_with_unreferenced<
    R: std::io::Read + std::io::Seek,
>(
    pdf: &mut crate::Pdf<R>,
    preserve_unreferenced: bool,
) -> crate::Result<ObjectStreamPlan> {
    // QPDFWriter captures source membership before the compressible-object
    // walk, which can resolve/recover objects and update the document xref.
    let mut source_membership = BTreeMap::new();
    pdf.get_object_stream_data(&mut source_membership);
    if source_membership.is_empty() {
        return Ok(ObjectStreamPlan::default());
    }
    let compressible = (!preserve_unreferenced)
        .then(|| compressible_objgens_qpdf_plan(pdf))
        .transpose()?;
    let ctx = eligibility_context(pdf)?;
    let length_exclusions = compressible
        .as_ref()
        .map(|plan| plan.indirect_objstm_length_refs.clone())
        .unwrap_or_default();
    let eligible: BTreeSet<ObjectRef> = compressible
        .as_ref()
        .map(|plan| plan.eligible.iter().copied().collect())
        .unwrap_or_default();
    let mut by_container: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();

    for (member, stream) in source_membership {
        by_container
            .entry(ObjectRef::new(stream, 0))
            .or_default()
            .push(ObjectRef::new(member, 0));
    }

    let mut groups = Vec::new();
    for (source, members) in by_container {
        let mut retained = Vec::new();
        for member in members {
            if length_exclusions.contains(&member)
                || (!preserve_unreferenced && !eligible.contains(&member))
            {
                continue;
            }
            let eligible_for_objstm = {
                let object = pdf.get_object_handle(member);
                is_eligible_for_objstm_handle(member, &object, &ctx)?
            };
            if eligible_for_objstm {
                retained.push(member);
            }
        }
        retained.sort_unstable_by_key(|member| (member.number, member.generation));
        if !retained.is_empty() {
            groups.push(ObjectStreamGroup::SourceBacked {
                source,
                members: retained,
            });
        }
    }

    Ok(ObjectStreamPlan {
        groups,
        removed_refs: compressible
            .map(|plan| plan.removed_refs)
            .unwrap_or_default(),
    })
}

/// Eligible objects in qpdf's `QPDF::getCompressibleObjGens` order
/// (libqpdf/QPDF.cc:2392): a depth-first walk from the trailer, descending into
/// dictionary values in ascending key order and array items in order. This
/// traversal order — not object-number order — decides which objects co-locate
/// in a generated object stream when more than one container is needed, so the
/// port must reproduce it exactly.
///
/// Returns each reachable indirect object's reference in first-visit order.
/// qpdf hides dictionary entries whose values resolve to null, but retains
/// indirect identities reached from arrays even when they are missing, free,
/// or real-null objects.
/// Preserve mode: reconstruct source ObjStm grouping.
fn plan_preserve<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
    ctx: &EligibilityContext,
    length_exclusions: &BTreeSet<ObjectRef>,
) -> crate::Result<PackingPlan> {
    let entries = pdf.source_xref_entries();

    // Group members by (container_number, index) so we can reconstruct order.
    // Key: container object number; Value: list of (index, ObjectRef).
    let mut groups: BTreeMap<u32, Vec<(u32, ObjectRef)>> = BTreeMap::new();

    for (obj_ref, offset) in &entries {
        if let XrefEntry::Compressed { stream, index } = offset {
            groups.entry(*stream).or_default().push((*index, *obj_ref));
        }
    }

    let mut batches: Vec<Vec<ObjectRef>> = Vec::new();

    // Iterate containers in ascending container-number order.
    for (_container_num, mut members) in groups {
        // Sort by index within the container to get deterministic order.
        members.sort_by_key(|(idx, _)| *idx);

        // Filter ineligible members.
        let mut eligible: Vec<ObjectRef> = Vec::new();
        for (_idx, obj_ref) in members {
            if length_exclusions.contains(&obj_ref) {
                continue;
            }
            let eligible_for_objstm = {
                let obj = pdf.get_object_handle(obj_ref);
                is_eligible_for_objstm_handle(obj_ref, &obj, ctx)?
            };
            if !eligible_for_objstm {
                continue;
            }
            eligible.push(obj_ref);
        }

        if !eligible.is_empty() {
            batches.push(eligible);
        }
    }

    Ok(PackingPlan {
        batches,
        removed_refs: BTreeSet::new(),
    })
}

/// Generate mode: follow qpdf's live compressible-object traversal and evenly
/// split it across the minimum number of object streams.
fn plan_generate<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
    config: &PlannerConfig,
    length_exclusions: &BTreeSet<ObjectRef>,
    reachable: Option<&BTreeSet<ObjectRef>>,
) -> crate::Result<PackingPlan> {
    // QPDFWriter::generateObjectStreams obtains its candidates from
    // QPDF::getCompressibleObjGens (QPDFWriter.cc:1970-2004), not from the
    // object-number-sorted xref universe. That walk is the semantic source of
    // both member order and the set of reachable candidates.
    let mut compressible = compressible_objgens_qpdf_plan(pdf)?;
    compressible.eligible.retain(|member| {
        !length_exclusions.contains(member)
            && reachable.is_none_or(|reachable| reachable.contains(member))
    });
    let batches = even_split_into_streams_with_cap(&compressible.eligible, config.batch_size_cap);

    Ok(PackingPlan {
        batches,
        removed_refs: compressible.removed_refs,
    })
}

#[cfg(test)]
mod tests {
    use super::{ObjectStreamMode, PlannerConfig, DEFAULT_BATCH_SIZE_CAP};

    #[test]
    fn planner_config_default_uses_qpdf_defaults() {
        let config = PlannerConfig::default();
        assert_eq!(config.mode, ObjectStreamMode::Preserve);
        assert_eq!(config.batch_size_cap, DEFAULT_BATCH_SIZE_CAP);
        assert!(!config.preserve_unreferenced_objects);
    }
}
