//! qpdf correspondence: QPDF_optimization.cc optimization orchestration, inherited-page preparation, object-user maps, and compressed-object folding.

pub(crate) mod inherited_attrs;

use crate::object_handle::MAX_INLINE_DEPTH;
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

#[derive(Debug, Clone, Default)]
pub(crate) struct Optimization {
    user_to_objects: BTreeMap<ObjectUser, BTreeSet<ObjectRef>>,
    object_to_users: BTreeMap<ObjectRef, BTreeSet<ObjectUser>>,
    /// qpdf's Generate object-stream eligibility captured before optimization
    /// can mint inherited-attribute objects.
    generate_objstm_eligible: Option<Vec<ObjectRef>>,
    /// Object identities present before optimization. Newly minted first-half
    /// plain objects are emitted after qpdf's ObjStm containers.
    pre_optimization_object_refs: Option<BTreeSet<ObjectRef>>,
}

impl Optimization {
    pub(crate) fn objects_for(&self, user: &ObjectUser) -> &BTreeSet<ObjectRef> {
        match self.user_to_objects.get(user) {
            Some(objects) => objects,
            None => empty_object_refs(),
        }
    }

    pub(crate) fn users_for(&self, object: ObjectRef) -> &BTreeSet<ObjectUser> {
        match self.object_to_users.get(&object) {
            Some(users) => users,
            None => empty_object_users(),
        }
    }

    pub(crate) fn object_users(&self) -> impl Iterator<Item = (ObjectRef, &BTreeSet<ObjectUser>)> {
        self.object_to_users
            .iter()
            .map(|(&object, users)| (object, users))
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

    pub(crate) fn referenced_pages(&self, object: ObjectRef) -> BTreeSet<u32> {
        self.users_for(object)
            .iter()
            .filter_map(|user| match user {
                ObjectUser::Page(page_number) => Some(*page_number),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn thumbnail_objects(&self) -> BTreeSet<ObjectRef> {
        self.user_to_objects
            .iter()
            .filter_map(|(user, objects)| match user {
                ObjectUser::Thumbnail(_) => Some(objects),
                _ => None,
            })
            .flat_map(|objects| objects.iter().copied())
            .collect()
    }

    pub(crate) fn objects_for_root_key(&self, key: &[u8]) -> BTreeSet<ObjectRef> {
        self.objects_for(&ObjectUser::RootKey(key.to_vec())).clone()
    }

    pub(crate) fn objects_for_trailer_key(&self, key: &[u8]) -> BTreeSet<ObjectRef> {
        self.objects_for(&ObjectUser::TrailerKey(key.to_vec()))
            .clone()
    }

    fn record(&mut self, user: ObjectUser, object: ObjectRef) {
        self.user_to_objects
            .entry(user.clone())
            .or_default()
            .insert(object);
        self.object_to_users.entry(object).or_default().insert(user);
    }

    pub(crate) fn optimize<R, F>(
        pdf: &mut Pdf<R>,
        object_stream_data: &BTreeMap<u32, u32>,
        allow_changes: bool,
        skip_stream_parameters: F,
    ) -> crate::Result<Self>
    where
        R: Read + Seek,
        F: FnMut(Option<ObjectRef>, &ObjectHandle) -> crate::Result<u8>,
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
            pdf.resolve(&root)?;
            let outlines = root.try_get_key(b"/Outlines")?;
            if outlines.try_as_dictionary()?.is_some() && outlines.is_direct() {
                // qpdf's optimize makes a direct /Outlines dictionary indirect
                // without cloning its live allocation
                // (libqpdf/QPDF_optimization.cc:73-77).
                let outlines = pdf.make_indirect_from_object_handle(outlines)?;
                root.replace_key(b"/Outlines", outlines)?;
                pdf.mark_object_handle_dirty(&root)?;
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
        for (user, objects) in &self.user_to_objects {
            for &object in objects {
                let target = object_stream_data
                    .get(&object.number)
                    .map(|&stream| ObjectRef::new(stream, 0))
                    .unwrap_or(object);
                filtered.record(user.clone(), target);
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
        F: FnMut(Option<ObjectRef>, &ObjectHandle) -> crate::Result<u8>,
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
            pdf.resolve(&root)?;
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
        F: FnMut(Option<ObjectRef>, &ObjectHandle) -> crate::Result<u8>,
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
            if pending.inline_depth > MAX_INLINE_DEPTH {
                return Err(crate::Error::Unsupported(format!(
                    "optimization: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
                )));
            }

            pending.object.try_dereference()?;
            if pending.object.try_is_null()? {
                if pending.via_array {
                    if let Some(object_ref) = pending.object.object_ref() {
                        if object_ref.number > 0 && visited.insert(object_ref) {
                            self.record(pending.user, object_ref);
                        }
                    }
                }
                continue;
            }

            if is_page(&pending.object)? && !pending.top {
                continue;
            }
            if let Some(object_ref) = pending.object.object_ref() {
                if !visited.insert(object_ref) {
                    continue;
                }
                self.record(pending.user.clone(), object_ref);
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
                    skip_stream_parameters(pending.object.object_ref(), &pending.object)?;
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

            if pending.object.try_as_dictionary()?.is_some() {
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

fn empty_object_refs() -> &'static BTreeSet<ObjectRef> {
    static EMPTY: OnceLock<BTreeSet<ObjectRef>> = OnceLock::new();
    EMPTY.get_or_init(BTreeSet::new)
}

fn empty_object_users() -> &'static BTreeSet<ObjectUser> {
    static EMPTY: OnceLock<BTreeSet<ObjectUser>> = OnceLock::new();
    EMPTY.get_or_init(BTreeSet::new)
}

#[cfg(test)]
mod tests {
    use super::ObjectUser;

    #[test]
    fn non_page_users_have_no_page_number() {
        assert_eq!(ObjectUser::Root.page_number(), 0);
        assert_eq!(ObjectUser::RootKey(b"Root".to_vec()).page_number(), 0);
        assert_eq!(ObjectUser::TrailerKey(b"Info".to_vec()).page_number(), 0);
    }
}
