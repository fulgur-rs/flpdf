//! qpdf correspondence: QPDF_optimization.cc optimization orchestration, inherited-page preparation, object-user maps, and compressed-object folding.

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
        let prepared = Self::prepare_pdf(pdf, allow_changes)?;
        let page_refs = prepared
            .as_ref()
            .map(|prepared| prepared.pages.as_slice())
            .unwrap_or_default();
        let mut maps = Self::build_maps(pdf, page_refs, skip_stream_parameters)?;
        maps.filter_compressed_objects(object_stream_data);
        Ok(maps)
    }

    fn prepare_pdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        allow_changes: bool,
    ) -> crate::Result<Option<crate::pages::repair::PreparedPages>> {
        if let Some(root_ref) = pdf.root_ref() {
            let root = pdf.get_object_handle(root_ref);
            let outlines = root.try_get_key(b"/Outlines")?;
            if outlines.try_is_dictionary()? && outlines.is_direct() {
                // qpdf's optimize makes a direct /Outlines dictionary indirect
                // without cloning its live allocation
                // (libqpdf/QPDF_optimization.cc:73-77).
                let outlines = pdf.make_indirect_from_object_handle(outlines)?;
                root.replace_key(b"/Outlines", outlines)?;
            }
        }

        let prepared = crate::pages::repair::prepare_for_optimization(pdf)?;
        if let Some(ref prepared) = prepared {
            inherited_attrs::push(pdf, prepared, allow_changes, false)?;
        }
        Ok(prepared)
    }

    pub(crate) fn prepare_for_linearized_write<R: Read + Seek>(
        pdf: &mut Pdf<R>,
    ) -> crate::Result<()> {
        Self::prepare_pdf(pdf, true).map(|_| ())
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
        page_refs: &[ObjectRef],
        mut skip_stream_parameters: F,
    ) -> crate::Result<Self>
    where
        R: Read + Seek,
        F: FnMut(Option<QpdfObjGen>, &ObjectHandle) -> crate::Result<u8>,
    {
        let mut maps = Self::default();

        for (page_number, &page_ref) in page_refs.iter().enumerate() {
            let page = pdf.get_object_handle(page_ref);
            maps.update_object_maps(
                ObjectUser::Page(page_number as u32),
                page,
                &mut skip_stream_parameters,
            )?;
        }

        let trailer = pdf.trailer();
        for key in trailer.try_get_keys()? {
            if key != b"/Root" {
                let user_key = key.strip_prefix(b"/").unwrap_or(&key).to_vec();
                maps.update_object_maps(
                    ObjectUser::TrailerKey(user_key),
                    trailer.try_get_key(&key)?,
                    &mut skip_stream_parameters,
                )?;
            }
        }

        if let Some(root_ref) = pdf.root_ref() {
            let root = pdf.get_object_handle(root_ref);
            for key in root.try_get_keys()? {
                let user_key = key.strip_prefix(b"/").unwrap_or(&key).to_vec();
                maps.update_object_maps(
                    ObjectUser::RootKey(user_key),
                    root.try_get_key(&key)?,
                    &mut skip_stream_parameters,
                )?;
            }
            maps.record(ObjectUser::Root, root_ref);
        }

        Ok(maps)
    }

    fn update_object_maps<F>(
        &mut self,
        user: ObjectUser,
        object: ObjectHandle,
        skip_stream_parameters: &mut F,
    ) -> crate::Result<()>
    where
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
            if pending.inline_depth > MAX_PARSE_DEPTH {
                return Err(crate::Error::Unsupported(format!(
                    "optimization: inline object nesting exceeds maximum of {MAX_PARSE_DEPTH}"
                )));
            }

            pending.object.try_dereference()?;
            if pending.object.try_is_null()? {
                if pending.via_array {
                    if let Some(object_gen) = pending.object.qpdf_obj_gen() {
                        if object_gen.get_obj() > 0 && visited.insert(object_gen) {
                            self.record_raw(pending.user, object_gen);
                        }
                    }
                }
                continue;
            }

            if is_page(&pending.object)? && !pending.top {
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

            if let Some(items) = pending.object.try_as_array()? {
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
                for key in stream_dict.try_get_keys()?.into_iter().rev() {
                    if (skip_level >= 1 && key == b"/Length")
                        || (skip_level >= 2
                            && matches!(key.as_slice(), b"/Filter" | b"/DecodeParms"))
                    {
                        continue;
                    }
                    stack.push(Pending {
                        object: stream_dict.try_get_key(&key)?,
                        user: pending.user.clone(),
                        top: false,
                        via_array: false,
                        inline_depth: inline_depth + 1,
                    });
                }
                continue;
            }

            if pending.object.try_is_dictionary()? {
                let page = is_page(&pending.object)?;
                for key in pending.object.try_get_keys()?.into_iter().rev() {
                    if page && key == b"/Parent" {
                        continue;
                    }
                    let child_user = if page && key == b"/Thumb" {
                        ObjectUser::Thumbnail(pending.user.page_number())
                    } else {
                        pending.user.clone()
                    };
                    stack.push(Pending {
                        object: pending.object.try_get_key(&key)?,
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

fn is_page(object: &ObjectHandle) -> crate::Result<bool> {
    object.try_is_dictionary_of_type(b"Page", b"")
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
    fn non_page_users_have_no_page_number() {
        assert_eq!(ObjectUser::Root.page_number(), 0);
        assert_eq!(ObjectUser::RootKey(b"Root".to_vec()).page_number(), 0);
        assert_eq!(ObjectUser::TrailerKey(b"Info".to_vec()).page_number(), 0);
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
    fn object_user_map_retains_a_projectionless_raw_generation() {
        let mut pdf = Pdf::empty().expect("create raw identity owner");
        let raw = pdf.get_object_handle_by_raw_identity(5, 65_536);
        let root = ObjectHandle::array(vec![raw]);
        let mut optimization = Optimization::default();

        optimization
            .update_object_maps(ObjectUser::Root, root, &mut no_stream_parameter_skip)
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
        let mut optimization = Optimization::default();
        optimization
            .update_object_maps(
                ObjectUser::Root,
                ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new())),
                &mut no_stream_parameter_skip,
            )
            .expect("the test callback must be exercised by a stream");
        let error = optimization
            .update_object_maps(
                ObjectUser::Root,
                nested_direct_array(MAX_PARSE_DEPTH + 1),
                &mut no_stream_parameter_skip,
            )
            .expect_err("the object-user walk has a parser-depth guard");
        assert!(error.to_string().contains("maximum of 500"));
    }
}
