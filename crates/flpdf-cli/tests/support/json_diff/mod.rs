//! JSON tree diff + allowlist + report machinery for the qpdf JSON schema-diff
//! corpus test (flpdf-9hc.11.14).
//!
//! Public surface:
//! - [`Divergence`] — one path-level mismatch between qpdf and flpdf JSON.
//! - [`diff_values`] — recursive strict-equality tree diff.
//! - [`Allowlist`] — load + match + stale-entry detection.
//! - [`Report`] — fixture × top-level-key matrix + markdown/json writers.
//!
//! See `docs/plans/2026-05-27-flpdf-9hc-11-14-json-schema-diff.md` for the full
//! plan.

use serde_json::Value;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct Divergence {
    pub path: String,
    pub qpdf: Value,
    pub flpdf: Value,
}
