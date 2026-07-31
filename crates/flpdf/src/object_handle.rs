//! The core object-handle graph: shared, cloneable identity for direct and
//! indirect PDF objects, with qpdf-compatible parsed-offset tracking.
//!
//! qpdf correspondence: `QPDFObjectHandle` (`include/qpdf/QPDFObjectHandle.hh`) and its backing `QPDFValue` (`libqpdf/qpdf/QPDFValue.hh`).

// Deviation: shared handle identity uses Rc<RefCell<..>> in place of qpdf's
// std::shared_ptr<QPDFValue> — internal structure only, does not affect
// output bytes (see docs/qpdf-correspondence.md).

use crate::{Dictionary, Object, ObjectRef, Stream};
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
#[derive(Clone)]
pub struct ObjectHandle(Repr);

// Hand-written rather than derived: a resolved indirect value can hold
// other indirect `ObjectHandle`s sharing this same canonical `Rc` identity
// (array/dict/stream-dict children — see `Pdf::drop`'s own comment on the
// same cycle). A self- or reciprocal reference (e.g. a one-object
// `/Self 1 0 R` dictionary, or a `/Pages`/`/Parent` pair) would make a
// derived, recursively-expanding `Debug` walk back into the same slot
// forever, overflowing the stack. Stop at every indirect boundary instead
// of expanding its resolved value: since only an indirect handle can carry
// the document's shared identity, no cycle can exist that does not pass
// through one, so this bound is sufficient to make formatting total.
impl std::fmt::Debug for ObjectHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Repr::Direct(slot) => {
                let slot = slot.borrow();
                f.debug_struct("ObjectHandle::Direct")
                    .field("value", &slot.value)
                    .field("parsed_offset", &slot.parsed_offset)
                    .finish()
            }
            Repr::Indirect(slot) => {
                let slot = slot.borrow();
                let state: &str = match &slot.state {
                    IndirectState::NotYetResolved => "NotYetResolved",
                    IndirectState::Resolved(_) => "Resolved(..)",
                    IndirectState::Missing => "Missing",
                    IndirectState::Destroyed => "Destroyed",
                };
                f.debug_struct("ObjectHandle::Indirect")
                    .field("object_ref", &slot.object_ref)
                    .field("state", &state)
                    .field("parsed_offset", &slot.parsed_offset)
                    .finish()
            }
        }
    }
}

// Deliberately not `Debug`: see `ObjectHandle`'s own hand-written `Debug`
// impl above for why a derived one is unsafe here (indirect-handle cycles).
#[derive(Clone)]
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
    // An indirect object whose own resolved value is *itself* a bare
    // reference to another object (e.g. `4 0 obj\n5 0 R\nendobj`, or a
    // reference redirected in place via `Pdf::set_object`) -- never seen
    // from a file/ObjStm parse (`Pdf::resolve_object_handle`'s native path
    // integerizes a top-level bare reference to `Integer` instead, matching
    // qpdf), but a real value `Pdf::set_object` callers pass directly (used
    // throughout this crate to redirect/collapse holder chains). A child
    // array/dictionary entry that is a reference is represented as a
    // separate indirect `ObjectHandle`, never this variant -- see
    // `Pdf::lift_to_handle` and `materialize`'s own doc.
    Reference(ObjectRef),
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
    /// The owning document has been dropped and this slot's value has been
    /// severed (see [`ObjectHandle::disconnect`]). Distinct from `Missing`
    /// (a reference absent from the source) so a future diagnostic can still
    /// tell the two apart; presents the same externally-observable `Null`
    /// value as both other terminal states.
    Destroyed,
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
    ///
    /// Also resets the parsed offset to the no-offset sentinel: "An absent,
    /// freed, dangling, cyclic, or otherwise unresolvable indirect object
    /// retains its indirect identity but resolves to null with parsed offset
    /// `-1`" (design, Parsed-Offset Contract). Without this, a handle that
    /// was previously resolved (e.g. natively parsed with a real offset)
    /// and later marked missing — [`crate::Pdf::delete_object`] on an
    /// already-resolved handle — would keep reporting its former body's
    /// source position even though the value now reads as null.
    pub(crate) fn set_missing(&self) {
        if let Repr::Indirect(slot) = &self.0 {
            let mut slot = slot.borrow_mut();
            slot.state = IndirectState::Missing;
            slot.parsed_offset = NO_PARSED_OFFSET;
        }
    }

    /// Sever this indirect handle's resolved value, dropping any `ObjectHandle`
    /// children it holds. A no-op for a direct handle.
    ///
    /// A resolved indirect value can hold direct-owning [`ObjectHandle`]
    /// children (array/dictionary/stream-dict entries) that are themselves
    /// indirect handles sharing the same canonical `Rc` identity as this
    /// document's registry entries. Two objects that reference each other
    /// (e.g. a `/Pages` node and a page's `/Parent`, both common in real
    /// PDFs) therefore form a strong reference cycle once both are resolved,
    /// which `Rc` alone never collects.
    ///
    /// Mirrors qpdf's own teardown: `QPDF::~QPDF()` walks its object cache
    /// and disconnects every resolved object, replacing it with
    /// `QPDF_Destroyed()`, specifically to break cycles like this one
    /// (`libqpdf/QPDF.cc`, `QPDF::~QPDF`). The reader's `Pdf::drop` calls
    /// this for every entry in its handle registry — the sole owner of the
    /// canonical `Rc`s — before the registry itself is dropped, so no
    /// lingering cycle keeps a document's object graph (and any reachable
    /// stream buffers) alive past the `Pdf` that produced it.
    pub(crate) fn disconnect(&self) {
        if let Repr::Indirect(slot) = &self.0 {
            slot.borrow_mut().state = IndirectState::Destroyed;
        }
    }

    /// The `Rc` strong count backing this handle's identity. Test-only:
    /// lets a regression test prove a cycle-breaking fix actually frees the
    /// `Rc`s involved, without exposing reference counting as production API.
    #[cfg(test)]
    pub(crate) fn strong_count(&self) -> usize {
        match &self.0 {
            Repr::Direct(rc) => Rc::strong_count(rc),
            Repr::Indirect(rc) => Rc::strong_count(rc),
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
                IndirectState::Missing | IndirectState::Destroyed => f(Some(&ObjectValue::Null)),
            },
        }
    }

    /// Convert this handle's value into a legacy [`crate::Object`] tree
    /// (`Pdf::resolve`/`Pdf::resolve_borrowed`'s materialization bridge).
    ///
    /// An indirect array/dictionary child is *not* recursively resolved: it
    /// becomes `Object::Reference(child_ref)`, matching the parser's
    /// pre-existing `Object::Reference` semantics so every consumer match on
    /// that variant keeps working unchanged. A stream's own dictionary
    /// handle (a separately parsed handle with its own `<<`-start parsed
    /// offset) is flattened into a plain [`Dictionary`] for
    /// `Object::Stream`.
    ///
    /// An indirect handle that has not yet been resolved (see
    /// [`Self::is_resolved`]) materializes as `Object::Null` rather than
    /// performing hidden resolution; callers that need the real value must
    /// resolve first (e.g. via `Pdf::resolve_object_handle`).
    pub(crate) fn materialize(&self) -> Object {
        self.with_value(|value| match value {
            Some(value) => materialize_value(value),
            None => Object::Null,
        })
    }

    /// Replace this handle's own value in place, preserving its identity
    /// (every other outstanding clone observes the new value) and its
    /// already-recorded parsed offset (`parsed_offset` is untouched here --
    /// see [`Self::reset_parsed_offset`] to clear it). A no-op for an
    /// indirect handle; see [`Self::set_resolved`] for that case.
    ///
    /// Used by `Pdf::set_object` to update a stream's own dictionary handle
    /// in place when the replacement value is also a stream, so the
    /// dictionary handle's already-recorded `<<`-start parsed offset
    /// survives instead of being lost to a freshly minted handle.
    pub(crate) fn replace_direct_value(&self, value: ObjectValue) {
        if let Repr::Direct(slot) = &self.0 {
            slot.borrow_mut().value = value;
        }
    }

    /// Reset this handle's parsed offset back to the no-offset sentinel,
    /// overriding the set-once contract [`Self::set_parsed_offset_if_unset`]
    /// normally enforces.
    ///
    /// Used by `Pdf::set_object`: once it replaces an indirect handle's
    /// value with a caller-supplied one, any previously recorded source
    /// position no longer describes that value.
    pub(crate) fn reset_parsed_offset(&self) {
        match &self.0 {
            Repr::Direct(slot) => slot.borrow_mut().parsed_offset = NO_PARSED_OFFSET,
            Repr::Indirect(slot) => slot.borrow_mut().parsed_offset = NO_PARSED_OFFSET,
        }
    }
}

fn materialize_value(value: &ObjectValue) -> Object {
    match value {
        ObjectValue::Null => Object::Null,
        ObjectValue::Boolean(b) => Object::Boolean(*b),
        ObjectValue::Integer(n) => Object::Integer(*n),
        ObjectValue::Real(r) => Object::Real(*r),
        ObjectValue::RealLiteral { value, literal } => Object::RealLiteral {
            value: *value,
            literal: literal.clone(),
        },
        ObjectValue::Name(name) => Object::Name(name.clone()),
        ObjectValue::String(s) => Object::String(s.clone()),
        ObjectValue::Array(children) => {
            Object::Array(children.iter().map(materialize_child).collect())
        }
        ObjectValue::Dictionary(entries) => {
            let mut dict = Dictionary::new();
            for (key, value) in entries {
                dict.insert(key.as_slice(), materialize_child(value));
            }
            Object::Dictionary(dict)
        }
        ObjectValue::Stream { dict, data } => {
            let dict = match dict.materialize() {
                Object::Dictionary(dict) => dict,
                // A stream's own dictionary handle is always constructed as
                // a direct `ObjectValue::Dictionary` (see
                // `Pdf::native_parse_uncompressed_value`, `Pdf::lift`, and
                // `Pdf::lift_for_set_object`), never an indirect reference
                // or any other variant.
                _ => Dictionary::new(), // cov:ignore: unreachable per the invariant above
            };
            Object::Stream(Stream::new(dict, data.clone()))
        }
        ObjectValue::Reference(object_ref) => Object::Reference(*object_ref),
    }
}

// An array/dictionary child handle materializes to `Object::Reference`
// without recursing into it when indirect (identity-preserving, matching
// the parser's pre-existing `Object::Reference` semantics); a direct child
// is materialized in place.
fn materialize_child(handle: &ObjectHandle) -> Object {
    match handle.object_ref() {
        Some(object_ref) => Object::Reference(object_ref),
        None => handle.materialize(),
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
    fn set_missing_resets_a_previously_recorded_parsed_offset() {
        // Design's Parsed-Offset Contract: "An absent, freed, dangling,
        // cyclic, or otherwise unresolvable indirect object ... resolves to
        // null with parsed offset -1." A handle that was already resolved
        // with a real (non-negative) offset -- e.g. natively parsed, then
        // later deleted -- must not keep reporting its former body's source
        // position once it reads as null.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        handle.set_parsed_offset_if_unset(100);
        assert_eq!(handle.get_parsed_offset(), 100);

        handle.set_missing();

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        assert!(handle.is_null());
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

    #[test]
    fn disconnect_replaces_a_resolved_value_and_presents_as_null() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));

        handle.disconnect();

        assert!(handle.is_resolved());
        assert!(handle.is_null());
        assert_eq!(handle.as_integer(), None);
    }

    #[test]
    fn disconnect_is_a_no_op_on_a_direct_handle() {
        let handle = ObjectHandle::integer(42);
        handle.disconnect();
        assert_eq!(handle.as_integer(), Some(42));
    }

    #[test]
    fn strong_count_reports_a_direct_handles_rc_count_too() {
        let handle = ObjectHandle::integer(1);
        assert_eq!(handle.strong_count(), 1);
        let clone = handle.clone();
        assert_eq!(handle.strong_count(), 2);
        drop(clone);
        assert_eq!(handle.strong_count(), 1);
    }

    #[test]
    fn disconnect_drops_the_strong_rc_a_resolved_value_holds_to_a_cyclic_child() {
        // Two objects that reference each other (e.g. a /Pages node and a
        // page's /Parent) form a strong Rc cycle once both are resolved:
        // each slot's value embeds the other's canonical handle. `disconnect`
        // (called by `Pdf::drop` for every registry entry) must sever that
        // cycle so both slots free once external references are gone.
        let a = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        let b = ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0);
        a.set_resolved(ObjectValue::Dictionary(
            [(b"Kid".to_vec(), b.clone())].into_iter().collect(),
        ));
        b.set_resolved(ObjectValue::Dictionary(
            [(b"Parent".to_vec(), a.clone())].into_iter().collect(),
        ));
        assert_eq!(a.strong_count(), 2, "held by this test and by b's value");
        assert_eq!(b.strong_count(), 2, "held by this test and by a's value");

        a.disconnect();
        b.disconnect();

        assert_eq!(a.strong_count(), 1, "only this test's own handle remains");
        assert_eq!(b.strong_count(), 1, "only this test's own handle remains");
    }

    #[test]
    fn debug_format_does_not_recurse_through_a_self_referential_handle() {
        // A one-object `/Self 1 0 R` dictionary: the handle's own resolved
        // value embeds itself. A derived `Debug` would recurse into the
        // same slot forever and overflow the stack; formatting must stop at
        // the indirect boundary instead.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Dictionary(
            [(b"Self".to_vec(), handle.clone())].into_iter().collect(),
        ));

        let formatted = format!("{handle:?}");

        assert!(formatted.contains("ObjectHandle::Indirect"));
        assert!(formatted.contains("Resolved(..)"));
    }

    #[test]
    fn debug_format_does_not_recurse_through_a_reciprocal_cycle() {
        let a = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        let b = ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0);
        a.set_resolved(ObjectValue::Dictionary(
            [(b"Kid".to_vec(), b.clone())].into_iter().collect(),
        ));
        b.set_resolved(ObjectValue::Dictionary(
            [(b"Parent".to_vec(), a.clone())].into_iter().collect(),
        ));

        let formatted = format!("{a:?}");

        assert!(formatted.contains("ObjectHandle::Indirect"));
    }

    #[test]
    fn debug_format_of_a_direct_handle_shows_its_value() {
        let handle = ObjectHandle::integer(7);
        assert!(format!("{handle:?}").contains("ObjectHandle::Direct"));
    }

    #[test]
    fn debug_format_summarizes_every_indirect_resolution_state() {
        let not_yet_resolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert!(format!("{not_yet_resolved:?}").contains("NotYetResolved"));

        let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0);
        missing.set_missing();
        assert!(format!("{missing:?}").contains("Missing"));

        let destroyed = ObjectHandle::new_indirect_unresolved(ObjectRef::new(3, 0), 0);
        destroyed.set_resolved(ObjectValue::Integer(1));
        destroyed.disconnect();
        assert!(format!("{destroyed:?}").contains("Destroyed"));
    }
}

#[cfg(test)]
mod materialize_tests {
    use super::*;

    #[test]
    fn scalar_values_materialize_to_the_matching_object_variant() {
        assert_eq!(ObjectHandle::null().materialize(), Object::Null);
        assert_eq!(
            ObjectHandle::boolean(true).materialize(),
            Object::Boolean(true)
        );
        assert_eq!(ObjectHandle::integer(7).materialize(), Object::Integer(7));
        assert_eq!(ObjectHandle::real(1.5).materialize(), Object::Real(1.5));
        assert_eq!(
            ObjectHandle::name(b"Foo".to_vec()).materialize(),
            Object::Name(b"Foo".to_vec())
        );
        assert_eq!(
            ObjectHandle::string(b"bar".to_vec()).materialize(),
            Object::String(b"bar".to_vec())
        );
    }

    #[test]
    fn real_literal_materializes_with_its_source_literal_preserved() {
        let handle = ObjectHandle::real_literal(0.4, b".4".to_vec());
        assert_eq!(
            handle.materialize(),
            Object::RealLiteral {
                value: 0.4,
                literal: b".4".to_vec(),
            }
        );
    }

    #[test]
    fn a_direct_array_materializes_recursively_but_an_indirect_child_becomes_a_reference() {
        let indirect_child = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), indirect_child]);

        let materialized = array.materialize();
        assert_eq!(
            materialized,
            Object::Array(vec![
                Object::Integer(1),
                Object::Reference(ObjectRef::new(9, 0))
            ])
        );
    }

    #[test]
    fn a_dictionary_materializes_its_entries_by_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let Object::Dictionary(materialized) = dict.materialize() else {
            panic!("expected a dictionary"); // cov:ignore: unreachable in a passing run
        };
        assert_eq!(materialized.get("A"), Some(&Object::Integer(1)));
    }

    #[test]
    fn a_stream_value_flattens_its_dictionary_handle_into_a_plain_dictionary() {
        let dict_handle =
            ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(5))]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            dict: dict_handle,
            data: b"Hello".to_vec(),
        });

        let Object::Stream(materialized) = stream.materialize() else {
            panic!("expected a stream"); // cov:ignore: unreachable in a passing run
        };
        assert_eq!(materialized.data, b"Hello");
        assert_eq!(materialized.dict.get("Length"), Some(&Object::Integer(5)));
    }

    #[test]
    fn an_unresolved_indirect_handle_materializes_to_null_without_performing_resolution() {
        // `Pdf::resolve_borrowed` always resolves before materializing, but
        // `materialize` itself must not assume that precondition holds --
        // a caller that skips resolution sees `Object::Null`, not a panic.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        assert_eq!(handle.materialize(), Object::Null);
    }

    #[test]
    fn replace_direct_value_updates_a_direct_handles_value_but_keeps_its_offset() {
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);

        handle.replace_direct_value(ObjectValue::Integer(2));

        assert_eq!(handle.as_integer(), Some(2));
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn replace_direct_value_is_a_no_op_on_an_indirect_handle() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(1));

        handle.replace_direct_value(ObjectValue::Integer(2));

        assert_eq!(handle.as_integer(), Some(1));
    }

    #[test]
    fn reset_parsed_offset_clears_an_already_set_offset() {
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);

        handle.reset_parsed_offset();

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        // The set-once guard is not permanently defeated: a later value can
        // set a fresh offset after a reset.
        handle.set_parsed_offset_if_unset(200);
        assert_eq!(handle.get_parsed_offset(), 200);
    }

    #[test]
    fn reset_parsed_offset_works_on_an_indirect_handle_too() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_parsed_offset_if_unset(100);

        handle.reset_parsed_offset();

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }
}
