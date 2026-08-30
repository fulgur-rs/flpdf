//! qpdf correspondence: QPDF.cc getCompressibleObjGens and QPDFWriter.cc ObjStm planning and emission.

mod eligibility;
mod emission;
mod planning;

pub(crate) use eligibility::{
    collect_indirect_objstm_length_refs, compressible_objgens_qpdf_plan, eligibility_context,
    even_split_into_streams, get_compressible_objgens, is_eligible_for_objstm_handle,
    is_qpdf_signature_dict, EligibilityContext,
};
pub(crate) use emission::{
    emit_objstm_body_from_handles_with_writer, emit_objstm_body_from_handles_with_writer_qdf,
    wrap_objstm_body_as_handle, ObjStmBody,
};
pub use planning::ObjectStreamMode;
pub(crate) use planning::{
    filter_objstm_batches_for_output, plan_object_streams_with_reachability,
    plan_qpdf_preserve_object_streams_with_unreferenced, planner_config_from_options,
    ObjectStreamGroup, PlannerConfig,
};

// ── Tests ────────────────────────────────────────────────────────────────────
