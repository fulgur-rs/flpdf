//! The core object-handle graph: shared, cloneable identity for direct and
//! indirect PDF objects, with qpdf-compatible parsed-offset tracking.
//!
//! qpdf correspondence: `QPDFObjectHandle` (`include/qpdf/QPDFObjectHandle.hh`) and its backing `QPDFValue` (`libqpdf/qpdf/QPDFValue.hh`).

// Deviation: shared handle identity uses Rc<RefCell<..>> in place of qpdf's
// std::shared_ptr<QPDFValue> — internal structure only, does not affect
// output bytes (see docs/qpdf-correspondence.md).

use crate::ObjectRef;
use std::cell::RefCell;
use std::rc::Rc;

/// The no-offset sentinel qpdf uses for values that were not parsed from a
/// source position (`QPDFValue`'s parsed offset starts at `-1` and is set
/// only while still negative; see
/// `libqpdf/qpdf/QPDFValue.hh:90-100,149-152`).
pub(crate) const NO_PARSED_OFFSET: i64 = -1;

/// A shared, cloneable handle to a PDF object.
///
/// Cloning a handle is O(1) and does not deep-copy the underlying value;
/// every clone of an indirect handle shares the same canonical identity and
/// resolution state.
#[derive(Clone, Debug)]
pub struct ObjectHandle(Repr);

#[derive(Clone, Debug)]
enum Repr {
    Direct(Rc<RefCell<DirectSlot>>),
    Indirect(Rc<RefCell<IndirectSlot>>),
}

#[derive(Debug)]
struct DirectSlot {
    #[allow(dead_code)] // populated in a later task
    value: Option<()>, // placeholder until ObjectValue lands in a later task
    parsed_offset: i64,
}

#[derive(Debug)]
#[allow(dead_code)] // constructed only by this module's own test-only factories for now
pub(crate) enum IndirectState {
    Unresolved,
    // Resolved/Missing/etc. variants land in a later task alongside the
    // real resolution engine cutover.
}

#[derive(Debug)]
struct IndirectSlot {
    object_ref: ObjectRef,
    #[allow(dead_code)]
    state: IndirectState,
    parsed_offset: i64,
}

impl ObjectHandle {
    /// True if this handle wraps a value constructed directly, without an
    /// indirect object number/generation.
    pub fn is_direct(&self) -> bool {
        matches!(self.0, Repr::Direct(_))
    }

    /// True if this handle refers to an indirect object.
    pub fn is_indirect(&self) -> bool {
        matches!(self.0, Repr::Indirect(_))
    }

    /// The object number/generation for an indirect handle, or `None` for a
    /// direct one.
    pub fn object_ref(&self) -> Option<ObjectRef> {
        match &self.0 {
            Repr::Indirect(slot) => Some(slot.borrow().object_ref),
            Repr::Direct(_) => None,
        }
    }

    /// True if `self` and `other` share the same underlying storage — the
    /// same canonical object, not merely an equal value.
    #[allow(dead_code)] // exercised by this module's identity tests; wired into
                        // production resolution/comparison paths in a later task
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Direct(a), Repr::Direct(b)) => Rc::ptr_eq(a, b),
            (Repr::Indirect(a), Repr::Indirect(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    // Used by this module's identity tests today; the reader wires up real
    // callers (and the `Unresolved{offset}` state) in a later task.
    #[allow(dead_code)]
    pub(crate) fn new_indirect_unresolved(object_ref: ObjectRef, offset: i64) -> Self {
        let _ = offset; // real Unresolved{offset} state lands in a later task
        Self(Repr::Indirect(Rc::new(RefCell::new(IndirectSlot {
            object_ref,
            state: IndirectState::Unresolved,
            parsed_offset: NO_PARSED_OFFSET,
        }))))
    }

    fn new_direct(parsed_offset: i64) -> Self {
        Self(Repr::Direct(Rc::new(RefCell::new(DirectSlot {
            value: None,
            parsed_offset,
        }))))
    }

    // Minimal factory to satisfy this task's tests; the full factory set
    // (null/boolean/real/name/string/array/dictionary/stream) lands in a
    // later task.
    #[allow(dead_code)]
    pub(crate) fn integer(_value: i64) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }

    /// The qpdf-compatible signed parsed offset. `-1` means the value was
    /// not parsed from a source position (`QPDFObjectHandle::getParsedOffset`,
    /// `include/qpdf/QPDFObjectHandle.hh:415-419`).
    pub fn get_parsed_offset(&self) -> i64 {
        match &self.0 {
            Repr::Direct(slot) => slot.borrow().parsed_offset,
            Repr::Indirect(slot) => slot.borrow().parsed_offset,
        }
    }

    // Record `offset` as the parsed offset, but only if none has been set
    // yet (matches qpdf: "set only while still negative",
    // `libqpdf/qpdf/QPDFValue.hh:90-100`). The parser wires up real callers
    // in a later task; exposed here so this module's own tests can exercise
    // the set-once contract without a live parser.
    #[allow(dead_code)]
    pub(crate) fn set_parsed_offset_if_unset(&self, offset: i64) {
        let set = |current: &mut i64| {
            if *current < 0 {
                *current = offset;
            }
        };
        match &self.0 {
            Repr::Direct(slot) => set(&mut slot.borrow_mut().parsed_offset),
            Repr::Indirect(slot) => set(&mut slot.borrow_mut().parsed_offset),
        }
    }

    // Payload is still discarded (a placeholder) — the real `ObjectValue`
    // payload (Null/Boolean/Integer/Array/Dictionary/Stream) lands in a
    // later task. These factories exist now only so the parsed-offset
    // contract can be tested independently of value representation.

    /// Construct a direct null value.
    pub fn null() -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }

    /// Construct a direct boolean value.
    pub fn boolean(_value: bool) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }

    /// Construct a direct real (floating-point) value.
    pub fn real(_value: f64) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }

    /// Construct a direct name value.
    pub fn name(_value: Vec<u8>) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }

    /// Construct a direct string value.
    pub fn string(_value: Vec<u8>) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }

    /// Construct a direct array value.
    pub fn array(_children: Vec<ObjectHandle>) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }

    /// Construct a direct dictionary value.
    pub fn dictionary(_entries: Vec<(Vec<u8>, ObjectHandle)>) -> Self {
        Self::new_direct(NO_PARSED_OFFSET)
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn direct_handle_clone_shares_identity_not_a_deep_copy() {
        let handle = ObjectHandle::integer(42);
        let clone = handle.clone();
        assert!(handle.ptr_eq(&clone));
    }

    #[test]
    fn two_direct_handles_with_equal_value_are_distinct_identity() {
        let a = ObjectHandle::integer(42);
        let b = ObjectHandle::integer(42);
        assert!(!a.ptr_eq(&b));
    }

    #[test]
    fn direct_handle_reports_direct_not_indirect() {
        let handle = ObjectHandle::integer(1);
        assert!(handle.is_direct());
        assert!(!handle.is_indirect());
        assert_eq!(handle.object_ref(), None);
    }

    #[test]
    fn indirect_handle_retains_object_ref_before_resolution() {
        let object_ref = ObjectRef::new(5, 0);
        let handle = ObjectHandle::new_indirect_unresolved(object_ref, 0);
        assert!(handle.is_indirect());
        assert!(!handle.is_direct());
        assert_eq!(handle.object_ref(), Some(object_ref));
    }

    #[test]
    fn cloning_an_indirect_handle_shares_the_same_slot() {
        let object_ref = ObjectRef::new(5, 0);
        let handle = ObjectHandle::new_indirect_unresolved(object_ref, 0);
        let clone = handle.clone();
        assert!(handle.ptr_eq(&clone));
    }

    #[test]
    fn a_direct_and_an_indirect_handle_are_never_identical() {
        let direct = ObjectHandle::integer(42);
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(5, 0), 0);
        assert!(!direct.ptr_eq(&indirect));
        assert!(!indirect.ptr_eq(&direct));
    }
}

#[cfg(test)]
mod parsed_offset_tests {
    use super::*;

    #[test]
    fn public_factory_direct_handles_default_to_no_offset_sentinel() {
        for handle in [
            ObjectHandle::null(),
            ObjectHandle::boolean(true),
            ObjectHandle::integer(1),
            ObjectHandle::real(1.5),
            ObjectHandle::name(b"Foo".to_vec()),
            ObjectHandle::string(b"bar".to_vec()),
            ObjectHandle::array(Vec::new()),
            ObjectHandle::dictionary(Vec::new()),
        ] {
            assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        }
    }

    #[test]
    fn new_indirect_unresolved_starts_at_no_offset_sentinel() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn set_parsed_offset_is_retained_once_set() {
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn first_nonnegative_offset_is_retained_a_second_set_is_ignored() {
        // "The first nonnegative offset assigned to a value is retained.
        // Resolution, cache access, unparse, and writer planning do not
        // recompute or replace it." (design, Parsed-Offset Contract)
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);
        handle.set_parsed_offset_if_unset(200);
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn indirect_handle_honors_the_same_set_once_contract() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_parsed_offset_if_unset(100);
        handle.set_parsed_offset_if_unset(200);
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn zero_is_a_legitimate_parsed_offset_not_treated_as_unset() {
        // The guard is a strict `< 0` check, so `0` (a real token-start
        // offset) must count as "already set" and block later writes, the
        // same as any other non-negative value.
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(0);
        assert_eq!(handle.get_parsed_offset(), 0);
        handle.set_parsed_offset_if_unset(50);
        assert_eq!(handle.get_parsed_offset(), 0);
    }
}
