//! qpdf correspondence: QPDFWriter.cc object-stream planning and source-container preservation.
//! The planner chooses generated batches, reconstructs source-backed Preserve
//! groups, and applies writer reachability and output-placement policies.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::num::NonZeroUsize;

use super::eligibility::{
    compressible_objgens_qpdf_plan, eligibility_context, even_split_into_streams_with_cap,
    is_eligible_for_objstm_handle,
};
use crate::writer::WriterOptions;
use crate::ObjectRef;
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
    /// Source ObjStm identity for each batch. `None` denotes a generated
    /// batch; Preserve batches carry qpdf's source container ObjGen.
    pub source_containers: Vec<Option<ObjectRef>>,
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
    /// A legacy Generate group whose consumer has no source identity yet.
    Synthetic { members: Vec<ObjectRef> },
    /// A Generate group backed by qpdf's newly minted indirect null container.
    Generated {
        source: ObjectRef,
        members: Vec<ObjectRef>,
    },
}

impl ObjectStreamGroup {
    pub(crate) fn members(&self) -> &[ObjectRef] {
        match self {
            Self::SourceBacked { members, .. }
            | Self::Synthetic { members }
            | Self::Generated { members, .. } => members,
        }
    }

    pub(crate) fn members_mut(&mut self) -> &mut Vec<ObjectRef> {
        match self {
            Self::SourceBacked { members, .. }
            | Self::Synthetic { members } // cov:ignore: LLVM maps this shared pattern continuation to the generated arm counter.
            | Self::Generated { members, .. } => members,
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
    plan_object_streams_with_reachability_and_source_membership(pdf, config, reachable, None, None)
}

/// Variant of [`plan_object_streams_with_reachability`] that consumes the
/// source ObjStm membership captured during qpdf writer setup. qpdf records
/// that map before its later recovery/object-count walk
/// (`QPDFWriter.cc:2114-2140,2189-2195`); specialized live standard output
/// therefore passes the shared setup snapshot through this boundary instead
/// of silently re-reading a possibly changed xref view.
pub(crate) fn plan_object_streams_with_reachability_and_source_membership<
    R: std::io::Read + std::io::Seek,
>(
    pdf: &mut crate::Pdf<R>,
    config: &PlannerConfig,
    reachable: Option<&BTreeSet<ObjectRef>>,
    source_membership_snapshot: Option<&BTreeMap<u32, u32>>,
    generated_snapshot: Option<&super::CompressiblePlan>,
) -> crate::Result<PackingPlan> {
    if config.mode == ObjectStreamMode::Disable {
        return Ok(PackingPlan::default());
    }

    match config.mode {
        ObjectStreamMode::Disable => {
            unreachable!() // cov:ignore: the early Disable return makes this arm unreachable
        }
        ObjectStreamMode::Preserve => {
            let source_plan = if let Some(snapshot) = source_membership_snapshot {
                plan_qpdf_preserve_object_streams_with_source_membership(
                    pdf,
                    config.preserve_unreferenced_objects,
                    Some(snapshot),
                )? // cov:ignore: LLVM attributes the covered setup-snapshot planner continuation to the match arm.
            } else {
                plan_qpdf_preserve_object_streams_with_unreferenced(
                    pdf,
                    config.preserve_unreferenced_objects,
                )? // cov:ignore: LLVM attributes the covered live-membership fallback continuation to the match arm.
            }; // cov:ignore: LLVM attributes this covered multiline planner terminator to the call setup
            let mut batches = Vec::with_capacity(source_plan.groups.len());
            let mut source_containers = Vec::with_capacity(source_plan.groups.len());
            for group in source_plan.groups {
                match group {
                    ObjectStreamGroup::SourceBacked { source, members } => {
                        source_containers.push(Some(source));
                        batches.push(members);
                    }
                    ObjectStreamGroup::Synthetic { .. } | ObjectStreamGroup::Generated { .. } => {
                        // cov:ignore-start: the shared Preserve planner returns SourceBacked groups only.
                        return Err(crate::Error::Internal(
                            "Preserve planner returned a generated ObjStm group".to_string(),
                        )); // cov:ignore: defensive invariant rejects an impossible generated group
                            // cov:ignore-end
                    }
                }
            }
            Ok(PackingPlan {
                batches,
                source_containers,
                removed_refs: source_plan.removed_refs,
            })
        }
        ObjectStreamMode::Generate => {
            // Only Generate consumes the `/Length` exclusions, and only
            // QPDFWriter::generateObjectStreams calls getCompressibleObjGens
            // unconditionally. Preserve must not run the walk here: qpdf reads
            // the source membership map first and only then intersects it with
            // the compressible set (`QPDFWriter.cc:1941-1957`), an order the
            // shared Preserve planner already reproduces internally. Running
            // the walk up front inverted it, because the walk can drop stale
            // generations from the document xref that the membership map is
            // about to be read from.
            let length_exclusions = if let Some(snapshot) = generated_snapshot {
                snapshot.indirect_objstm_length_refs.clone()
            } else {
                compressible_objgens_qpdf_plan(pdf)?.indirect_objstm_length_refs
            };
            plan_generate(
                pdf,
                config,
                &length_exclusions,
                reachable,
                generated_snapshot,
            )
        }
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
    source_containers: &mut Vec<Option<ObjectRef>>,
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

    let mut retained_batches = Vec::with_capacity(batches.len());
    let mut retained_sources = Vec::with_capacity(source_containers.len());
    for (mut batch, source) in batches.drain(..).zip(source_containers.drain(..)) {
        batch.retain(|member| root != Some(*member) && !page_refs.contains(member));
        if !batch.is_empty() {
            retained_batches.push(batch);
            retained_sources.push(source);
        }
    }
    *batches = retained_batches;
    *source_containers = retained_sources;
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
    plan_qpdf_preserve_object_streams_with_source_membership(pdf, preserve_unreferenced, None)
}

/// Reuse the source ObjStm map captured during qpdf writer setup.
/// `QPDFWriter::preserveObjectStreams` runs before the later `getObjectCount`
/// xref-reconstruction walk (`QPDFWriter.cc:2114-2140,2189-2195`), so a
/// malformed document can expose a different current xref map by the time
/// body emission begins. Keep that setup-time ownership explicit rather than
/// silently consulting a later recovery result.
pub(crate) fn plan_qpdf_preserve_object_streams_with_source_membership<
    R: std::io::Read + std::io::Seek,
>(
    pdf: &mut crate::Pdf<R>,
    preserve_unreferenced: bool,
    source_membership_snapshot: Option<&BTreeMap<u32, u32>>,
) -> crate::Result<ObjectStreamPlan> {
    // QPDFWriter captures source membership before the compressible-object
    // walk, which can resolve/recover objects and update the document xref.
    let source_membership = source_membership_snapshot.cloned().unwrap_or_else(|| {
        let mut source_membership = BTreeMap::new();
        pdf.get_object_stream_data(&mut source_membership);
        source_membership
    });
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
            // qpdf's preserve path applies the compressible eligibility filter
            // only when `preserveUnreferencedObjects` is false
            // (`QPDFWriter.cc:1939-1967`). With the flag enabled, even a
            // malformed source stream member stays in its source container;
            // the writer later warns and substitutes null while emitting it
            // (`QPDFWriter.cc:1690-1705`).
            let retain = if preserve_unreferenced {
                true
            } else {
                let object = pdf.get_object_handle(member);
                is_eligible_for_objstm_handle(member, &object, &ctx)?
            };
            if retain {
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
/// Generate mode: follow qpdf's live compressible-object traversal and evenly
/// split it across the minimum number of object streams.
fn plan_generate<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::Pdf<R>,
    config: &PlannerConfig,
    length_exclusions: &BTreeSet<ObjectRef>,
    reachable: Option<&BTreeSet<ObjectRef>>,
    generated_snapshot: Option<&super::CompressiblePlan>,
) -> crate::Result<PackingPlan> {
    // QPDFWriter::generateObjectStreams obtains its candidates from
    // QPDF::getCompressibleObjGens (QPDFWriter.cc:1970-2004), not from the
    // object-number-sorted xref universe. That walk is the semantic source of
    // both member order and the set of reachable candidates.
    let mut compressible = if let Some(snapshot) = generated_snapshot {
        snapshot.clone()
    } else {
        compressible_objgens_qpdf_plan(pdf)?
    };
    compressible.eligible.retain(|member| {
        !length_exclusions.contains(member)
            && reachable.is_none_or(|reachable| reachable.contains(member))
    });
    // A fresh multi-source page target has new local ObjectRefs for both the
    // primary and foreign graphs. qpdf keeps primary objects in source-number
    // order while copyForeignObject assigns foreign objects in discovery
    // order; the merge records that provenance on Pdf so generated ObjStm
    // members can use the same order without changing ordinary documents.
    sort_compressible_for_writer_order(pdf, &mut compressible.eligible);
    let batches = even_split_into_streams_with_cap(&compressible.eligible, config.batch_size_cap);
    let source_containers = vec![None; batches.len()];

    Ok(PackingPlan {
        batches,
        source_containers,
        removed_refs: compressible.removed_refs,
    })
}

fn sort_compressible_for_writer_order<R: Read + Seek + 'static>(
    pdf: &crate::Pdf<R>,
    eligible: &mut [ObjectRef],
) {
    if pdf.writer_object_order.is_some() {
        eligible.sort_unstable_by_key(|object_ref| pdf.writer_object_order_key(*object_ref));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        plan_object_streams_with_reachability, sort_compressible_for_writer_order,
        ObjectStreamMode, PlannerConfig, DEFAULT_BATCH_SIZE_CAP,
    };
    use crate::pdf::WriterObjectOrderKey;
    use crate::{ObjectRef, Pdf};
    use std::collections::BTreeMap;
    use std::io::Cursor;

    #[test]
    fn planner_config_default_uses_qpdf_defaults() {
        let config = PlannerConfig::default();
        assert_eq!(config.mode, ObjectStreamMode::Preserve);
        assert_eq!(config.batch_size_cap, DEFAULT_BATCH_SIZE_CAP);
        assert!(!config.preserve_unreferenced_objects);
    }

    #[test]
    fn generated_merge_candidates_use_primary_then_foreign_writer_order() {
        let mut pdf = crate::Pdf::empty().expect("create merge target");
        let primary = ObjectRef::new(17, 0);
        let foreign = ObjectRef::new(3, 0);
        let mut order = BTreeMap::new();
        order.insert(primary, WriterObjectOrderKey::primary(ObjectRef::new(4, 0)));
        order.insert(foreign, WriterObjectOrderKey::foreign(ObjectRef::new(2, 0)));
        pdf.set_writer_object_order(order);

        let mut eligible = vec![foreign, primary];
        sort_compressible_for_writer_order(&pdf, &mut eligible);

        assert_eq!(eligible, [primary, foreign]);
    }

    #[test]
    fn preserve_plan_keeps_qpdf_source_container_identity() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../../../tests/fixtures/compat/three-page-objstm.pdf").to_vec(),
        ))
        .expect("open ObjStm fixture");
        let config = PlannerConfig::default();
        let plan = plan_object_streams_with_reachability(&mut pdf, &config, None)
            .expect("build Preserve plan");

        assert_eq!(
            plan.source_containers,
            vec![Some(ObjectRef::new(1, 0))],
            "specialized Preserve must carry the source ObjStm identity from qpdf's map"
        );
    }
}
