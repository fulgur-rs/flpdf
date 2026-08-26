//! qpdf correspondence: NNTree.cc behavior implemented with Rust-specific storage, error, and ownership boundaries.
//!
//! This module provides the shared engine plus public wrappers corresponding
//! to `QPDFNameTreeObjectHelper` and `QPDFNumberTreeObjectHelper`.
//!
//! The traversal/mutation path is canonical `ObjectHandle` graph state: qpdf's
//! `QPDFObjectHandle` nodes and arrays are kept live through lookup, cursor
//! movement, repair, split, insert, and remove (`libqpdf/NNTree.cc:34-75,
//! 106-168, 216-390, 391-520, 560-700`). The public `NameTree` and
//! `NumberTree` facades and the shared generic engine are handle-native with no
//! raw `Object` fixture or projection route remaining. Production tree
//! mutations do not write nodes back through `Pdf::set_object`. Array replacement
//! follows qpdf's `QPDFObjectHandle` live-array mutators and
//! `QPDF_Array::setFromVector` ownership/order boundary
//! (`libqpdf/QPDFObjectHandle.cc:869-955`, `libqpdf/QPDF_Array.cc:220-313`),
//! and direct-node promotion preserves the existing allocation like `QPDF::makeIndirectObject`
//! (`libqpdf/QPDF.cc:1835-1902`).
//! Indirect traversal identity follows qpdf's `QPDFObjGen::set`
//! (`include/qpdf/QPDFObjGen.hh:87-120`, `libqpdf/QPDFObjGen.cc:25-35`);
//! direct canonical handles use the existing `ObjectHandleIdentity` primitive,
//! which matches `QPDFObjectHandle::isSameObjectAs`
//! (`include/qpdf/QPDFObjectHandle.hh:304-309`,
//! `libqpdf/QPDFObjectHandle.cc:224-227`).

use crate::object_handle::{canonical_dictionary_key, ObjectHandleIdentity};
use crate::pdf_string::{new_unicode_string, normalized_utf8_value, utf8_value};
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Debug;
use std::io::{Read, Seek};
use std::marker::PhantomData;
use std::sync::Arc;

pub(crate) const DEFAULT_SPLIT_THRESHOLD: usize = 32;

/// Default `/Kids` descent depth limit for name and number tree traversal.
pub const DEFAULT_MAX_TREE_DEPTH: usize = 100;

/// Default qpdf split threshold for name and number tree leaves.
pub const LEAF_MAX: usize = DEFAULT_SPLIT_THRESHOLD;

pub(crate) trait TreeKey {
    type Key: Clone + Debug + Eq + Ord;
    const ITEMS_KEY: &'static str;

    fn from_handle(handle: &ObjectHandle) -> Option<Self::Key>;
    fn to_handle(key: &Self::Key) -> ObjectHandle;

    fn compare(left: &Self::Key, right: &Self::Key) -> Ordering {
        left.cmp(right)
    }
}

pub(crate) enum NameKey {}

impl TreeKey for NameKey {
    type Key = Vec<u8>;
    const ITEMS_KEY: &'static str = "Names";

    fn from_handle(handle: &ObjectHandle) -> Option<Self::Key> {
        handle.as_string().map(|value| utf8_value(&value))
    }

    fn to_handle(key: &Self::Key) -> ObjectHandle {
        let normalized = normalized_utf8_value(key);
        ObjectHandle::string(new_unicode_string(&normalized))
    }
}

pub(crate) enum NumberKey {}

impl TreeKey for NumberKey {
    type Key = i64;
    const ITEMS_KEY: &'static str = "Nums";

    fn from_handle(handle: &ObjectHandle) -> Option<Self::Key> {
        handle.as_integer()
    }

    fn to_handle(key: &Self::Key) -> ObjectHandle {
        ObjectHandle::integer(*key)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum NodeAnchor {
    Root,
    Indirect(ObjectRef),
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum NodeIdentity {
    /// qpdf's `QPDFObjGen::set` identity for an indirect node.
    Indirect(ObjectRef),
    /// The live allocation identity for a direct canonical ObjectHandle.
    Direct(ObjectHandleIdentity),
}

/// A node path keeps the qpdf diagnostic anchor and the live node handle
/// separately. Direct canonical children are identified by their live handle;
/// indirect children keep their own ObjGen anchor. The `handle` field is the
/// only value the canonical engine reads or mutates.
#[derive(Clone, Debug)]
struct NodeHandle {
    anchor: NodeAnchor,
    direct_kids: Vec<usize>,
    handle: ObjectHandle,
}

impl NodeHandle {
    fn root(handle: ObjectHandle) -> Self {
        Self {
            anchor: NodeAnchor::Root,
            direct_kids: Vec::new(),
            handle,
        }
    }

    fn indirect(object_ref: ObjectRef, handle: ObjectHandle) -> Self {
        Self {
            anchor: NodeAnchor::Indirect(object_ref),
            direct_kids: Vec::new(),
            handle,
        }
    }

    fn direct_kid(&self, kid_index: usize, handle: ObjectHandle) -> Self {
        let mut direct_kids = self.direct_kids.clone();
        direct_kids.push(kid_index);
        Self {
            anchor: self.anchor.clone(),
            direct_kids,
            handle,
        }
    }

    fn handle(&self) -> ObjectHandle {
        self.handle.clone()
    }

    fn identity(&self) -> NodeIdentity {
        if self.direct_kids.is_empty() {
            if let NodeAnchor::Indirect(object_ref) = self.anchor {
                return NodeIdentity::Indirect(object_ref);
            }
        }
        NodeIdentity::Direct(self.handle.identity_key())
    }

    fn diagnostic_ref(&self) -> Option<ObjectRef> {
        if !self.direct_kids.is_empty() {
            return None;
        }
        match self.anchor {
            NodeAnchor::Indirect(object_ref) => Some(object_ref),
            NodeAnchor::Root => None,
        }
    }
}

/// A live array view. `values` is only a short-lived vector of handle clones;
/// the array itself remains `handle`, so `store` mutates the canonical array
/// allocation and preserves every alias to it.
struct ResolvedArray {
    handle: ObjectHandle,
    values: Vec<ObjectHandle>,
}

impl ResolvedArray {
    fn store<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> Result<()> {
        self.handle.set_array_items(self.values.clone())?;
        pdf.mark_object_handle_dirty(&self.handle)
    }
}

fn resolved_array<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    value: Option<&ObjectHandle>,
) -> Result<Option<ResolvedArray>> {
    let Some(source) = value else {
        return Ok(None);
    };
    source.try_dereference()?;
    let source = source.clone();
    let Some(values) = source.try_as_array()? else {
        return Ok(None);
    };
    Ok(Some(ResolvedArray {
        handle: source,
        values,
    }))
}

fn resolved_key<K: TreeKey, R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    value: &ObjectHandle,
) -> Result<Option<K::Key>> {
    value.try_dereference()?;
    let value = value.clone();
    Ok(K::from_handle(&value))
}

/// Reject a handle-native tree value that does not belong to `pdf`.
///
/// A foreign indirect handle stored into this tree would keep resolving
/// through its original document's resolver, mixing object graphs and
/// risking that document's object reference being serialized into `pdf`.
/// This mirrors the check `EmbeddedFileDocumentHelper::replace_embedded_file`
/// already performs before its own tree insert.
///
/// An indirect `value` is checked with the same canonical-registration
/// strictness `replace_embedded_file` uses. A direct `value` is checked with
/// [`ObjectHandle::belongs_exclusively_to_pdf`], not the shallower
/// [`ObjectHandle::belongs_to_pdf`]: a freshly constructed direct wrapper
/// (e.g. `ObjectHandle::dictionary`) always reports no owner of its own even
/// when it nests an already-indirect handle from a different `Pdf`, so only
/// a full descendant walk catches that case.
fn ensure_value_owned_by_pdf<R: Read + Seek>(pdf: &Pdf<R>, value: &ObjectHandle) -> Result<()> {
    if let Some(object_ref) = value.object_ref() {
        if !pdf.is_canonical_object_handle(value) {
            return Err(Error::Unsupported(
                "name/number tree value belongs to a different Pdf".to_string(),
            ));
        }
        debug_assert_eq!(value.object_ref(), Some(object_ref));
    } else if !value.belongs_exclusively_to_pdf(pdf.unique_id()) {
        return Err(Error::Unsupported(
            "name/number tree value belongs to a different Pdf".to_string(),
        ));
    }
    Ok(())
}

fn ensure_tree_root_pdf(root_pdf_id: Option<u64>, pdf_id: u64) -> Result<()> {
    if root_pdf_id.is_some_and(|owner| owner != pdf_id) {
        return Err(Error::Unsupported(
            "name/number tree root belongs to a different Pdf".to_string(),
        ));
    }
    Ok(())
}

/// A dictionary facade over one live `ObjectHandle`. It intentionally exposes
/// only live handle values so callers cannot accidentally detach a canonical
/// tree node from its owning document.
#[derive(Clone)]
struct LiveDictionary {
    handle: ObjectHandle,
}

impl LiveDictionary {
    fn new(handle: ObjectHandle) -> Result<Self> {
        handle.try_dereference()?;
        if handle.try_as_dictionary()?.is_none() {
            return Err(structural_error(None, "bad node"));
        }
        Ok(Self { handle })
    }

    fn actual_key(&self, key: &str) -> Vec<u8> {
        canonical_dictionary_key(key.as_bytes())
    }

    fn get(&self, key: &str) -> Option<ObjectHandle> {
        let key = self.actual_key(key);
        let value = self.handle.try_get_key(&key).ok();
        // qpdf dereferences the value before its array/type checks, so a
        // dangling reference has the same null outcome as a literal null.
        value.filter(|value| !value.is_null())
    }

    fn insert(&self, key: &str, value: ObjectHandle) -> Result<()> {
        let key = self.actual_key(key);
        self.handle.replace_key(&key, value)
    }

    fn remove(&self, key: &str) {
        let key = self.actual_key(key);
        self.handle.remove_key(&key);
    }

    fn contains(&self, key: &str) -> Result<bool> {
        let key = self.actual_key(key);
        self.handle.try_has_key(&key)
    }

    fn mark_dirty<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> Result<()> {
        pdf.mark_object_handle_dirty(&self.handle)
    }
}

#[derive(Clone, Debug)]
struct PathElement {
    node: NodeHandle,
    kid_number: usize,
}

pub(crate) struct NNTreeCursor<K: TreeKey> {
    path: Vec<PathElement>,
    leaf: Option<NodeHandle>,
    item_number: Option<usize>,
    current: Option<(K::Key, ObjectHandle)>,
    /// qpdf's iterator retains `QPDFObjectHandle`s owned by one `QPDF`
    /// (`QPDFObjectHandle.hh:852-872`, `NNTree.cc:30-73`). Keep the same
    /// document boundary when the Rust API receives a cursor and a `Pdf`
    /// separately.
    pdf_id: Option<u64>,
    marker: PhantomData<K>,
}

impl<K: TreeKey> NNTreeCursor<K> {
    fn empty() -> Self {
        Self {
            path: Vec::new(),
            leaf: None,
            item_number: None,
            current: None,
            pdf_id: None,
            marker: PhantomData,
        }
    }

    fn for_pdf(pdf_id: u64) -> Self {
        let mut cursor = Self::empty();
        cursor.pdf_id = Some(pdf_id);
        cursor
    }

    fn ensure_pdf<R: Read + Seek>(&mut self, pdf: &Pdf<R>) -> Result<()> {
        let pdf_id = pdf.unique_id();
        match self.pdf_id {
            None => self.pdf_id = Some(pdf_id),
            Some(owner) if owner == pdf_id => {}
            Some(_) => {
                return Err(Error::Unsupported(
                    "name/number tree cursor belongs to a different Pdf".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Whether traversal has selected an array slot.
    ///
    /// Malformed keys may leave a cursor positioned while [`Self::current`]
    /// is `None`; callers must not treat this as a usable key/value pair.
    pub(crate) fn positioned(&self) -> bool {
        self.item_number.is_some()
    }

    fn cloned_current(&self) -> Option<(K::Key, ObjectHandle)> {
        self.current.clone()
    }

    fn current_key(&self) -> Option<&K::Key> {
        self.current.as_ref().map(|(key, _value)| key)
    }

    fn clear_position(&mut self) {
        self.leaf = None;
        self.item_number = None;
        self.current = None;
    }

    fn same_position(&self, other: &Self) -> bool {
        if self.item_number.is_none() && other.item_number.is_none() {
            return true;
        }
        self.item_number == other.item_number
            && self.path.len() == other.path.len()
            && self
                .path
                .iter()
                .zip(&other.path)
                .all(|(left, right)| left.kid_number == right.kid_number)
    }
}

impl<K: TreeKey> Clone for NNTreeCursor<K> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            leaf: self.leaf.clone(),
            item_number: self.item_number,
            current: self.current.clone(),
            pdf_id: self.pdf_id,
            marker: PhantomData,
        }
    }
}

pub(crate) struct NNTree<K: TreeKey> {
    root: ObjectHandle,
    root_pdf_id: Option<u64>,
    auto_repair: bool,
    split_threshold: usize,
    max_depth: Option<usize>,
    marker: PhantomData<K>,
}

/// qpdf-compatible helper for a PDF name tree.
///
/// Name keys are supplied as UTF-8 bytes. Both byte slices and strings are
/// accepted through [`AsRef<[u8]>`].
pub struct NameTree {
    inner: NNTree<NameKey>,
    cursor_owner: Arc<()>,
}

impl NameTree {
    /// Wrap an existing name-tree root handle.
    ///
    /// The first PDF passed to an operation claims the handle's document
    /// boundary; subsequent operations reject handles from another PDF.
    pub fn new(root: ObjectHandle, auto_repair: bool) -> Self {
        Self {
            inner: NNTree::new(root, auto_repair),
            cursor_owner: Arc::new(()),
        }
    }

    /// Create an empty name tree with an indirect root.
    ///
    /// # Errors
    ///
    /// Returns an error when the PDF object-number space is exhausted.
    pub fn new_empty<R: Read + Seek>(pdf: &mut Pdf<R>, auto_repair: bool) -> Result<Self> {
        let root = ObjectHandle::dictionary(vec![(
            canonical_dictionary_key(NameKey::ITEMS_KEY.as_bytes()),
            ObjectHandle::array(Vec::new()),
        )]);
        let root = pdf.make_indirect_from_object_handle(root)?;
        Ok(Self::new(root, auto_repair))
    }

    /// Return the live root handle, matching qpdf's `getObjectHandle`.
    pub fn get_object_handle(&self) -> ObjectHandle {
        self.inner.root.clone()
    }

    /// Return an invalid cursor representing the position past the tree.
    pub fn end(&self) -> NameTreeCursor {
        NameTreeCursor {
            inner: self.inner.end(),
            owner: Arc::clone(&self.cursor_owner),
        }
    }

    /// Return a cursor positioned at the first entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed.
    pub fn begin<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NameTreeCursor> {
        self.inner.begin(pdf).map(|inner| NameTreeCursor {
            inner,
            owner: Arc::clone(&self.cursor_owner),
        })
    }

    /// Return a cursor positioned at the last entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed.
    pub fn last<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NameTreeCursor> {
        self.inner.last(pdf).map(|inner| NameTreeCursor {
            inner,
            owner: Arc::clone(&self.cursor_owner),
        })
    }

    /// Find `key`, optionally returning the closest lower entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or searched.
    pub fn find<R: Read + Seek, K: AsRef<[u8]>>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: K,
        return_previous_if_missing: bool,
    ) -> Result<NameTreeCursor> {
        let key = key.as_ref().to_vec();
        self.inner
            .find(pdf, &key, return_previous_if_missing)
            .map(|inner| NameTreeCursor {
                inner,
                owner: Arc::clone(&self.cursor_owner),
            })
    }

    /// Insert or replace an entry and return a cursor positioned at it.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or mutated.
    pub fn insert<R: Read + Seek, K: AsRef<[u8]>>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: K,
        value: ObjectHandle,
    ) -> Result<NameTreeCursor> {
        self.inner
            .insert(pdf, key.as_ref().to_vec(), value)
            .map(|inner| NameTreeCursor {
                inner,
                owner: Arc::clone(&self.cursor_owner),
            })
    }

    /// Return whether the tree contains an explicit entry for `key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or searched.
    pub fn has_name<R: Read + Seek, K: AsRef<[u8]>>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: K,
    ) -> Result<bool> {
        Ok(self.find_object(pdf, key)?.is_some())
    }

    /// Find the value stored at `key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or searched.
    pub fn find_object<R: Read + Seek, K: AsRef<[u8]>>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: K,
    ) -> Result<Option<ObjectHandle>> {
        Ok(self
            .inner
            .find(pdf, &key.as_ref().to_vec(), false)?
            .cloned_current()
            .map(|(_, value)| value))
    }

    /// Remove the entry at `key`, returning its former value.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or mutated.
    pub fn remove<R: Read + Seek, K: AsRef<[u8]>>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: K,
    ) -> Result<Option<ObjectHandle>> {
        self.inner.remove(pdf, &key.as_ref().to_vec())
    }

    /// Materialize the tree as a sorted map.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed.
    pub fn as_map<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> {
        let mut result = BTreeMap::new();
        let mut cursor = self.inner.begin(pdf)?;
        if cursor.positioned() && cursor.cloned_current().is_none() {
            return Err(Error::Internal(
                "attempt made to dereference an invalid name/number tree iterator".to_string(),
            ));
        }
        while let Some((key, value)) = cursor.cloned_current() {
            result.insert(key, value);
            self.inner.next(pdf, &mut cursor)?;
        }
        Ok(result)
    }

    /// Set the node split threshold.
    ///
    /// This exists for qpdf-compatible tests; production callers normally
    /// retain the default threshold of 32.
    pub fn set_split_threshold(&mut self, threshold: usize) {
        self.inner.set_split_threshold(threshold);
    }

    /// Bound `/Kids` chain traversal to `max_depth` levels.
    ///
    /// Unbounded by default; a caller reading an untrusted document should
    /// set this (e.g. to [`DEFAULT_MAX_TREE_DEPTH`]) to reject a
    /// pathologically deep tree with [`crate::Error::Unsupported`] instead
    /// of recursing without bound.
    pub fn set_max_depth(&mut self, max_depth: usize) {
        self.inner.max_depth = Some(max_depth);
    }
}

/// Cursor over a [`NameTree`] whose values retain live `ObjectHandle` identity.
pub struct NameTreeCursor {
    inner: NNTreeCursor<NameKey>,
    owner: Arc<()>,
}

impl Clone for NameTreeCursor {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            owner: Arc::clone(&self.owner),
        }
    }
}

impl PartialEq for NameTreeCursor {
    fn eq(&self, other: &Self) -> bool {
        self.inner.same_position(&other.inner)
    }
}

impl Eq for NameTreeCursor {}

impl NameTreeCursor {
    /// Whether the cursor points to a valid key/value pair.
    pub fn valid(&self) -> bool {
        self.inner.cloned_current().is_some()
    }

    /// Return a clone of the current key/value pair.
    pub fn current(&self) -> Option<(Vec<u8>, ObjectHandle)> {
        self.inner.cloned_current()
    }

    /// Advance to the next entry.
    ///
    /// Advancing an end cursor selects the first entry, matching qpdf.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed, or the
    /// cursor belongs to another tree.
    pub fn next<R: Read + Seek>(&mut self, tree: &mut NameTree, pdf: &mut Pdf<R>) -> Result<()> {
        self.ensure_owner(tree)?;
        tree.inner.next(pdf, &mut self.inner)
    }

    /// Move to the previous entry.
    ///
    /// Moving an end cursor backward selects the last entry, matching qpdf.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed, or the
    /// cursor belongs to another tree.
    pub fn previous<R: Read + Seek>(
        &mut self,
        tree: &mut NameTree,
        pdf: &mut Pdf<R>,
    ) -> Result<()> {
        self.ensure_owner(tree)?;
        tree.inner.previous(pdf, &mut self.inner)
    }

    /// Insert an entry immediately after the cursor and select it.
    ///
    /// This fast path does not validate key ordering, matching qpdf.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or mutated, or the
    /// cursor belongs to another tree.
    pub fn insert_after<R: Read + Seek, K: AsRef<[u8]>>(
        &mut self,
        tree: &mut NameTree,
        pdf: &mut Pdf<R>,
        key: K,
        value: ObjectHandle,
    ) -> Result<()> {
        self.ensure_owner(tree)?;
        tree.inner
            .insert_after(pdf, &mut self.inner, key.as_ref().to_vec(), value)
    }

    /// Remove the current entry and advance to the next entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or mutated, the
    /// cursor belongs to another tree, or the cursor is invalid.
    pub fn remove<R: Read + Seek>(&mut self, tree: &mut NameTree, pdf: &mut Pdf<R>) -> Result<()> {
        self.ensure_owner(tree)?;
        if !self.valid() {
            return Err(Error::Unsupported(
                "attempted to remove an invalid name-tree cursor".to_string(),
            ));
        }
        tree.inner.remove_at(pdf, &mut self.inner).map(|_| ())
    }

    fn ensure_owner(&self, tree: &NameTree) -> Result<()> {
        if Arc::ptr_eq(&self.owner, &tree.cursor_owner) {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "name-tree cursor belongs to a different tree".to_string(),
            ))
        }
    }
}

/// qpdf-compatible helper for a PDF number tree.
///
/// The helper owns the tree root while all indirect nodes remain in the
/// supplied [`Pdf`]. Mutating a direct root therefore updates this helper's
/// owned root; use the helper for subsequent operations.
pub struct NumberTree {
    inner: NNTree<NumberKey>,
    cursor_owner: Arc<()>,
}

impl NumberTree {
    /// Wrap an existing number-tree root handle.
    ///
    /// The first PDF passed to an operation claims the handle's document
    /// boundary; subsequent operations reject handles from another PDF.
    pub fn new(root: ObjectHandle, auto_repair: bool) -> Self {
        Self {
            inner: NNTree::new(root, auto_repair),
            cursor_owner: Arc::new(()),
        }
    }

    /// Create an empty number tree with an indirect root.
    ///
    /// # Errors
    ///
    /// Returns an error when the PDF object-number space is exhausted.
    pub fn new_empty<R: Read + Seek>(pdf: &mut Pdf<R>, auto_repair: bool) -> Result<Self> {
        let root = ObjectHandle::dictionary(vec![(
            canonical_dictionary_key(NumberKey::ITEMS_KEY.as_bytes()),
            ObjectHandle::array(Vec::new()),
        )]);
        let root = pdf.make_indirect_from_object_handle(root)?;
        Ok(Self::new(root, auto_repair))
    }

    /// Return the live root handle, matching qpdf's `getObjectHandle`.
    pub fn get_object_handle(&self) -> ObjectHandle {
        self.inner.root.clone()
    }

    /// Return an invalid cursor representing the position past the tree.
    pub fn end(&self) -> NumberTreeCursor {
        NumberTreeCursor {
            inner: self.inner.end(),
            owner: Arc::clone(&self.cursor_owner),
        }
    }

    /// Return a cursor positioned at the first entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed.
    pub fn begin<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NumberTreeCursor> {
        self.inner.begin(pdf).map(|inner| NumberTreeCursor {
            inner,
            owner: Arc::clone(&self.cursor_owner),
        })
    }

    /// Return a cursor positioned at the last entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed.
    pub fn last<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NumberTreeCursor> {
        self.inner.last(pdf).map(|inner| NumberTreeCursor {
            inner,
            owner: Arc::clone(&self.cursor_owner),
        })
    }

    /// Find `key`, optionally returning the closest lower entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or searched.
    pub fn find<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: i64,
        return_previous_if_missing: bool,
    ) -> Result<NumberTreeCursor> {
        self.inner
            .find(pdf, &key, return_previous_if_missing)
            .map(|inner| NumberTreeCursor {
                inner,
                owner: Arc::clone(&self.cursor_owner),
            })
    }

    /// Insert or replace an entry and return a cursor positioned at it.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or mutated.
    pub fn insert<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: i64,
        value: ObjectHandle,
    ) -> Result<NumberTreeCursor> {
        self.inner
            .insert(pdf, key, value)
            .map(|inner| NumberTreeCursor {
                inner,
                owner: Arc::clone(&self.cursor_owner),
            })
    }

    /// Find the value stored at `key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or searched.
    pub fn find_object<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: i64,
    ) -> Result<Option<ObjectHandle>> {
        Ok(self
            .inner
            .find(pdf, &key, false)?
            .cloned_current()
            .map(|(_, value)| value))
    }

    /// Return the smallest index, or `0` when the tree is empty.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed.
    pub fn min<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<i64> {
        Ok(self
            .inner
            .begin(pdf)?
            .cloned_current()
            .map_or(0, |(key, _)| key))
    }

    /// Return the largest index, or `0` when the tree is empty.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed.
    pub fn max<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<i64> {
        Ok(self
            .inner
            .last(pdf)?
            .cloned_current()
            .map_or(0, |(key, _)| key))
    }

    /// Return whether the tree contains an explicit entry at `key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or searched.
    pub fn has_index<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>, key: i64) -> Result<bool> {
        Ok(self.find_object(pdf, key)?.is_some())
    }

    /// Find the value at `key`, or the closest value below it.
    ///
    /// The returned offset is `key - actual_key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be searched or the signed offset
    /// cannot be represented as an [`i64`].
    pub fn find_object_at_or_below<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: i64,
    ) -> Result<Option<(ObjectHandle, i64)>> {
        let Some((actual_key, value)) = self.inner.find(pdf, &key, true)?.cloned_current() else {
            return Ok(None);
        };
        let offset = key.checked_sub(actual_key).ok_or_else(|| {
            Error::Unsupported("number-tree at-or-below offset overflow".to_string())
        })?;
        Ok(Some((value, offset)))
    }

    /// Remove the entry at `key`, returning its former value.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or mutated.
    pub fn remove<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: i64,
    ) -> Result<Option<ObjectHandle>> {
        self.inner.remove(pdf, &key)
    }

    /// Materialize the tree as a sorted map.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed.
    pub fn as_map<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> Result<BTreeMap<i64, ObjectHandle>> {
        let mut result = BTreeMap::new();
        let mut cursor = self.inner.begin(pdf)?;
        if cursor.positioned() && cursor.cloned_current().is_none() {
            return Err(Error::Internal(
                "attempt made to dereference an invalid name/number tree iterator".to_string(),
            ));
        }
        while let Some((key, value)) = cursor.cloned_current() {
            result.insert(key, value);
            self.inner.next(pdf, &mut cursor)?;
        }
        Ok(result)
    }

    /// Set the node split threshold.
    ///
    /// This exists for qpdf-compatible tests; production callers normally
    /// retain the default threshold of 32.
    pub fn set_split_threshold(&mut self, threshold: usize) {
        self.inner.set_split_threshold(threshold);
    }

    /// Bound `/Kids` chain traversal to `max_depth` levels.
    ///
    /// Unbounded by default; a caller reading an untrusted document should
    /// set this (e.g. to [`DEFAULT_MAX_TREE_DEPTH`]) to reject a
    /// pathologically deep tree with [`crate::Error::Unsupported`] instead
    /// of recursing without bound.
    pub fn set_max_depth(&mut self, max_depth: usize) {
        self.inner.max_depth = Some(max_depth);
    }
}

/// Cursor over a [`NumberTree`].
pub struct NumberTreeCursor {
    inner: NNTreeCursor<NumberKey>,
    owner: Arc<()>,
}

impl Clone for NumberTreeCursor {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            owner: Arc::clone(&self.owner),
        }
    }
}

impl PartialEq for NumberTreeCursor {
    fn eq(&self, other: &Self) -> bool {
        self.inner.same_position(&other.inner)
    }
}

impl Eq for NumberTreeCursor {}

impl NumberTreeCursor {
    /// Whether the cursor points to a valid key/value pair.
    pub fn valid(&self) -> bool {
        self.inner.cloned_current().is_some()
    }

    /// Return a clone of the current key/value pair.
    pub fn current(&self) -> Option<(i64, ObjectHandle)> {
        self.inner.cloned_current()
    }

    /// Advance to the next entry.
    ///
    /// Advancing an end cursor selects the first entry, matching qpdf.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed, or the
    /// cursor belongs to another tree.
    pub fn next<R: Read + Seek>(&mut self, tree: &mut NumberTree, pdf: &mut Pdf<R>) -> Result<()> {
        self.ensure_owner(tree)?;
        tree.inner.next(pdf, &mut self.inner)
    }

    /// Move to the previous entry.
    ///
    /// Moving an end cursor backward selects the last entry, matching qpdf.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or traversed, or the
    /// cursor belongs to another tree.
    pub fn previous<R: Read + Seek>(
        &mut self,
        tree: &mut NumberTree,
        pdf: &mut Pdf<R>,
    ) -> Result<()> {
        self.ensure_owner(tree)?;
        tree.inner.previous(pdf, &mut self.inner)
    }

    /// Insert an entry immediately after the cursor and select it.
    ///
    /// This fast path does not validate key ordering, matching qpdf.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or mutated, or the
    /// cursor belongs to another tree.
    pub fn insert_after<R: Read + Seek>(
        &mut self,
        tree: &mut NumberTree,
        pdf: &mut Pdf<R>,
        key: i64,
        value: ObjectHandle,
    ) -> Result<()> {
        self.ensure_owner(tree)?;
        tree.inner.insert_after(pdf, &mut self.inner, key, value)
    }

    /// Remove the current entry and advance to the next entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the tree cannot be resolved or mutated, the
    /// cursor belongs to another tree, or the cursor is invalid.
    pub fn remove<R: Read + Seek>(
        &mut self,
        tree: &mut NumberTree,
        pdf: &mut Pdf<R>,
    ) -> Result<()> {
        self.ensure_owner(tree)?;
        if !self.valid() {
            return Err(Error::Unsupported(
                "attempted to remove an invalid number-tree cursor".to_string(),
            ));
        }
        tree.inner.remove_at(pdf, &mut self.inner).map(|_| ())
    }

    fn ensure_owner(&self, tree: &NumberTree) -> Result<()> {
        if Arc::ptr_eq(&self.owner, &tree.cursor_owner) {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "number-tree cursor belongs to a different tree".to_string(),
            ))
        }
    }
}

impl<K: TreeKey> NNTree<K> {
    fn new(root: ObjectHandle, auto_repair: bool) -> Self {
        Self {
            root,
            root_pdf_id: None,
            auto_repair,
            split_threshold: DEFAULT_SPLIT_THRESHOLD,
            max_depth: None,
            marker: PhantomData,
        }
    }

    fn ensure_root<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<ObjectHandle> {
        let pdf_id = pdf.unique_id();
        if self.root_pdf_id.is_none() {
            if !self.root.belongs_exclusively_to_pdf(pdf_id) {
                return Err(Error::Unsupported(
                    "name/number tree root belongs to a different Pdf".to_string(),
                ));
            }
            self.root.claim_tree_pdf(pdf_id)?;
            self.root_pdf_id = Some(pdf_id);
        } else {
            ensure_tree_root_pdf(self.root_pdf_id, pdf_id)?;
        }
        Ok(self.root.clone())
    }

    pub(crate) fn begin<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NNTreeCursor<K>> {
        let mut cursor = NNTreeCursor::for_pdf(pdf.unique_id());
        let root = self.root_node(pdf)?;
        self.descend(pdf, &mut cursor, root, true, true)?;
        Ok(cursor)
    }

    pub(crate) fn end(&self) -> NNTreeCursor<K> {
        NNTreeCursor::empty()
    }

    pub(crate) fn last<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NNTreeCursor<K>> {
        let mut cursor = NNTreeCursor::for_pdf(pdf.unique_id());
        let root = self.root_node(pdf)?;
        self.descend(pdf, &mut cursor, root, false, true)?;
        Ok(cursor)
    }

    pub(crate) fn next<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<()> {
        cursor.ensure_pdf(pdf)?;
        self.increment(pdf, cursor, false)
    }

    pub(crate) fn previous<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<()> {
        cursor.ensure_pdf(pdf)?;
        self.increment(pdf, cursor, true)
    }

    pub(crate) fn find<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
        return_previous_if_missing: bool,
    ) -> Result<NNTreeCursor<K>> {
        match self.find_internal(pdf, key, return_previous_if_missing) {
            Ok(cursor) => Ok(cursor),
            Err(Error::Parse { message, .. }) if self.auto_repair => {
                let root = self.root_node(pdf)?;
                // qpdf NNTreeImpl::find appends QPDFExc::what(). The
                // qpdf-shaped structural text lives in Error::Parse::message;
                // Error's Display adds flpdf's `parse error at byte` wrapper.
                self.warn(
                    pdf,
                    &root,
                    format!("attempting to repair after error: {message}"),
                )?;
                self.repair(pdf)?;
                self.find_internal(pdf, key, return_previous_if_missing)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn set_split_threshold(&mut self, threshold: usize) {
        self.split_threshold = threshold;
    }

    fn insert<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: K::Key,
        value: ObjectHandle,
    ) -> Result<NNTreeCursor<K>> {
        ensure_value_owned_by_pdf(pdf, &value)?;
        let mut allocator = ObjectAllocator::default();
        self.insert_with_allocator(pdf, &mut allocator, key, value)
    }

    fn insert_with_allocator<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        key: K::Key,
        value: ObjectHandle,
    ) -> Result<NNTreeCursor<K>> {
        self.insert_pair_with_allocator(pdf, allocator, K::to_handle(&key), value)
    }

    fn insert_resolved_with_allocator<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        key: K::Key,
        raw_key: ObjectHandle,
        value: ObjectHandle,
    ) -> Result<NNTreeCursor<K>> {
        let mut cursor = self.find(pdf, &key, true)?;
        if !cursor.positioned() {
            return self.insert_first(pdf, allocator, raw_key, value);
        }

        let is_exact = cursor
            .current_key()
            .is_some_and(|current_key| K::compare(&key, current_key) == Ordering::Equal);
        if is_exact {
            let leaf = cursor.leaf.clone().expect("valid cursor has a leaf");
            let item_number = cursor.item_number.expect("valid cursor has an item");
            let dictionary = self.load_node(pdf, &leaf)?;
            // cov:ignore-start: find just returned this leaf with an items array and no callback can mutate it here
            let Some(mut items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())?
            else {
                return Err(structural_error(
                    leaf.diagnostic_ref(),
                    "node contains no items array",
                ));
            };
            // cov:ignore-end
            items.values[item_number + 1] = value;
            items.store(pdf)?;
            self.update_current(pdf, &mut cursor, false)?;
        } else {
            self.insert_after_with_allocator(pdf, allocator, &mut cursor, raw_key, value)?;
        }
        Ok(cursor)
    }

    fn insert_after<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        key: K::Key,
        value: ObjectHandle,
    ) -> Result<()> {
        cursor.ensure_pdf(pdf)?;
        ensure_value_owned_by_pdf(pdf, &value)?;
        let mut allocator = ObjectAllocator::default();
        self.insert_after_with_allocator(pdf, &mut allocator, cursor, K::to_handle(&key), value)
    }

    fn insert_after_with_allocator<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        cursor: &mut NNTreeCursor<K>,
        raw_key: ObjectHandle,
        value: ObjectHandle,
    ) -> Result<()> {
        if !cursor.positioned() {
            *cursor = self.insert_first(pdf, allocator, raw_key, value)?;
            return Ok(());
        }

        let leaf = cursor.leaf.clone().expect("valid cursor has a leaf");
        let item_number = cursor.item_number.expect("valid cursor has an item");
        let dictionary = self.load_node(pdf, &leaf)?;
        let Some(mut items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())? else {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "node contains no items array",
            ));
        };
        if items.values.len() < item_number + 2 {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "insert: items array is too short",
            ));
        }
        self.ensure_split_allocations_available(pdf, allocator, cursor, items.values.len() + 2)?;
        items.values.insert(item_number + 2, raw_key);
        items.values.insert(item_number + 3, value);
        items.store(pdf)?;
        self.reset_limits(pdf, cursor, leaf, cursor.path.len().checked_sub(1))?;
        cursor.item_number = Some(item_number + 2);
        self.update_current(pdf, cursor, false)?;
        let leaf = cursor.leaf.clone().expect("inserted item has a leaf");
        self.split_node_live(pdf, cursor, leaf, cursor.path.len().checked_sub(1))
    }

    fn remove<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
    ) -> Result<Option<ObjectHandle>> {
        let mut cursor = self.find(pdf, key, false)?;
        let Some((_, value)) = cursor.cloned_current() else {
            return Ok(None);
        };
        self.remove_at(pdf, &mut cursor)?;
        Ok(Some(value))
    }

    fn remove_at<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<Option<ObjectHandle>> {
        cursor.ensure_pdf(pdf)?;
        self.remove_at_inner(pdf, cursor)
    }

    fn remove_at_inner<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<Option<ObjectHandle>> {
        let Some((_, removed_value)) = cursor.cloned_current() else {
            return Ok(None);
        };
        let leaf = cursor.leaf.clone().expect("valid cursor has a leaf");
        let item_number = cursor.item_number.expect("valid cursor has an item");
        let dictionary = self.load_node(pdf, &leaf)?;
        let Some(mut items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())? else {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "node contains no items array",
            ));
        };
        if item_number + 2 > items.values.len() {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "found short items array while removing an item",
            ));
        }
        items.values.drain(item_number..item_number + 2);
        let remaining = items.values.len();
        items.store(pdf)?;

        if remaining > 0 {
            if item_number == 0 || item_number == remaining {
                self.reset_limits(pdf, cursor, leaf.clone(), cursor.path.len().checked_sub(1))?;
            }
            if item_number == remaining {
                cursor.item_number = item_number.checked_sub(2);
                self.update_current(pdf, cursor, false)?;
                self.next(pdf, cursor)?;
            } else {
                self.update_current(pdf, cursor, false)?;
            }
            return Ok(Some(removed_value));
        }

        if cursor.path.is_empty() {
            cursor.item_number = None;
            cursor.current = None;
            self.reset_limits(pdf, cursor, leaf, None)?;
            return Ok(Some(removed_value));
        }

        self.remove_empty_leaf(pdf, cursor)?;
        Ok(Some(removed_value))
    }

    fn insert_first<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        raw_key: ObjectHandle,
        value: ObjectHandle,
    ) -> Result<NNTreeCursor<K>> {
        let mut cursor = self.begin(pdf)?;
        let leaf = cursor
            .leaf
            .clone()
            .ok_or_else(|| structural_error(None, "unable to find a valid items node"))?;
        let dictionary = self.load_node(pdf, &leaf)?;
        // cov:ignore-start: begin returns an empty cursor leaf only after observing its items array
        let Some(mut items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())? else {
            return Err(structural_error(
                self.root_node(pdf)?.diagnostic_ref(),
                "unable to find a valid items node",
            ));
        };
        // cov:ignore-end
        self.ensure_split_allocations_available(pdf, allocator, &cursor, items.values.len() + 2)?;
        items.values.insert(0, raw_key);
        items.values.insert(1, value);
        items.store(pdf)?;
        cursor.item_number = Some(0);
        self.update_current(pdf, &mut cursor, true)?;
        let parent_index = cursor.path.len().checked_sub(1);
        self.reset_limits(pdf, &cursor, leaf.clone(), parent_index)?;
        self.split_node_live(pdf, &mut cursor, leaf, parent_index)?;
        Ok(cursor)
    }

    fn repair<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<()> {
        let replacement_root = ObjectHandle::dictionary(vec![(
            canonical_dictionary_key(K::ITEMS_KEY.as_bytes()),
            ObjectHandle::array(Vec::new()),
        )]);
        let mut replacement = NNTree::<K>::new(replacement_root, false);
        replacement.root_pdf_id = Some(pdf.unique_id());

        let mut allocator = ObjectAllocator::default();
        let mut cursor = self.begin(pdf)?;
        while cursor.positioned() {
            let leaf = cursor
                .leaf
                .clone()
                .expect("a positioned NNTree cursor retains a leaf");
            let dictionary = self.load_node(pdf, &leaf)?;
            let items = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())?
                .expect("a positioned NNTree cursor retains an items array");
            let item_number = cursor
                .item_number
                .expect("a positioned NNTree cursor retains an item number");
            let key = items.values[item_number].clone();
            let value = items.values[item_number + 1].clone();
            replacement.insert_pair_with_allocator(pdf, &mut allocator, key, value)?;
            self.increment(pdf, &mut cursor, false)?;
        }

        let replacement = replacement.ensure_root(pdf)?;
        self.replace_root_contents(pdf, replacement)
    }

    fn ensure_split_allocations_available<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &ObjectAllocator,
        cursor: &NNTreeCursor<K>,
        item_array_len: usize,
    ) -> Result<()> {
        if item_array_len <= 2 * self.split_threshold {
            return Ok(());
        }

        // qpdf's split allocation calls nextObjGen(), which first prepares the
        // canonical cache through fixDanglingReferences. Do that before the
        // caller mutates the live items array so parser-discovered dangling
        // references participate in the same signed object-number preflight.
        pdf.next_obj_gen()?;

        let mut allocations = 0usize;
        let mut parent_index = cursor.path.len().checked_sub(1);
        loop {
            let Some(index) = parent_index else {
                allocations += 2;
                break;
            };
            allocations += 1;

            let parent_handle = &cursor.path[index].node;
            let parent = self.load_node(pdf, parent_handle)?;
            // cov:ignore-start: the cursor path was built from this parent's /Kids array
            let Some(kids) = resolved_array(pdf, parent.get("Kids").as_ref())? else {
                return Err(structural_error(
                    parent_handle.diagnostic_ref(),
                    "node is missing /Kids",
                ));
            };
            // cov:ignore-end
            if kids.values.len() < self.split_threshold {
                break;
            }
            parent_index = index.checked_sub(1);
        }

        allocator.ensure_available(pdf, allocations)
    }

    fn replace_root_contents<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        replacement: ObjectHandle,
    ) -> Result<()> {
        let root = self.root_node(pdf)?;
        let current = self.load_node(pdf, &root)?;
        let replacement = self.load_node(pdf, &NodeHandle::root(replacement))?;
        current.remove("Kids");
        current.remove(K::ITEMS_KEY);
        if let Some(kids) = replacement.get("Kids") {
            current.insert("Kids", kids)?;
        }
        if let Some(items) = replacement.get(K::ITEMS_KEY) {
            current.insert(K::ITEMS_KEY, items)?;
        }
        current.mark_dirty(pdf)?;
        Ok(())
    }

    fn split_node_live<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        mut node: NodeHandle,
        mut parent_index: Option<usize>,
    ) -> Result<()> {
        let dictionary = self.load_node(pdf, &node)?;
        let kids = resolved_array(pdf, dictionary.get("Kids").as_ref())?;
        let items = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())?;
        let (array_key, array, threshold, is_leaf) = if let Some(kids) = kids {
            if kids.values.is_empty() {
                return Ok(());
            }
            ("Kids", kids, self.split_threshold, false)
        } else if let Some(items) = items {
            if items.values.is_empty() {
                return Ok(());
            }
            (K::ITEMS_KEY, items, 2 * self.split_threshold, true)
        } else {
            return Err(structural_error(
                node.diagnostic_ref(),
                "split called on invalid node",
            ));
        };
        if array.values.len() <= threshold {
            return Ok(());
        }

        let is_root = parent_index.is_none();
        if is_root {
            let first_dictionary = ObjectHandle::dictionary(vec![(
                canonical_dictionary_key(array_key.as_bytes()),
                array.handle.clone(),
            )]);
            let first_object = pdf.make_indirect_from_object_handle(first_dictionary)?;
            let first_ref = first_object
                .object_ref()
                .expect("canonical allocation returns an indirect node");
            let first_handle = NodeHandle::indirect(first_ref, first_object);

            let root = self.load_node(pdf, &node)?;
            root.remove("Limits");
            root.remove(K::ITEMS_KEY);
            root.insert("Kids", ObjectHandle::array(vec![first_handle.handle()]))?; // cov:ignore: split allocates the replacement node in this same PDF
            root.mark_dirty(pdf)?;

            if is_leaf {
                cursor.leaf = Some(first_handle.clone());
            } else if let Some(first_path) = cursor.path.first_mut() {
                first_path.node = first_handle.clone();
            }
            cursor.path.insert(
                0,
                PathElement {
                    node: node.clone(),
                    kid_number: 0,
                },
            );
            parent_index = Some(0);
            node = first_handle;
        }

        let parent_index = parent_index.expect("root was normalized above");
        let first_dictionary = self.load_node(pdf, &node)?;
        // cov:ignore-start: array_key was selected from this same node before root normalization
        let Some(mut first_half) = resolved_array(pdf, first_dictionary.get(array_key).as_ref())?
        else {
            return Err(structural_error(
                node.diagnostic_ref(),
                format!("/{array_key} is not an array"),
            ));
        };
        // cov:ignore-end
        // Item arrays alternate key/value pairs; /Kids entries are independent.
        let midpoint = first_half.values.len() / 2;
        let start_index = if is_leaf { midpoint & !1 } else { midpoint };
        let second_half = first_half.values.split_off(start_index);
        first_half.store(pdf)?;
        self.reset_limits(pdf, cursor, node.clone(), Some(parent_index))?;

        let second_dictionary = ObjectHandle::dictionary(vec![(
            canonical_dictionary_key(array_key.as_bytes()),
            ObjectHandle::array(second_half),
        )]);
        let second_object = pdf.make_indirect_from_object_handle(second_dictionary)?;
        let second_ref = second_object
            .object_ref()
            .expect("canonical allocation returns an indirect node");
        let second_handle = NodeHandle::indirect(second_ref, second_object);
        self.reset_limits(pdf, cursor, second_handle.clone(), Some(parent_index))?;

        let parent_handle = cursor.path[parent_index].node.clone();
        let parent = self.load_node(pdf, &parent_handle)?;
        // cov:ignore-start: split cursor path was built from this parent Kids array
        let Some(mut parent_kids) = resolved_array(pdf, parent.get("Kids").as_ref())? else {
            return Err(structural_error(
                parent_handle.diagnostic_ref(),
                "node is missing /Kids",
            ));
        };
        // cov:ignore-end
        let first_kid_index = cursor.path[parent_index].kid_number;
        parent_kids
            .values
            .insert(first_kid_index + 1, second_handle.handle());
        parent_kids.store(pdf)?;

        let old_index = if is_leaf {
            cursor.item_number.expect("split cursor points to an item")
        } else {
            cursor.path[parent_index + 1].kid_number
        };
        if old_index >= start_index {
            cursor.path[parent_index].kid_number += 1;
            if is_leaf {
                cursor.leaf = Some(second_handle);
                cursor.item_number = Some(old_index - start_index);
                self.update_current(pdf, cursor, false)?;
            } else {
                cursor.path[parent_index + 1].node = second_handle;
                cursor.path[parent_index + 1].kid_number -= start_index;
            }
        } // cov:ignore: LLVM assigns an uncovered region to this delimiter although both cursor-adjustment arms above execute

        if !is_root {
            let parent_handle = cursor.path[parent_index].node.clone();
            let grandparent_index = parent_index.checked_sub(1);
            self.reset_limits(pdf, cursor, parent_handle.clone(), grandparent_index)?;
            self.split_node_live(pdf, cursor, parent_handle, grandparent_index)?;
            // cov:ignore: LLVM assigns the covered recursive call terminator a zero-count region
        }
        Ok(())
    }

    fn reset_limits<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &NNTreeCursor<K>,
        mut node: NodeHandle,
        mut parent_index: Option<usize>,
    ) -> Result<()> {
        loop {
            let dictionary = self.load_node(pdf, &node)?;
            let Some(index) = parent_index else {
                dictionary.remove("Limits");
                dictionary.mark_dirty(pdf)?;
                return Ok(());
            };

            let new_limits = self.edge_limits(pdf, &dictionary)?;
            let changed = match new_limits {
                Some((first, last)) => {
                    let old_limits = resolved_array(pdf, dictionary.get("Limits").as_ref())?;
                    let unchanged = if let Some(old_limits) = old_limits {
                        if old_limits.values.len() == 2 {
                            let old_first = resolved_key::<K, _>(pdf, &old_limits.values[0])?;
                            let old_last = resolved_key::<K, _>(pdf, &old_limits.values[1])?;
                            let new_first = resolved_key::<K, _>(pdf, &first)?;
                            let new_last = resolved_key::<K, _>(pdf, &last)?;
                            matches!(
                                (old_first, old_last, new_first, new_last),
                                (
                                    Some(old_first),
                                    Some(old_last),
                                    Some(new_first),
                                    Some(new_last)
                                ) if K::compare(&old_first, &new_first) == Ordering::Equal
                                    && K::compare(&old_last, &new_last) == Ordering::Equal
                            )
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if unchanged {
                        false
                    } else {
                        dictionary.insert("Limits", ObjectHandle::array(vec![first, last]))?;
                        dictionary.mark_dirty(pdf)?;
                        true
                    }
                }
                None => {
                    self.warn(pdf, &node, "unable to determine limits")?;
                    true
                }
            };

            if !changed || index == 0 {
                return Ok(());
            }
            node = cursor.path[index].node.clone();
            parent_index = index.checked_sub(1);
        }
    }

    fn edge_limits<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        dictionary: &LiveDictionary,
    ) -> Result<Option<(ObjectHandle, ObjectHandle)>> {
        if let Some(items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())? {
            if items.values.len() >= 2 {
                return Ok(Some((
                    items.values[0].clone(),
                    items.values[(items.values.len() - 1) & !1].clone(),
                )));
            }
        }
        if let Some(kids) = resolved_array(pdf, dictionary.get("Kids").as_ref())? {
            if let (Some(first_kid), Some(last_kid)) = (kids.values.first(), kids.values.last()) {
                first_kid.try_dereference()?;
                let first_kid = first_kid.clone();
                if first_kid.try_as_dictionary()?.is_none() {
                    return Ok(None);
                }
                last_kid.try_dereference()?;
                let last_kid = last_kid.clone();
                if last_kid.try_as_dictionary()?.is_none() {
                    return Ok(None);
                }
                let first = LiveDictionary::new(first_kid.clone())?;
                let last = LiveDictionary::new(last_kid.clone())?;
                let Some(first_limits) = resolved_array(pdf, first.get("Limits").as_ref())? else {
                    return Ok(None);
                };
                let Some(last_limits) = resolved_array(pdf, last.get("Limits").as_ref())? else {
                    return Ok(None);
                };
                if first_limits.values.len() >= 2 && last_limits.values.len() >= 2 {
                    return Ok(Some((
                        first_limits.values[0].clone(),
                        last_limits.values[1].clone(),
                    )));
                } // cov:ignore: LLVM assigns the covered successful edge-limit return region to this delimiter
            } // cov:ignore: LLVM assigns the covered non-empty Kids branch region to this delimiter
        }
        Ok(None)
    }

    fn remove_empty_leaf<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<()> {
        loop {
            let path_index = cursor.path.len() - 1;
            let parent_handle = cursor.path[path_index].node.clone();
            let removed_kid = cursor.path[path_index].kid_number;
            let parent = self.load_node(pdf, &parent_handle)?;
            let Some(mut kids) = resolved_array(pdf, parent.get("Kids").as_ref())? else {
                return Err(structural_error(
                    parent_handle.diagnostic_ref(),
                    "node is missing /Kids",
                ));
            };
            kids.values.remove(removed_kid);
            let remaining_kids = kids.values.len();
            let remaining_kid_values = kids.values.clone();
            kids.store(pdf)?;

            if remaining_kids > 0 {
                if removed_kid == 0 || removed_kid == remaining_kids {
                    self.reset_limits(
                        pdf,
                        cursor,
                        parent_handle.clone(),
                        path_index.checked_sub(1),
                    )?; // cov:ignore: LLVM maps this covered multi-line call terminator to a zero-count region
                } // cov:ignore: LLVM maps this covered limit-reset branch delimiter to a zero-count region
                cursor.clear_position();
                if removed_kid == remaining_kids {
                    cursor.path[path_index].kid_number -= 1;
                    let previous = remaining_kid_values.last().expect("non-empty").clone();
                    let child =
                        self.prepare_kid(pdf, &parent_handle, remaining_kids - 1, previous)?;
                    self.descend(pdf, cursor, child, false, true)?;
                    if cursor.positioned() {
                        self.next(pdf, cursor)?;
                    } // cov:ignore: LLVM maps this covered conditional delimiter to a zero-count region
                } else {
                    let next = remaining_kid_values[removed_kid].clone();
                    let child = self.prepare_kid(pdf, &parent_handle, removed_kid, next)?;
                    self.descend(pdf, cursor, child, true, true)?;
                }
                return Ok(());
            }

            if path_index == 0 {
                let root = self.load_node(pdf, &parent_handle)?;
                root.remove("Kids");
                root.insert(K::ITEMS_KEY, ObjectHandle::array(Vec::new()))?;
                root.mark_dirty(pdf)?;
                cursor.path.clear();
                cursor.clear_position();
                return Ok(());
            }
            cursor.path.pop();
        }
    }

    fn find_internal<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
        return_previous_if_missing: bool,
    ) -> Result<NNTreeCursor<K>> {
        let first = self.begin(pdf)?;
        if !first.positioned() {
            return Ok(self.end());
        }
        if let Some(first_key) = first.current_key() {
            if K::compare(key, first_key) == Ordering::Less {
                return Ok(self.end());
            }
        }
        // qpdf 11.9.0 initializes its `last_item` check with end(), not
        // last(), so after-maximum keys intentionally use the general search.

        let root = self.root_node(pdf)?;
        let root_diagnostic_ref = root.diagnostic_ref();
        let mut node = root;
        // `ObjectHandleIdentity` hashes and compares only its Rc allocation
        // pointer; the RefCell payload cannot change the set key.
        #[allow(clippy::mutable_key_type)]
        let mut seen: HashSet<NodeIdentity> = HashSet::new();
        let mut cursor = NNTreeCursor::for_pdf(pdf.unique_id());

        loop {
            if self
                .max_depth
                .is_some_and(|max_depth| cursor.path.len() >= max_depth)
            {
                let max_depth = self.max_depth.expect("checked above");
                return Err(Error::Unsupported(format!(
                    "name/number tree: /Kids depth limit {max_depth} exceeded"
                )));
            }
            if !seen.insert(node.identity()) {
                return Err(structural_error(
                    node.diagnostic_ref(),
                    "loop detected in find",
                ));
            }

            let dictionary = self
                .load_node(pdf, &node)
                .map_err(|_| structural_error(node.diagnostic_ref(), "bad node during find"))?;
            let items_source = dictionary.get(K::ITEMS_KEY);
            let items = resolved_array(pdf, items_source.as_ref())?;
            let kids_source = dictionary.get("Kids");
            let kids = resolved_array(pdf, kids_source.as_ref())?;

            if let Some(items) = items.as_ref().filter(|items| !items.values.is_empty()) {
                let index = binary_search(
                    items.values.len() / 2,
                    return_previous_if_missing,
                    |index| {
                        let item_number = 2 * index;
                        // cov:ignore-start: binary_search only supplies indices below items length divided by two
                        let Some(item) = items.values.get(item_number) else {
                            return Err(structural_error(
                                root_diagnostic_ref,
                                format!("item at index {item_number} is not the right type"),
                            ));
                        };
                        // cov:ignore-end
                        let Some(item_key) = resolved_key::<K, _>(pdf, item)? else {
                            return Err(structural_error(
                                root_diagnostic_ref,
                                format!("item at index {item_number} is not the right type"),
                            ));
                        };
                        Ok(K::compare(key, &item_key))
                    },
                )?;
                if let Some(index) = index {
                    cursor.leaf = Some(node);
                    cursor.item_number = Some(2 * index);
                    self.update_current(pdf, &mut cursor, false)?;
                }
                return Ok(cursor);
            }

            if let Some(kids) = kids.filter(|kids| !kids.values.is_empty()) {
                let index = binary_search(kids.values.len(), true, |index| {
                    let kid = kids
                        .values
                        .get(index)
                        .expect("binary-search index is in range");
                    let kid_dictionary = LiveDictionary::new(kid.clone()).map_err(|_| {
                        structural_error(
                            root_diagnostic_ref,
                            format!("invalid kid at index {index}"),
                        )
                    })?;
                    self.within_limits(pdf, key, &kid_dictionary, kid.object_ref())
                })?;
                let index = index.ok_or_else(|| {
                    structural_error(
                        node.diagnostic_ref(),
                        "unexpected -1 from binary search of kids; limits may by wrong",
                    )
                })?;
                let kid_object = kids.values[index].clone();
                cursor.path.push(PathElement {
                    node: node.clone(),
                    kid_number: index,
                });
                node = self.handle_for_kid(pdf, &node, index, &kid_object)?;
                continue;
            }

            return Err(structural_error(
                node.diagnostic_ref(),
                "bad node during find",
            ));
        }
    }

    fn within_limits<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
        dictionary: &LiveDictionary,
        object_ref: Option<ObjectRef>,
    ) -> Result<Ordering> {
        let Some(limits) = resolved_array(pdf, dictionary.get("Limits").as_ref())? else {
            return Err(structural_error(object_ref, "node is missing /Limits"));
        };
        let (Some(first), Some(last)) = (
            limits
                .values
                .first()
                .map(|value| resolved_key::<K, _>(pdf, value))
                .transpose()?
                .flatten(),
            limits
                .values
                .get(1)
                .map(|value| resolved_key::<K, _>(pdf, value))
                .transpose()?
                .flatten(),
        ) else {
            return Err(structural_error(object_ref, "node is missing /Limits"));
        };
        if K::compare(key, &first) == Ordering::Less {
            Ok(Ordering::Less)
        } else if K::compare(key, &last) == Ordering::Greater {
            Ok(Ordering::Greater)
        } else {
            Ok(Ordering::Equal)
        }
    }

    fn handle_for_kid<R: Read + Seek>(
        &mut self,
        _pdf: &mut Pdf<R>,
        parent: &NodeHandle,
        kid_number: usize,
        kid: &ObjectHandle,
    ) -> Result<NodeHandle> {
        let kid = kid.clone();
        if let Some(object_ref) = kid.object_ref() {
            Ok(NodeHandle::indirect(object_ref, kid))
        } else {
            Ok(parent.direct_kid(kid_number, kid))
        }
    }

    fn root_node<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NodeHandle> {
        let root = self.ensure_root(pdf)?;
        Ok(match root.object_ref() {
            Some(object_ref) => NodeHandle::indirect(object_ref, root),
            None => NodeHandle::root(root),
        })
    }

    fn descend<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        start: NodeHandle,
        first: bool,
        allow_empty: bool,
    ) -> Result<bool> {
        let original_path = cursor.path.clone();
        let original_leaf = cursor.leaf.clone();
        let original_item_number = cursor.item_number;
        let original_current = cursor.current.clone();
        // `ObjectHandleIdentity` hashes and compares only its Rc allocation
        // pointer; the RefCell payload cannot change the set key.
        #[allow(clippy::mutable_key_type)]
        let mut seen: HashSet<NodeIdentity> = cursor
            .path
            .iter()
            .map(|element| element.node.identity())
            .collect();
        let mut node = start;

        loop {
            if self
                .max_depth
                .is_some_and(|max_depth| cursor.path.len() >= max_depth)
            {
                let max_depth = self.max_depth.expect("checked above");
                return Err(Error::Unsupported(format!(
                    "name/number tree: /Kids depth limit {max_depth} exceeded"
                )));
            }
            if !seen.insert(node.identity()) {
                self.warn(
                    pdf,
                    &node,
                    "loop detected while traversing name/number tree",
                )?;
                break;
            }

            let dictionary = match self.load_node(pdf, &node) {
                Ok(dictionary) => dictionary,
                Err(_) => {
                    self.warn(
                        pdf,
                        &node,
                        "non-dictionary node while traversing name/number tree",
                    )?;
                    break;
                }
            };
            let items_source = dictionary.get(K::ITEMS_KEY);
            let items = resolved_array(pdf, items_source.as_ref())?;
            let kids_source = dictionary.get("Kids");
            let kids = resolved_array(pdf, kids_source.as_ref())?;

            if let Some(items) = items.as_ref().filter(|items| !items.values.is_empty()) {
                let item_number = if first {
                    0
                } else {
                    // qpdf 11.9.0 NNTreeIterator::deepen uses nitems - 2
                    // verbatim, including value-slot selection in odd arrays.
                    items.values.len().saturating_sub(2)
                };
                cursor.leaf = Some(node);
                cursor.item_number = Some(item_number);
                self.update_current(pdf, cursor, true)?;
                return Ok(true);
            }

            if let Some(kids) = kids.filter(|kids| !kids.values.is_empty()) {
                let kid_number = if first { 0 } else { kids.values.len() - 1 };
                let kid_object = kids.values[kid_number].clone();
                cursor.path.push(PathElement {
                    node: node.clone(),
                    kid_number,
                });
                node = self.prepare_kid(pdf, &node, kid_number, kid_object)?;
                continue;
            }

            if allow_empty && items.is_some() {
                cursor.leaf = Some(node);
                cursor.item_number = None;
                cursor.current = None;
                return Ok(true);
            }

            self.warn(
                pdf,
                &node,
                format!(
                    "name/number tree node has neither non-empty /{} nor /Kids",
                    K::ITEMS_KEY
                ),
            )?;
            break;
        }

        cursor.path = original_path;
        cursor.leaf = original_leaf;
        cursor.item_number = original_item_number;
        cursor.current = original_current;
        Ok(false)
    }

    fn increment<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        backward: bool,
    ) -> Result<()> {
        if cursor.item_number.is_none() {
            cursor.path.clear();
            cursor.clear_position();
            let root = self.root_node(pdf)?;
            self.descend(pdf, cursor, root, !backward, true)?;
            return Ok(());
        }

        loop {
            let leaf = cursor.leaf.clone().expect("valid cursor has a leaf");
            let dictionary = self.load_node(pdf, &leaf)?;
            let Some(items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())? else {
                cursor.clear_position();
                return Ok(());
            };
            let item_number = cursor.item_number.expect("checked above");
            let candidate = if backward {
                item_number.checked_sub(2)
            } else {
                item_number
                    .checked_add(2)
                    .filter(|index| *index < items.values.len())
            };

            if let Some(candidate) = candidate {
                cursor.item_number = Some(candidate);
                if candidate + 1 >= items.values.len() {
                    self.warn(pdf, &leaf, "items array doesn't have enough elements")?;
                    cursor.current = None;
                    continue;
                }
                self.update_current(pdf, cursor, true)?;
                if cursor.current.is_some() {
                    return Ok(());
                }
                self.warn(pdf, &leaf, format!("item {candidate} has the wrong type"))?;
                continue;
            }

            cursor.clear_position();
            let mut descended = false;
            while let Some(last_index) = cursor.path.len().checked_sub(1) {
                let parent = cursor.path[last_index].node.clone();
                let dictionary = self.load_node(pdf, &parent)?;
                let Some(kids) = resolved_array(pdf, dictionary.get("Kids").as_ref())? else {
                    cursor.path.pop();
                    continue;
                };
                let mut kid_number = cursor.path[last_index].kid_number;
                loop {
                    let next_index = if backward {
                        kid_number.checked_sub(1)
                    } else {
                        kid_number
                            .checked_add(1)
                            .filter(|index| *index < kids.values.len())
                    };
                    let Some(next_index) = next_index else {
                        break;
                    };
                    kid_number = next_index;
                    let kid_object = kids.values[kid_number].clone();
                    if !self.kid_has_tree_shape(pdf, &kid_object)? {
                        self.warn(
                            pdf,
                            &parent,
                            format!("skipping over invalid kid at index {kid_number}"),
                        )?;
                        continue;
                    }
                    cursor.path[last_index].kid_number = kid_number;
                    let kid = self.prepare_kid(pdf, &parent, kid_number, kid_object)?;
                    if self.descend(pdf, cursor, kid, !backward, false)? {
                        if cursor.current.is_none() {
                            let item_number = cursor
                                .item_number
                                .expect("descended non-empty leaf has an item number");
                            self.warn(
                                pdf,
                                cursor.leaf.as_ref().expect("descended leaf is present"),
                                format!("item {item_number} has the wrong type"),
                            )?;
                        }
                        descended = true;
                        break;
                    }
                }
                if descended {
                    break;
                }
                cursor.path.pop();
            }

            if !descended {
                return Ok(());
            }
            if cursor.current.is_some() {
                return Ok(());
            }
        }
    }

    fn update_current<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        allow_invalid: bool,
    ) -> Result<()> {
        cursor.current = None;
        let (Some(leaf), Some(item_number)) = (&cursor.leaf, cursor.item_number) else {
            return Ok(());
        };
        let dictionary = self.load_node(pdf, leaf)?;
        let Some(items) = resolved_array(pdf, dictionary.get(K::ITEMS_KEY).as_ref())? else {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                format!("update ivalue: /{} is not an array", K::ITEMS_KEY),
            ));
        };
        if item_number + 1 >= items.values.len() {
            return Err(structural_error(
                leaf.diagnostic_ref(),
                "update ivalue: items array is too short",
            ));
        }
        let raw_key = items.values[item_number].clone();
        let raw_value = items.values[item_number + 1].clone();
        let Some(key) = resolved_key::<K, _>(pdf, &raw_key)? else {
            if allow_invalid {
                return Ok(());
            }
            return Err(structural_error(
                leaf.diagnostic_ref(),
                format!("item at index {item_number} is not the right type"),
            ));
        };
        cursor.current = Some((key, raw_value));
        Ok(())
    }

    fn insert_pair_with_allocator<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        allocator: &mut ObjectAllocator,
        key: ObjectHandle,
        value: ObjectHandle,
    ) -> Result<NNTreeCursor<K>> {
        if let Some(resolved_key) = resolved_key::<K, _>(pdf, &key)? {
            self.insert_resolved_with_allocator(pdf, allocator, resolved_key, key, value)
        } else {
            // qpdf can insert the first pair into an empty replacement
            // before its next insert observes that the key is invalid. Later
            // malformed keys are skipped by increment and never reach this
            // path.
            let cursor = self.begin(pdf)?;
            if cursor.positioned() {
                // cov:ignore-start: increment skips later malformed keys before repair observes them
                Err(structural_error(
                    self.root_node(pdf)?.diagnostic_ref(),
                    "item at index 0 is not the right type",
                ))
                // cov:ignore-end
            } else {
                self.insert_first(pdf, allocator, key, value)
            }
        }
    }

    fn prepare_kid<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        parent: &NodeHandle,
        kid_number: usize,
        kid_object: ObjectHandle,
    ) -> Result<NodeHandle> {
        if let Some(object_ref) = kid_object.object_ref() {
            return Ok(NodeHandle::indirect(object_ref, kid_object));
        }

        if self.auto_repair {
            self.warn(
                pdf,
                parent,
                format!("converting kid number {kid_number} to an indirect object"),
            )?;
            let indirect = pdf.make_indirect_from_object_handle(kid_object)?;
            let object_ref = indirect
                .object_ref()
                .expect("canonical allocation returns an indirect kid");
            let dictionary = self.load_node(pdf, parent)?;
            // cov:ignore-start: prepare_kid receives kid_object from this same parent Kids array
            let Some(mut kids) = resolved_array(pdf, dictionary.get("Kids").as_ref())? else {
                return Err(structural_error(
                    parent.diagnostic_ref(),
                    "node is missing /Kids",
                ));
            };
            // cov:ignore-end
            kids.values[kid_number] = indirect.clone();
            kids.store(pdf)?;
            Ok(NodeHandle::indirect(object_ref, indirect))
        } else {
            self.warn(
                pdf,
                parent,
                format!("kid number {kid_number} is not an indirect object"),
            )?;
            Ok(parent.direct_kid(kid_number, kid_object))
        }
    }

    fn kid_has_tree_shape<R: Read + Seek>(
        &mut self,
        _pdf: &mut Pdf<R>,
        kid: &ObjectHandle,
    ) -> Result<bool> {
        if kid.try_as_dictionary()?.is_none() {
            return Ok(false);
        }
        let dictionary = LiveDictionary::new(kid.clone())?;
        if dictionary.contains("Kids")? {
            return Ok(true);
        }
        dictionary.contains(K::ITEMS_KEY)
    }

    fn warn<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        node: &NodeHandle,
        message: impl AsRef<str>,
    ) -> Result<()> {
        pdf.push_warning(structural_message(node.diagnostic_ref(), message))
    }

    fn load_node<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        handle: &NodeHandle,
    ) -> Result<LiveDictionary> {
        let node = self.load_anchor(pdf, handle)?;
        LiveDictionary::new(node)
    }

    fn load_anchor<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        handle: &NodeHandle,
    ) -> Result<ObjectHandle> {
        let live = handle.handle();
        if matches!(handle.anchor, NodeAnchor::Root)
            && handle.direct_kids.is_empty()
            && self.root.is_same_object_as(&live)
        {
            return self.ensure_root(pdf);
        }
        Ok(live)
    }
}

#[derive(Default)]
struct ObjectAllocator {
    next: Option<u64>,
}

impl ObjectAllocator {
    fn next_number<R: Read + Seek>(&self, pdf: &Pdf<R>) -> u64 {
        self.next.unwrap_or_else(|| {
            let legacy_max = pdf
                .object_refs()
                .into_iter()
                .map(|object_ref| u64::from(object_ref.number))
                .max()
                .unwrap_or(0);
            let canonical_max = pdf.resolver.max_object_number().map(u64::from).unwrap_or(0);
            legacy_max.max(canonical_max) + 1
        })
    }

    fn ensure_available<R: Read + Seek>(&self, pdf: &Pdf<R>, count: usize) -> Result<()> {
        debug_assert!(count > 0);
        let available = (i32::MAX as u64 + 1).saturating_sub(self.next_number(pdf));
        if (count as u64) <= available {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "object-number space exhausted".to_string(),
            ))
        }
    }
}

fn binary_search<F>(
    count: usize,
    return_previous_if_missing: bool,
    mut compare: F,
) -> Result<Option<usize>>
where
    F: FnMut(usize) -> Result<Ordering>,
{
    let mut max_index = 1usize;
    while max_index < count {
        max_index <<= 1;
    }
    let mut step = max_index / 2;
    let mut checks = max_index;
    let mut index = step;
    let mut found_index = None;
    let mut found = false;
    let mut found_less_or_equal = false;

    while !found && checks > 0 {
        let status = if index < count {
            let status = compare(index)?;
            if status != Ordering::Less {
                found_less_or_equal = true;
                found_index = Some(index);
            }
            status
        } else {
            Ordering::Less
        };
        if status == Ordering::Equal {
            found = true;
        } else {
            checks >>= 1;
            if checks > 0 {
                step >>= 1;
                if step == 0 {
                    step = 1;
                }
                if status == Ordering::Less {
                    index = index.saturating_sub(step);
                } else {
                    index = index.saturating_add(step);
                }
            }
        }
    }

    Ok(
        if found || (found_less_or_equal && return_previous_if_missing) {
            found_index
        } else {
            None
        },
    )
}

fn structural_error(object_ref: Option<ObjectRef>, message: impl AsRef<str>) -> Error {
    Error::parse(0, structural_message(object_ref, message))
}

fn structural_message(object_ref: Option<ObjectRef>, message: impl AsRef<str>) -> String {
    let prefix = match object_ref {
        Some(object_ref) => format!("Name/Number tree node (object {}): ", object_ref.number),
        None => "Name/Number tree node: ".to_string(),
    };
    format!("{prefix}{}", message.as_ref())
}
