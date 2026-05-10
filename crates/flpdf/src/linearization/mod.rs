//! Linearization support — data model and planning structures.
//!
//! This module implements the planning layer for producing PDF linearized output
//! (ISO 32000-1 Annex F / "fast web view").  It intentionally contains **no I/O**:
//! the types here are pure data that downstream writer subtasks consume.

pub mod back_patch;
pub mod hint_page;
pub mod hint_shared;
pub mod hint_stream;
pub mod part1;
pub mod plan;
pub mod renumber;
pub mod writer;

pub use back_patch::back_patch_param_dict;
pub use hint_page::{bits_needed, PageOffsetEntry, PageOffsetHeader, PageOffsetHintTable};
pub use hint_shared::{
    SharedGroupEntry, SharedObjectEntry, SharedObjectHeader, SharedObjectHintTable,
};
pub use hint_stream::{encode_hint_stream, HintStreamBuilder, HintStreamBytes};
pub use part1::{Part1Bytes, Part1Placeholders, PLACEHOLDER_WIDTH};
pub use plan::{LinearizationPlan, PageHintEntry, SharedObjectHintEntry};
pub use renumber::RenumberMap;
pub use writer::{write_linearized, LinearizedDocument, LinearizedOffsets};
