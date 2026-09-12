//! qpdf correspondence: QPDF_pages.cc page-tree preparation responsibilities.
//! Repairs the page tree before optimization and returns qpdf's effective page
//! order. The normal non-linearized writer does not call this path.

use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Seek};

use crate::object_handle::{ObjectHandle, ObjectHandleIdentity};
use crate::ObjectRef;
use crate::{Error, Pdf, QpdfErrorCode, QpdfExc, Result};

/// The effective `/Pages` root and leaf order after qpdf-compatible repair.
///
/// Returned by [`prepare_for_optimization`]; see that function for what "repair" covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPages {
    /// The effective `/Pages` root, after correcting a catalog whose `/Pages` points
    /// into the tree instead of at the true root.
    pub root: PageTreeRoot,
    /// Every `Page` leaf in document order, with qpdf's `getAllPagesInternal` repairs
    /// applied (see [`prepare_for_optimization`]).
    pub pages: Vec<ObjectRef>,
}

/// Location of the repaired `/Pages` root.
///
/// qpdf's object handles preserve a direct `/Pages` dictionary embedded in the
/// catalog. `Direct` therefore records its catalog owner instead of minting an
/// object for the page-tree root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTreeRoot {
    Indirect(ObjectRef),
    Direct {
        catalog: ObjectRef,
    },
    /// The `/Pages` root is direct and its Catalog is also direct in the
    /// trailer. The Catalog itself has no `ObjectRef`; consumers recover the
    /// same canonical handle through `Pdf::root_handle()`.
    DirectCatalog,
}

/// Repair the `/Pages` tree and return its effective root and leaf order.
///
/// # Errors
///
/// Propagates any [`Error`] from resolving an object while walking the tree, and
/// follows qpdf's unbounded default page-tree walk and returns
/// [`Error::Unsupported`] only for other traversal failures.
pub fn prepare_for_optimization<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Option<PreparedPages>> {
    prepare_for_optimization_canonical(pdf, None)
}

/// Like [`prepare_for_optimization`], but uses the caller's page-tree depth
/// bound for both qpdf-style repair and page enumeration.
pub(crate) fn prepare_for_optimization_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Option<PreparedPages>> {
    prepare_for_optimization_canonical(pdf, Some(max_depth))
}

/// Canonical qpdf-style page-tree preparation.
///
/// The complete repair walk is intentionally expressed in terms of live
/// [`ObjectHandle`]s. This is the `QPDF::getAllPages` / `getAllPagesInternal`
/// boundary (`libqpdf/QPDF_pages.cc:39-150`): the catalog, `/Pages` root,
/// `/Kids` array, and every leaf all retain their canonical identity while
/// repair mutates them.
fn prepare_for_optimization_canonical<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: Option<usize>,
) -> Result<Option<PreparedPages>> {
    if max_depth.is_none() {
        if let Some(cached) = pdf.cached_page_list() {
            pdf.mark_get_all_pages_called();
            return Ok(Some(cached));
        }
    } else {
        // The public qpdf path has one fixed traversal contract. A caller
        // using flpdf's explicit test/depth variant may perform a different
        // repair walk, so it must not leave the default page cache stale.
        pdf.invalidate_page_list_cache();
    }
    pdf.mark_get_all_pages_called();

    let root_candidate = pdf.trailer_key_handle(b"Root");
    if root_candidate.try_is_null()? {
        return Ok(None);
    }
    root_candidate.try_dereference()?;
    if !root_candidate.try_is_dictionary()? {
        return Ok(None);
    }
    let catalog = root_candidate;
    let mut pages = catalog.try_get_key(b"/Pages")?;

    // qpdf corrects a catalog that points into the tree by following
    // `/Parent` until the true root (`QPDF_pages.cc:50-67`). Track canonical
    // handle identity so the guard covers both indirect ObjGen slots and
    // direct dictionaries that share the same live allocation.
    let mut seen_parent: BTreeSet<ObjectRef> = BTreeSet::new();
    // The key hashes only the canonical slot pointer; its Rc is retained so
    // that an allocation cannot be dropped and reused while it is tracked.
    #[allow(
        clippy::mutable_key_type,
        reason = "identity key compares only Rc pointer identity and retains the slot deliberately"
    )]
    let mut seen_parent_direct: HashSet<ObjectHandleIdentity> = HashSet::new();
    let mut changed_pages = false;
    let mut warned = false;
    loop {
        let repeated = if let Some(object_ref) = pages.object_ref() {
            !seen_parent.insert(object_ref)
        } else {
            !seen_parent_direct.insert(pages.identity_key())
        };
        if repeated {
            break;
        }
        // QPDF::getAllPages checks isDictionary before asking the current
        // node for /Parent. This keeps a missing or scalar /Pages value on
        // qpdf's warning-tolerant empty-page path rather than manufacturing
        // a hard page-tree error.
        if !pages.try_is_dictionary()? {
            break;
        }
        if !pages.try_has_key(b"/Parent")? {
            break;
        }
        let parent = pages.try_get_key(b"/Parent")?;
        if parent.try_is_null()? {
            break; // cov:ignore: qpdf-compatible try_has_key hides direct and indirect null values first
        }
        if !warned {
            catalog.warn_if_possible(
                "document page tree root (root -> /Pages) doesn't point to the root of the page tree; attempting to correct",
            )?; // cov:ignore: warning-sink failure is not injectable through the qpdf success oracle
            warned = true;
        }
        pages = parent;
        changed_pages = true;
    }
    if changed_pages {
        catalog.replace_key(b"/Pages", pages.clone())?;
    }

    // qpdf's getAllPages asks the final node for /Kids even when it is not a
    // dictionary. That access is observable as a type warning for a missing
    // or scalar /Pages entry and is followed by an empty page cache.
    let has_kids = pages.try_has_key(b"/Kids")?;
    if !pages.try_is_dictionary()? {
        return Ok(None);
    }

    let mut state = CanonicalRepairState {
        seen: BTreeSet::new(),
        visited: BTreeSet::new(),
        visited_direct: HashSet::new(),
        pages: Vec::new(),
    };
    if has_kids {
        repair_page_tree_handle(pdf, pages.clone(), &mut state, 0, false, max_depth)?;
    }

    let root = match pages.object_ref() {
        Some(object_ref) => PageTreeRoot::Indirect(object_ref),
        None => match catalog.object_ref() {
            Some(catalog_ref) => PageTreeRoot::Direct {
                catalog: catalog_ref,
            },
            None => PageTreeRoot::DirectCatalog,
        },
    };
    let prepared = PreparedPages {
        root,
        pages: state.pages,
    };
    if max_depth.is_none() {
        pdf.cache_page_list(&prepared);
    }
    Ok(Some(prepared))
}

struct CanonicalRepairState {
    seen: BTreeSet<ObjectRef>,
    visited: BTreeSet<ObjectRef>,
    visited_direct: HashSet<ObjectHandleIdentity>,
    pages: Vec<ObjectRef>,
}

/// One suspended `getAllPagesInternal` frame.
///
/// qpdf uses the native call stack for this walk. Keep the same parent-before-
/// child and child-array order while moving the Rust stack frames to the heap,
/// so deeply nested valid trees do not hit the debug thread-stack limit.
struct RepairFrame {
    node: ObjectHandle,
    kids: ObjectHandle,
    next_kid: usize,
    kid_count: usize,
    depth: usize,
    inherited_media_box: bool,
}

/// Canonical `QPDF::getAllPagesInternal` walk (`QPDF_pages.cc:77-138`).
///
/// The holder and every child remain live handles throughout this function.
/// In particular, a direct child is promoted through
/// `QPDF::makeIndirectObject`'s shared-allocation counterpart before the
/// containing `/Kids` array is updated; no raw-object replacement is used.
fn repair_page_tree_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: ObjectHandle,
    state: &mut CanonicalRepairState,
    depth: usize,
    inherited_media_box: bool,
    max_depth: Option<usize>,
) -> Result<()> {
    let Some(frame) =
        repair_page_tree_frame(pdf, node, state, depth, inherited_media_box, max_depth)?
    else {
        return Ok(());
    };
    let mut frames = vec![frame];

    while let Some(frame) = frames.last_mut() {
        if frame.next_kid >= frame.kid_count {
            frames.pop();
            continue;
        }

        let index = frame.next_kid;
        frame.next_kid += 1;
        let parent = frame.node.clone();
        let kids = frame.kids.clone();
        let frame_depth = frame.depth;
        let media_box = frame.inherited_media_box;
        let Some(mut kid) = kids.try_array_item(index)? else {
            continue;
        };

        if kid.try_has_key(b"/Kids")? {
            if let Some(child) =
                repair_page_tree_frame(pdf, kid, state, frame_depth + 1, media_box, max_depth)?
            {
                frames.push(child);
            }
            continue;
        }

        // qpdf applies the default before either direct promotion or duplicate
        // cloning (`QPDF_pages.cc:104-130`). This order is observable because a
        // duplicate receives the already-repaired page dictionary.
        if !media_box && !is_rectangle_handle(&kid.try_get_key(b"/MediaBox")?)? {
            kid.warn_if_possible(
                format!("kid {index} (from 0) MediaBox is undefined; setting to letter / ANSI A")
                    .as_str(),
            )?; // cov:ignore: warning-sink failure is not injectable through the qpdf success oracle
            replace_handle_key(
                &kid,
                b"/MediaBox",
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(612),
                    ObjectHandle::integer(792),
                ]),
            )?; // cov:ignore: canonical page owners make this replacement infallible here
        }

        if kid.is_direct() {
            parent.warn_if_possible(
                format!("kid {index} (from 0) is direct; converting to indirect").as_str(),
            )?; // cov:ignore: warning-sink failure is not injectable through the qpdf success oracle
            kid = promote_page_handle(pdf, kid)?;
            let promoted_ref = kid
                .object_ref()
                .expect("promote_page_handle returns an indirect handle");
            state.seen.insert(promoted_ref);
            kids.set_array_item(index, kid.clone())?;
        } else if let Some(object_ref) = kid.object_ref() {
            if !state.seen.insert(object_ref) {
                parent.warn_if_possible(
                    format!(
                        "kid {index} (from 0) appears more than once in the pages tree; creating a new page object as a copy"
                    )
                    .as_str(),
                )?; // cov:ignore: LLVM maps this covered duplicate-repair warning terminator separately
                let copied = kid.shallow_copy()?;
                kid = promote_page_handle(pdf, copied)?;
                let copied_ref = kid
                    .object_ref()
                    .expect("promote_page_handle returns an indirect handle");
                state.seen.insert(copied_ref);
                kids.set_array_item(index, kid.clone())?;
            }
        }

        if !kid.try_is_dictionary_of_type(b"Page", b"")? {
            kid.warn_if_possible("/Type key should be /Page but is not; overriding")?;
            replace_handle_key(&kid, b"/Type", ObjectHandle::name(b"Page".to_vec()))?;
        }
        let page_ref = kid
            .object_ref()
            .expect("every qpdf page-tree leaf is indirect after repair");
        state.pages.push(page_ref);
    }
    Ok(())
}

fn repair_page_tree_frame<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node: ObjectHandle,
    state: &mut CanonicalRepairState,
    depth: usize,
    inherited_media_box: bool,
    max_depth: Option<usize>,
) -> Result<Option<RepairFrame>> {
    if max_depth.is_some_and(|max_depth| depth >= max_depth) {
        let location = node
            .object_ref()
            .map_or_else(|| "direct /Pages node".to_owned(), |r| r.to_string());
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {} at {location}",
            max_depth.expect("checked above")
        )));
    }
    if let Some(object_ref) = node.object_ref() {
        if !state.visited.insert(object_ref) {
            return Err(page_tree_cycle_error(pdf));
        }
    } else if !state.visited_direct.insert(node.identity_key()) {
        return Err(page_tree_cycle_error(pdf)); // cov:ignore: parsed PDF /Pages nodes are indirect; retain this defensive direct-handle cycle guard for canonical ObjectHandle callers.
    }

    node.try_dereference()?;
    if !node.try_is_dictionary()? || !node.try_has_key(b"/Kids")? {
        return Ok(None); // cov:ignore: callers recurse only after observing a dictionary /Kids key
    }

    if !node.try_is_dictionary_of_type(b"Pages", b"")? {
        node.warn_if_possible("/Type key should be /Pages but is not; overriding")?;
        replace_handle_key(&node, b"/Type", ObjectHandle::name(b"Pages".to_vec()))?;
    }

    let media_box = if inherited_media_box {
        true
    } else {
        is_rectangle_handle(&node.try_get_key(b"/MediaBox")?)?
    };
    let kids = node.try_get_key(b"/Kids")?;
    let Some(kid_count) = kids.try_array_len()? else {
        // QPDFObjectHandle::getArrayNItems warns and treats a non-array as
        // empty (`QPDFObjectHandle.cc:758-768`).
        let type_name = kids.type_name()?;
        kids.warn_if_possible(
            format!(
                "operation for array attempted on object of type {}: treating as empty",
                type_name
            )
            .as_str(),
        )?; // cov:ignore: warning-sink failure is not injectable through the qpdf success oracle
        return Ok(None);
    };
    Ok(Some(RepairFrame {
        node,
        kids,
        next_kid: 0,
        kid_count,
        depth,
        inherited_media_box: media_box,
    }))
}

/// Construct qpdf's `QPDFExc(qpdf_e_pages, ...)` for a repeated page-tree
/// node. qpdf uses the document's current `m->last_object_description`, not
/// the repeated node's object number, so preserve that state independently
/// from the rendered `what()` string (`QPDF_pages.cc:81-87`).
pub(crate) fn page_tree_cycle_error<R: Read + Seek>(pdf: &Pdf<R>) -> Error {
    Error::QpdfExc(QpdfExc::new(
        QpdfErrorCode::Pages,
        pdf.input_source_description(),
        pdf.resolver.last_object_description(),
        0,
        b"Loop detected in /Pages structure (getAllPages)",
    ))
}

fn promote_page_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: ObjectHandle,
) -> Result<ObjectHandle> {
    let promoted = pdf.make_indirect_from_object_handle(handle)?;
    Ok(promoted)
}

fn replace_handle_key(holder: &ObjectHandle, key: &[u8], value: ObjectHandle) -> Result<()> {
    holder.replace_key(key, value)?;
    Ok(())
}

fn is_rectangle_handle(value: &ObjectHandle) -> Result<bool> {
    let Some(length) = value.try_array_len()? else {
        return Ok(false);
    };
    if length != 4 {
        return Ok(false);
    }
    for index in 0..length {
        let Some(item) = value.try_array_item(index)? else {
            return Ok(false);
        };
        if item.try_as_integer()?.is_none() {
            item.try_dereference()?;
            if item.as_real().is_none() {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
