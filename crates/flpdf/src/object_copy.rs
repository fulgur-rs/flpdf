//! Cross-document deep object copier (renumber + cycle handling).
//!
//! [`copy_objects`] copies a pre-closed set of source [`ObjectRef`]s into a
//! target [`Pdf`], assigning fresh object numbers and returning the
//! source→target renumber map.  It is the building block beneath single-page
//! extract and multi-document merge: callers first compute the curated object
//! set (e.g. via [`page_object_closure`](crate::page_closure::page_object_closure))
//! and hand it to the copier.
//!
//! # Boundary semantics
//!
//! The provided `refs` set is treated as **both the work-list and the
//! boundary**.  The copier does not re-traverse the graph to discover new
//! objects, so it never follows `/Parent` up the page tree or pulls in sibling
//! pages.  A reference inside a copied object that points *outside* `refs`
//! (e.g. a cross-page link's sibling-page `/Contents`) is replaced with
//! [`Object::Null`]; repairing link semantics is a higher layer's job.
//!
//! # Cycle handling
//!
//! Because the full set is known up front, every target number is allocated
//! *before* any reference is rewritten.  Cycles (A→B→A) therefore need no
//! special bookkeeping: both endpoints already have target numbers when their
//! references are remapped.
//!
//! # Independence
//!
//! Each call uses a fresh map, so copying the same source set twice produces
//! independent, non-shared target copies.

use crate::object::{Dictionary, Stream};
use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Copy the pre-closed object set `refs` from `source` into `target`, assigning
/// fresh target object numbers, and return the source→target renumber map.
///
/// References inside copied objects are rewritten: those landing in `refs` are
/// remapped to their new target number, while references outside `refs` are
/// replaced with [`Object::Null`].  Stream byte payloads are copied verbatim.
///
/// # Errors
///
/// Returns [`Err`] if [`Pdf::resolve`] fails for any ref in `refs` (e.g. a
/// corrupt or missing xref entry).
///
/// # Examples
///
/// ```no_run
/// use std::collections::BTreeSet;
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{copy_objects, page_closure, pages, Pdf};
///
/// let mut source = Pdf::open(BufReader::new(File::open("source.pdf")?))?;
/// let mut target = Pdf::open(BufReader::new(File::open("target.pdf")?))?;
/// let page_ref = pages::page_refs(&mut source)?[0];
/// let closure = page_closure::page_object_closure(&mut source, page_ref)?;
/// let renumber = copy_objects(&mut source, &mut target, &closure)?;
/// println!("copied {} objects", renumber.len());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn copy_objects<RS: Read + Seek, RT: Read + Seek>(
    source: &mut Pdf<RS>,
    target: &mut Pdf<RT>,
    refs: &BTreeSet<ObjectRef>,
) -> Result<BTreeMap<ObjectRef, ObjectRef>> {
    // Next free target object number: one past the current maximum.
    let mut next = target
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        + 1;

    // Pre-allocate a fresh target number for every ref in the set, iterating in
    // sorted order (BTreeSet) for deterministic output.  Building the complete
    // map before rewriting is what makes cycles safe.
    let mut map: BTreeMap<ObjectRef, ObjectRef> = BTreeMap::new();
    for &src_ref in refs {
        map.insert(src_ref, ObjectRef::new(next, 0));
        next += 1;
    }

    // Resolve each source object, rewrite its references, and store the copy.
    for &src_ref in refs {
        let obj = source.resolve(src_ref)?;
        let rewritten = rewrite_refs(&obj, &map);
        target.set_object(map[&src_ref], rewritten);
    }

    Ok(map)
}

/// Deep-rewrite every [`Object::Reference`] in `obj`: refs present in `map` are
/// remapped, refs outside `map` become [`Object::Null`].  Stream bytes are
/// carried verbatim; scalars are returned unchanged.
fn rewrite_refs(obj: &Object, map: &BTreeMap<ObjectRef, ObjectRef>) -> Object {
    match obj {
        Object::Reference(r) => match map.get(r) {
            Some(&t) => Object::Reference(t),
            None => Object::Null,
        },
        Object::Array(items) => {
            Object::Array(items.iter().map(|i| rewrite_refs(i, map)).collect())
        }
        Object::Dictionary(dict) => Object::Dictionary(rewrite_dict(dict, map)),
        Object::Stream(stream) => {
            Object::Stream(Stream::new(rewrite_dict(&stream.dict, map), stream.data.clone()))
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => obj.clone(),
    }
}

/// Rewrite every value of `dict` via [`rewrite_refs`], preserving keys.
fn rewrite_dict(dict: &Dictionary, map: &BTreeMap<ObjectRef, ObjectRef>) -> Dictionary {
    let mut out = Dictionary::new();
    for (key, value) in dict.iter() {
        out.insert(key, rewrite_refs(value, map));
    }
    out
}
