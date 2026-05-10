//! Linearization support — data model and planning structures.
//!
//! This module implements the planning layer for producing PDF linearized output
//! (ISO 32000-1 Annex F / "fast web view").  It intentionally contains **no I/O**:
//! the types here are pure data that downstream writer subtasks consume.

pub mod hint_page;
pub mod part1;
pub mod plan;
pub mod renumber;

pub use hint_page::{bits_needed, PageOffsetEntry, PageOffsetHeader, PageOffsetHintTable};
pub use part1::{Part1Bytes, Part1Placeholders, PLACEHOLDER_WIDTH};
pub use plan::{LinearizationPlan, PageHintEntry, SharedObjectHintEntry};
pub use renumber::RenumberMap;
