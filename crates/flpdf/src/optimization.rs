//! Writer optimization helpers for page attributes, reachability, and object streams.
//!
//! qpdf correspondence: QPDF_optimization.cc optimization orchestration, inherited-page preparation, object-user maps, and compressed-object folding.
//!

pub(crate) mod inherited_attrs;

use crate::parser::MAX_PARSE_DEPTH;
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::{ObjectHandle, ObjectRef, Pdf};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ObjectUser {
    Page(u32),
    Thumbnail(u32),
    TrailerKey(Vec<u8>),
    RootKey(Vec<u8>),
    Root,
}

const COMPACT_OBJECT_USER_SET_TREE_THRESHOLD: usize = 8;

/// Ordered, duplicate-free object-user values with a compact representation
/// for the one- and two-user case that dominates parsed PDF objects.
///
/// qpdf owns this responsibility as `std::set<ObjUser>`. The small sorted
/// vector is an allocation-conscious Rust representation of the same
/// ordered-set contract; larger sets promote to the tree-backed representation
/// so insert and lookup behavior remains bounded for high-cardinality objects.
#[derive(Debug, Clone)]
pub(crate) enum CompactObjectUserSet {
    Small(Vec<ObjectUser>),
    Tree(BTreeSet<ObjectUser>),
}

impl Default for CompactObjectUserSet {
    fn default() -> Self {
        Self::Small(Vec::new())
    }
}

pub(crate) enum CompactObjectUserIter<'a> {
    Small(std::slice::Iter<'a, ObjectUser>),
    Tree(std::collections::btree_set::Iter<'a, ObjectUser>),
}

impl<'a> Iterator for CompactObjectUserIter<'a> {
    type Item = &'a ObjectUser;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(values) => values.next(),
            Self::Tree(values) => values.next(),
        }
    }
}

impl<'a> IntoIterator for &'a CompactObjectUserSet {
    type Item = &'a ObjectUser;
    type IntoIter = CompactObjectUserIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl CompactObjectUserSet {
    fn insert(&mut self, user: ObjectUser) -> bool {
        match self {
            Self::Small(values) => {
                let index = match values.binary_search(&user) {
                    Ok(_) => return false,
                    Err(index) => index,
                };
                values.insert(index, user);
                if values.len() > COMPACT_OBJECT_USER_SET_TREE_THRESHOLD {
                    let values = std::mem::take(values);
                    *self = Self::Tree(values.into_iter().collect());
                }
                true
            }
            Self::Tree(values) => values.insert(user),
        }
    }

    pub(crate) fn iter(&self) -> CompactObjectUserIter<'_> {
        match self {
            Self::Small(values) => CompactObjectUserIter::Small(values.iter()),
            Self::Tree(values) => CompactObjectUserIter::Tree(values.iter()),
        }
    }

    pub(crate) fn contains(&self, user: &ObjectUser) -> bool {
        match self {
            Self::Small(values) => values.binary_search(user).is_ok(),
            Self::Tree(values) => values.contains(user),
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Small(values) => values.len(),
            Self::Tree(values) => values.len(),
        }
    }

    #[cfg(test)]
    fn is_tree(&self) -> bool {
        matches!(self, Self::Tree(_))
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Optimization {
    /// Canonical qpdf object-user map. Keep the raw QpdfObjGen identity all
    /// the way through linearization; qpdf has no second ObjectRef projection
    /// pair (`QPDF.hh:1516-1517`).
    raw_user_to_objects: BTreeMap<ObjectUser, BTreeSet<QpdfObjGen>>,
    raw_object_to_users: BTreeMap<QpdfObjGen, CompactObjectUserSet>,
    /// qpdf's Generate object-stream eligibility captured before optimization
    /// can mint inherited-attribute objects.
    generate_objstm_eligible: Option<Vec<ObjectRef>>,
    /// Object identities present before optimization. Newly minted first-half
    /// plain objects are emitted after qpdf's ObjStm containers.
    pre_optimization_object_refs: Option<BTreeSet<ObjectRef>>,
}

impl Optimization {
    /// Project the canonical raw map at the consumer boundary. The iterator
    /// does not retain a second ObjectRef-keyed map and intentionally omits
    /// raw generations that cannot be represented by ObjectRef.
    pub(crate) fn objects_for(&self, user: &ObjectUser) -> impl Iterator<Item = ObjectRef> + '_ {
        self.raw_objects_for(user)
            .iter()
            .filter_map(|object| object.to_object_ref())
    }

    pub(crate) fn users_for(&self, object: ObjectRef) -> &CompactObjectUserSet {
        let Ok(object) = QpdfObjGen::try_from_object_ref(object) else {
            return empty_object_users();
        };
        self.raw_users_for(object)
    }

    pub(crate) fn object_users(&self) -> impl Iterator<Item = (ObjectRef, &CompactObjectUserSet)> {
        self.raw_object_users()
            .filter_map(|(object, users)| object.to_object_ref().map(|object| (object, users)))
    }

    pub(crate) fn raw_objects_for(&self, user: &ObjectUser) -> &BTreeSet<QpdfObjGen> {
        match self.raw_user_to_objects.get(user) {
            Some(objects) => objects,
            None => empty_qpdf_obj_gens(),
        }
    }

    pub(crate) fn raw_users_for(&self, object: QpdfObjGen) -> &CompactObjectUserSet {
        match self.raw_object_to_users.get(&object) {
            Some(users) => users,
            None => empty_object_users(),
        }
    }

    pub(crate) fn raw_object_users(
        &self,
    ) -> impl Iterator<Item = (QpdfObjGen, &CompactObjectUserSet)> {
        self.raw_object_to_users
            .iter()
            .map(|(&object, users)| (object, users))
    }

    /// Return the projectable identities reached by qpdf's optimization walk.
    ///
    /// `QPDF::optimize` records every object reached from pages, trailer keys,
    /// and root keys in `object_to_obj_users` and the linearization classifier
    /// consumes that map directly (`QPDF_optimization.cc:57-118,261-338`;
    /// `QPDF_linearization.cc:963-1155`). The linearization plan uses this
    /// view when no source ObjStm member-to-container projection has replaced
    /// those raw keys.
    pub(crate) fn linearization_reachable_object_refs(
        &self,
    ) -> impl Iterator<Item = ObjectRef> + '_ {
        self.raw_object_users()
            .filter_map(|(object, _)| object.to_object_ref())
    }

    pub(crate) fn raw_page_users(&self, object: QpdfObjGen) -> impl Iterator<Item = u32> + '_ {
        self.raw_users_for(object)
            .iter()
            .filter_map(|user| match user {
                ObjectUser::Page(page_number) => Some(*page_number),
                _ => None,
            })
    }

    pub(crate) fn raw_other_page_private_owner(&self, object: QpdfObjGen) -> Option<u32> {
        let mut owner = None;
        for user in self.raw_users_for(object).iter() {
            match user {
                ObjectUser::Page(page) if *page != 0 => {
                    if owner.replace(*page).is_some() {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        owner
    }

    pub(crate) fn raw_thumbnail_objects(&self) -> BTreeSet<QpdfObjGen> {
        self.raw_user_to_objects
            .iter()
            .filter_map(|(user, objects)| match user {
                ObjectUser::Thumbnail(_) => Some(objects),
                _ => None,
            })
            .flat_map(|objects| objects.iter().copied())
            .collect()
    }

    pub(crate) fn raw_objects_for_root_key(&self, key: &[u8]) -> BTreeSet<QpdfObjGen> {
        self.raw_objects_for(&ObjectUser::RootKey(key.to_vec()))
            .clone()
    }

    pub(crate) fn raw_objects_for_trailer_key(&self, key: &[u8]) -> BTreeSet<QpdfObjGen> {
        self.raw_objects_for(&ObjectUser::TrailerKey(key.to_vec()))
            .clone()
    }

    pub(crate) fn page_users(&self, object: ObjectRef) -> impl Iterator<Item = u32> + '_ {
        self.users_for(object).iter().filter_map(|user| match user {
            ObjectUser::Page(page_number) => Some(*page_number),
            _ => None,
        })
    }

    pub(crate) fn other_page_private_owner(&self, object: ObjectRef) -> Option<u32> {
        let mut owner = None;
        for user in self.users_for(object).iter() {
            match user {
                ObjectUser::Page(page) if *page != 0 => {
                    if owner.replace(*page).is_some() {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        owner
    }

    pub(crate) fn thumbnail_objects(&self) -> impl Iterator<Item = ObjectRef> + '_ {
        self.raw_thumbnail_objects()
            .into_iter()
            .filter_map(|object| object.to_object_ref())
    }

    pub(crate) fn objects_for_root_key(&self, key: &[u8]) -> impl Iterator<Item = ObjectRef> + '_ {
        self.objects_for(&ObjectUser::RootKey(key.to_vec()))
    }

    pub(crate) fn objects_for_trailer_key(
        &self,
        key: &[u8],
    ) -> impl Iterator<Item = ObjectRef> + '_ {
        self.objects_for(&ObjectUser::TrailerKey(key.to_vec()))
    }

    pub(crate) fn set_generate_objstm_eligible(&mut self, eligible: Vec<ObjectRef>) {
        self.generate_objstm_eligible = Some(eligible);
    }

    pub(crate) fn generate_objstm_eligible(&self) -> Option<&[ObjectRef]> {
        self.generate_objstm_eligible.as_deref()
    }

    pub(crate) fn set_pre_optimization_object_refs(&mut self, refs: BTreeSet<ObjectRef>) {
        self.pre_optimization_object_refs = Some(refs);
    }

    pub(crate) fn pre_optimization_object_refs(&self) -> Option<&BTreeSet<ObjectRef>> {
        self.pre_optimization_object_refs.as_ref()
    }

    fn record_raw(&mut self, user: ObjectUser, object: QpdfObjGen) {
        self.raw_user_to_objects
            .entry(user.clone())
            .or_default()
            .insert(object);
        self.raw_object_to_users
            .entry(object)
            .or_default()
            .insert(user);
    }

    fn record(&mut self, user: ObjectUser, object: ObjectRef) {
        let object = QpdfObjGen::try_from_object_ref(object)
            .expect("ObjectRef must fit qpdf's signed object-number range");
        self.record_raw(user, object);
    }

    #[cfg(test)]
    pub(crate) fn record_for_test(&mut self, user: ObjectUser, object: ObjectRef) {
        self.record(user, object);
    }

    #[cfg(test)]
    pub(crate) fn record_raw_for_test(&mut self, user: ObjectUser, object: QpdfObjGen) {
        self.record_raw(user, object);
    }

    pub(crate) fn optimize<R, F>(
        pdf: &mut Pdf<R>,
        object_stream_data: &BTreeMap<u32, u32>,
        allow_changes: bool,
        skip_stream_parameters: F,
    ) -> crate::Result<Self>
    where
        R: Read + Seek,
        F: FnMut(Option<QpdfObjGen>, &ObjectHandle) -> crate::Result<u8>,
    {
        let prepared = Self::prepare_pdf(pdf, allow_changes, true)?;
        let page_handles = prepared
            .as_ref()
            .map(|prepared| prepared.pages.as_slice())
            .unwrap_or_default();
        let mut maps = Self::build_maps(pdf, page_handles, skip_stream_parameters)?;
        maps.filter_compressed_objects(object_stream_data);
        Ok(maps)
    }

    fn prepare_pdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        allow_changes: bool,
        warn_missing_page_tree: bool,
    ) -> crate::Result<Option<crate::pages::repair::PreparedPages>> {
        let root = pdf.root_handle()?;
        let outlines = root.try_get_key(b"/Outlines")?;
        if outlines.try_is_dictionary()? && outlines.is_direct() {
            // qpdf's optimize makes a direct /Outlines dictionary indirect
            // without cloning its live allocation
            // (libqpdf/QPDF_optimization.cc:73-77).
            let outlines = pdf.make_indirect_from_object_handle(outlines)?;
            root.replace_key(b"/Outlines", outlines)?;
        }

        let prepared = crate::pages::repair::prepare_for_optimization(pdf)?;
        if let Some(ref prepared) = prepared {
            inherited_attrs::push(pdf, prepared, allow_changes, false)?;
        } else if warn_missing_page_tree {
            inherited_attrs::warn_missing_page_tree(pdf)?;
        }
        Ok(prepared)
    }

    pub(crate) fn prepare_for_linearized_write<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> crate::Result<()> {
        Self::prepare_pdf(pdf, true, false).map(|_| ())
    }

    pub(crate) fn filter_compressed_objects(&mut self, object_stream_data: &BTreeMap<u32, u32>) {
        if object_stream_data.is_empty() {
            return;
        }
        let mut filtered = Self::default();
        for (user, objects) in &self.raw_user_to_objects {
            for &object in objects {
                let target = u32::try_from(object.get_obj())
                    .ok()
                    .and_then(|number| object_stream_data.get(&number).copied())
                    .map(|stream| QpdfObjGen::new(stream as i32, 0))
                    .unwrap_or(object);
                filtered.record_raw(user.clone(), target);
            }
        }
        *self = filtered;
    }

    pub(crate) fn users_for_members<'a>(
        &self,
        members: impl IntoIterator<Item = &'a ObjectRef>,
    ) -> BTreeSet<ObjectUser> {
        let mut users = BTreeSet::new();
        for member in members {
            users.extend(self.users_for(*member).iter().cloned());
        }
        users
    }

    fn build_maps<R, F>(
        pdf: &mut Pdf<R>,
        page_handles: &[ObjectHandle],
        mut skip_stream_parameters: F,
    ) -> crate::Result<Self>
    where
        R: Read + Seek,
        F: FnMut(Option<QpdfObjGen>, &ObjectHandle) -> crate::Result<u8>,
    {
        let mut maps = Self::default();

        for (page_number, page) in page_handles.iter().enumerate() {
            maps.update_object_maps(
                pdf,
                ObjectUser::Page(page_number as u32),
                page.clone(),
                &mut skip_stream_parameters,
            )?;
        }

        let trailer = pdf.trailer();
        for key in trailer.try_get_keys()? {
            if key != b"/Root" {
                let user_key = key.strip_prefix(b"/").unwrap_or(&key).to_vec();
                maps.update_object_maps(
                    pdf,
                    ObjectUser::TrailerKey(user_key),
                    trailer.try_get_key(&key)?,
                    &mut skip_stream_parameters,
                )?;
            }
        }

        let root = pdf.root_handle()?;
        for key in root.try_get_keys()? {
            let user_key = key.strip_prefix(b"/").unwrap_or(&key).to_vec();
            maps.update_object_maps(
                pdf,
                ObjectUser::RootKey(user_key),
                root.try_get_key(&key)?,
                &mut skip_stream_parameters,
            )?;
        }
        if let Some(root_ref) = root.object_ref() {
            maps.record(ObjectUser::Root, root_ref);
        }

        Ok(maps)
    }

    fn update_object_maps<R, F>(
        &mut self,
        pdf: &Pdf<R>,
        user: ObjectUser,
        object: ObjectHandle,
        skip_stream_parameters: &mut F,
    ) -> crate::Result<()>
    where
        R: Read + Seek,
        F: FnMut(Option<QpdfObjGen>, &ObjectHandle) -> crate::Result<u8>,
    {
        let mut visited = BTreeSet::new();
        let mut stack = vec![Pending {
            object,
            user,
            top: true,
            via_array: false,
            inline_depth: 0,
        }];

        while let Some(pending) = stack.pop() {
            // qpdf-deviation: QPDF::getCompressibleObjGens uses an explicit work queue without a MAX_PARSE_DEPTH cap.
            if pending.inline_depth > MAX_PARSE_DEPTH {
                return Err(crate::Error::Unsupported(format!(
                    "optimization: inline object nesting exceeds maximum of {MAX_PARSE_DEPTH}"
                )));
            }

            if pending.object.is_indirect() {
                // qpdf's QPDFWriter::enqueueObject checks the owning QPDF
                // before it accepts any indirect handle
                // (libqpdf/QPDFWriter.cc:1072-1083). The direct-container
                // mutation boundary intentionally remains shallow, so this
                // is the first traversal point that can see a foreign
                // indirect descendant.
                crate::writer::rewrite_renumber::ensure_canonical_owner(pdf, &pending.object)?;
            }
            pending.object.try_dereference()?;
            if pending.object.is_null() {
                if pending.via_array {
                    if let Some(object_gen) = pending.object.qpdf_obj_gen() {
                        if object_gen.get_obj() > 0 && visited.insert(object_gen) {
                            self.record_raw(pending.user, object_gen);
                        }
                    }
                }
                continue;
            }

            if is_page_resolved(&pending.object)? && !pending.top {
                continue;
            }
            if let Some(object_gen) = pending.object.qpdf_obj_gen() {
                if !visited.insert(object_gen) {
                    continue;
                }
                self.record_raw(pending.user.clone(), object_gen);
            }
            // The inline-depth guard counts only direct container nesting.
            // Crossing an indirect handle resets that count, matching the
            // old resolver's reference arm and qpdf's handle traversal.
            let inline_depth = if pending.object.is_indirect() {
                0
            } else {
                pending.inline_depth
            };

            if let Some(items) = pending.object.as_array() {
                for item in items.into_iter().rev() {
                    stack.push(Pending {
                        object: item,
                        user: pending.user.clone(),
                        top: false,
                        via_array: true,
                        inline_depth: inline_depth + 1,
                    });
                }
                continue;
            }

            if let Some(stream_dict) = pending.object.as_stream_dict() {
                let skip_level =
                    skip_stream_parameters(pending.object.qpdf_obj_gen(), &pending.object)?;
                stream_dict.try_dereference()?;
                for (key, object) in resolved_visible_children(&stream_dict)?.into_iter().rev() {
                    if (skip_level >= 1 && key == b"/Length")
                        || (skip_level >= 2
                            && matches!(key.as_slice(), b"/Filter" | b"/DecodeParms"))
                    {
                        continue;
                    }
                    stack.push(Pending {
                        object,
                        user: pending.user.clone(),
                        top: false,
                        via_array: false,
                        inline_depth: inline_depth + 1,
                    });
                }
                continue;
            }

            if pending.object.with_value(|value| {
                matches!(
                    value,
                    Some(crate::object_handle::ObjectValue::Dictionary(_))
                )
            }) {
                let page = is_page_resolved(&pending.object)?;
                for (key, object) in resolved_visible_children(&pending.object)?
                    .into_iter()
                    .rev()
                {
                    if page && key == b"/Parent" {
                        continue;
                    }
                    let child_user = if page && key == b"/Thumb" {
                        ObjectUser::Thumbnail(pending.user.page_number())
                    } else {
                        pending.user.clone()
                    };
                    stack.push(Pending {
                        object,
                        user: child_user,
                        top: false,
                        via_array: false,
                        inline_depth: inline_depth + 1,
                    });
                }
            }
        }

        Ok(())
    }
}

impl ObjectUser {
    fn page_number(&self) -> u32 {
        match self {
            Self::Page(page_number) | Self::Thumbnail(page_number) => *page_number,
            Self::TrailerKey(_) | Self::RootKey(_) | Self::Root => 0,
        }
    }
}

struct Pending {
    object: ObjectHandle,
    user: ObjectUser,
    top: bool,
    via_array: bool,
    inline_depth: usize,
}

fn is_page_resolved(object: &ObjectHandle) -> crate::Result<bool> {
    resolved_page_type(object)
}

/// Check the page marker from the already-resolved dictionary without routing
/// through resolved_get_key's context bookkeeping. qpdf's
/// isDictionaryOfType only needs the direct /Type child in this case.
fn resolved_page_type(object: &ObjectHandle) -> crate::Result<bool> {
    let (is_dictionary, type_child) = object.with_value(|value| match value {
        Some(crate::object_handle::ObjectValue::Dictionary(entries)) => {
            (true, entries.get(b"/Type".as_slice()).cloned())
        }
        _ => (false, None),
    });
    if !is_dictionary {
        // Keep the existing qpdf-shaped helper reachable for non-dictionaries
        // only; the common dictionary path avoids its context bookkeeping.
        return object.resolved_is_dictionary_of_type(b"Page", b"");
    }
    let Some(type_child) = type_child else {
        return Ok(false);
    };
    type_child.try_is_name_and_equals(b"Page")
}

/// Return qpdf-ordered visible dictionary children while cloning each child
/// handle only once.
///
/// qpdf's getKeys/getKey pair returns sorted, non-null dictionary entries
/// (QPDF_Dictionary.cc:117-127). The old Rust consumer copied the key list,
/// looked up each key once for null filtering, then looked it up again to push
/// the child. Keep the same resolution/filtering order but retain the child
/// handle from the first lookup.
fn resolved_visible_children(handle: &ObjectHandle) -> crate::Result<Vec<(Vec<u8>, ObjectHandle)>> {
    let Some(entries) = handle.with_value(|value| match value {
        Some(crate::object_handle::ObjectValue::Dictionary(entries)) => Some(
            entries
                .iter()
                .map(|(key, child)| (key.clone(), child.clone()))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    }) else {
        // Ordinary callers establish the dictionary/stream boundary before
        // entering this helper. Preserve the qpdf type-warning behavior for
        // a malformed direct caller without putting the snapshotting helper
        // back on the hot path.
        let _ = handle.resolved_get_visible_keys()?;
        return Ok(Vec::new());
    };

    let mut visible = Vec::with_capacity(entries.len());
    for (key, child) in entries {
        if !child.try_is_null()? {
            visible.push((key, child));
        }
    }
    Ok(visible)
}

fn empty_qpdf_obj_gens() -> &'static BTreeSet<QpdfObjGen> {
    static EMPTY: OnceLock<BTreeSet<QpdfObjGen>> = OnceLock::new();
    EMPTY.get_or_init(BTreeSet::new)
}

fn empty_object_users() -> &'static CompactObjectUserSet {
    static EMPTY: OnceLock<CompactObjectUserSet> = OnceLock::new();
    EMPTY.get_or_init(CompactObjectUserSet::default)
}

#[cfg(test)]
mod tests {
    use super::{CompactObjectUserSet, ObjectUser, Optimization};
    use crate::object_handle::ObjectHandle;
    use crate::parser::MAX_PARSE_DEPTH;
    use crate::qpdf_obj_gen::QpdfObjGen;
    use crate::{ObjectRef, Pdf, Result};
    use std::collections::{BTreeMap, BTreeSet};
    use std::rc::Rc;

    fn nested_direct_array(depth: usize) -> ObjectHandle {
        let mut value = ObjectHandle::null();
        for _ in 0..depth {
            value = ObjectHandle::array(vec![value]);
        }
        value
    }

    fn no_stream_parameter_skip(
        _object_ref: Option<QpdfObjGen>,
        _handle: &ObjectHandle,
    ) -> Result<u8> {
        Ok(0)
    }

    #[test]
    fn object_user_walk_reuses_the_resolved_receiver_boundary() {
        let pdf = Pdf::empty().expect("create owner for direct test values");
        let child = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Font".to_vec())),
            (b"/Value".to_vec(), ObjectHandle::integer(7)),
        ]);
        let root = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Catalog".to_vec())),
            (b"/Child".to_vec(), child),
        ]);
        let mut optimization = Optimization::default();

        ObjectHandle::reset_try_dereference_call_count();
        optimization
            .update_object_maps(&pdf, ObjectUser::Root, root, &mut no_stream_parameter_skip)
            .expect("walk direct dictionary");

        assert_eq!(
            ObjectHandle::try_dereference_call_count(),
            13,
            "resolved receiver was probed too many times"
        );
    }

    #[test]
    fn resolved_receiver_helpers_keep_type_and_visible_key_semantics() {
        let typed = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Annot".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Link".to_vec())),
            (b"/Null".to_vec(), ObjectHandle::null()),
        ]);
        typed.try_dereference().expect("direct dictionary resolves");

        assert!(typed
            .resolved_is_dictionary_of_type(b"Annot", b"Link")
            .expect("type probe"));
        assert!(!typed
            .resolved_is_dictionary_of_type(b"Annot", b"Widget")
            .expect("mismatching subtype probe"));
        assert!(!typed
            .resolved_is_dictionary_of_type(b"Page", b"")
            .expect("mismatching type probe"));
        assert_eq!(
            typed.resolved_get_keys().expect("visible keys"),
            BTreeSet::from([b"/Subtype".to_vec(), b"/Type".to_vec()])
        );
        assert_eq!(
            typed
                .resolved_get_visible_keys()
                .expect("key-only visible keys"),
            vec![b"/Subtype".to_vec(), b"/Type".to_vec()]
        );

        let scalar = ObjectHandle::integer(1);
        scalar.try_dereference().expect("direct scalar resolves");
        let error = scalar
            .resolved_get_visible_keys()
            .expect_err("non-dictionary visible keys warn like qpdf");
        assert!(error.to_string().contains("operation for dictionary"));
        let error = super::resolved_visible_children(&scalar)
            .expect_err("context-less non-dictionary child walk must error");
        assert!(error.to_string().contains("operation for dictionary"));

        let pdf = Pdf::empty().expect("create a warning context");
        let contextual_scalar = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(2))
            .expect("contextual scalar");
        contextual_scalar
            .try_dereference()
            .expect("contextual scalar resolves");
        assert!(contextual_scalar
            .resolved_get_visible_keys()
            .expect("contextual non-dictionary visible keys")
            .is_empty());
        assert!(super::resolved_visible_children(&contextual_scalar)
            .expect("contextual child walk keeps qpdf warning boundary")
            .is_empty());

        let missing_type =
            ObjectHandle::dictionary(vec![(b"/Child".to_vec(), ObjectHandle::integer(3))]);
        missing_type
            .try_dereference()
            .expect("missing-type dictionary resolves");
        assert!(!missing_type
            .resolved_is_dictionary_of_type(b"Page", b"")
            .expect("missing type probe"));
        assert!(!super::resolved_page_type(&missing_type).expect("missing page type probe"));
        assert_eq!(
            missing_type
                .resolved_get_key(b"/Missing")
                .expect("missing key")
                .as_integer(),
            None
        );
    }

    #[test]
    fn visible_children_are_sorted_and_drop_null_values() {
        let dictionary = ObjectHandle::dictionary(vec![
            (b"/Z".to_vec(), ObjectHandle::integer(3)),
            (b"/Null".to_vec(), ObjectHandle::null()),
            (b"/A".to_vec(), ObjectHandle::integer(1)),
        ]);
        dictionary
            .try_dereference()
            .expect("direct dictionary resolves");

        let children = super::resolved_visible_children(&dictionary)
            .expect("visible child traversal succeeds");
        assert_eq!(
            children
                .iter()
                .map(|(key, _)| key.as_slice())
                .collect::<Vec<_>>(),
            vec![b"/A".as_slice(), b"/Z".as_slice()]
        );
        assert_eq!(
            children
                .iter()
                .map(|(_, child)| child.as_integer())
                .collect::<Vec<_>>(),
            vec![Some(1), Some(3)]
        );
    }

    #[test]
    fn page_type_probe_reads_live_type_children_without_a_context_snapshot() {
        let page = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (b"/Subtype".to_vec(), ObjectHandle::name(b"Link".to_vec())),
        ]);
        page.try_dereference().expect("direct page resolves");

        assert!(
            super::resolved_page_type(&page).expect("page type probe succeeds"),
            "a resolved Page dictionary must be recognized"
        );
    }

    #[test]
    fn object_user_walk_preserves_page_parent_thumb_stream_and_array_boundaries() {
        let pdf = Pdf::empty().expect("create owner for indirect test values");
        let parent = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(1))
            .expect("parent object");
        let null_child = pdf
            .make_indirect_from_object_handle(ObjectHandle::null())
            .expect("null child");
        let stream_length = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(2))
            .expect("stream length");
        let stream_filter = pdf
            .make_indirect_from_object_handle(ObjectHandle::name(b"FlateDecode".to_vec()))
            .expect("stream filter");
        let stream_decode_parms = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(Vec::new()))
            .expect("stream decode parameters");
        let payload = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(9))
            .expect("stream payload");
        let thumb_payload = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(10))
            .expect("thumbnail payload");
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Length".to_vec(), stream_length.clone()),
                (b"/Filter".to_vec(), stream_filter.clone()),
                (b"/DecodeParms".to_vec(), stream_decode_parms.clone()),
                (b"/Payload".to_vec(), payload.clone()),
            ]),
            Rc::new(Vec::new()),
        );
        let thumb = pdf
            .make_indirect_from_object_handle(ObjectHandle::array(vec![thumb_payload.clone()]))
            .expect("thumbnail array");
        let page = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
                (b"/Parent".to_vec(), parent.clone()),
                (b"/Thumb".to_vec(), thumb.clone()),
                (
                    b"/Array".to_vec(),
                    ObjectHandle::array(vec![payload.clone()]),
                ),
                (b"/Null".to_vec(), null_child.clone()),
                (b"/Stream".to_vec(), stream),
            ]))
            .expect("page object");
        let page_ref = page.object_ref().expect("page identity");
        let parent_ref = parent.object_ref().expect("parent identity");
        let null_ref = null_child.object_ref().expect("null identity");
        let length_ref = stream_length.object_ref().expect("length identity");
        let filter_ref = stream_filter.object_ref().expect("filter identity");
        let decode_parms_ref = stream_decode_parms
            .object_ref()
            .expect("decode params identity");
        let payload_ref = payload.object_ref().expect("payload identity");
        let thumb_payload_ref = thumb_payload
            .object_ref()
            .expect("thumbnail payload identity");
        let thumb_ref = thumb.object_ref().expect("thumbnail identity");

        let mut optimization = Optimization::default();
        let mut skip_stream_parameters = |_: Option<QpdfObjGen>, _: &ObjectHandle| Ok(2);
        optimization
            .update_object_maps(&pdf, ObjectUser::Page(0), page, &mut skip_stream_parameters)
            .expect("walk page graph");

        assert!(optimization
            .raw_objects_for(&ObjectUser::Page(0))
            .contains(&QpdfObjGen::try_from_object_ref(page_ref).unwrap()));
        assert!(!optimization
            .raw_objects_for(&ObjectUser::Page(0))
            .contains(&QpdfObjGen::try_from_object_ref(parent_ref).unwrap()));
        assert!(!optimization
            .raw_objects_for(&ObjectUser::Page(0))
            .contains(&QpdfObjGen::try_from_object_ref(null_ref).unwrap()));
        assert!(!optimization
            .raw_objects_for(&ObjectUser::Page(0))
            .contains(&QpdfObjGen::try_from_object_ref(length_ref).unwrap()));
        assert!(!optimization
            .raw_objects_for(&ObjectUser::Page(0))
            .contains(&QpdfObjGen::try_from_object_ref(filter_ref).unwrap()));
        assert!(!optimization
            .raw_objects_for(&ObjectUser::Page(0))
            .contains(&QpdfObjGen::try_from_object_ref(decode_parms_ref).unwrap()));
        assert!(optimization
            .raw_objects_for(&ObjectUser::Page(0))
            .contains(&QpdfObjGen::try_from_object_ref(payload_ref).unwrap()));
        assert!(optimization
            .raw_objects_for(&ObjectUser::Thumbnail(0))
            .contains(&QpdfObjGen::try_from_object_ref(thumb_ref).unwrap()));
        assert!(optimization
            .raw_users_for(QpdfObjGen::try_from_object_ref(payload_ref).unwrap())
            .contains(&ObjectUser::Page(0)));
        assert!(optimization
            .raw_users_for(QpdfObjGen::try_from_object_ref(thumb_payload_ref).unwrap())
            .contains(&ObjectUser::Thumbnail(0)));
    }

    #[test]
    fn non_page_users_have_no_page_number() {
        assert_eq!(ObjectUser::Root.page_number(), 0);
        assert_eq!(ObjectUser::RootKey(b"Root".to_vec()).page_number(), 0);
        assert_eq!(ObjectUser::TrailerKey(b"Info".to_vec()).page_number(), 0);
    }

    #[test]
    fn missing_pages_runs_qpdfs_inherited_push_warning_boundary() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        pdf.root_handle()
            .expect("empty catalog")
            .remove_key(b"/Pages");

        Optimization::optimize(&mut pdf, &BTreeMap::new(), true, no_stream_parameter_skip)
            .expect("qpdf tolerates a missing page tree while optimizing");

        let messages: Vec<_> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message_string())
            .collect();
        assert_eq!(messages.len(), 6);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.contains("returning false for a key containment request"))
                .count(),
            1
        );
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.contains("dictionary attempted")
                    && message.contains("treating as empty"))
                .count(),
            1
        );
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.contains("returning null for attempted key retrieval"))
                .count(),
            1
        );
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.contains("operation for array attempted")
                    && message.contains("treating as empty"))
                .count(),
            3
        );
    }

    #[test]
    fn missing_page_warning_boundary_ignores_an_absent_root() {
        let mut pdf = Pdf::<std::io::Cursor<Vec<u8>>>::uninitialized();
        super::inherited_attrs::warn_missing_page_tree(&mut pdf)
            .expect("an absent root has no inherited-page warning boundary");
    }

    #[test]
    fn missing_page_warning_boundary_ignores_a_non_dictionary_root() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        pdf.trailer()
            .replace_key(b"/Root", ObjectHandle::integer(7))
            .expect("replace root");
        super::inherited_attrs::warn_missing_page_tree(&mut pdf)
            .expect("a non-dictionary root has no inherited-page warning boundary");
    }

    #[test]
    fn compact_object_user_set_preserves_qpdf_order_and_deduplicates() {
        let users = [
            ObjectUser::Root,
            ObjectUser::Page(2),
            ObjectUser::TrailerKey(b"Info".to_vec()),
            ObjectUser::Page(0),
            ObjectUser::RootKey(b"Outlines".to_vec()),
            ObjectUser::Thumbnail(1),
            ObjectUser::Page(2),
        ];
        let mut set = CompactObjectUserSet::default();
        for user in users.iter().cloned() {
            set.insert(user);
        }
        let expected: Vec<_> = users
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(set.iter().cloned().collect::<Vec<_>>(), expected);
        assert_eq!(set.len(), 6);
        assert!(!set.is_tree());
    }

    #[test]
    fn compact_object_user_set_promotes_large_cardinality_to_tree() {
        let mut set = CompactObjectUserSet::default();
        for page in 0..=8 {
            assert!(set.insert(ObjectUser::Page(page)));
        }
        assert!(set.is_tree());
        assert_eq!(set.len(), 9);
        assert!(set.contains(&ObjectUser::Page(4)));
        assert!(!set.contains(&ObjectUser::Page(99)));
        assert!(set.insert(ObjectUser::Root));
        assert_eq!(
            set.iter()
                .filter_map(|user| match user {
                    ObjectUser::Page(page) => Some(*page),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            (0..=8).collect::<Vec<_>>()
        );
        assert!(!set.insert(ObjectUser::Page(4)));
        assert_eq!(set.len(), 10);
    }

    #[test]
    fn optimization_does_not_retain_a_second_object_user_projection_pair() {
        // The qpdf-shaped state is two bidirectional maps plus the two
        // existing optional setup snapshots. Persistent ObjectRef projections
        // would add another two map-sized fields and defeat the ownership
        // convergence this type is meant to provide.
        let map_size = std::mem::size_of::<BTreeMap<ObjectUser, BTreeSet<QpdfObjGen>>>();
        assert!(
            std::mem::size_of::<Optimization>() <= map_size * 5,
            "Optimization retains a second persistent object-user projection pair"
        );
    }

    #[test]
    fn page_users_view_filters_non_page_users_without_cloning_a_set() {
        let object = ObjectRef::new(7, 0);
        let mut optimization = Optimization::default();
        optimization.record(ObjectUser::Page(2), object);
        optimization.record(ObjectUser::Root, object);
        optimization.record(ObjectUser::Page(0), object);
        assert_eq!(
            optimization.page_users(object).collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    #[test]
    fn linearization_reachable_object_refs_projects_the_canonical_user_map() {
        let mut optimization = Optimization::default();
        optimization.record(ObjectUser::Page(0), ObjectRef::new(7, 0));
        optimization.record(ObjectUser::Root, ObjectRef::new(9, 0));

        assert_eq!(
            optimization
                .linearization_reachable_object_refs()
                .collect::<Vec<_>>(),
            vec![ObjectRef::new(7, 0), ObjectRef::new(9, 0)]
        );
    }

    #[test]
    fn object_user_map_retains_a_projectionless_raw_generation() {
        let mut pdf = Pdf::empty().expect("create raw identity owner");
        let raw = pdf.get_object_handle_by_raw_identity(5, 65_536);
        let root = ObjectHandle::array(vec![raw]);
        let mut optimization = Optimization::default();

        optimization
            .update_object_maps(&pdf, ObjectUser::Root, root, &mut no_stream_parameter_skip)
            .expect("walk raw child");

        let raw = QpdfObjGen::new(5, 65_536);
        assert!(optimization
            .raw_objects_for(&ObjectUser::Root)
            .contains(&raw));
        assert!(!optimization
            .objects_for(&ObjectUser::Root)
            .any(|object| object == ObjectRef::new(5, 65_534)));
        assert!(optimization
            .raw_users_for(QpdfObjGen::new(99, 0))
            .iter()
            .next()
            .is_none());
        assert_eq!(optimization.users_for(ObjectRef::new(u32::MAX, 0)).len(), 0);
    }

    #[test]
    fn other_page_private_owner_applies_qpdf_user_gates() {
        let private = ObjectRef::new(8, 0);
        let shared = ObjectRef::new(9, 0);
        let first_page = ObjectRef::new(10, 0);
        let thumbnail = ObjectRef::new(11, 0);
        let document_other = ObjectRef::new(12, 0);
        let mut optimization = Optimization::default();
        optimization.record(ObjectUser::Page(2), private);
        optimization.record(ObjectUser::Page(2), shared);
        optimization.record(ObjectUser::Page(3), shared);
        optimization.record(ObjectUser::Page(0), first_page);
        optimization.record(ObjectUser::Page(2), first_page);
        optimization.record(ObjectUser::Page(2), thumbnail);
        optimization.record(ObjectUser::Thumbnail(1), thumbnail);
        optimization.record(ObjectUser::Page(2), document_other);
        optimization.record(ObjectUser::RootKey(b"Metadata".to_vec()), document_other);

        assert_eq!(optimization.other_page_private_owner(private), Some(2));
        assert_eq!(optimization.other_page_private_owner(shared), None);
        assert_eq!(optimization.other_page_private_owner(first_page), None);
        assert_eq!(optimization.other_page_private_owner(thumbnail), None);
        assert_eq!(optimization.other_page_private_owner(document_other), None);
    }

    #[test]
    fn object_user_walk_rejects_programmatic_depth_beyond_parser_limit() {
        let pdf = Pdf::empty().expect("create owner for direct test values");
        let mut optimization = Optimization::default();
        optimization
            .update_object_maps(
                &pdf,
                ObjectUser::Root,
                ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new())),
                &mut no_stream_parameter_skip,
            )
            .expect("the test callback must be exercised by a stream");
        let error = optimization
            .update_object_maps(
                &pdf,
                ObjectUser::Root,
                nested_direct_array(MAX_PARSE_DEPTH + 1),
                &mut no_stream_parameter_skip,
            )
            .expect_err("the object-user walk has a parser-depth guard");
        assert!(error.to_string().contains("maximum of 500"));
    }
}
