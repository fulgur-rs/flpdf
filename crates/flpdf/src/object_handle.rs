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

/// The value payload of a direct `ObjectHandle`, mirroring qpdf's
/// `QPDFValue` type family (`libqpdf/qpdf/QPDFValue.hh`) and this crate's
/// existing [`crate::Object`] enum.
///
/// Array and dictionary children are [`ObjectHandle`]s rather than raw
/// nested `ObjectValue`s, so cloning a container clones only `Rc` handles
/// (O(1) per child), not the subtree.
#[derive(Debug)]
pub(crate) enum ObjectValue {
    Null,
    #[allow(dead_code)] // as_boolean accessor lands in a later task
    Boolean(bool),
    Integer(i64),
    #[allow(dead_code)] // as_real accessor lands in a later task
    Real(f64),
    /// Preserves a non-canonical source spelling (e.g. `.4`) alongside its
    /// parsed value, mirroring [`crate::Object::RealLiteral`], so that a
    /// real number written in the source PDF unparses byte-identically.
    RealLiteral {
        value: f64,
        literal: Vec<u8>,
    },
    #[allow(dead_code)] // as_name accessor lands in a later task
    Name(Vec<u8>),
    #[allow(dead_code)] // as_string accessor lands in a later task
    String(Vec<u8>),
    Array(Vec<ObjectHandle>),
    Dictionary(std::collections::BTreeMap<Vec<u8>, ObjectHandle>),
    /// A stream's own value: its dictionary (a separately parsed handle
    /// carrying its own `<<`-start parsed offset) and its raw encoded byte
    /// payload. The stream value's own parsed offset (see
    /// [`ObjectHandle::get_parsed_offset`]) is the encoded stream-data
    /// start, distinct from the dictionary's.
    Stream {
        dict: ObjectHandle,
        data: Vec<u8>,
    },
}

#[derive(Debug)]
struct DirectSlot {
    value: ObjectValue,
    parsed_offset: i64,
}

/// The resolution state of an indirect handle's backing slot.
///
/// `Missing` and `Resolved(ObjectValue::Null)` are kept as distinct variants
/// even though both currently present the same externally-observable value
/// (`is_null() == true`): the former is a reference absent from — or broken
/// in — the source cross-reference table (`Pdf::resolve_object_handle`'s
/// fallback arm), the latter is a genuinely parsed literal `null` object.
/// Collapsing them into one variant would lose that distinction the moment a
/// later task needs it (e.g. to tell a dangling reference apart from a real
/// null value for diagnostics).
#[derive(Debug)]
pub(crate) enum IndirectState {
    NotYetResolved,
    Resolved(ObjectValue),
    Missing,
}

#[derive(Debug)]
struct IndirectSlot {
    object_ref: ObjectRef,
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
            state: IndirectState::NotYetResolved,
            parsed_offset: NO_PARSED_OFFSET,
        }))))
    }

    fn new_direct(value: ObjectValue, parsed_offset: i64) -> Self {
        Self(Repr::Direct(Rc::new(RefCell::new(DirectSlot {
            value,
            parsed_offset,
        }))))
    }

    /// Construct a direct handle wrapping an already-built [`ObjectValue`], at
    /// the no-offset sentinel. Used by the resolution bridge
    /// (`Pdf::lift`/`Pdf::lift_to_handle`) to wrap a value lifted from a
    /// legacy [`crate::Object`] without going through one of the typed public
    /// factories above.
    pub(crate) fn from_value(value: ObjectValue) -> Self {
        Self::new_direct(value, NO_PARSED_OFFSET)
    }

    /// Consume a directly-constructed, exclusively-owned handle and return
    /// its value and parsed offset without cloning.
    ///
    /// Used by the parser's top-level file-object handle entry point
    /// (`parser::parse_qpdf_direct_object_handle`), which builds the
    /// top-level value as a handle purely to reuse the same
    /// offset-assignment machinery as every nested child, then immediately
    /// unwraps it into the pre-existing indirect slot the resolved object
    /// actually belongs to.
    ///
    /// Returns `None` for an indirect handle, or for a direct handle whose
    /// `Rc` is still shared elsewhere (refcount > 1) — the latter cannot
    /// happen for a handle a caller alone constructed and never cloned.
    pub(crate) fn into_direct_value(self) -> Option<(ObjectValue, i64)> {
        match self.0 {
            Repr::Direct(rc) => {
                let slot = Rc::try_unwrap(rc).ok()?.into_inner();
                Some((slot.value, slot.parsed_offset))
            }
            // Unreachable via this module's sole caller
            // (`parser::parse_qpdf_direct_object_handle`): `top_level_no_reference`
            // forces every top-level parse to `Integer`, never a reference,
            // so the handle it builds and consumes here is always direct.
            Repr::Indirect(_) => None, // cov:ignore: unreachable per the invariant noted above
        }
    }

    /// Mark this indirect handle's value as resolved to `value`. A no-op for
    /// a direct handle, which has no resolution state to update.
    pub(crate) fn set_resolved(&self, value: ObjectValue) {
        if let Repr::Indirect(slot) = &self.0 {
            slot.borrow_mut().state = IndirectState::Resolved(value);
        }
    }

    /// Mark this indirect handle as resolved-to-null because its reference is
    /// absent from — or broken in — the source cross-reference table (see
    /// [`IndirectState`]). A no-op for a direct handle, which has no
    /// resolution state to update.
    pub(crate) fn set_missing(&self) {
        if let Repr::Indirect(slot) = &self.0 {
            slot.borrow_mut().state = IndirectState::Missing;
        }
    }

    /// True if this handle's value is known without performing resolution: a
    /// direct handle always is; an indirect handle is once its state has left
    /// [`IndirectState::NotYetResolved`], whether that landed on a real value
    /// or on [`IndirectState::Missing`].
    pub(crate) fn is_resolved(&self) -> bool {
        match &self.0 {
            Repr::Direct(_) => true,
            Repr::Indirect(slot) => !matches!(slot.borrow().state, IndirectState::NotYetResolved),
        }
    }

    /// Construct a direct integer value.
    pub fn integer(value: i64) -> Self {
        Self::new_direct(ObjectValue::Integer(value), NO_PARSED_OFFSET)
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

    /// Construct a direct null value.
    pub fn null() -> Self {
        Self::new_direct(ObjectValue::Null, NO_PARSED_OFFSET)
    }

    /// Construct a direct boolean value.
    pub fn boolean(value: bool) -> Self {
        Self::new_direct(ObjectValue::Boolean(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct real (floating-point) value.
    pub fn real(value: f64) -> Self {
        Self::new_direct(ObjectValue::Real(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct name value.
    pub fn name(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::Name(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct string value.
    pub fn string(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::String(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct array value. Child values are handles, so cloning
    /// or re-reading this array's children never deep-copies their subtrees.
    pub fn array(children: Vec<ObjectHandle>) -> Self {
        Self::new_direct(ObjectValue::Array(children), NO_PARSED_OFFSET)
    }

    /// Construct a direct dictionary value from `entries`. Iteration order
    /// is the lexicographic order of the keys, not insertion order (matching
    /// [`crate::Dictionary`]); a repeated key keeps its last value. Values
    /// are handles, so cloning or re-reading this dictionary's entries never
    /// deep-copies their subtrees.
    pub fn dictionary(entries: Vec<(Vec<u8>, ObjectHandle)>) -> Self {
        Self::new_direct(
            ObjectValue::Dictionary(entries.into_iter().collect()),
            NO_PARSED_OFFSET,
        )
    }

    /// Construct a direct real value that preserves a non-canonical source
    /// literal (e.g. `.4`) alongside its parsed value, mirroring
    /// [`crate::Object::RealLiteral`], so that a real number written in the
    /// source PDF unparses byte-identically. `literal` is expected to parse
    /// back to `value` and to differ from `value`'s canonical string form —
    /// see [`crate::Object::RealLiteral`]'s own documented invariant.
    pub fn real_literal(value: f64, literal: Vec<u8>) -> Self {
        Self::new_direct(
            ObjectValue::RealLiteral { value, literal },
            NO_PARSED_OFFSET,
        )
    }

    /// The value as an `f64`/literal-bytes pair if this handle is a direct
    /// real value with a preserved source literal, or `None` otherwise —
    /// including for any indirect handle, whose value is not read here.
    pub fn as_real_literal(&self) -> Option<(f64, Vec<u8>)> {
        self.with_value(|value| match value {
            Some(ObjectValue::RealLiteral { value, literal }) => Some((*value, literal.clone())),
            _ => None,
        })
    }

    /// True if this handle's value is known to be null. An indirect handle
    /// whose value has not yet been resolved returns `false` — this method
    /// never performs resolution itself, so an unresolved handle is not
    /// assumed to be null. Once resolved, this reflects the real value:
    /// `true` both for a genuinely parsed `null` object and for a reference
    /// that turned out to be missing from the source.
    pub fn is_null(&self) -> bool {
        self.with_value(|value| matches!(value, Some(ObjectValue::Null)))
    }

    /// The value as `i64` if this handle is a direct integer value, or
    /// `None` otherwise — including for any indirect handle, whose value
    /// is not read here.
    pub fn as_integer(&self) -> Option<i64> {
        self.with_value(|value| match value {
            Some(ObjectValue::Integer(n)) => Some(*n),
            _ => None,
        })
    }

    /// The child handles if this handle is a direct array value, or `None`
    /// otherwise — including for any indirect handle, whose value is not
    /// read here. Cloning the returned `Vec` clones only the child `Rc`
    /// handles, not their subtrees.
    pub fn as_array(&self) -> Option<Vec<ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => Some(children.clone()),
            _ => None,
        })
    }

    /// The entries if this handle is a direct dictionary value, or `None`
    /// otherwise — including for any indirect handle, whose value is not
    /// read here. Cloning the returned map clones only the child `Rc`
    /// handles, not their subtrees.
    pub fn as_dictionary(&self) -> Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => Some(entries.clone()),
            _ => None,
        })
    }

    /// The stream's own dictionary handle if this handle is a direct stream
    /// value, or `None` otherwise — including for any indirect handle, whose
    /// value is not read here. Cloning the returned handle is O(1): it
    /// shares the dictionary's identity rather than copying its subtree.
    pub fn as_stream_dict(&self) -> Option<ObjectHandle> {
        self.with_value(|value| match value {
            Some(ObjectValue::Stream { dict, .. }) => Some(dict.clone()),
            _ => None,
        })
    }

    /// The stream's raw encoded byte payload if this handle is a direct
    /// stream value, or `None` otherwise — including for any indirect
    /// handle, whose value is not read here.
    pub fn as_stream_data(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Stream { data, .. }) => Some(data.clone()),
            _ => None,
        })
    }

    // `None` for an indirect handle that has not yet been resolved — value
    // access on an unresolved handle must not perform hidden I/O (design,
    // `Pdf` section). A resolved indirect handle exposes its real value;
    // `Missing` (see [`IndirectState`]) presents as `ObjectValue::Null`,
    // matching the externally-observable behavior of a resolved literal
    // `null` object.
    fn with_value<T>(&self, f: impl FnOnce(Option<&ObjectValue>) -> T) -> T {
        match &self.0 {
            Repr::Direct(slot) => f(Some(&slot.borrow().value)),
            Repr::Indirect(slot) => match &slot.borrow().state {
                IndirectState::NotYetResolved => f(None),
                IndirectState::Resolved(value) => f(Some(value)),
                IndirectState::Missing => f(Some(&ObjectValue::Null)),
            },
        }
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
mod object_value_tests {
    use super::*;

    #[test]
    fn integer_handle_round_trips_its_value() {
        let handle = ObjectHandle::integer(42);
        assert_eq!(handle.as_integer(), Some(42));
    }

    #[test]
    fn array_handle_holds_child_handles_not_raw_values() {
        let child = ObjectHandle::integer(7);
        let array = ObjectHandle::array(vec![child.clone()]);
        let children = array.as_array().expect("array");
        assert_eq!(children.len(), 1);
        assert!(children[0].ptr_eq(&child));
    }

    #[test]
    fn dictionary_handle_preserves_insertion_of_child_handles() {
        let value = ObjectHandle::name(b"Type".to_vec());
        let dict = ObjectHandle::dictionary(vec![(b"Key".to_vec(), value.clone())]);
        let entries = dict.as_dictionary().expect("dictionary");
        assert!(entries.get(b"Key".as_slice()).unwrap().ptr_eq(&value));
    }

    #[test]
    fn null_handle_is_null() {
        assert!(ObjectHandle::null().is_null());
        assert!(!ObjectHandle::integer(0).is_null());
    }

    #[test]
    fn real_literal_handle_preserves_the_non_canonical_source_literal() {
        // Object::RealLiteral exists so a non-canonical source spelling
        // (e.g. ".4") survives unparse byte-identically. The handle payload
        // must carry the same two fields, or byte-identical output breaks
        // the moment a real-literal round-trips through this layer.
        let handle = ObjectHandle::real_literal(0.4, b".4".to_vec());
        assert_eq!(handle.as_real_literal(), Some((0.4, b".4".to_vec())));
    }

    #[test]
    fn accessors_return_none_for_a_mismatched_direct_value() {
        // `as_integer`/`as_array`/`as_dictionary`/`as_real_literal`/
        // `as_stream_dict`/`as_stream_data` must reject a direct value of
        // the wrong variant, not just a missing one — the same `_ => None`
        // arm handles both cases.
        let handle = ObjectHandle::string(b"not-an-integer".to_vec());
        assert_eq!(handle.as_integer(), None);
        assert!(handle.as_array().is_none());
        assert!(handle.as_dictionary().is_none());
        assert_eq!(handle.as_real_literal(), None);
        assert!(handle.as_stream_dict().is_none());
        assert!(handle.as_stream_data().is_none());
    }

    #[test]
    fn accessors_return_none_for_an_indirect_handle_before_resolution() {
        // `with_value` never performs hidden I/O to resolve an indirect
        // handle (design, `Pdf` section), so today every indirect handle
        // reads as "value not known" — surfaced as `None` from the typed
        // accessors. `is_null` is not an exception: an unresolved indirect
        // handle is not assumed to be null (matches qpdf's `isDirectNull`).
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        assert!(!handle.is_null());
        assert_eq!(handle.as_integer(), None);
        assert!(handle.as_array().is_none());
        assert!(handle.as_dictionary().is_none());
        assert_eq!(handle.as_real_literal(), None);
        assert!(handle.as_stream_dict().is_none());
        assert!(handle.as_stream_data().is_none());
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

#[cfg(test)]
mod resolution_state_tests {
    use super::*;

    #[test]
    fn direct_handle_is_always_resolved() {
        // A direct handle has no resolution state to wait on — its value was
        // known at construction time.
        assert!(ObjectHandle::integer(1).is_resolved());
    }

    #[test]
    fn fresh_indirect_handle_is_not_resolved() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert!(!handle.is_resolved());
    }

    #[test]
    fn set_resolved_marks_the_handle_resolved_and_exposes_its_value() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        assert!(handle.is_resolved());
        assert_eq!(handle.as_integer(), Some(7));
    }

    #[test]
    fn set_missing_marks_the_handle_resolved_to_null() {
        // `Missing` (dangling/broken reference) must present the same
        // observable value as a genuinely parsed `null` object — but see
        // `set_resolved_with_a_null_value_is_indistinguishable_from_the_outside`
        // for proof the two routes are not literally the same variant.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_missing();
        assert!(handle.is_resolved());
        assert!(handle.is_null());
        assert_eq!(handle.as_integer(), None);
    }

    #[test]
    fn set_resolved_and_set_missing_are_a_no_op_on_a_direct_handle() {
        // Direct handles have no resolution state; calling either setter must
        // not panic and must leave the original value untouched.
        let handle = ObjectHandle::integer(42);
        handle.set_resolved(ObjectValue::Integer(99));
        handle.set_missing();
        assert_eq!(handle.as_integer(), Some(42));
    }

    #[test]
    fn from_value_constructs_a_direct_handle_at_the_offset_sentinel() {
        let handle = ObjectHandle::from_value(ObjectValue::Integer(3));
        assert!(handle.is_direct());
        assert_eq!(handle.as_integer(), Some(3));
        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }
}
