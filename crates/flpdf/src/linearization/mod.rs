//! Linearization support — data model and planning structures.
//!
//! This module implements the planning layer for producing PDF linearized output
//! (ISO 32000-1 Annex F / "fast web view").  It intentionally contains **no I/O**:
//! the types here are pure data that downstream writer subtasks consume.

pub mod back_patch;
pub mod check;
pub mod hint_page;
pub mod hint_shared;
pub mod hint_stream;
pub mod part1;
pub mod plan;
pub mod renumber;
pub mod show;
pub mod writer;

// The qpdf-faithful cross-reference *stream* encoder used by the linearized
// ObjStm writer (compressed `/Predictor 12` `/W [1 2 1]` streams in qpdf's fixed
// key order, with qpdf's two-pass writePad region sizing). Also reused by the
// non-linearized `--object-streams=generate` writer (`writer::write_pdf_generate`).
pub(crate) mod xref_stream;

pub use back_patch::back_patch_param_dict;
pub use check::{
    check_linearization, check_linearization_bytes, check_linearization_path, CheckResult,
    LinearizationCheckError,
};
pub use hint_page::{bits_needed, PageOffsetEntry, PageOffsetHeader, PageOffsetHintTable};
pub use hint_shared::{
    SharedGroupEntry, SharedObjectEntry, SharedObjectHeader, SharedObjectHintTable,
};
pub use hint_stream::{encode_hint_stream, HintStreamBuilder, HintStreamBytes};
pub use part1::{Part1Bytes, Part1Placeholders, PLACEHOLDER_WIDTH};
pub use plan::{LinearizationPlan, PageHintEntry, SharedObjectHintEntry};
pub use renumber::{ObjStmRelocation, RenumberMap};
pub use show::{show_linearization_bytes, show_linearization_path, ShowLinearizationError};
pub use writer::{write_linearized, LinearizedDocument, LinearizedOffsets};
