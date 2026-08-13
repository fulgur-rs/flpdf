//! qpdf correspondence: QPDFOutlineDocumentHelper.cc and QPDFOutlineObjectHelper.cc responsibilities split with outline.rs.
//! High-level outline (`/Outlines`) document helper.
//!
//! [`OutlineDocumentHelper`] wraps a `&mut Pdf<R>` and materializes the document
//! outline (bookmarks) into an arena-backed [`crate::OutlineTree`], mirroring
//! qpdf's raw-object traversal for direct and indirect outline values.
//!
//! # Example
//!
//! ```no_run
//! use flpdf::Pdf;
//! use std::io::Cursor;
//!
//! # fn f(bytes: Vec<u8>) -> flpdf::Result<()> {
//! let mut pdf = Pdf::open(Cursor::new(bytes))?;
//! if pdf.outline().has_outlines()? {
//!     let tree = pdf.outline().get_tree()?;
//!     for (depth, _id, item) in tree.preorder() {
//!         println!("{:indent$}{}", "", item.title, indent = (depth - 1) * 2);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! qpdf-incompatible outline policy APIs were removed before flpdf 1.0.
//!
//! ```compile_fail
//! use flpdf::Dest;
//! ```
//!
//! ```compile_fail
//! use flpdf::{check_legacy_dests, check_name_tree_dests, check_outline_links};
//! ```
//!
//! ```compile_fail
//! use flpdf::{prune_outline_se, prune_outline_se_with_max_depth};
//! ```
//!
//! ```compile_fail
//! # use flpdf::Pdf;
//! # use std::io::Cursor;
//! # let mut pdf = Pdf::open(Cursor::new(Vec::<u8>::new())).unwrap();
//! let _ = pdf.outline().get_root_with_max_depth(10);
//! ```

use crate::nntree::HandleNameTree;
use crate::outline::{OutlineId, OutlineItem, OutlineTree};
use crate::{ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

const QPDF_MAX_EXPANDED_OUTLINE_DEPTH: usize = 50;

fn object_key(object: &ObjectHandle, key: &[u8]) -> Result<Option<ObjectHandle>> {
    let value = object.try_get_key(key)?;
    if value.try_is_null()? {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

/// Detect an actual repeat among **direct** outline sibling handles by live
/// identity, mirroring `form_field_object_helper.rs`'s
/// `mark_direct_node_seen` (`ObjectRef`-keyed seen sets never terminate a
/// `/Next` cycle built entirely from direct dictionaries: a direct handle's
/// `object_ref()` is always `None`, the same `QPDFObjGen::set` gap
/// documented there). Real PDF bytes cannot produce two direct dictionaries
/// that reciprocally reference each other, but it is reachable in memory
/// through the public [`ObjectHandle::replace_key`] API. Unlike that helper
/// (which raises an error), this returns a bool like the existing `ObjectRef`
/// seen-set checks in `get_tree`/`build_item`'s sibling walks, so a cycle
/// here silently stops the walk the same way a repeated indirect reference
/// already does — matching qpdf's own `QPDFObjGen::set`-guarded constructor
/// loop, which also just stops (`libqpdf/QPDFOutlineDocumentHelper.cc:16-21`).
fn mark_direct_sibling_seen(direct_seen: &mut Vec<ObjectHandle>, current: &ObjectHandle) -> bool {
    if !current.is_direct() {
        return true;
    }
    if direct_seen
        .iter()
        .any(|handle: &ObjectHandle| handle.is_same_object_as(current))
    {
        return false;
    }
    direct_seen.push(current.clone());
    true
}

/// High-level outline helper for a document. See module docs.
pub struct OutlineDocumentHelper<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> OutlineDocumentHelper<'a, R> {
    /// Wrap a document for outline access. Prefer [`Pdf::outline`].
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Return `true` if the resolved catalog `/Outlines` dictionary has a
    /// non-null `/First` value. Mirrors qpdf `hasOutlines` construction.
    ///
    /// # Errors
    ///
    /// Propagates errors from resolving the catalog and `/Outlines` cursor.
    pub fn has_outlines(&mut self) -> Result<bool> {
        let Some(outlines) = self.catalog_outlines()? else {
            return Ok(false);
        };
        self.pdf.resolve_object_handle(&outlines)?;
        if outlines.try_as_dictionary()?.is_none() {
            return Ok(false);
        }
        if !outlines.try_has_key(b"/First")? {
            return Ok(false);
        }
        Ok(!outlines.try_get_key(b"/First")?.try_is_null()?)
    }

    fn resolve_handle(&mut self, handle: &ObjectHandle) -> Result<()> {
        self.pdf.resolve_object_handle(handle)
    }

    /// Follow the temporary bare-reference redirect that `Pdf::set_object`
    /// can install in a canonical slot. Parsed qpdf objects never contain
    /// this state: a normal indirect child resolves in one hop and is
    /// returned unchanged. Keeping this compatibility chase handle-native
    /// lets the outline consumer preserve live identity without reopening the
    /// raw `Object` route while the legacy replacement bridge is removed.
    fn resolve_value_handle(&mut self, handle: ObjectHandle) -> Result<ObjectHandle> {
        self.pdf.resolve_object_handle_to_terminal(&handle)
    }

    fn catalog_handle(&mut self) -> Result<Option<ObjectHandle>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let catalog = self.pdf.get_object_handle(catalog_ref);
        self.resolve_handle(&catalog)?;
        if catalog.try_as_dictionary()?.is_none() {
            return Ok(None);
        }
        Ok(Some(catalog))
    }

    fn catalog_outlines(&mut self) -> Result<Option<ObjectHandle>> {
        let Some(catalog) = self.catalog_handle()? else {
            return Ok(None);
        };
        if !catalog.try_has_key(b"/Outlines")? {
            return Ok(None);
        }
        Ok(Some(catalog.try_get_key(b"/Outlines")?))
    }

    /// Materialize the qpdf-compatible outline arena.
    ///
    /// # Errors
    ///
    /// Propagates outline-resolution errors.
    pub fn get_tree(&mut self) -> Result<OutlineTree> {
        let mut tree = OutlineTree::new();
        let Some(outlines) = self.catalog_outlines()? else {
            return Ok(tree);
        };
        self.resolve_handle(&outlines)?;
        if outlines.try_as_dictionary()?.is_none() {
            return Ok(tree);
        }
        if !outlines.try_has_key(b"/First")? {
            return Ok(tree);
        }
        let first = outlines.try_get_key(b"/First")?;
        if first.try_is_null()? {
            return Ok(tree);
        }
        let mut cursor = first;

        let mut top_level_seen = BTreeSet::new();
        let mut top_level_direct_seen = Vec::new();
        let mut constructor_seen = BTreeSet::new();
        loop {
            if let Some(reference) = cursor.object_ref() {
                if !top_level_seen.insert(reference) {
                    break;
                }
            } else if !mark_direct_sibling_seen(&mut top_level_direct_seen, &cursor) {
                break;
            }

            let Some(id) = self.build_item(cursor, None, &mut tree, &mut constructor_seen)? else {
                break;
            };
            tree.roots.push(id);
            let Some(next) = object_key(&tree[id].object, b"/Next")? else {
                break;
            };
            cursor = next;
        }
        Ok(tree)
    }

    /// Materialize one item and all descendants using an explicit frame stack.
    /// The stack preserves qpdf's constructor seen-set placement without using
    /// one native call frame per outline level.
    fn build_item(
        &mut self,
        cursor: ObjectHandle,
        parent: Option<OutlineId>,
        tree: &mut OutlineTree,
        constructor_seen: &mut BTreeSet<ObjectRef>,
    ) -> Result<Option<OutlineId>> {
        let Some(root) = self.materialize_item(cursor, parent, tree)? else {
            return Ok(None);
        };
        if let Some(reference) = tree[root].source_ref {
            if !constructor_seen.insert(reference) {
                return Ok(Some(root));
            }
        }

        struct Frame {
            owner: OutlineId,
            next: Option<ObjectHandle>,
            depth: usize,
            siblings_seen: BTreeSet<ObjectRef>,
            direct_siblings_seen: Vec<ObjectHandle>,
        }

        let mut frames = Vec::new();
        let first = object_key(&tree[root].object, b"/First")?;
        if first.is_some() {
            frames.push(Frame {
                owner: root,
                next: first,
                depth: 2,
                siblings_seen: BTreeSet::new(),
                direct_siblings_seen: Vec::new(),
            });
        }

        while !frames.is_empty() {
            let next_cursor = frames.last_mut().and_then(|frame| frame.next.take());
            let Some(cursor) = next_cursor else {
                frames.pop();
                continue;
            };
            let (owner, child_depth) = {
                let frame = frames
                    .last_mut()
                    .expect("outline construction frame exists");
                if let Some(reference) = cursor.object_ref() {
                    if !frame.siblings_seen.insert(reference) {
                        frame.next = None;
                        continue;
                    }
                } else if !mark_direct_sibling_seen(&mut frame.direct_siblings_seen, &cursor) {
                    frame.next = None;
                    continue;
                }
                (frame.owner, frame.depth)
            };
            let Some(child) = self.materialize_item(cursor, Some(owner), tree)? else {
                continue;
            };
            tree.items[owner.0].kids.push(child);

            let expand_child = if child_depth > QPDF_MAX_EXPANDED_OUTLINE_DEPTH {
                false
            } else if let Some(reference) = tree[child].source_ref {
                constructor_seen.insert(reference)
            } else {
                true
            };

            // qpdf advances the parent's raw child `/Next` chain even when the
            // child's constructor seen check prevented that child expanding.
            frames
                .last_mut()
                .expect("outline construction frame exists")
                .next = object_key(&tree[child].object, b"/Next")?;

            if expand_child {
                let first = object_key(&tree[child].object, b"/First")?;
                if first.is_some() {
                    frames.push(Frame {
                        owner: child,
                        next: first,
                        depth: child_depth + 1,
                        siblings_seen: BTreeSet::new(),
                        direct_siblings_seen: Vec::new(),
                    });
                }
            }
        }

        Ok(Some(root))
    }

    fn materialize_item(
        &mut self,
        cursor: ObjectHandle,
        parent: Option<OutlineId>,
        tree: &mut OutlineTree,
    ) -> Result<Option<OutlineId>> {
        // Chase a direct handle whose own resolved value is itself a bare
        // `Object::Reference` (installed via `shallow_copy` on a
        // `Pdf::set_object`-redirected holder, then set as `/First`/`/Next`
        // through the public `ObjectHandle::replace_key` API) to its real
        // target, the same way every other exit path in this file already
        // chases this shape via `resolve_value_handle`. `object_ref()` is
        // captured AFTER chasing so cycle detection (`source_ref`, used by
        // `build_item`'s `constructor_seen`) keys off the terminal identity,
        // not the pre-chase holder.
        let cursor = self.resolve_value_handle(cursor)?;
        let source_ref = cursor.object_ref();
        if cursor.try_is_null()? {
            return Ok(None);
        }
        let (title_src, count_src, dest_src, action_src) = if cursor.try_as_dictionary()?.is_some()
        {
            (
                cursor
                    .try_has_key(b"/Title")?
                    .then(|| cursor.try_get_key(b"/Title")),
                cursor
                    .try_has_key(b"/Count")?
                    .then(|| cursor.try_get_key(b"/Count")),
                cursor
                    .try_has_key(b"/Dest")?
                    .then(|| cursor.try_get_key(b"/Dest")),
                cursor
                    .try_has_key(b"/A")?
                    .then(|| cursor.try_get_key(b"/A")),
            )
        } else {
            (None, None, None, None)
        };
        let title_src = title_src.transpose()?;
        let count_src = count_src.transpose()?;
        let dest_src = dest_src.transpose()?;
        let action_src = action_src.transpose()?;
        let title = resolve_title(self.pdf, title_src)?;
        let count = resolve_count(self.pdf, count_src)?;
        let dest = self.resolve_node_dest(dest_src, action_src)?;
        self.resolve_handle(&dest)?;
        let id = OutlineId(tree.items.len());
        tree.items.push(OutlineItem {
            source_ref,
            parent,
            kids: Vec::new(),
            object: cursor,
            title,
            count,
            dest,
        });

        Ok(Some(id))
    }

    /// Resolve a node's destination from `/Dest`, else a `/A` GoTo action's `/D`.
    fn resolve_node_dest(
        &mut self,
        dest: Option<ObjectHandle>,
        action: Option<ObjectHandle>,
    ) -> Result<ObjectHandle> {
        let Some(candidate) = (if let Some(dest) = dest {
            Some(dest)
        } else {
            self.goto_action_dest(action)?
        }) else {
            return Ok(ObjectHandle::null());
        };

        let candidate = self.resolve_value_handle(candidate)?;
        if let Some(name) = candidate.try_as_name()? {
            return self.resolve_legacy_node_dest(&name);
        }
        if let Some(bytes) = candidate.as_string() {
            return self.resolve_name_tree_node_dest(&bytes);
        }
        Ok(candidate)
    }

    fn goto_action_dest(&mut self, action: Option<ObjectHandle>) -> Result<Option<ObjectHandle>> {
        let Some(action) = action else {
            return Ok(None);
        };
        let action = self.resolve_value_handle(action)?;
        if action.try_as_dictionary()?.is_none() {
            return Ok(None);
        }
        // Chase the same `Pdf::set_object` redirect this file's other
        // dest-resolution call sites already chase (`resolve_node_dest`'s
        // `candidate`, `resolve_legacy_node_dest`'s `value`,
        // `resolve_name_tree_node_dest`'s `found`, and this function's own
        // `action` above): `try_is_name_and_equals` only dereferences its
        // receiver once and never follows a bare-reference-valued result.
        let subtype = self.resolve_value_handle(action.try_get_key(b"/S")?)?;
        if !subtype.try_is_name_and_equals(b"GoTo")? {
            return Ok(None);
        }
        if !action.try_has_key(b"/D")? {
            return Ok(None);
        }
        Ok(Some(action.try_get_key(b"/D")?))
    }

    fn resolve_legacy_node_dest(&mut self, name: &[u8]) -> Result<ObjectHandle> {
        let Some(dests) = self.catalog_value_handle(b"/Dests")? else {
            return Ok(ObjectHandle::null());
        };
        let dests = self.resolve_value_handle(dests)?;
        if dests.try_as_dictionary()?.is_none() {
            return Ok(ObjectHandle::null());
        }
        let mut key = Vec::with_capacity(name.len() + 1);
        key.push(b'/');
        key.extend_from_slice(name);
        if !dests.try_has_key(&key)? {
            return Ok(ObjectHandle::null());
        }
        // Chase the selected entry the same way every other exit path in
        // this file does (`resolve_node_dest`'s `candidate`,
        // `goto_action_dest`'s `action`, `resolve_name_tree_node_dest`'s
        // `found`): a `Pdf::set_object` legacy redirect can still sit behind
        // this dictionary entry even after `dests` itself was chased above.
        let value = dests.try_get_key(&key)?;
        self.resolve_value_handle(value)
    }

    fn resolve_name_tree_node_dest(&mut self, bytes: &[u8]) -> Result<ObjectHandle> {
        let lookup =
            crate::pdf_string::normalized_utf8_value(&crate::pdf_string::utf8_value(bytes));
        let Some(names) = self.catalog_value_handle(b"/Names")? else {
            return Ok(ObjectHandle::null());
        };
        let names = self.resolve_value_handle(names)?;
        if names.try_as_dictionary()?.is_none() || !names.try_has_key(b"/Dests")? {
            return Ok(ObjectHandle::null());
        }

        let root = names.try_get_key(b"/Dests")?;
        let root = self.resolve_value_handle(root)?;
        if root.try_as_dictionary()?.is_none() {
            return Ok(ObjectHandle::null());
        }
        let mut tree = HandleNameTree::new(root, self.pdf.unique_id(), true);
        let found = tree.find(self.pdf, lookup.as_slice())?;
        found
            .map(|value| self.resolve_value_handle(value))
            .transpose()
            .map(|value| value.unwrap_or_else(ObjectHandle::null))
    }

    fn catalog_value_handle(&mut self, key: &[u8]) -> Result<Option<ObjectHandle>> {
        let Some(catalog) = self.catalog_handle()? else {
            return Ok(None);
        };
        if !catalog.try_has_key(key)? {
            return Ok(None);
        }
        Ok(Some(catalog.try_get_key(key)?))
    }
}
impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level outline helper for this document.
    pub fn outline(&mut self) -> OutlineDocumentHelper<'_, R> {
        OutlineDocumentHelper::new(self)
    }
}

/// Decode an outline `/Title`, resolving one level of indirection (review rule 2).
fn resolve_title<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<ObjectHandle>) -> Result<String> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    pdf.resolve_object_handle(&value)?;
    qpdf_title(&value)
}

fn qpdf_title(value: &ObjectHandle) -> Result<String> {
    if let Some(bytes) = value.as_string() {
        Ok(String::from_utf8_lossy(&crate::pdf_string::utf8_value(&bytes)).into_owned())
    } else {
        value.type_warning("string", "returning empty string")?;
        Ok(String::new())
    }
}

/// Read an outline `/Count`, resolving one level of indirection (review rule 2/3).
fn resolve_count<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<ObjectHandle>) -> Result<i32> {
    let Some(value) = value else {
        return Ok(0);
    };
    pdf.resolve_object_handle(&value)?;
    qpdf_count(&value)
}

fn qpdf_count(value: &ObjectHandle) -> Result<i32> {
    value.try_get_int_value_as_int()
}

#[cfg(test)]
mod tests {
    use super::{mark_direct_sibling_seen, qpdf_count, qpdf_title};
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::PipelineHandle;
    use crate::{ObjectHandle, ObjectRef};

    /// Mirrors `form_field_object_helper.rs`'s
    /// `direct_seen_set_ignores_indirect_handles_and_tracks_direct_identity`:
    /// an indirect handle is ignored (the `BTreeSet<ObjectRef>` seen-set in
    /// `get_tree`/`build_item` already owns its identity), a direct handle is
    /// recorded on first sight, and an actual repeat of the SAME underlying
    /// allocation is rejected while a distinct direct handle with equal
    /// contents is not treated as a repeat.
    #[test]
    fn direct_sibling_seen_set_ignores_indirect_handles_and_tracks_direct_identity() {
        let mut direct_seen = Vec::new();

        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1);
        assert!(mark_direct_sibling_seen(&mut direct_seen, &indirect));
        assert!(mark_direct_sibling_seen(&mut direct_seen, &indirect));
        assert!(direct_seen.is_empty());

        let direct = ObjectHandle::dictionary(Vec::new());
        assert!(mark_direct_sibling_seen(&mut direct_seen, &direct));
        assert!(!mark_direct_sibling_seen(&mut direct_seen, &direct));

        let other_direct = ObjectHandle::dictionary(Vec::new());
        assert!(mark_direct_sibling_seen(&mut direct_seen, &other_direct));
    }

    #[test]
    fn scalar_warning_sink_failures_propagate() {
        for title in [true, false] {
            let mut pdf = crate::Pdf::empty().unwrap();
            let logger = crate::QPDFLogger::create();
            logger.set_warn(Some(PipelineHandle::new(NthWriteFailure::new(1))));
            pdf.set_logger(logger);

            let result = if title {
                let value = pdf
                    .lift_object_to_handle(&crate::Object::Integer(42))
                    .unwrap();
                qpdf_title(&value).map(|_| ())
            } else {
                let value = pdf
                    .lift_object_to_handle(&crate::Object::String(b"wrong".to_vec()))
                    .unwrap();
                qpdf_count(&value).map(|_| ())
            };
            assert!(matches!(
                result,
                Err(crate::Error::System(ref message)) if message == "sink write failure 1"
            ));
            assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
        }
    }
}
