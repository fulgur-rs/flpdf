//! Object-stream planning and emission.
//!
//! qpdf correspondence: QPDF.cc getCompressibleObjGens and QPDFWriter.cc ObjStm planning and emission.
//!

mod eligibility;
mod emission;
mod planning;

pub(crate) use eligibility::{
    compressible_objgens_qpdf_plan, eligibility_context, even_split_into_streams,
    get_compressible_objgens, is_eligible_for_objstm_handle, is_qpdf_signature_dict,
    CompressiblePlan, EligibilityContext,
};
#[cfg(test)]
pub(crate) use eligibility::{compressible_plan_call_count, reset_compressible_plan_call_count};
pub(crate) use emission::{
    emit_objstm_body_from_handles_with_sink, emit_objstm_body_from_handles_with_sink_qdf,
    emit_objstm_body_from_handles_with_writer, wrap_objstm_body_as_handle, ObjStmBody,
};
#[cfg(test)]
pub(crate) use planning::sort_source_backed_members_qpdf_order;
pub use planning::ObjectStreamMode;
pub(crate) use planning::{
    filter_objstm_batches_for_output, filter_preserve_object_stream_plan_for_output,
    plan_qpdf_preserve_object_streams_with_source_membership,
    plan_qpdf_preserve_object_streams_with_unreferenced, planner_config_from_options,
    ObjectStreamGroup, ObjectStreamPlan, PlannerConfig,
};
// ── Tests ────────────────────────────────────────────────────────────────────
