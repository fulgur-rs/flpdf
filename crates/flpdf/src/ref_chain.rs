//! Terminal normalization of indirect-reference chains.
//!
//! A PDF value reached by indirection may be stored behind *more than one*
//! indirect hop (`a 0 R → b 0 R → value`) — a "holder chain". Any code that
//! matches, resolves, or rewrites a reference must follow the chain to its
//! terminal, or a doubled-indirect reference is silently mishandled (a `/Kids`
//! node dropped, an array carrier treated as empty, a copy-map lookup missed).
//! This module owns that one bounded follow-the-chain primitive so every
//! consumer shares a single implementation.

use crate::{Object, ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

/// Maximum indirect-reference hops [`resolve_ref_chain`] follows before stopping.
/// A cyclic or maliciously deep chain terminates at this bound rather than
/// looping forever, preserving the no-panic core guarantee on hostile input.
pub(crate) const MAX_REF_CHAIN_DEPTH: usize = 64;

/// Follow a chain of [`Object::Reference`] indirections up to
/// [`MAX_REF_CHAIN_DEPTH`], returning the terminal non-reference object and the
/// last [`ObjectRef`] traversed (for in-place rewrite of, or copy-map matching
/// against, an indirect target). A cyclic / over-deep chain terminates at the
/// bound and yields the last resolved value, so a hostile target cannot loop
/// forever.
pub(crate) fn resolve_ref_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: &Object,
) -> Result<(Object, Option<ObjectRef>)> {
    // `start` is a reference in the hot path; resolve it directly rather than
    // cloning the object first. A non-reference start has no chain to follow and
    // is returned as-is — the one unavoidable clone, since it is the return value.
    let Object::Reference(first) = start else {
        return Ok((start.clone(), None));
    };
    let mut last_ref = Some(*first);
    let mut cur = pdf.resolve(*first)?;
    // First hop taken above; follow the remaining hops up to the bound.
    for _ in 1..MAX_REF_CHAIN_DEPTH {
        match cur {
            Object::Reference(r) => {
                last_ref = Some(r);
                cur = pdf.resolve(r)?;
            }
            _ => break,
        }
    }
    Ok((cur, last_ref))
}
