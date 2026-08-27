//! qpdf correspondence: no shared reference-to-reference chain primitive.
//!
//! qpdf resolves an indirect `QPDFObjectHandle` through its own canonical
//! cache and does not expose a stored reference value for consumers to chase.
//! The page-selection consumers therefore use live handles directly. This
//! bound remains temporarily for the reader-owned legacy bridge and is removed
//! with that bridge in the reader cleanup slice.

/// Temporary depth bound for the reader cleanup slice.
pub(crate) const MAX_REF_CHAIN_DEPTH: usize = 64;
