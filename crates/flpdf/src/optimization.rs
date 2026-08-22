//! qpdf correspondence: QPDF_optimization.cc optimization orchestration, inherited-page preparation, object-user maps, and compressed-object folding.

pub(crate) mod inherited_attrs;

use crate::object::MAX_INLINE_DEPTH;
use crate::{Object, ObjectRef, Pdf, Stream};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ObjectUser {
    #[allow(dead_code)]
    Bad,
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
        F: FnMut(Option<ObjectRef>, &Stream) -> u8,
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
            if let Object::Dictionary(mut root) = pdf.resolve(root_ref)? {
                if let Some(Object::Dictionary(outlines)) = root.get("Outlines").cloned() {
                    let outlines_ref = next_object_ref(pdf)?;
                    pdf.set_object(outlines_ref, Object::Dictionary(outlines));
                    root.insert("Outlines", Object::Reference(outlines_ref));
                    pdf.set_object(root_ref, Object::Dictionary(root));
                }
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
        F: FnMut(Option<ObjectRef>, &Stream) -> u8,
    {
        let mut maps = Self::default();

        for (page_number, &page_ref) in page_refs.iter().enumerate() {
            maps.update_object_maps(
                pdf,
                ObjectUser::Page(page_number as u32),
                Object::Reference(page_ref),
                &mut skip_stream_parameters,
            )?;
        }

        let trailer_entries = crate::qpdf_null::snapshot_entries(pdf.trailer(), false);
        for (key, value) in crate::qpdf_null::visible_entries(pdf, trailer_entries)? {
            if key != b"Root" {
                let update = maps.update_object_maps(
                    pdf,
                    ObjectUser::TrailerKey(key),
                    value,
                    &mut skip_stream_parameters,
                );
                update?;
            }
        }

        if let Some(root_ref) = pdf.root_ref() {
            if let Object::Dictionary(root) = pdf.resolve(root_ref)? {
                let root_entries = crate::qpdf_null::snapshot_entries(&root, false);
                for (key, value) in crate::qpdf_null::visible_entries(pdf, root_entries)? {
                    let update = maps.update_object_maps(
                        pdf,
                        ObjectUser::RootKey(key),
                        value,
                        &mut skip_stream_parameters,
                    );
                    update?;
                }
            }
            maps.record(ObjectUser::Root, root_ref);
        }

        Ok(maps)
    }

    fn update_object_maps<R, F>(
        &mut self,
        pdf: &mut Pdf<R>,
        user: ObjectUser,
        object: Object,
        skip_stream_parameters: &mut F,
    ) -> crate::Result<()>
    where
        R: Read + Seek,
        F: FnMut(Option<ObjectRef>, &Stream) -> u8,
    {
        let mut visited = BTreeSet::new();
        let mut stack = vec![Pending {
            object,
            object_ref: None,
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

            match pending.object {
                Object::Reference(object_ref) => {
                    let reference = Object::Reference(object_ref);
                    if crate::qpdf_null::value_is_null(pdf, &reference)? {
                        if !visited.insert(object_ref) {
                            continue;
                        }
                        if pending.via_array && object_ref.number > 0 {
                            self.record(pending.user, object_ref);
                        }
                        continue;
                    }

                    let resolved = pdf.resolve(object_ref)?;
                    if is_page(&resolved) && !pending.top {
                        continue;
                    }
                    if !visited.insert(object_ref) {
                        continue;
                    }
                    self.record(pending.user.clone(), object_ref);
                    stack.push(Pending {
                        object: resolved,
                        object_ref: Some(object_ref),
                        user: pending.user,
                        top: pending.top,
                        via_array: false,
                        inline_depth: 0,
                    });
                }
                Object::Array(items) => {
                    for item in items.into_iter().rev() {
                        stack.push(Pending {
                            object: item,
                            object_ref: None,
                            user: pending.user.clone(),
                            top: false,
                            via_array: true,
                            inline_depth: pending.inline_depth + 1,
                        });
                    }
                }
                Object::Dictionary(dict) => {
                    let page = is_page_dictionary(&dict);
                    if page && !pending.top {
                        continue;
                    }

                    let entries = crate::qpdf_null::snapshot_entries(&dict, false);
                    let mut children = Vec::new();
                    for (key, value) in crate::qpdf_null::visible_entries(pdf, entries)? {
                        if page && key == b"Parent" {
                            continue;
                        }
                        let child_user = if page && key == b"Thumb" {
                            ObjectUser::Thumbnail(pending.user.page_number())
                        } else {
                            pending.user.clone()
                        };
                        children.push((value, child_user));
                    }
                    for (object, user) in children.into_iter().rev() {
                        stack.push(Pending {
                            object,
                            object_ref: None,
                            user,
                            top: false,
                            via_array: false,
                            inline_depth: pending.inline_depth + 1,
                        });
                    }
                }
                Object::Stream(stream) => {
                    let skip_level = skip_stream_parameters(pending.object_ref, &stream);
                    let entries = crate::qpdf_null::snapshot_entries(&stream.dict, false);
                    let entries = entries
                        .into_iter()
                        .filter(|(key, _)| {
                            !((skip_level >= 1 && key == b"Length")
                                || (skip_level >= 2 && (key == b"Filter" || key == b"DecodeParms")))
                        })
                        .collect();
                    let children = crate::qpdf_null::visible_entries(pdf, entries)?;
                    for (_, object) in children.into_iter().rev() {
                        stack.push(Pending {
                            object,
                            object_ref: None,
                            user: pending.user.clone(),
                            top: false,
                            via_array: false,
                            inline_depth: pending.inline_depth + 1,
                        });
                    }
                }
                Object::Null
                | Object::Boolean(_)
                | Object::Integer(_)
                | Object::Real(_)
                | Object::RealLiteral { .. }
                | Object::Name(_)
                | Object::String(_)
                | Object::Operator(_)
                | Object::InlineImage(_) => {}
            }
        }

        Ok(())
    }
}

impl ObjectUser {
    fn page_number(&self) -> u32 {
        match self {
            Self::Page(page_number) | Self::Thumbnail(page_number) => *page_number,
            Self::Bad | Self::TrailerKey(_) | Self::RootKey(_) | Self::Root => 0,
        }
    }
}

struct Pending {
    object: Object,
    object_ref: Option<ObjectRef>,
    user: ObjectUser,
    top: bool,
    via_array: bool,
    inline_depth: usize,
}

fn is_page(object: &Object) -> bool {
    matches!(object, Object::Dictionary(dict) if is_page_dictionary(dict))
}

fn is_page_dictionary(dict: &crate::Dictionary) -> bool {
    matches!(dict.get("Type"), Some(Object::Name(name)) if name.as_slice() == b"Page")
}

fn empty_object_refs() -> &'static BTreeSet<ObjectRef> {
    static EMPTY: OnceLock<BTreeSet<ObjectRef>> = OnceLock::new();
    EMPTY.get_or_init(BTreeSet::new)
}

fn empty_object_users() -> &'static BTreeSet<ObjectUser> {
    static EMPTY: OnceLock<BTreeSet<ObjectUser>> = OnceLock::new();
    EMPTY.get_or_init(BTreeSet::new)
}

fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> crate::Result<ObjectRef> {
    let number = pdf
        .object_refs()
        .into_iter()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("object-number space exhausted".to_owned()))?;
    Ok(ObjectRef::new(number, 0))
}

#[cfg(test)]
mod tests {
    use super::{ObjectUser, Optimization};
    use crate::{Object, ObjectRef, Pdf};
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Cursor;

    fn pdf_bytes(bodies: &[(u32, &[u8])], trailer_entries: &[u8]) -> Vec<u8> {
        let size = bodies.iter().map(|(number, _)| *number).max().unwrap_or(0) + 1;
        let mut pdf = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = vec![0usize; size as usize];
        for (number, body) in bodies {
            offsets[*number as usize] = pdf.len();
            pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for offset in offsets.iter().skip(1) {
            if *offset == 0 {
                pdf.extend_from_slice(b"0000000000 65535 f \n");
            } else {
                pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
        }
        pdf.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R").as_bytes());
        if !trailer_entries.is_empty() {
            pdf.push(b' ');
            pdf.extend_from_slice(trailer_entries);
        }
        pdf.extend_from_slice(format!(" >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
        pdf
    }

    fn open_pdf(bodies: &[(u32, &[u8])], trailer_entries: &[u8]) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open_mem_owned(pdf_bytes(bodies, trailer_entries)).expect("fixture should parse")
    }

    fn build_maps(pdf: &mut Pdf<Cursor<Vec<u8>>>, skip_level: u8) -> Optimization {
        Optimization::optimize(pdf, &BTreeMap::new(), true, |_, _| skip_level)
            .expect("object-user maps should build")
    }

    fn too_deep_object() -> Object {
        let mut nested = Object::Null;
        for _ in 0..=crate::object::MAX_INLINE_DEPTH {
            nested = Object::Array(vec![nested]);
        }
        nested
    }

    fn direct_outlines_pdf() -> Pdf<Cursor<Vec<u8>>> {
        open_pdf(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R /Outlines << /Count 0 >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
                ),
            ],
            b"",
        )
    }

    #[test]
    fn optimize_makes_direct_outlines_indirect_before_building_maps() {
        let mut pdf = direct_outlines_pdf();

        let maps = Optimization::optimize(&mut pdf, &BTreeMap::new(), true, |_, _| 1).unwrap();

        assert_eq!(
            maps.objects_for(&ObjectUser::RootKey(b"Outlines".to_vec()))
                .len(),
            1
        );
        let catalog = pdf.resolve(pdf.root_ref().unwrap()).unwrap();
        assert!(matches!(
            catalog,
            Object::Dictionary(ref dict)
                if matches!(dict.get("Outlines"), Some(Object::Reference(_)))
        ));
    }

    #[test]
    fn no_change_optimize_still_indirectizes_outlines_like_qpdf() {
        let mut pdf = direct_outlines_pdf();
        let root = pdf.root_ref().unwrap();

        Optimization::optimize(&mut pdf, &BTreeMap::new(), false, |_, _| 1).unwrap();

        assert!(matches!(
            pdf.resolve(root).unwrap(),
            Object::Dictionary(ref dict)
                if matches!(dict.get("Outlines"), Some(Object::Reference(_)))
        ));
    }

    #[test]
    fn optimize_accepts_a_non_dictionary_root_without_outline_rewrite() {
        let mut pdf = open_pdf(&[(1, b"null")], b"");
        let root = pdf.root_ref().unwrap();

        let maps = Optimization::optimize(&mut pdf, &BTreeMap::new(), true, |_, _| 1).unwrap();

        assert!(maps.users_for(root).contains(&ObjectUser::Root));
        assert!(matches!(pdf.resolve(root).unwrap(), Object::Null));
    }

    #[test]
    fn filter_compressed_objects_rekeys_both_maps_to_container() {
        let member = ObjectRef::new(7, 3);
        let container = ObjectRef::new(20, 0);
        let user = ObjectUser::Page(0);
        let mut maps = Optimization::default();
        maps.record(user.clone(), member);

        maps.filter_compressed_objects(&BTreeMap::from([(7, 20)]));

        assert!(!maps.users_for(member).contains(&user));
        assert!(maps.users_for(container).contains(&user));
        assert!(maps.objects_for(&user).contains(&container));
    }

    #[test]
    fn users_for_members_returns_the_union_once() {
        let mut maps = Optimization::default();
        let a = ObjectRef::new(3, 0);
        let b = ObjectRef::new(4, 0);
        maps.record(ObjectUser::Page(0), a);
        maps.record(ObjectUser::Thumbnail(1), b);

        assert_eq!(
            maps.users_for_members([&a, &b]),
            BTreeSet::from([ObjectUser::Page(0), ObjectUser::Thumbnail(1)])
        );
    }

    #[test]
    fn object_user_order_matches_qpdf_discriminant_page_and_key_order() {
        let users = BTreeSet::from([
            ObjectUser::Root,
            ObjectUser::RootKey(b"Z".to_vec()),
            ObjectUser::Page(2),
            ObjectUser::Page(1),
            ObjectUser::Thumbnail(0),
            ObjectUser::TrailerKey(b"Info".to_vec()),
            ObjectUser::Bad,
        ]);
        assert_eq!(
            users.into_iter().collect::<Vec<_>>(),
            vec![
                ObjectUser::Bad,
                ObjectUser::Page(1),
                ObjectUser::Page(2),
                ObjectUser::Thumbnail(0),
                ObjectUser::TrailerKey(b"Info".to_vec()),
                ObjectUser::RootKey(b"Z".to_vec()),
                ObjectUser::Root,
            ]
        );
    }

    #[test]
    fn non_page_users_use_qpdfs_zero_page_number_fallback() {
        for user in [
            ObjectUser::Bad,
            ObjectUser::TrailerKey(b"Info".to_vec()),
            ObjectUser::RootKey(b"Pages".to_vec()),
            ObjectUser::Root,
        ] {
            assert_eq!(user.page_number(), 0);
        }
    }

    #[test]
    fn record_updates_both_maps_and_deduplicates() {
        let mut maps = Optimization::default();
        let user = ObjectUser::Page(0);
        let object = ObjectRef::new(7, 0);
        maps.record(user.clone(), object);
        maps.record(user.clone(), object);
        assert_eq!(maps.objects_for(&user), &BTreeSet::from([object]));
        assert_eq!(maps.users_for(object), &BTreeSet::from([user.clone()]));
        assert_eq!(
            maps.object_users()
                .map(|(object, users)| (object, users.clone()))
                .collect::<Vec<_>>(),
            vec![(object, BTreeSet::from([user]))]
        );
    }

    #[test]
    fn missing_queries_share_empty_sets_without_allocating() {
        let maps = Optimization::default();
        let first_objects = maps.objects_for(&ObjectUser::Page(0));
        let second_objects = maps.objects_for(&ObjectUser::Page(1));
        assert!(first_objects.is_empty());
        assert!(std::ptr::eq(first_objects, second_objects));

        let first_users = maps.users_for(ObjectRef::new(1, 0));
        let second_users = maps.users_for(ObjectRef::new(2, 0));
        assert!(first_users.is_empty());
        assert!(std::ptr::eq(first_users, second_users));
    }

    #[test]
    fn page_and_thumbnail_share_one_visited_set() {
        let mut pdf = open_pdf(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                      /Thumb << /Image 5 0 R >> /Zzz 5 0 R >>",
                ),
                (5, b"<< /Marker /thumbnail-first >>"),
            ],
            b"",
        );
        let maps = build_maps(&mut pdf, 1);
        let target = ObjectRef::new(5, 0);
        assert_eq!(
            maps.users_for(target),
            &BTreeSet::from([ObjectUser::Thumbnail(0)])
        );
        assert!(maps.referenced_pages(target).is_empty());
        assert_eq!(maps.thumbnail_objects(), BTreeSet::from([target]));
        assert!(!maps.objects_for(&ObjectUser::Page(0)).contains(&target));
        assert!(
            !maps
                .objects_for(&ObjectUser::Page(0))
                .contains(&ObjectRef::new(2, 0)),
            "page /Parent must not be traversed"
        );
    }

    #[test]
    fn non_top_page_is_a_boundary() {
        let mut pdf = open_pdf(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                      /Foreign 4 0 R >>",
                ),
                (4, b"<< /Type /Page /Leak 5 0 R >>"),
                (5, b"<< /MustNotBeReached true >>"),
            ],
            b"",
        );
        let maps = build_maps(&mut pdf, 1);
        let page = maps.objects_for(&ObjectUser::Page(0));
        assert!(!page.contains(&ObjectRef::new(4, 0)));
        assert!(!page.contains(&ObjectRef::new(5, 0)));
    }

    #[test]
    fn dictionary_null_is_hidden_but_array_null_keeps_indirect_identity() {
        let mut pdf = open_pdf(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                      /Array [6 0 R] /Direct 6 0 R /Missing 7 0 R >>",
                ),
                (6, b"null"),
            ],
            b"",
        );
        let maps = build_maps(&mut pdf, 1);
        let page = maps.objects_for(&ObjectUser::Page(0));
        assert!(page.contains(&ObjectRef::new(6, 0)));
        assert!(!page.contains(&ObjectRef::new(7, 0)));
    }

    #[test]
    fn cyclic_references_terminate_and_record_each_object_once() {
        let mut pdf = open_pdf(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                      /Resources 4 0 R >>",
                ),
                (4, b"<< /Next 5 0 R >>"),
                (5, b"<< /Next 4 0 R >>"),
            ],
            b"",
        );
        let maps = build_maps(&mut pdf, 1);
        assert_eq!(
            maps.objects_for(&ObjectUser::Page(0)),
            &BTreeSet::from([
                ObjectRef::new(3, 0),
                ObjectRef::new(4, 0),
                ObjectRef::new(5, 0),
            ])
        );
    }

    #[test]
    fn stream_skip_levels_exclude_only_the_requested_parameter_refs() {
        let bodies: &[(u32, &[u8])] = &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                  /Contents 4 0 R >>",
            ),
            (
                4,
                b"<< /Length 5 0 R /Filter 6 0 R /DecodeParms 7 0 R /Other 8 0 R >>\
                  \nstream\nA\nendstream",
            ),
            (5, b"1"),
            (6, b"/FlateDecode"),
            (7, b"<< /Predictor 1 >>"),
            (8, b"<< /Visible true >>"),
        ];

        let mut keep_all_pdf = open_pdf(bodies, b"");
        let keep_all = build_maps(&mut keep_all_pdf, 0);
        let mut skip_length_pdf = open_pdf(bodies, b"");
        let skip_length = build_maps(&mut skip_length_pdf, 1);
        let mut skip_all_pdf = open_pdf(bodies, b"");
        let skip_all = build_maps(&mut skip_all_pdf, 3);
        let user = ObjectUser::Page(0);

        for number in 5..=8 {
            assert!(keep_all
                .objects_for(&user)
                .contains(&ObjectRef::new(number, 0)));
        }
        assert!(!skip_length
            .objects_for(&user)
            .contains(&ObjectRef::new(5, 0)));
        for number in 6..=8 {
            assert!(skip_length
                .objects_for(&user)
                .contains(&ObjectRef::new(number, 0)));
        }
        for number in 5..=7 {
            assert!(!skip_all
                .objects_for(&user)
                .contains(&ObjectRef::new(number, 0)));
        }
        assert!(skip_all.objects_for(&user).contains(&ObjectRef::new(8, 0)));
    }

    #[test]
    fn catalog_and_trailer_keys_receive_distinct_users() {
        let mut pdf = open_pdf(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R \
                      /OpenAction 7 0 R /Names 9 0 R >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
                ),
                (5, b"<< /First 6 0 R >>"),
                (6, b"<< /Title (item) >>"),
                (7, b"<< /Next 8 0 R >>"),
                (8, b"<< /S /Named >>"),
                (9, b"<< /Dests 10 0 R >>"),
                (10, b"<< /Names [] >>"),
                (11, b"<< /Producer (flpdf) >>"),
                (12, b"<< /Filter /Standard >>"),
            ],
            b"/Info 11 0 R /CustomTrailer 12 0 R",
        );
        let maps = build_maps(&mut pdf, 1);
        assert_eq!(
            maps.objects_for(&ObjectUser::RootKey(b"Outlines".to_vec())),
            &BTreeSet::from([ObjectRef::new(5, 0), ObjectRef::new(6, 0)])
        );
        assert_eq!(
            maps.objects_for_root_key(b"Outlines"),
            BTreeSet::from([ObjectRef::new(5, 0), ObjectRef::new(6, 0)])
        );
        assert_eq!(
            maps.objects_for(&ObjectUser::RootKey(b"OpenAction".to_vec())),
            &BTreeSet::from([ObjectRef::new(7, 0), ObjectRef::new(8, 0)])
        );
        assert_eq!(
            maps.objects_for(&ObjectUser::RootKey(b"Names".to_vec())),
            &BTreeSet::from([ObjectRef::new(9, 0), ObjectRef::new(10, 0)])
        );
        assert_eq!(
            maps.objects_for(&ObjectUser::TrailerKey(b"Info".to_vec())),
            &BTreeSet::from([ObjectRef::new(11, 0)])
        );
        assert_eq!(
            maps.objects_for_trailer_key(b"Info"),
            BTreeSet::from([ObjectRef::new(11, 0)])
        );
        assert_eq!(
            maps.objects_for(&ObjectUser::TrailerKey(b"CustomTrailer".to_vec())),
            &BTreeSet::from([ObjectRef::new(12, 0)])
        );
        assert_eq!(
            maps.objects_for(&ObjectUser::Root),
            &BTreeSet::from([ObjectRef::new(1, 0)])
        );
        assert!(
            !maps
                .objects_for(&ObjectUser::RootKey(b"Pages".to_vec()))
                .contains(&ObjectRef::new(3, 0)),
            "catalog /Pages traversal must stop at the page boundary"
        );
    }

    #[test]
    fn missing_and_non_dictionary_roots_keep_only_qpdf_root_identity() {
        let mut without_root = pdf_bytes(&[(1, b"<<>>")], b"");
        let marker = b" /Root 1 0 R";
        let offset = without_root
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("fixture should contain /Root");
        without_root[offset..offset + marker.len()].fill(b' ');
        let mut without_root =
            Pdf::open_mem_owned(without_root).expect("rootless fixture should parse");
        assert!(without_root.root_ref().is_none());
        let maps = Optimization::build_maps(&mut without_root, &[], |_, _| 1)
            .expect("rootless document should build empty maps");
        assert!(maps.objects_for(&ObjectUser::Root).is_empty());

        let mut non_dictionary_root = open_pdf(&[(1, b"42")], b"");
        let maps = Optimization::build_maps(&mut non_dictionary_root, &[], |_, _| 1)
            .expect("non-dictionary root should retain its identity");
        assert_eq!(
            maps.objects_for(&ObjectUser::Root),
            &BTreeSet::from([ObjectRef::new(1, 0)])
        );
    }

    #[test]
    fn excessive_trailer_and_catalog_key_depth_errors_propagate() {
        let mut trailer_pdf = open_pdf(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
                ),
                (5, b"null"),
            ],
            b"/CustomTrailer 5 0 R",
        );
        trailer_pdf.set_object(ObjectRef::new(5, 0), too_deep_object());
        let error = Optimization::build_maps(&mut trailer_pdf, &[], |_, _| 1)
            .expect_err("trailer traversal error must propagate");
        assert!(matches!(error, crate::Error::Unsupported(_)));

        let mut catalog_pdf = open_pdf(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R /Deep 5 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
                ),
                (5, b"null"),
            ],
            b"",
        );
        catalog_pdf.set_object(ObjectRef::new(5, 0), too_deep_object());
        let error = Optimization::build_maps(&mut catalog_pdf, &[], |_, _| 1)
            .expect_err("catalog traversal error must propagate");
        assert!(matches!(error, crate::Error::Unsupported(_)));
    }

    #[test]
    fn excessive_direct_inline_depth_is_rejected() {
        let mut pdf = open_pdf(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
                ),
            ],
            b"",
        );
        Optimization::prepare_for_linearized_write(&mut pdf).unwrap();
        let pages = crate::pages::page_refs(&mut pdf).unwrap();
        let mut page = pdf
            .resolve(ObjectRef::new(3, 0))
            .unwrap()
            .into_dict()
            .expect("page should be a dictionary");
        page.insert("Deep", too_deep_object());
        pdf.set_object(ObjectRef::new(3, 0), Object::Dictionary(page));

        let error = Optimization::build_maps(&mut pdf, &pages, |_, _| 1)
            .expect_err("excessive direct nesting must fail");
        assert!(
            matches!(error, crate::Error::Unsupported(ref message) if message.contains("inline object nesting exceeds maximum")),
            "unexpected error: {error:?}"
        );
    }
}
