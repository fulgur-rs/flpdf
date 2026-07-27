//! qpdf correspondence: qpdf 11.9.0 `libqpdf/QPDF_optimization.cc`
//! object-user map portion.

use crate::ObjectRef;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ObjectUser {
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

    fn record(&mut self, user: ObjectUser, object: ObjectRef) {
        self.user_to_objects
            .entry(user.clone())
            .or_default()
            .insert(object);
        self.object_to_users.entry(object).or_default().insert(user);
    }
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
    use super::{ObjectUser, Optimization};
    use crate::ObjectRef;
    use std::collections::BTreeSet;

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
}
