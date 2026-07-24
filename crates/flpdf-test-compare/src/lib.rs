// Public modules the binary re-uses. Additional modules land in later tasks.
pub mod clean;
pub mod compare;
pub mod orchestrator;
pub mod output;

pub use orchestrator::compare_files;
