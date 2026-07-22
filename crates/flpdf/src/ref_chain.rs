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

/// Follow a chain of [`Object::Reference`] indirections from `start` up to
/// [`MAX_REF_CHAIN_DEPTH`], returning the last [`ObjectRef`] reached — the ref
/// that points directly at the terminal (non-reference) value.
///
/// Unlike [`resolve_ref_chain`], this never clones the terminal object: each hop
/// is resolved by borrow ([`Pdf::resolve_borrowed`]), so a caller that only needs
/// the terminal *ref* — to key a map, count sharing, or re-inspect the target by
/// borrow — pays no per-call allocation even when the target is a large
/// dictionary. A cyclic / over-deep chain terminates at the bound and returns the
/// last ref reached, so a hostile target cannot loop forever.
///
/// # Errors
///
/// Propagates any [`Error`](crate::Error) from resolving an object in the chain.
pub(crate) fn terminal_ref_of_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
) -> Result<ObjectRef> {
    let mut cur = start;
    for _ in 0..MAX_REF_CHAIN_DEPTH {
        match pdf.resolve_borrowed(cur)? {
            Object::Reference(next) => cur = *next,
            _ => break,
        }
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    /// Minimal PDF carrying a holder chain `4 0 R → 5 0 R → <<dict>>` and a
    /// two-node cycle `7 0 R → 8 0 R → 7 0 R`.
    fn build_chain_pdf() -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offs: BTreeMap<u32, u64> = BTreeMap::new();
        let objs: [(u32, &str); 8] = [
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "5 0 R"),
            (5, "6 0 R"),
            (6, "<< /X 1 >>"),
            (7, "8 0 R"),
            (8, "7 0 R"),
        ];
        for (n, s) in objs {
            offs.insert(n, out.len() as u64);
            out.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }
        let xref_start = out.len() as u64;
        let total = 9u32;
        out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for i in 1..total {
            out.extend_from_slice(format!("{:010} 00000 n \n", offs[&i]).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    fn open_chain_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let mut pdf = Pdf::open(Cursor::new(build_chain_pdf())).expect("parse");
        for (from, to) in [(4, 5), (5, 6), (7, 8), (8, 7)] {
            pdf.set_object(
                ObjectRef::new(from, 0),
                Object::Reference(ObjectRef::new(to, 0)),
            );
        }
        pdf
    }

    #[test]
    fn terminal_ref_of_chain_follows_multi_hop_to_terminal() {
        let mut pdf = open_chain_pdf();
        let terminal = terminal_ref_of_chain(&mut pdf, ObjectRef::new(4, 0)).expect("ok");
        assert_eq!(terminal, ObjectRef::new(6, 0));
    }

    #[test]
    fn terminal_ref_of_chain_single_hop_is_self() {
        let mut pdf = open_chain_pdf();
        // Object 6 is already a dictionary (not a reference), so its terminal ref
        // is itself.
        let terminal = terminal_ref_of_chain(&mut pdf, ObjectRef::new(6, 0)).expect("ok");
        assert_eq!(terminal, ObjectRef::new(6, 0));
    }

    #[test]
    fn terminal_ref_of_chain_cyclic_terminates_at_bound() {
        let mut pdf = open_chain_pdf();
        // 7 → 8 → 7 → … never reaches a non-reference; the depth bound must stop
        // it and return a ref from the cycle rather than looping forever.
        let terminal = terminal_ref_of_chain(&mut pdf, ObjectRef::new(7, 0)).expect("ok");
        assert!(
            terminal == ObjectRef::new(7, 0) || terminal == ObjectRef::new(8, 0),
            "cycle must terminate at a ref within the cycle, got {terminal}"
        );
    }
}
