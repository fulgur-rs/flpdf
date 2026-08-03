//! The core object-handle graph: shared, cloneable identity for direct and
//! indirect PDF objects, with qpdf-compatible parsed-offset tracking.
//!
//! qpdf correspondence: `QPDFObjectHandle`, `QPDFObject`, and `QPDFValue` identity and payload ownership.
//!
//! `QPDFObjectHandle` (`include/qpdf/QPDFObjectHandle.hh`) shares a canonical `QPDFObject`
//! (`libqpdf/qpdf/QPDFObject.hh`), which owns the `QPDFValue` payload
//! (`libqpdf/qpdf/QPDFValue.hh`).

// Deviation: shared handle identity uses Rc<RefCell<..>> in place of qpdf's
// std::shared_ptr<QPDFObject>; ObjectValue is the QPDFValue payload. This is
// internal structure only and does not affect output bytes (see
// docs/qpdf-correspondence.md).

use crate::{Dictionary, Error, Object, ObjectRef, Result, Stream};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// The no-offset sentinel qpdf uses for values that were not parsed from a
/// source position (`QPDFValue`'s parsed offset starts at `-1` and is set
/// only while still negative; see
/// `libqpdf/qpdf/QPDFValue.hh:90-100,149-152`).
pub(crate) const NO_PARSED_OFFSET: i64 = -1;

/// The conflicts-tracking map [`ObjectHandle::merge_resources`] populates:
/// `rtype -> old_key -> new_key`, mirroring
/// `QPDFObjectHandle::mergeResources`'s own
/// `std::map<std::string, std::map<std::string, std::string>>` parameter.
pub type ResourceConflicts =
    std::collections::BTreeMap<Vec<u8>, std::collections::BTreeMap<Vec<u8>, Vec<u8>>>;

/// The document-owned resolver qpdf's `QPDFObject` calls through its owning
/// `QPDF*` and object identity. Kept crate-private so only the canonical
/// document implementation can resolve an indirect slot.
#[allow(dead_code)] // production QPDF::Resolver wiring is flpdf-25kg.3.5
pub(crate) trait DocumentResolver {
    fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()>;
}

/// A shared, cloneable handle to a PDF object.
///
/// Cloning a handle is O(1) and does not deep-copy the underlying value;
/// every clone of an indirect handle shares the same canonical identity and
/// resolution state.
///
/// Lazy dereference stays crate-internal until every document-created handle
/// is attached to the complete qpdf-native resolver.
///
/// ```compile_fail
/// let handle = flpdf::ObjectHandle::integer(1);
/// handle.try_dereference()?;
/// # Ok::<(), flpdf::Error>(())
/// ```
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
#[derive(Debug, Clone)]
pub(crate) enum ObjectValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    /// Preserves a non-canonical source spelling (e.g. `.4`) alongside its
    /// parsed value, mirroring [`crate::Object::RealLiteral`], so that a
    /// real number written in the source PDF unparses byte-identically.
    RealLiteral {
        value: f64,
        literal: Vec<u8>,
    },
    Name(Vec<u8>),
    String(Vec<u8>),
    /// A content-stream operator token (e.g. `q`, `Do`), mirroring
    /// [`crate::Object::Operator`]. Only meaningful inside a content stream
    /// (`include/qpdf/QPDFObjectHandle.hh:318-319`: "Operator and
    /// InlineImage are only allowed in content streams").
    Operator(Vec<u8>),
    /// Raw inline-image (`BI`...`ID`...`EI`) bytes, mirroring
    /// [`crate::Object::InlineImage`]. Same content-stream-only constraint
    /// as `Operator` above.
    InlineImage(Vec<u8>),
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
    // qpdf-cutover-delete(flpdf-25kg.3.3): qpdf cannot store an indirect
    // handle as another indirect object's replacement value. Delete this
    // legacy redirect variant after `set_object` and ref-chain consumers move
    // to canonical in-place slot replacement.
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
    /// Canonical indirect objects that contain this direct value. qpdf keeps
    /// one shared QPDFObject payload, so direct mutation has no document-wide
    /// owner-discovery phase. The Rust port records the same containment at
    /// insertion/resolution time for incremental-write dirty tracking.
    containing_object_refs: std::collections::BTreeSet<ContainmentOwner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ContainmentOwner {
    pdf_unique_id: Option<u64>,
    object_ref: ObjectRef,
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

struct IndirectSlot {
    object_ref: ObjectRef,
    pdf_unique_id: Option<u64>,
    #[allow(dead_code)] // read through the flpdf-25kg.3.5 resolver cutover
    resolver: Option<Weak<dyn DocumentResolver>>,
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
    ///
    /// This is qpdf's `QPDFObjectHandle::isSameObjectAs`: mutations and lazy
    /// resolution observed through either handle affect the same object.
    pub fn is_same_object_as(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Direct(a), Repr::Direct(b)) => Rc::ptr_eq(a, b),
            (Repr::Indirect(a), Repr::Indirect(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    #[cfg(test)]
    fn ptr_eq(&self, other: &Self) -> bool {
        self.is_same_object_as(other)
    }

    // Used by this module's identity tests, and by `Pdf::get_object_handle`
    // (reader.rs) to lazily create the canonical handle it registers into
    // `handle_registry` the first time a given ref is requested.
    #[cfg(test)]
    pub(crate) fn new_indirect_unresolved(object_ref: ObjectRef, offset: i64) -> Self {
        Self::new_indirect_unresolved_with_identity(object_ref, offset, None, None)
    }

    pub(crate) fn new_indirect_unresolved_for_pdf(
        object_ref: ObjectRef,
        offset: i64,
        pdf_unique_id: u64,
    ) -> Self {
        Self::new_indirect_unresolved_with_identity(object_ref, offset, Some(pdf_unique_id), None)
    }

    /// Construct a canonical unresolved slot carrying both its owning
    /// document's identity and that document's resolver — what
    /// `Pdf::get_object_handle` needs to hand out.
    ///
    /// Neither half is sufficient alone. The resolver is what
    /// [`Self::try_dereference`] upgrades and calls; the identity is what
    /// [`Self::belongs_to_pdf`] answers on, and what [`Self::set_resolved`]
    /// stamps onto each direct child for
    /// [`Self::containing_object_refs_for_pdf`].
    ///
    /// `pdf_unique_id` is an flpdf-internal document tag with no qpdf
    /// counterpart: qpdf's object reaches its document through a raw `QPDF*`
    /// back-pointer (`libqpdf/qpdf/QPDFValue.hh:150`, `QPDF* qpdf{nullptr}`)
    /// that `QPDFObject::doResolve` hands straight to
    /// `QPDF::Resolver::resolve` (`libqpdf/QPDFObject.cc:7-11`), so one
    /// pointer serves as both identity and resolver there. See
    /// [`Self::new_indirect_with_resolver`] for why this port splits them and
    /// keeps the resolver link weak.
    #[allow(dead_code)] // no non-test caller yet: `Pdf::get_object_handle`
                        // attaches it in the next step of flpdf-25kg.3.5
    pub(crate) fn new_indirect_for_pdf_with_resolver(
        object_ref: ObjectRef,
        offset: i64,
        pdf_unique_id: u64,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        Self::new_indirect_unresolved_with_identity(
            object_ref,
            offset,
            Some(pdf_unique_id),
            Some(resolver),
        )
    }

    /// Construct a canonical unresolved slot attached to its owning document
    /// resolver. This is the qpdf-native constructor; the resolver link is
    /// weak so a surviving handle cannot keep its document alive.
    #[allow(dead_code)] // production QPDF::Resolver wiring is flpdf-25kg.3.5;
                        // this primitive slice exercises the constructor with
                        // sealed resolver unit tests only
    pub(crate) fn new_indirect_with_resolver(
        object_ref: ObjectRef,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        Self::new_indirect_unresolved_with_identity(
            object_ref,
            NO_PARSED_OFFSET,
            None,
            Some(resolver),
        )
    }

    fn new_indirect_unresolved_with_identity(
        object_ref: ObjectRef,
        offset: i64,
        pdf_unique_id: Option<u64>,
        resolver: Option<Weak<dyn DocumentResolver>>,
    ) -> Self {
        let _ = offset; // real Unresolved{offset} state lands in a later task
        Self(Repr::Indirect(Rc::new(RefCell::new(IndirectSlot {
            object_ref,
            pdf_unique_id,
            resolver,
            state: IndirectState::NotYetResolved,
            parsed_offset: NO_PARSED_OFFSET,
        }))))
    }

    fn new_direct(value: ObjectValue, parsed_offset: i64) -> Self {
        Self(Repr::Direct(Rc::new(RefCell::new(DirectSlot {
            value,
            parsed_offset,
            containing_object_refs: Default::default(),
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

    /// This handle's value, cloned, if it is direct — `None` for an
    /// indirect handle. Unlike [`Self::into_direct_value`], this works
    /// regardless of how many other clones of this handle are outstanding
    /// (it clones the value rather than requiring exclusive `Rc`
    /// ownership). Used by `Pdf::make_indirect_object_handle`, which
    /// cannot assume its caller holds the only reference to the direct
    /// handle it passes in.
    ///
    /// A `Stream` value's `dict` gets the same `shallow_copy_child`
    /// treatment [`Self::shallow_copy`] gives it, rather than the plain
    /// `ObjectValue::clone()` every other variant gets: `#[derive(Clone)]`
    /// would leave `dict` Rc-shared with `self` while deep-cloning `data`,
    /// so a later `replace_stream_data` on either handle would update the
    /// other's `/Length`/`/Filter`/`/DecodeParms` without touching its
    /// (independently cloned) data bytes — the exact asymmetry
    /// `shallow_copy`'s own doc comment explains for that method.
    pub(crate) fn direct_value_clone(&self) -> Option<ObjectValue> {
        match &self.0 {
            Repr::Direct(slot) => Some(match &slot.borrow().value {
                ObjectValue::Stream { dict, data } => ObjectValue::Stream {
                    dict: shallow_copy_child(dict),
                    data: data.clone(),
                },
                other => other.clone(),
            }),
            Repr::Indirect(_) => None,
        }
    }

    /// Mark this indirect handle's value as resolved to `value`. A no-op for
    /// a direct handle, which has no resolution state to update.
    pub(crate) fn set_resolved(&self, value: ObjectValue) {
        if let Repr::Indirect(slot) = &self.0 {
            let owner = {
                let slot = slot.borrow();
                ContainmentOwner {
                    pdf_unique_id: slot.pdf_unique_id,
                    object_ref: slot.object_ref,
                }
            };
            Self::associate_value_with_owners(&value, &[owner], 0);
            slot.borrow_mut().state = IndirectState::Resolved(value);
        }
    }

    /// Return the canonical indirect objects that contain this direct handle.
    /// Indirect handles own themselves and are intentionally excluded: callers
    /// that need that case already use [`Self::object_ref`].
    #[cfg(test)]
    pub(crate) fn containing_object_refs(&self) -> Vec<ObjectRef> {
        match &self.0 {
            Repr::Direct(slot) => slot
                .borrow()
                .containing_object_refs
                .iter()
                .map(|owner| owner.object_ref)
                .collect(),
            Repr::Indirect(_) => Vec::new(),
        }
    }

    pub(crate) fn containing_object_refs_for_pdf(&self, pdf_unique_id: u64) -> Vec<ObjectRef> {
        match &self.0 {
            Repr::Direct(slot) => slot
                .borrow()
                .containing_object_refs
                .iter()
                .filter(|owner| owner.pdf_unique_id == Some(pdf_unique_id))
                .map(|owner| owner.object_ref)
                .collect(),
            Repr::Indirect(_) => Vec::new(),
        }
    }

    pub(crate) fn belongs_to_pdf(&self, pdf_unique_id: u64) -> bool {
        match &self.0 {
            Repr::Indirect(slot) => slot.borrow().pdf_unique_id == Some(pdf_unique_id),
            Repr::Direct(slot) => {
                let owners = &slot.borrow().containing_object_refs;
                owners.is_empty()
                    || owners
                        .iter()
                        .any(|owner| owner.pdf_unique_id == Some(pdf_unique_id))
            }
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
    ///
    /// Also resets the parsed offset to the no-offset sentinel, mirroring
    /// [`Self::set_missing`]'s own reset and the same Parsed-Offset Contract
    /// clause it cites: a surviving handle that now presents as null must
    /// not keep reporting the destroyed value's former source position.
    pub(crate) fn disconnect(&self) {
        if let Repr::Indirect(slot) = &self.0 {
            let mut slot = slot.borrow_mut();
            slot.state = IndirectState::Destroyed;
            slot.parsed_offset = NO_PARSED_OFFSET;
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
    /// direct handle always is; an indirect handle is once it has left its
    /// initial state, whether that landed on a real value, on a reference
    /// that turned out to be missing from the source, or on a value severed
    /// because its owning document was dropped.
    pub fn is_resolved(&self) -> bool {
        match &self.0 {
            Repr::Direct(_) => true,
            Repr::Indirect(slot) => !matches!(slot.borrow().state, IndirectState::NotYetResolved),
        }
    }

    /// Resolve this handle's own canonical slot in place, mirroring
    /// `QPDFObjectHandle::dereference` → `QPDFObject::resolve`.
    ///
    /// Direct and already-terminal handles are no-ops. An unresolved handle
    /// whose document has been dropped returns an error and stays unresolved.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_dereference(&self) -> Result<()> {
        let (object_ref, resolver) = match &self.0 {
            Repr::Direct(_) => return Ok(()),
            Repr::Indirect(slot) => {
                let slot = slot.borrow();
                if !matches!(slot.state, IndirectState::NotYetResolved) {
                    return Ok(());
                }
                (slot.object_ref, slot.resolver.clone())
            }
        };

        let Some(resolver) = resolver.and_then(|resolver| resolver.upgrade()) else {
            return Err(Error::Internal(format!(
                "object {} {} belongs to a dropped PDF",
                object_ref.number, object_ref.generation
            )));
        };
        resolver.resolve_indirect(object_ref, self)
    }

    /// qpdf-compatible null inspection with lazy dereference.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_is_null(&self) -> Result<bool> {
        self.try_dereference()?;
        Ok(self.is_null())
    }

    /// qpdf-compatible dictionary inspection with lazy dereference.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_as_dictionary(
        &self,
    ) -> Result<Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>>> {
        self.try_dereference()?;
        Ok(self.as_dictionary())
    }

    /// qpdf-compatible name inspection with lazy dereference.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_as_name(&self) -> Result<Option<Vec<u8>>> {
        self.try_dereference()?;
        Ok(self.as_name())
    }

    /// qpdf-compatible array inspection with lazy dereference. Only the array
    /// itself is resolved; each returned child keeps its own identity.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_as_array(&self) -> Result<Option<Vec<ObjectHandle>>> {
        self.try_dereference()?;
        Ok(self.as_array())
    }

    /// qpdf-compatible array *length* with lazy dereference — the item count
    /// without materializing the items.
    ///
    /// `QPDFObjectHandle::getArrayNItems` is `asArray()->size()`
    /// (`libqpdf/QPDFObjectHandle.cc:758-768`), and `asArray` is
    /// `return dereference() ? obj->as<QPDF_Array>() : nullptr;` — a borrowed
    /// `QPDF_Array*`, not a copy (`libqpdf/QPDFObjectHandle.cc:252-256`), so
    /// qpdf reads the length in place. `QPDF_Stream::filterable` uses that to
    /// size its `/Filter` and `/DecodeParms` loops
    /// (`libqpdf/QPDF_Stream.cc:398`, `:443`, `:447`) before touching a single
    /// item. [`Self::try_as_array`]
    /// cannot serve that caller: it snapshots the child vector, so a length
    /// that is only going to be rejected still costs a `Vec` allocation and
    /// one `Rc` clone per child.
    ///
    /// **Deliberately not qpdf's non-array answer.** `getArrayNItems` warns
    /// `typeWarning("array", "treating as empty")` and returns 0 for a
    /// non-array (`libqpdf/QPDFObjectHandle.cc:763-766`), so qpdf reads a
    /// non-array as an empty one. This returns `None`, matching
    /// [`Self::try_as_array`], and leaves the meaning of "not an array" to the
    /// caller — for [`crate::stream_filter::decode_filter_specs_from_handle`]
    /// that is the "stream filter type is not name or array" error. That
    /// divergence predates this accessor and is not widened by it; folding
    /// qpdf's treat-as-empty in here would silently turn a rejected `/Filter`
    /// into an accepted unfiltered stream.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_array_len(&self) -> Result<Option<usize>> {
        self.try_dereference()?;
        Ok(self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => Some(children.len()),
            _ => None,
        }))
    }

    /// qpdf-compatible integer inspection with lazy dereference.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_as_integer(&self) -> Result<Option<i64>> {
        self.try_dereference()?;
        Ok(self.as_integer())
    }

    /// qpdf-compatible dictionary lookup. The holder dictionary is resolved;
    /// the returned child retains its own direct/indirect identity.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_get_key(&self, key: &[u8]) -> Result<ObjectHandle> {
        self.try_dereference()?;
        Ok(self.get_key(key))
    }

    /// qpdf-compatible visible-key test. A present value that resolves to
    /// null is treated as absent, matching `QPDF_Dictionary::hasKey`.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_has_key(&self, key: &[u8]) -> Result<bool> {
        self.try_dereference()?;
        let child = self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
            _ => None,
        });
        match child {
            Some(child) => Ok(!child.try_is_null()?),
            None => Ok(false),
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

    /// Construct a direct content-stream operator token value.
    pub fn operator(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::Operator(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct raw inline-image byte payload value.
    pub fn inline_image(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::InlineImage(value), NO_PARSED_OFFSET)
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

    /// Construct a direct stream value from `dict` (a dictionary handle —
    /// typically built via [`Self::dictionary`]) and `data` (the stream's
    /// raw, undecoded bytes). qpdf's own model never allows a stream to be a
    /// direct value (only ever a top-level indirect object,
    /// `libqpdf/QPDF_Stream.cc:173-178`); this crate's own types do not
    /// forbid it, matching [`Self::unparse_resolved`]'s own doc for that
    /// case. Mainly useful for building a handle that is deliberately never
    /// attached to a [`crate::Pdf`]'s object graph, e.g. in tests.
    pub fn stream(dict: ObjectHandle, data: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::Stream { dict, data }, NO_PARSED_OFFSET)
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

    /// The value as an `f64`/literal-bytes pair if this handle's value — its
    /// own if direct, or its already-resolved value if indirect — is a real
    /// value with a preserved source literal, or `None` otherwise. This
    /// never performs resolution itself: an indirect handle that has not
    /// yet been resolved returns `None` too, the same as a resolved value
    /// of a different type.
    pub fn as_real_literal(&self) -> Option<(f64, Vec<u8>)> {
        self.with_value(|value| match value {
            Some(ObjectValue::RealLiteral { value, literal }) => Some((*value, literal.clone())),
            _ => None,
        })
    }

    /// The value as `bool` if this handle's value — its own if direct, or
    /// its already-resolved value if indirect — is a boolean, or `None`
    /// otherwise. Never performs resolution itself.
    pub fn as_boolean(&self) -> Option<bool> {
        self.with_value(|value| match value {
            Some(ObjectValue::Boolean(b)) => Some(*b),
            _ => None,
        })
    }

    /// The value as `f64` if this handle's value — its own if direct, or
    /// its already-resolved value if indirect — is a real number (including
    /// one with a preserved non-canonical source literal), or `None`
    /// otherwise. Mirrors [`crate::Object::as_real`]'s own real-or-real-literal
    /// arm. Never performs resolution itself.
    pub fn as_real(&self) -> Option<f64> {
        self.with_value(|value| match value {
            Some(ObjectValue::Real(v) | ObjectValue::RealLiteral { value: v, .. }) => Some(*v),
            _ => None,
        })
    }

    /// The value as decoded PDF name bytes if this handle's value — its own
    /// if direct, or its already-resolved value if indirect — is a name, or
    /// `None` otherwise. Never performs resolution itself.
    pub fn as_name(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Name(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The value as string bytes if this handle's value — its own if
    /// direct, or its already-resolved value if indirect — is a string, or
    /// `None` otherwise. Never performs resolution itself.
    pub fn as_string(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The target as an indirect-object reference if this handle's value —
    /// its own if direct, or its already-resolved value if indirect — is
    /// itself a bare reference (e.g. one redirected in place to another
    /// object via `Pdf::set_object`), mirroring [`crate::Object::Reference`],
    /// or `None` otherwise. This is distinct from an indirect *child*
    /// handle, which is exposed via [`Self::is_indirect`]/[`Self::object_ref`]
    /// on the child handle itself rather than through this accessor. Never
    /// performs resolution itself.
    pub fn as_reference(&self) -> Option<ObjectRef> {
        self.with_value(|value| match value {
            Some(ObjectValue::Reference(object_ref)) => Some(*object_ref),
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

    /// The value as `i64` if this handle's value — its own if direct, or its
    /// already-resolved value if indirect — is an integer, or `None`
    /// otherwise. This never performs resolution itself: an indirect handle
    /// that has not yet been resolved returns `None` too, the same as a
    /// resolved value of a different type.
    pub fn as_integer(&self) -> Option<i64> {
        self.with_value(|value| match value {
            Some(ObjectValue::Integer(n)) => Some(*n),
            _ => None,
        })
    }

    /// The child handles if this handle's value — its own if direct, or its
    /// already-resolved value if indirect — is an array, or `None`
    /// otherwise. This never performs resolution itself: an indirect handle
    /// that has not yet been resolved returns `None` too, the same as a
    /// resolved value of a different type. Cloning the returned `Vec` clones
    /// only the child `Rc` handles, not their subtrees.
    pub fn as_array(&self) -> Option<Vec<ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => Some(children.clone()),
            _ => None,
        })
    }

    /// The entries if this handle's value — its own if direct, or its
    /// already-resolved value if indirect — is a dictionary, or `None`
    /// otherwise. This never performs resolution itself: an indirect handle
    /// that has not yet been resolved returns `None` too, the same as a
    /// resolved value of a different type. Cloning the returned map clones
    /// only the child `Rc` handles, not their subtrees.
    pub fn as_dictionary(&self) -> Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => Some(entries.clone()),
            _ => None,
        })
    }

    /// The value at `key` if this handle's value is a dictionary and `key`
    /// is present, or a direct null handle otherwise (a missing key, or
    /// this handle not being a dictionary at all) — mirrors
    /// `QPDFObjectHandle::getKey`'s own "returns null for a missing key or
    /// a non-dictionary handle" contract (`libqpdf/QPDFObjectHandle.cc:979-988`).
    /// Unlike
    /// [`Self::as_dictionary`], this never snapshots the whole dictionary —
    /// it returns the one live child handle directly, so a caller that only
    /// needs one key does not pay for every sibling. Never performs
    /// resolution itself.
    pub fn get_key(&self, key: &[u8]) -> ObjectHandle {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
            _ => None,
        })
        .unwrap_or_else(ObjectHandle::null)
    }

    /// True if this handle's value is a dictionary that has `key`, distinct
    /// from [`Self::get_key`] returning a null handle for `key` (which
    /// cannot tell a missing key apart from one whose value is genuinely
    /// null) — mirrors `QPDFObjectHandle::hasKey`
    /// (`libqpdf/QPDFObjectHandle.cc:966-976`). `false` for a non-dictionary
    /// handle. Never performs resolution itself.
    pub fn has_key(&self, key: &[u8]) -> bool {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => entries.contains_key(key),
            _ => false,
        })
    }

    /// Insert or overwrite `key` in this handle's dictionary with `value`,
    /// mutating the live value every other clone of this handle also
    /// observes — mirrors `QPDFObjectHandle::replaceKey`
    /// (`libqpdf/QPDFObjectHandle.cc:1199-1209`). A no-op on a
    /// non-dictionary handle or an unresolved/missing/destroyed indirect
    /// handle, matching qpdf's own `typeWarning`-and-ignore contract rather
    /// than panicking. Also a no-op if `value` is the same direct handle as
    /// `self` — inserting a dictionary into itself would otherwise create a
    /// direct cycle that none of this crate's recursive walkers
    /// (`shallow_copy`, `materialize`, `Debug`) guard against, since they
    /// only stop recursion at an indirect-handle boundary. This does not
    /// detect a multi-hop reciprocal cycle built from two or more
    /// `replace_key` calls across distinct direct dictionaries. Unlike
    /// qpdf's `replaceKey`, this does not check that `value` belongs to the
    /// same document (`checkOwnership`) — no caller in this crate crosses
    /// document boundaries this way today. Never performs resolution
    /// itself.
    ///
    /// This mutates the live handle graph directly. If `self`'s ref has
    /// already been read through [`crate::Pdf::resolve`] or
    /// [`crate::Pdf::resolve_borrowed`], those methods cache the
    /// materialized value the first time a ref is resolved and do not
    /// re-derive it — a later call to either will keep returning the
    /// pre-mutation value for that ref rather than observing this change.
    /// Callers that need `resolve`/`resolve_borrowed` to reflect a
    /// mutation made through this API must not have resolved the same ref
    /// through them first.
    ///
    /// This also has no path to inform the owning [`crate::Pdf`] that
    /// `self`'s ref changed. A default (incremental) call to
    /// [`crate::write_pdf`] emits only refs marked dirty by
    /// [`crate::Pdf::set_object`]/[`crate::Pdf::delete_object`] — after
    /// mutating an already-registered indirect handle through this method,
    /// call [`crate::Pdf::mark_object_dirty`] with the same ref or the
    /// change is silently dropped from the written output.
    pub fn replace_key(&self, key: &[u8], value: ObjectHandle) {
        if self.is_same_direct_handle(&value) {
            return;
        }
        let owner_refs = self.child_owner_refs();
        let inserted = self.with_value_mut(|v| {
            if let Some(ObjectValue::Dictionary(entries)) = v {
                entries.insert(key.to_vec(), value.clone());
                return true;
            }
            false
        });
        if inserted {
            value.associate_with_owners(&owner_refs, 0);
        }
    }

    /// Replace an existing array item with `value`, preserving `value`'s
    /// shared handle identity. Returns `false` when this handle is not an
    /// array or `index` is out of bounds.
    pub(crate) fn replace_array_item(&self, index: usize, value: ObjectHandle) -> bool {
        if self.is_same_direct_handle(&value) {
            return false; // cov:ignore: exercised by replace_array_item_preserves_identity_and_rejects_invalid_slots but attributed to closure setup
        }
        let owner_refs = self.child_owner_refs();
        let replaced = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(items)) = current else {
                return false; // cov:ignore: exercised by replace_array_item_preserves_identity_and_rejects_invalid_slots but attributed to closure setup
            };
            let Some(item) = items.get_mut(index) else {
                return false; // cov:ignore: exercised by replace_array_item_preserves_identity_and_rejects_invalid_slots but attributed to closure setup
            };
            *item = value.clone();
            true
        });
        if replaced {
            value.associate_with_owners(&owner_refs, 0);
        }
        replaced
    }

    /// Replace every item in this live array while preserving the array
    /// handle itself. Returns `false` for a non-array handle or when the
    /// replacement would create a direct self-cycle.
    pub(crate) fn replace_array_items(&self, items: Vec<ObjectHandle>) -> bool {
        if items.iter().any(|item| self.is_same_direct_handle(item)) {
            return false; // cov:ignore: internal callers only replay materialized child arrays
        }
        let owner_refs = self.child_owner_refs();
        let replaced = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(current_items)) = current else {
                return false; // cov:ignore: internal callers confirm the array type first
            };
            *current_items = items.clone();
            true
        });
        if !replaced {
            return false; // cov:ignore: internal callers confirm the array type first
        }
        for item in items {
            item.associate_with_owners(&owner_refs, 0);
        }
        true
    }

    /// True if `self` and `other` are both direct handles sharing the same
    /// underlying storage — i.e. `other` is `self` itself (or a clone of
    /// it), not merely a distinct direct handle with an equal value. Unlike
    /// [`Self::is_same_object_as`], an indirect/indirect match returns `false` here:
    /// an indirect handle referencing itself is not a direct cycle and is
    /// already handled correctly by every recursive walker's
    /// indirect-boundary stop.
    fn is_same_direct_handle(&self, other: &Self) -> bool {
        matches!((&self.0, &other.0), (Repr::Direct(a), Repr::Direct(b)) if Rc::ptr_eq(a, b))
    }

    /// The indirect object refs inherited by a newly inserted direct child.
    /// An indirect parent is its own containment root; a direct parent can be
    /// shared by more than one indirect root and therefore propagates all of
    /// its recorded owners.
    fn child_owner_refs(&self) -> Vec<ContainmentOwner> {
        match &self.0 {
            Repr::Direct(slot) => slot
                .borrow()
                .containing_object_refs
                .iter()
                .copied()
                .collect(),
            Repr::Indirect(slot) => {
                let slot = slot.borrow();
                vec![ContainmentOwner {
                    pdf_unique_id: slot.pdf_unique_id,
                    object_ref: slot.object_ref,
                }]
            }
        }
    }

    fn associate_value_with_owners(value: &ObjectValue, owners: &[ContainmentOwner], depth: usize) {
        let children = match value {
            ObjectValue::Array(children) => children.clone(),
            ObjectValue::Dictionary(entries) => entries.values().cloned().collect(),
            ObjectValue::Stream { dict, .. } => vec![dict.clone()],
            _ => return,
        };
        for child in children {
            child.associate_with_owners(owners, depth + 1);
        }
    }

    fn associate_with_owners(&self, owners: &[ContainmentOwner], depth: usize) {
        if !self.is_direct() || owners.is_empty() || depth >= crate::object::MAX_INLINE_DEPTH {
            return;
        }
        let Repr::Direct(slot) = &self.0 else {
            return; // cov:ignore: is_direct guard above excludes Indirect
        };
        let children = {
            let mut slot = slot.borrow_mut();
            slot.containing_object_refs.extend(owners.iter().copied());
            match &slot.value {
                ObjectValue::Array(children) => children.clone(),
                ObjectValue::Dictionary(entries) => entries.values().cloned().collect(),
                ObjectValue::Stream { dict, .. } => vec![dict.clone()],
                _ => Vec::new(),
            }
        };
        for child in children {
            child.associate_with_owners(owners, depth + 1);
        }
    }

    /// Remove `key` from this handle's dictionary if present, mutating the
    /// live value every other clone of this handle also observes — mirrors
    /// `QPDFObjectHandle::removeKey` (`libqpdf/QPDFObjectHandle.cc:1226-1234`).
    /// A no-op if `key` is absent, this handle is not a dictionary, or the
    /// indirect handle is unresolved/missing/destroyed. Never performs
    /// resolution itself.
    ///
    /// See [`Self::replace_key`]'s doc comment for the same
    /// `resolve`/`resolve_borrowed` staleness caveat and the
    /// [`crate::Pdf::mark_object_dirty`] requirement — both apply here too.
    pub fn remove_key(&self, key: &[u8]) {
        self.with_value_mut(|v| {
            if let Some(ObjectValue::Dictionary(entries)) = v {
                entries.remove(key);
            }
        });
    }

    /// A fresh, direct handle with a value copied from `self` — mirrors
    /// `QPDFObjectHandle::shallowCopy` (`libqpdf/QPDFObjectHandle.cc:2073-2079`,
    /// which defers to each type's own `copy(shallow=false)` default —
    /// `libqpdf/QPDF_Dictionary.cc`/`libqpdf/QPDF_Array.cc`). Despite the
    /// name, this recursively copies through every *direct* array/dictionary
    /// descendant (each direct child is itself shallow-copied), stopping
    /// only at an *indirect* child, which keeps its existing shared
    /// identity rather than being copied — "shallow" describes not
    /// resolving/duplicating through indirection, not a single-level-only
    /// copy. A scalar value is cloned outright. Always returns a direct
    /// handle regardless of whether `self` is indirect. Never performs
    /// resolution itself: shallow-copying an unresolved/missing/destroyed
    /// indirect handle produces a direct null handle, matching every other
    /// accessor's "no hidden I/O" rule.
    ///
    /// qpdf's own `QPDF_Stream::copy` throws outright ("stream objects
    /// cannot be cloned", `libqpdf/QPDF_Stream.cc`), and this crate has no
    /// exception channel to mirror that with — the same precedent
    /// [`Self::unparse_resolved`]'s own doc comment already establishes for
    /// a different qpdf-throws case. Instead of leaving a stream's `dict`
    /// Rc-shared with the source (which would let a later
    /// [`Self::replace_stream_data`] on the copy silently corrupt the
    /// source's `/Length`/`/Filter`/`/DecodeParms`), a stream's `dict` is
    /// treated as a child exactly like an array/dictionary entry: copied
    /// independently when direct, shared when indirect.
    pub fn shallow_copy(&self) -> ObjectHandle {
        stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
            self.with_value(|value| match value {
                Some(v) => ObjectHandle::from_value(shallow_copy_value(v)),
                None => ObjectHandle::null(),
            })
        })
    }

    /// Merge `other`'s top-level entries into this handle's dictionary,
    /// mirroring `QPDFObjectHandle::mergeResources`
    /// (`libqpdf/QPDFObjectHandle.cc:1063-1153`; intended for merging two
    /// `/Resources`- or `/DR`-shaped dictionaries, per its own header doc,
    /// `include/qpdf/QPDFObjectHandle.hh:820-829`). `conflicts`, if given,
    /// records `rtype -> old_key -> new_key` for some (not all — see below)
    /// inner keys `other` had that collided with an existing key under the
    /// same top-level `rtype`.
    ///
    /// A no-op unless both `self` and `other` are dictionaries. For each of
    /// `other`'s top-level entries `(rtype, other_val)`:
    /// - if `self` has no `rtype` key yet, `other_val` is privatized via
    ///   [`Self::shallow_copy`] and installed via [`Self::replace_key`].
    /// - if `self`'s existing `rtype` value and `other_val` are both
    ///   dictionaries: `self`'s value is privatized first if it is
    ///   indirect (`shallow_copy` + `replace_key`, mirroring
    ///   `replaceKeyAndGetNew`'s combined mutate-and-rebind). Then each of
    ///   `other_val`'s own entries is merged in: a key the (now-private)
    ///   sub-dictionary does not have yet is installed directly
    ///   (privatized first unless already indirect); a key it already has
    ///   is left untouched unless `conflicts` is given, in which case an
    ///   incoming *indirect* value whose object identity already exists
    ///   somewhere in the sub-dictionary (as of the first such conflict
    ///   this call encounters — a snapshot taken once per `rtype`, not
    ///   re-taken per key) is reused under its existing name (`conflicts`
    ///   records this rename only when that existing name differs from the
    ///   incoming key — no rename is recorded, and nothing is installed,
    ///   when they already match); anything else is installed verbatim
    ///   under a freshly minted unique name (`conflicts` always records
    ///   this one).
    /// - if `self`'s existing `rtype` value and `other_val` are both
    ///   arrays: every scalar item in `other_val` whose
    ///   [`Self::unparse`] text does not already match a scalar item
    ///   already in `self`'s array is appended to it — a set union by
    ///   unparsed text, not object identity.
    /// - any other existing-`rtype` shape combination (mismatched types,
    ///   or neither dictionary nor array) leaves that entry untouched.
    ///
    /// The uniqueness pool for a freshly minted name is
    /// `this_val.getResourceNames()`'s own "second-level keys" definition
    /// (`libqpdf/QPDFObjectHandle.cc:1156-1170`) applied to the *inner*
    /// sub-dictionary itself, not to `self` as a whole — i.e. the keys of
    /// whichever of the sub-dictionary's *own* values are themselves
    /// dictionaries, not the sub-dictionary's own key set. This looks like
    /// it checks the wrong level (it does not, in general, collect the
    /// F1/F2-style names actually in scope), but it is qpdf's real,
    /// verified behavior, not a paraphrase — port it exactly rather than
    /// the more "sensible"-looking alternative of the sub-dictionary's own
    /// keys.
    ///
    /// See [`Self::replace_key`]'s doc comment for the same
    /// `resolve`/`resolve_borrowed` staleness caveat and the
    /// [`crate::Pdf::mark_object_dirty`] requirement — both apply here too,
    /// since this method installs and rebinds entries via `replace_key`.
    pub fn merge_resources(
        &self,
        other: &ObjectHandle,
        mut conflicts: Option<&mut ResourceConflicts>,
    ) {
        let (Some(_), Some(other_entries)) = (self.as_dictionary(), other.as_dictionary()) else {
            return;
        };
        for (rtype, other_val) in other_entries {
            if !self.has_key(&rtype) {
                self.replace_key(&rtype, other_val.shallow_copy());
                continue;
            }
            let mut this_val = self.get_key(&rtype);
            if this_val.as_dictionary().is_some() && other_val.as_dictionary().is_some() {
                if this_val.is_indirect() {
                    let privatized = this_val.shallow_copy();
                    self.replace_key(&rtype, privatized.clone());
                    this_val = privatized;
                }
                merge_resource_subdict(&this_val, &other_val, &rtype, conflicts.as_deref_mut());
            } else if this_val.as_array().is_some() && other_val.as_array().is_some() {
                merge_resource_array(&this_val, &other_val);
            }
            // Any other shape combination for an existing rtype: untouched,
            // matching qpdf's own fallthrough (neither the dictionary nor
            // the array arm matches, and there is no further branch).
        }
    }

    /// Replace this handle's stream data, and — when given — its `/Filter`
    /// and `/DecodeParms` dictionary keys, mirroring
    /// `QPDFObjectHandle::replaceStreamData`'s buffer overload
    /// (`libqpdf/QPDFObjectHandle.cc:1345-1350`, delegating to
    /// `QPDF_Stream::replaceStreamData`/`replaceFilterData`,
    /// `libqpdf/QPDF_Stream.cc:637-649,669-685`). `filter`/`decode_parms`
    /// are `Some` exactly where qpdf's own overload checks
    /// `QPDFObjectHandle::isInitialized()`: `Some` installs the key via
    /// [`Self::replace_key`], `None` leaves it untouched rather than
    /// removing it. `/Length` is always set to `data`'s byte length —
    /// qpdf's "unknown length, remove `/Length`" branch only applies to its
    /// deferred-`StreamDataProvider` overloads, which this method does not
    /// port (no caller in this crate needs deferred stream production). A
    /// no-op if this handle's value is not a stream.
    ///
    /// See [`Self::replace_key`]'s doc comment for the same
    /// `resolve`/`resolve_borrowed` staleness caveat and the
    /// [`crate::Pdf::mark_object_dirty`] requirement — both apply here too,
    /// since this method installs `/Filter`/`/DecodeParms`/`/Length` via
    /// `replace_key` and mutates the stream data in place.
    pub fn replace_stream_data(
        &self,
        data: Vec<u8>,
        filter: Option<ObjectHandle>,
        decode_parms: Option<ObjectHandle>,
    ) {
        let Some(dict) = self.as_stream_dict() else {
            return;
        };
        if let Some(filter) = filter {
            dict.replace_key(b"Filter", filter);
        }
        if let Some(decode_parms) = decode_parms {
            dict.replace_key(b"DecodeParms", decode_parms);
        }
        dict.replace_key(
            b"Length",
            ObjectHandle::integer(i64::try_from(data.len()).unwrap_or(i64::MAX)),
        );
        self.with_value_mut(|v| {
            if let Some(ObjectValue::Stream { data: existing, .. }) = v {
                *existing = data;
            }
        });
    }

    /// The stream's own dictionary handle if this handle's value — its own
    /// if direct, or its already-resolved value if indirect — is a stream,
    /// or `None` otherwise. This never performs resolution itself: an
    /// indirect handle that has not yet been resolved returns `None` too,
    /// the same as a resolved value of a different type. Cloning the
    /// returned handle is O(1): it shares the dictionary's identity rather
    /// than copying its subtree.
    pub fn as_stream_dict(&self) -> Option<ObjectHandle> {
        self.with_value(|value| match value {
            Some(ObjectValue::Stream { dict, .. }) => Some(dict.clone()),
            _ => None,
        })
    }

    /// The stream's raw encoded byte payload if this handle's value — its
    /// own if direct, or its already-resolved value if indirect — is a
    /// stream, or `None` otherwise. This never performs resolution itself:
    /// an indirect handle that has not yet been resolved returns `None`
    /// too, the same as a resolved value of a different type.
    pub fn as_stream_data(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Stream { data, .. }) => Some(data.clone()),
            _ => None,
        })
    }

    /// The value as raw operator bytes if this handle's value — its own if
    /// direct, or its already-resolved value if indirect — is a
    /// content-stream operator token, or `None` otherwise. Never performs
    /// resolution itself.
    pub fn as_operator(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Operator(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The value as raw inline-image bytes if this handle's value — its own
    /// if direct, or its already-resolved value if indirect — is an
    /// inline-image payload, or `None` otherwise. Never performs resolution
    /// itself.
    pub fn as_inline_image(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::InlineImage(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The qpdf-compatible numeric type code of this handle's current known
    /// value: `include/qpdf/Constants.h:108-127`'s `qpdf_object_type_e`
    /// ordinals. qpdf's own `getTypeCode()`/`getTypeName()`
    /// (`include/qpdf/QPDFObjectHandle.hh:311-316`,
    /// `libqpdf/QPDFObjectHandle.cc:240-250`) call `dereference()`, which
    /// unconditionally resolves the handle first
    /// (`libqpdf/QPDFObjectHandle.cc:2376-2382`); this method never performs
    /// that hidden resolution (design, `Pdf` section: no hidden I/O), so an
    /// indirect handle's *reachable* resolution states surface as their own
    /// qpdf ordinals instead: not-yet-resolved reports `13`
    /// (`ot_unresolved`) and a destroyed (owning document dropped) handle
    /// reports `14` (`ot_destroyed`) — both real `qpdf_object_type_e`
    /// entries, not invented here. `ot_uninitialized`/`ot_reserved` (qpdf's
    /// two remaining entries) are construction-time-only states this port's
    /// `ObjectHandle` never occupies, since every handle is fully
    /// constructed at birth.
    ///
    /// A resolved indirect handle whose own value is itself a bare
    /// reference (mirroring [`crate::Object::Reference`]; see
    /// [`Self::as_reference`]'s own doc), a `Pdf::set_object`-driven
    /// redirect, also reports `13`. This looks like a
    /// contradiction with [`Self::is_resolved`] returning `true` for the
    /// same handle, but it is not: the *value* is known (it is a reference),
    /// while the *referenced object's own type* is not known without
    /// following the chain further, which this method never does — this
    /// case is not chased to its terminal type the way it would be
    /// elsewhere in this crate's own object-inspection code, and `13`
    /// (`ot_unresolved`) is reported as a placeholder rather than the
    /// terminal object's real ordinal.
    pub fn type_code(&self) -> u8 {
        if let Repr::Indirect(slot) = &self.0 {
            // Bind the borrow to a local first and match on it, mirroring
            // this file's own `Debug` impl (see above) rather than matching
            // directly against a temporary. The borrow ends at this block's
            // closing brace, strictly before `with_value` below takes its
            // own borrow of the same slot — never nested.
            let slot_ref = slot.borrow();
            match &slot_ref.state {
                IndirectState::NotYetResolved => return 13,
                IndirectState::Destroyed => return 14,
                IndirectState::Missing | IndirectState::Resolved(_) => {}
            }
        }
        self.with_value(|value| {
            match value.expect(
                "every state reaching here (direct, indirect Missing, indirect Resolved) carries a value",
            ) {
                ObjectValue::Null => 2,
                ObjectValue::Boolean(_) => 3,
                ObjectValue::Integer(_) => 4,
                ObjectValue::Real(_) | ObjectValue::RealLiteral { .. } => 5,
                ObjectValue::String(_) => 6,
                ObjectValue::Name(_) => 7,
                ObjectValue::Array(_) => 8,
                ObjectValue::Dictionary(_) => 9,
                ObjectValue::Stream { .. } => 10,
                ObjectValue::Operator(_) => 11,
                ObjectValue::InlineImage(_) => 12,
                // See this method's own doc for why this maps to
                // `ot_unresolved`: a real, reachable state (via
                // `Pdf::set_object`), not speculative dead code — see
                // `resolved_to_a_reference_indirect_handle_reports_unresolved`
                // for a test that exercises it via the same `set_resolved`
                // call `Pdf::set_object` itself makes.
                ObjectValue::Reference(_) => 13,
            }
        })
    }

    /// The qpdf-compatible type name string for [`Self::type_code`]'s
    /// ordinal (`libqpdf/QPDFObjectHandle.cc:240-250`'s `getTypeName`, via
    /// each `QPDFValue` subclass's own registered name, e.g.
    /// `libqpdf/QPDF_InlineImage.cc:6`). See [`Self::type_code`]'s own doc
    /// for the states this port surfaces instead of qpdf's silent resolve.
    pub fn type_name(&self) -> &'static str {
        match self.type_code() {
            2 => "null",
            3 => "boolean",
            4 => "integer",
            5 => "real",
            6 => "string",
            7 => "name",
            8 => "array",
            9 => "dictionary",
            10 => "stream",
            11 => "operator",
            12 => "inline-image",
            14 => "destroyed",
            // `type_code` only ever returns 13 for any other value it can
            // produce, so this is exhaustive in practice, not a silent
            // catch-all for an unhandled ordinal.
            _ => "unresolved",
        }
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

    // Mutable twin of `with_value` above: `None` for an indirect handle not
    // yet resolved (mutation on an unresolved handle must not perform
    // hidden I/O, same rule as every read accessor), and for
    // `Missing`/`Destroyed` (there is no live `ObjectValue::Null` slot to
    // hand out a `&mut` into — those states only *present* as null, they do
    // not store one).
    fn with_value_mut<T>(&self, f: impl FnOnce(Option<&mut ObjectValue>) -> T) -> T {
        match &self.0 {
            Repr::Direct(slot) => f(Some(&mut slot.borrow_mut().value)),
            Repr::Indirect(slot) => match &mut slot.borrow_mut().state {
                IndirectState::Resolved(value) => f(Some(value)),
                IndirectState::NotYetResolved
                | IndirectState::Missing
                | IndirectState::Destroyed => f(None),
            },
        }
    }

    /// Convert this handle's value into a legacy [`crate::Object`] tree —
    /// `Pdf::resolve`/`Pdf::resolve_borrowed`'s own materialization bridge,
    /// also public for a caller outside this crate that still needs a
    /// legacy `Object`/[`Dictionary`] for one value reached through an
    /// otherwise `ObjectHandle`-native walk (e.g. `flpdf-qtest-tools`' qtest
    /// driver, which ports a `&Dictionary`-shaped qpdf filter/`DecodeParms`
    /// resolution routine).
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
    ///
    /// A tree built through the public [`Self::array`]/[`Self::dictionary`]
    /// factories carries no depth bound the way parsed input does, so this
    /// walk is wrapped with the same stack-growth protection
    /// [`Self::unparse_resolved`]/[`Self::shallow_copy`] already rely on
    /// during construction, *and* capped at `parser::MAX_PARSE_DEPTH`,
    /// substituting `Object::Null` for anything nested past that — no
    /// document this crate accepts could parse a value nested deeper, so
    /// the cap only ever bites a tree built directly through those
    /// factories. Growing the construction stack alone would not be
    /// enough on its own: the *returned* `Object` tree's own ordinary
    /// recursive `Drop` runs later, unprotected, once this method has
    /// already returned and the grown stack is gone — the depth cap keeps
    /// that later drop within a size every other `MAX_PARSE_DEPTH`-bounded
    /// `Object` tree in this crate already handles routinely, rather than
    /// trying to protect `Drop` itself (`Object`'s recursive `Drop` glue
    /// lives in `object.rs`, outside this file's scope to change).
    ///
    /// This does not protect `self` — the handle passed in — the same way:
    /// an `ObjectHandle` tree that deep, built through the same public
    /// factories, is a separate, pre-existing gap in `ObjectHandle`'s own
    /// `Drop`, reachable without ever calling `materialize` at all (it
    /// existed as long as those factories have been public), not something
    /// introduced or fixable here.
    pub fn materialize(&self) -> Object {
        materialize_bounded(self, 0)
    }

    /// This handle's qpdf-syntax unparse form
    /// (`include/qpdf/QPDFObjectHandle.hh:1159`,
    /// `libqpdf/QPDFObjectHandle.cc:1574-1584`): an indirect handle always
    /// unparses to its own `"N G R"`, regardless of resolution state; a
    /// direct handle delegates to [`Self::unparse_resolved`].
    pub fn unparse(&self) -> Vec<u8> {
        match self.object_ref() {
            Some(object_ref) => {
                let mut out = Vec::new();
                Object::Reference(object_ref).write_pdf(&mut out);
                out
            }
            None => self.unparse_resolved(),
        }
    }

    /// This handle's resolved value in qpdf syntax
    /// (`libqpdf/QPDFObjectHandle.cc:1586-1593`), except that an *indirect*
    /// handle whose resolved value is a stream always reports its own
    /// reference form instead (`libqpdf/QPDF_Stream.cc:173-178`) — a stream
    /// is only ever a top-level indirect object in valid qpdf usage. A
    /// direct handle wrapping a stream value (a shape this crate's own
    /// types do not forbid, though qpdf's do) falls through to the same
    /// inlining fallback as any other direct container value; see
    /// `unparse_tests`' own direct-stream test for that case.
    ///
    /// This port diverges from qpdf's own `unparseResolved()` in two
    /// internal resolution states that qpdf itself does not reach the same
    /// way:
    /// - **Not yet resolved**: qpdf silently dereferences (resolves) an
    ///   unresolved indirect handle before unparsing it; this method does
    ///   not perform that hidden I/O, matching every other accessor in this
    ///   file (see e.g. [`Self::as_integer`]'s own doc) — no accessor here
    ///   resolves on the caller's behalf. Reports the same `null` fallback
    ///   the value would show before resolution.
    /// - **Destroyed** (the owning document has been dropped and this
    ///   handle's value severed): qpdf's `QPDF_Destroyed::unparse()`
    ///   (`libqpdf/QPDF_Destroyed.cc:24-29`) throws `std::logic_error`; this
    ///   method has no exception channel to mirror that with (`Vec<u8>`
    ///   return, no `Result`) and instead presents the same `null` fallback
    ///   this file's other value accessors (e.g. [`Self::is_null`]) already
    ///   give a destroyed handle, rather than panicking.
    pub fn unparse_resolved(&self) -> Vec<u8> {
        // Bridges through a null-omission-aware materialization walk
        // (`unparse_materialize`, distinct from the general `materialize`/
        // `Pdf::resolve_borrowed` bridge -- see that function's own doc)
        // and `Object::write_pdf`'s own already-byte-identical-tested
        // formatter rather than duplicating array/dict/string-escaping
        // logic against `ObjectValue` directly.
        let is_stream = self.object_ref().is_some()
            && self.with_value(|value| matches!(value, Some(ObjectValue::Stream { .. })));
        if is_stream {
            return self.unparse();
        }
        let mut out = Vec::new();
        let materialized = unparse_materialize(self);
        materialized.write_pdf(&mut out);
        // `Object`'s own recursive Drop glue would walk this tree exactly
        // as deep as the walk above just did, unprotected by
        // `stacker::maybe_grow` -- protecting construction alone would
        // still let a deep enough tree crash the process immediately after
        // serialization completes, right here. Tear it down iteratively
        // instead of letting it drop normally.
        unparse_drop_iteratively(materialized);
        out
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
        let owner_refs = self.child_owner_refs();
        if let Repr::Direct(slot) = &self.0 {
            slot.borrow_mut().value = value;
            self.with_value(|value| {
                if let Some(value) = value {
                    Self::associate_value_with_owners(value, &owner_refs, 0);
                }
            });
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

// The sole recursion hub for `ObjectHandle::materialize` — every nested
// descent (array items, dictionary values, a stream's own dictionary handle)
// goes through this function via `materialize_child`, so the depth cap and
// `stacker::maybe_grow` wrap apply uniformly regardless of which container
// shape carries the nesting. `depth` past `parser::MAX_PARSE_DEPTH`
// substitutes `Object::Null`: no document this crate accepts could parse a
// value nested deeper than that, so only a tree built directly through the
// public `ObjectHandle::array`/`dictionary` factories (which impose no depth
// bound themselves) can reach the cap at all.
fn materialize_bounded(handle: &ObjectHandle, depth: usize) -> Object {
    if depth > crate::parser::MAX_PARSE_DEPTH {
        return Object::Null;
    }
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        handle.with_value(|value| match value {
            Some(value) => materialize_value(value, depth),
            None => Object::Null,
        })
    })
}

fn materialize_value(value: &ObjectValue, depth: usize) -> Object {
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
        ObjectValue::Operator(bytes) => Object::Operator(bytes.clone()),
        ObjectValue::InlineImage(bytes) => Object::InlineImage(bytes.clone()),
        ObjectValue::Array(children) => Object::Array(
            children
                .iter()
                .map(|child| materialize_child(child, depth + 1))
                .collect(),
        ),
        ObjectValue::Dictionary(entries) => {
            let mut dict = Dictionary::new();
            for (key, value) in entries {
                dict.insert(key.as_slice(), materialize_child(value, depth + 1));
            }
            Object::Dictionary(dict)
        }
        ObjectValue::Stream { dict, data } => {
            let dict = match materialize_bounded(dict, depth + 1) {
                Object::Dictionary(dict) => dict,
                // A stream's own dictionary handle is always constructed as
                // a direct `ObjectValue::Dictionary` (see
                // `Pdf::native_parse_uncompressed_value`, `Pdf::lift`, and
                // `Pdf::lift_for_set_object`), never an indirect reference
                // or any other variant, unless the depth cap above already
                // substituted `Object::Null` for it.
                _ => Dictionary::new(), // cov:ignore: unreachable outside the depth-cap fallback, itself covered separately
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
fn materialize_child(handle: &ObjectHandle, depth: usize) -> Object {
    match handle.object_ref() {
        Some(object_ref) => Object::Reference(object_ref),
        None => materialize_bounded(handle, depth),
    }
}

// A separate materialization walk used only by `ObjectHandle::unparse_resolved`,
// not by the general `materialize`/`Pdf::resolve_borrowed` bridge above (whose
// existing behavior other callers depend on unchanged). Applies qpdf's
// dictionary-entry null-omission rule (`QPDF_Dictionary::unparse()`,
// `libqpdf/QPDF_Dictionary.cc:59-69`: `if (!iter.second.isNull()) { ... }`) —
// an explicit null value is equivalent to a missing key. `QPDF_Array::unparse()`
// (`libqpdf/QPDF_Array.cc:123-140`) has no such rule; array elements keep
// their null values verbatim, so only the `Dictionary` arm differs from
// `materialize_value` above.
fn unparse_materialize_value(value: &ObjectValue) -> Object {
    match value {
        ObjectValue::Array(children) => {
            Object::Array(children.iter().map(unparse_materialize_child).collect())
        }
        ObjectValue::Dictionary(entries) => {
            let mut dict = Dictionary::new();
            for (key, value) in entries {
                if unparse_is_known_null(value) {
                    continue;
                }
                dict.insert(key.as_slice(), unparse_materialize_child(value));
            }
            Object::Dictionary(dict)
        }
        ObjectValue::Stream { dict, data } => {
            let dict = match unparse_materialize(dict) {
                Object::Dictionary(dict) => dict,
                _ => Dictionary::new(), // cov:ignore: same invariant as materialize_value's own Stream arm
            };
            Object::Stream(Stream::new(dict, data.clone()))
        }
        // No other variant nests a dictionary, so the omission rule cannot
        // apply anywhere beneath it; delegate to the ordinary materializer.
        // Every remaining variant is a scalar with no further recursion, so
        // the depth this arm passes never actually matters.
        other => materialize_value(other, 0),
    }
}

// Stack-safety constants for `unparse_materialize`'s recursive walk,
// mirroring `parser.rs`'s own `STACK_RED_ZONE`/`STACK_GROWTH_SIZE` values
// (kept as separate local constants rather than imported cross-module,
// since this slice's own scope is limited to this file). A tree built
// directly through the public `ObjectHandle::array`/`dictionary` factories
// carries no depth bound the way parsed input does (`parser::MAX_PARSE_DEPTH`
// rejects a document too deep to parse before an `ObjectHandle` tree that
// deep can even exist for it), so this walk needs the same stack-growth
// protection the parser already relies on for its own recursion.
const UNPARSE_STACK_RED_ZONE: usize = 32 * 1024;
const UNPARSE_STACK_GROWTH_SIZE: usize = 1024 * 1024;

// The sole recursion hub for `unparse_materialize_value`'s `Array`/
// `Dictionary`/`Stream` arms (every nested descent goes through
// `unparse_materialize_child`, which calls back into this function) --
// wrapping recursion here, in one place, bounds every nesting path the same
// way `parser::Parser::object`'s own single hub does for parsing.
fn unparse_materialize(handle: &ObjectHandle) -> Object {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        handle.with_value(|value| match value {
            Some(value) => unparse_materialize_value(value),
            None => Object::Null,
        })
    })
}

fn unparse_materialize_child(handle: &ObjectHandle) -> Object {
    match handle.object_ref() {
        Some(object_ref) => Object::Reference(object_ref),
        None => unparse_materialize(handle),
    }
}

// `ObjectHandle::shallow_copy`'s per-variant dispatch: an Array/Dictionary
// child is recursively shallow-copied through `shallow_copy_child` (which
// re-enters `ObjectHandle::shallow_copy`, the recursion hub carrying its
// own `stacker::maybe_grow` wrap — the same hub-per-call shape as
// `unparse_materialize`/`unparse_materialize_child` above). A `Stream`'s
// `dict` field is a child in exactly the same sense as an array/dictionary
// entry and gets the same `shallow_copy_child` treatment, so the copy's
// dictionary is independent of the source's rather than Rc-shared while
// only `data` is deep-cloned (see `shallow_copy`'s own doc comment). Every
// other variant is cloned as-is with no further recursion.
fn shallow_copy_value(value: &ObjectValue) -> ObjectValue {
    match value {
        ObjectValue::Array(items) => {
            ObjectValue::Array(items.iter().map(shallow_copy_child).collect())
        }
        ObjectValue::Dictionary(entries) => ObjectValue::Dictionary(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), shallow_copy_child(v)))
                .collect(),
        ),
        ObjectValue::Stream { dict, data } => ObjectValue::Stream {
            dict: shallow_copy_child(dict),
            data: data.clone(),
        },
        other => other.clone(),
    }
}

fn shallow_copy_child(child: &ObjectHandle) -> ObjectHandle {
    if child.is_indirect() {
        child.clone()
    } else {
        child.shallow_copy()
    }
}

// `ObjectHandle::merge_resources`'s per-rtype dictionary merge (the
// `this_val.isDictionary() && other_val.isDictionary()` arm of
// `QPDFObjectHandle::mergeResources`, `libqpdf/QPDFObjectHandle.cc:1095-1129`).
// `this_val` is already the privatized (non-indirect) sub-dictionary by the
// time this is called.
fn merge_resource_subdict(
    this_val: &ObjectHandle,
    other_val: &ObjectHandle,
    rtype: &[u8],
    mut conflicts: Option<&mut ResourceConflicts>,
) {
    let mut og_to_name: Option<std::collections::HashMap<ObjectRef, Vec<u8>>> = None;
    let mut rnames: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
    let mut min_suffix: usize = 1;
    let Some(other_sub_entries) = other_val.as_dictionary() else {
        return; // cov:ignore: caller already confirmed other_val.as_dictionary().is_some()
    };
    for (key, rval) in other_sub_entries {
        if !this_val.has_key(&key) {
            let installed = if rval.is_indirect() {
                rval
            } else {
                rval.shallow_copy()
            };
            this_val.replace_key(&key, installed);
            continue;
        }
        let Some(conflicts_map) = conflicts.as_deref_mut() else {
            continue;
        };
        if og_to_name.is_none() {
            og_to_name = Some(build_og_to_name(this_val));
            rnames = get_resource_names(this_val);
        }
        let reused = rval
            .object_ref()
            .and_then(|r| og_to_name.as_ref().and_then(|m| m.get(&r).cloned()));
        if let Some(existing_key) = reused {
            if existing_key != key {
                conflicts_map
                    .entry(rtype.to_vec())
                    .or_default()
                    .insert(key, existing_key);
            }
        } else {
            let new_key = unique_resource_name(&key, &mut min_suffix, &rnames);
            conflicts_map
                .entry(rtype.to_vec())
                .or_default()
                .insert(key, new_key.clone());
            this_val.replace_key(&new_key, rval);
        }
    }
}

// `ObjectHandle::merge_resources`'s per-rtype array merge (the
// `this_val.isArray() && other_val.isArray()` arm,
// `libqpdf/QPDFObjectHandle.cc:1130-1146`): union `other_val`'s scalar
// items into `this_val` by unparsed text, appending only what is not
// already present.
fn merge_resource_array(this_val: &ObjectHandle, other_val: &ObjectHandle) {
    let Some(other_items) = other_val.as_array() else {
        return; // cov:ignore: caller already confirmed other_val.as_array().is_some()
    };
    let mut scalars: std::collections::BTreeSet<Vec<u8>> = this_val
        .as_array()
        .into_iter()
        .flatten()
        .filter(is_scalar)
        .map(|item| item.unparse())
        .collect();
    for item in other_items {
        if !is_scalar(&item) {
            continue;
        }
        let text = item.unparse();
        if scalars.insert(text) {
            append_array_item(this_val, item);
        }
    }
}

fn append_array_item(handle: &ObjectHandle, item: ObjectHandle) {
    handle.with_value_mut(|v| {
        if let Some(ObjectValue::Array(items)) = v {
            items.push(item);
        }
    });
}

// Mirrors `isScalar()` (`libqpdf/QPDFObjectHandle.cc:450-453`): bool,
// integer, name, null, real, or string. Checks only already-resolved/
// direct state, matching every other accessor in this file's "no hidden
// I/O" rule.
fn is_scalar(handle: &ObjectHandle) -> bool {
    handle.as_boolean().is_some()
        || handle.as_integer().is_some()
        || handle.as_name().is_some()
        || handle.is_null()
        || handle.as_real().is_some()
        || handle.as_string().is_some()
}

// Mirrors `mergeResources`'s local `make_og_to_name` lambda
// (`libqpdf/QPDFObjectHandle.cc:1071-1078`): every currently-indirect
// entry in `dict`, keyed by object identity.
fn build_og_to_name(dict: &ObjectHandle) -> std::collections::HashMap<ObjectRef, Vec<u8>> {
    let mut map = std::collections::HashMap::new();
    if let Some(entries) = dict.as_dictionary() {
        for (key, value) in entries {
            if let Some(object_ref) = value.object_ref() {
                map.insert(object_ref, key);
            }
        }
    } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact; the body above is exercised by merge_resources_reuses_an_existing_key_for_the_same_indirect_object
    map
}

// Mirrors `getResourceNames` (`libqpdf/QPDFObjectHandle.cc:1156-1170`,
// `include/qpdf/QPDFObjectHandle.hh:831-835`): the union of every key
// belonging to a dictionary-valued entry of `dict` -- i.e. `dict`'s own
// *grandchildren's* keys, not `dict`'s own keys. See `merge_resources`'s
// own doc comment for why this is the correct level to port here despite
// looking mismatched against its call site.
fn get_resource_names(dict: &ObjectHandle) -> std::collections::BTreeSet<Vec<u8>> {
    let mut result = std::collections::BTreeSet::new();
    if let Some(entries) = dict.as_dictionary() {
        for (_, value) in entries {
            if let Some(sub_entries) = value.as_dictionary() {
                result.extend(sub_entries.into_keys());
            }
        }
    } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact; the body above is exercised by merge_resources_mints_a_second_unique_name_when_the_first_candidate_is_taken
    result
}

// Mirrors `getUniqueResourceName` (`libqpdf/QPDFObjectHandle.cc:1175-1192`):
// append a decimal suffix (starting at `*min_suffix`) to `key` + `"_"`
// until the result is absent from `names`, leaving `*min_suffix` at the
// value just used (not incremented past it -- a caller minting several
// names in the same sub-dictionary reuses the search position, matching
// qpdf's own "used, not next" contract).
fn unique_resource_name(
    key: &[u8],
    min_suffix: &mut usize,
    names: &std::collections::BTreeSet<Vec<u8>>,
) -> Vec<u8> {
    let mut prefix = key.to_vec();
    prefix.push(b'_');
    let max_suffix = *min_suffix + names.len();
    while *min_suffix <= max_suffix {
        let mut candidate = prefix.clone();
        candidate.extend(min_suffix.to_string().into_bytes());
        if !names.contains(&candidate) {
            return candidate;
        }
        *min_suffix += 1;
    }
    // Unreachable per qpdf's own invariant: this loop tests strictly more
    // candidates (names.len() + 1) than there are names to conflict with,
    // so by pigeonhole one must be free. qpdf itself treats reaching this
    // point as a coding error (throws std::logic_error); this crate has no
    // exception channel to mirror that with, so panic the same way an
    // internal invariant violation elsewhere in this crate would.
    unreachable!("no unconflicting resource name found") // cov:ignore: unreachable, see comment above
}

// True if `handle`'s value is already known (no resolution performed here)
// to be null: a direct null, or an indirect handle that is already resolved
// (`ObjectHandle::is_resolved`) and reads as null. qpdf's own check
// (`QPDFObjectHandle::isNull()`, `libqpdf/QPDFObjectHandle.cc:353-356`)
// dereferences an indirect child to decide this; this port never performs
// that hidden resolution (matching every other accessor in this file), so a
// not-yet-resolved indirect entry is conservatively treated as *not* known
// to be null and is kept rather than guessed away.
fn unparse_is_known_null(handle: &ObjectHandle) -> bool {
    handle.is_resolved() && handle.is_null()
}

// Tears down a materialized `Object` tree without using its own recursive
// Drop glue, which -- like `unparse_materialize`'s construction walk this
// mirrors -- has no protection against a deeply nested tree's per-frame
// stack cost. Takes ownership so the caller's normal drop of the same value
// never runs (its children have already been moved out and pushed onto this
// function's own explicit, heap-allocated stack by the time each node's
// turn to drop trivially arrives).
//
// Only `Array`/`Dictionary`/`Stream` nest another `Object`; every other
// variant holds no `Object` children and drops in O(1) once popped, whether
// or not this function ever visits it -- `Dictionary`/`Stream` are drained
// through their existing public `iter()`/`remove()` API (no new access
// needed into `object.rs`, kept outside this slice's file allowlist).
fn unparse_drop_iteratively(root: Object) {
    let mut stack = vec![root];
    while let Some(mut node) = stack.pop() {
        match &mut node {
            Object::Array(items) => stack.extend(std::mem::take(items)),
            Object::Dictionary(dict) => drain_dictionary_onto(dict, &mut stack),
            Object::Stream(stream) => drain_dictionary_onto(&mut stream.dict, &mut stack),
            _ => {}
        }
        // `node`'s own nested `Object` children (if any) were just moved
        // out above, so its normal drop here -- an empty `Vec`/`Dictionary`
        // plus whatever non-recursive fields it holds -- is O(1).
    }
}

fn drain_dictionary_onto(dict: &mut Dictionary, stack: &mut Vec<Object>) {
    let keys: Vec<Vec<u8>> = dict.iter().map(|(key, _)| key.to_vec()).collect();
    for key in keys {
        if let Some(value) = dict.remove(key) {
            stack.push(value);
        }
    }
}

#[cfg(test)]
pub(crate) mod identity_tests {
    use super::*;

    struct RecordingResolver {
        calls: ResolutionLog,
        value: ObjectValue,
    }

    /// Every `resolve_indirect` a [`RecordingResolver`] performed, in order.
    ///
    /// Shared with the caller by [`logged_resolver_bearing_handle`] so a test
    /// can assert a *negative*: that a position was never resolved at all.
    /// `ObjectHandle::is_resolved` is not a substitute — a resolver that
    /// errored would leave the handle unresolved despite having been called.
    pub(crate) type ResolutionLog = Rc<RefCell<Vec<ObjectRef>>>;

    impl RecordingResolver {
        /// Install `value` instead of the default one-key dictionary, so a
        /// test can exercise a resolving accessor for a non-dictionary shape.
        ///
        /// One instance installs the *same* child handles on every resolution:
        /// cloning an `ObjectValue` container clones child `Rc`s rather than
        /// the subtree (see that enum's own doc). Resolving two handles through
        /// a single resolver therefore leaves their children `ptr_eq`, which no
        /// current test wants — give each such test its own resolver.
        fn installing(value: ObjectValue) -> Self {
            Self::logging_into(Rc::new(RefCell::new(Vec::new())), value)
        }

        /// [`Self::installing`] with the call log owned by the caller instead.
        fn logging_into(calls: ResolutionLog, value: ObjectValue) -> Self {
            Self { calls, value }
        }
    }

    impl Default for RecordingResolver {
        fn default() -> Self {
            Self::installing(ObjectValue::Dictionary(
                [(b"A".to_vec(), ObjectHandle::integer(1))]
                    .into_iter()
                    .collect(),
            ))
        }
    }

    impl DocumentResolver for RecordingResolver {
        fn resolve_indirect(
            &self,
            object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            self.calls.borrow_mut().push(object_ref);
            handle.set_resolved(self.value.clone());
            Ok(())
        }
    }

    /// An unresolved indirect handle whose resolver installs `value`.
    ///
    /// `pub(crate)` so `stream_filter.rs`'s handle-shape reader tests can
    /// build an indirect child without a second harness; the returned
    /// resolver is erased, so `RecordingResolver` itself stays private here.
    ///
    /// **The caller must keep the returned resolver alive**, and bind it to a
    /// named `_resolver` rather than to `_` — the latter drops it immediately.
    /// The handle holds only a `Weak`, so a dropped resolver turns every
    /// accessor into `Error::Internal("object 20 0 belongs to a dropped
    /// PDF")`: a test expecting a resolved value then fails confusingly, and
    /// one expecting an error passes for the wrong reason. Dropping it
    /// *deliberately* is how to build a dropped-document handle, as
    /// `handle_reader_surfaces_a_dropped_document_from_every_child_position`
    /// does.
    pub(crate) fn resolver_bearing_handle(
        value: ObjectValue,
    ) -> (ObjectHandle, Rc<dyn DocumentResolver>) {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::installing(value));
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver),
        );
        // The resolver is returned so the caller keeps it alive: the handle
        // holds only a `Weak`, and dropping it here would turn every accessor
        // into the dropped-document error instead.
        (handle, resolver)
    }

    /// [`resolver_bearing_handle`] plus the resolver's [`ResolutionLog`].
    ///
    /// For the one question the plain helper cannot answer: whether a child
    /// position was resolved *at all*. The same "keep the resolver alive"
    /// rule applies — an empty log proves nothing if the resolver was
    /// dropped, since a severed handle never reaches `resolve_indirect`
    /// either.
    pub(crate) fn logged_resolver_bearing_handle(
        value: ObjectValue,
    ) -> (ObjectHandle, Rc<dyn DocumentResolver>, ResolutionLog) {
        let calls: ResolutionLog = Rc::new(RefCell::new(Vec::new()));
        let resolver: Rc<dyn DocumentResolver> =
            Rc::new(RecordingResolver::logging_into(Rc::clone(&calls), value));
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver),
        );
        (handle, resolver, calls)
    }

    struct MissingResolver;

    impl DocumentResolver for MissingResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            handle.set_missing();
            Ok(())
        }
    }

    struct ErrorResolver;

    impl DocumentResolver for ErrorResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            _handle: &ObjectHandle,
        ) -> crate::Result<()> {
            Err(Error::System("resolver failed".to_string()))
        }
    }

    #[test]
    fn try_get_key_resolves_the_same_indirect_slot_once() {
        let resolver = Rc::new(RecordingResolver::default());
        let erased: Rc<dyn DocumentResolver> = resolver.clone();
        let handle =
            ObjectHandle::new_indirect_with_resolver(ObjectRef::new(7, 0), Rc::downgrade(&erased));
        let clone = handle.clone();

        assert_eq!(handle.try_get_key(b"A").unwrap().as_integer(), Some(1));
        assert!(clone.try_has_key(b"A").unwrap());
        assert_eq!(*resolver.calls.borrow(), vec![ObjectRef::new(7, 0)]);
        assert!(handle.ptr_eq(&clone));
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(7, 0)));
    }

    #[test]
    fn try_dereference_reports_a_dropped_document_without_reconnecting() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(8, 0),
            Rc::downgrade(&resolver),
        );
        drop(resolver);

        let error = handle.try_dereference().unwrap_err();
        assert_eq!(error.to_string(), "object 8 0 belongs to a dropped PDF");
        assert!(!handle.is_resolved());
    }

    /// A handle needs its owning document's identity *and* that document's
    /// resolver at once: `Pdf::get_object_handle` hands out one handle that
    /// must answer both questions.
    ///
    /// The identity is not decorative. `set_resolved` stamps the slot's
    /// `pdf_unique_id` onto every direct child it installs (via
    /// `associate_value_with_owners`), and that stamp is what
    /// [`ObjectHandle::belongs_to_pdf`] and
    /// [`ObjectHandle::containing_object_refs_for_pdf`] read — the
    /// foreign-object rejection and owner lookup in
    /// `Pdf::mark_object_handle_dirty`, `filespec_helper`, and
    /// `embedded_files`. Building this handle with `pdf_unique_id: None`
    /// fails 61 tests in `cargo test -p flpdf --lib`, measured by patching
    /// `new_indirect_unresolved_for_pdf` to discard its argument.
    ///
    /// Note this is *not* what `Pdf::is_canonical_object_handle` compares on:
    /// that one looks the ref up in `handle_registry` and compares `Rc`
    /// pointers, never touching `pdf_unique_id`.
    #[test]
    fn an_indirect_slot_carries_both_its_pdf_identity_and_its_resolver() {
        const PDF_ID: u64 = 4242;
        let object_ref = ObjectRef::new(13, 0);
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let handle = ObjectHandle::new_indirect_for_pdf_with_resolver(
            object_ref,
            NO_PARSED_OFFSET,
            PDF_ID,
            Rc::downgrade(&resolver),
        );

        // Identity: preserved, and specific to this document rather than
        // matching any id put to it.
        assert!(handle.belongs_to_pdf(PDF_ID));
        assert!(!handle.belongs_to_pdf(PDF_ID + 1));

        // Resolver: reachable through `try_dereference`'s real path — upgrade
        // the `Weak`, call `resolve_indirect` — not merely stored in the slot.
        // Without it this is the dropped-document error instead.
        handle.try_dereference().unwrap();
        assert!(handle.is_resolved());

        // Both at once. The child's owner stamp is written by `set_resolved`
        // out of the slot's own `pdf_unique_id`, so it can only carry this id
        // if the identity survived *into* the resolution the resolver drove.
        let child = handle.get_key(b"A");
        assert_eq!(child.as_integer(), Some(1));
        assert_eq!(
            child.containing_object_refs_for_pdf(PDF_ID),
            vec![object_ref]
        );
        assert!(child.containing_object_refs_for_pdf(PDF_ID + 1).is_empty());
    }

    #[test]
    fn resolver_bearing_indirect_slot_starts_without_a_parsed_offset() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(12, 0),
            Rc::downgrade(&resolver),
        );

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn missing_indirect_slot_resolves_in_place_to_null() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(MissingResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(9, 0),
            Rc::downgrade(&resolver),
        );

        assert!(handle.try_is_null().unwrap());
        assert!(handle.is_resolved());
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(9, 0)));
    }

    #[test]
    fn every_fallible_accessor_propagates_the_resolver_error() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(ErrorResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(10, 0),
            Rc::downgrade(&resolver),
        );

        assert_eq!(
            handle.try_is_null().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_as_dictionary().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_get_key(b"A").unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_has_key(b"A").unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_as_name().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_as_array().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_array_len().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_as_integer().unwrap_err().to_string(),
            "resolver failed"
        );
        assert!(!handle.is_resolved());
    }

    #[test]
    fn try_has_key_treats_a_present_null_value_as_absent() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);

        assert!(!dict.try_has_key(b"A").unwrap());
        assert!(!dict.try_has_key(b"Missing").unwrap());
    }

    #[test]
    fn fallible_dictionary_accessors_cover_resolved_and_non_dictionary_values() {
        let resolver = Rc::new(RecordingResolver::default());
        let erased: Rc<dyn DocumentResolver> = resolver;
        let dict =
            ObjectHandle::new_indirect_with_resolver(ObjectRef::new(11, 0), Rc::downgrade(&erased));

        let entries = dict
            .try_as_dictionary()
            .unwrap()
            .expect("recording resolver installs a dictionary");
        assert_eq!(entries.get(b"A".as_slice()).unwrap().as_integer(), Some(1));

        let scalar = ObjectHandle::integer(1);
        assert!(!scalar.try_has_key(b"A").unwrap());
    }

    #[test]
    fn try_as_name_resolves_an_indirect_name_through_its_document() {
        let (handle, _resolver) =
            resolver_bearing_handle(ObjectValue::Name(b"FlateDecode".to_vec()));

        // The non-resolving accessor cannot see through an unresolved handle,
        // and reports the same `None` it would for a wrong-typed value. Closing
        // that gap is the whole reason the `try_` form exists.
        assert_eq!(handle.as_name(), None);
        assert!(!handle.is_resolved());

        assert_eq!(handle.try_as_name().unwrap(), Some(b"FlateDecode".to_vec()));
        assert!(handle.is_resolved());
    }

    #[test]
    fn try_as_integer_resolves_an_indirect_integer_through_its_document() {
        let (handle, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));

        assert_eq!(handle.as_integer(), None);
        assert!(!handle.is_resolved());

        assert_eq!(handle.try_as_integer().unwrap(), Some(7));
        assert!(handle.is_resolved());
    }

    #[test]
    fn try_as_array_resolves_an_indirect_array_through_its_document() {
        let (handle, _resolver) =
            resolver_bearing_handle(ObjectValue::Array(vec![ObjectHandle::from_value(
                ObjectValue::Name(b"FlateDecode".to_vec()),
            )]));

        assert!(handle.as_array().is_none());
        assert!(!handle.is_resolved());

        let items = handle
            .try_as_array()
            .unwrap()
            .expect("recording resolver installs an array");
        // `ObjectHandle` equality is identity rather than value (see
        // `two_direct_handles_with_equal_value_are_distinct_identity`), so
        // inspect the child's value instead of comparing the `Vec`.
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].as_name(), Some(b"FlateDecode".to_vec()));
        assert!(handle.is_resolved());
    }

    #[test]
    fn try_array_len_counts_in_place_and_keeps_none_for_a_non_array() {
        // The count qpdf reads off the borrowed array
        // (`getArrayNItems` → `asArray()->size()`,
        // `libqpdf/QPDFObjectHandle.cc:758-768`), including the empty array
        // `QPDF_Stream::filterable` special-cases at
        // `libqpdf/QPDF_Stream.cc:443`.
        let array = ObjectHandle::array(vec![
            ObjectHandle::name(b"FlateDecode".to_vec()),
            ObjectHandle::name(b"ASCII85Decode".to_vec()),
        ]);
        assert_eq!(array.try_array_len().unwrap(), Some(2));
        // Counting must not consume or replace the value.
        assert_eq!(array.as_array().map(|items| items.len()), Some(2));

        assert_eq!(
            ObjectHandle::array(Vec::new()).try_array_len().unwrap(),
            Some(0)
        );

        // Deliberately *not* qpdf's non-array answer. `getArrayNItems` warns
        // `typeWarning("array", "treating as empty")` and returns 0
        // (`libqpdf/QPDFObjectHandle.cc:763-766`); returning `Some(0)` here
        // would make `stream_filter::decode_filter_specs_from_handle` read a
        // scalar `/Filter` as an empty chain — an accepted unfiltered stream —
        // instead of raising its type error.
        for non_array in [
            ObjectHandle::null(),
            ObjectHandle::integer(1),
            ObjectHandle::name(b"FlateDecode".to_vec()),
            ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]),
        ] {
            assert_eq!(non_array.try_array_len().unwrap(), None);
        }
    }

    #[test]
    fn try_array_len_resolves_an_indirect_array_through_its_document() {
        let (handle, _resolver) =
            resolver_bearing_handle(ObjectValue::Array(vec![ObjectHandle::null()]));

        // Nothing non-resolving can see through an unresolved handle, so
        // dropping `try_dereference` from `try_array_len` reports this array
        // as "not an array" — the mutation the `stream_filter` call sites
        // cannot kill on their own, because a preceding `try_*` has already
        // resolved the slot by the time they count it.
        assert!(handle.as_array().is_none());
        assert!(!handle.is_resolved());

        assert_eq!(handle.try_array_len().unwrap(), Some(1));
        assert!(handle.is_resolved());
    }

    #[test]
    fn every_value_accessor_reports_a_dropped_document_rather_than_none() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(21, 0),
            Rc::downgrade(&resolver),
        );
        drop(resolver);

        // A `None` here would be indistinguishable from a resolved value of
        // the wrong type, so each accessor must surface the dropped document.
        for error in [
            handle.try_as_name().unwrap_err(),
            handle.try_as_array().unwrap_err(),
            handle.try_array_len().unwrap_err(),
            handle.try_as_integer().unwrap_err(),
        ] {
            assert_eq!(error.to_string(), "object 21 0 belongs to a dropped PDF");
        }
        assert!(!handle.is_resolved());
    }

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
    fn stream_handle_round_trips_its_dict_and_data() {
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(3))]);
        let stream = ObjectHandle::stream(dict.clone(), b"abc".to_vec());
        assert!(stream.as_stream_dict().expect("stream dict").ptr_eq(&dict));
        assert_eq!(stream.as_stream_data(), Some(b"abc".to_vec()));
        assert_eq!(stream.type_code(), 10, "ot_stream");
    }

    #[test]
    fn materialize_is_now_a_public_bridge_usable_outside_this_crate() {
        // `materialize` was widened from `pub(crate)` to `pub` so
        // `flpdf-qtest-tools`' qtest driver can bridge one `ObjectHandle`
        // value (a stream's own dictionary handle) back to a legacy
        // `Object`/`Dictionary` for its still-`&Dictionary`-shaped
        // filter/`DecodeParms` resolution routine — see this method's own
        // doc. `object_value_tests` above already exercises its per-variant
        // behavior exhaustively; this only pins the visibility contract.
        let handle = ObjectHandle::integer(1);
        let _: Object = ObjectHandle::materialize(&handle);
    }

    #[test]
    fn materialize_caps_a_direct_tree_deeper_than_any_parseable_document() {
        // Codex Review on PR #610 reproduced a process-aborting stack
        // overflow by building a 100,000-level-deep direct array through
        // the public `ObjectHandle::array` factory (which imposes no depth
        // bound the way parsed input does), calling the newly-public
        // `materialize` on it, then letting both the input handle and the
        // materialized result drop normally. `materialize` now caps its own
        // recursion at `parser::MAX_PARSE_DEPTH`, substituting `Object::Null`
        // past that point -- verified below to actually take effect, not
        // merely fail to crash by luck.
        //
        // `std::mem::forget(handle)` isolates what this fix is actually
        // responsible for: dropping the *input* `handle` here, built the
        // same way, independently overflows the stack even with no call to
        // `materialize` at all (confirmed while narrowing this down) --
        // `ObjectHandle`'s own recursive `Drop` is unprotected the same way
        // `Object`'s is, and was already reachable this way before this PR
        // (`array`/`dictionary` were already public). That is a real,
        // separate, pre-existing gap this fix does not and cannot close
        // from inside `materialize` -- forgetting `handle` here keeps this
        // test scoped to materialize's own contribution rather than
        // silently also depending on a fix for the unrelated one.
        let mut handle = ObjectHandle::integer(1);
        for _ in 0..100_000 {
            handle = ObjectHandle::array(vec![handle]);
        }

        let materialized = handle.materialize();
        std::mem::forget(handle);

        let mut cursor = &materialized;
        let mut depth = 0;
        loop {
            match cursor {
                Object::Array(items) if items.len() == 1 => {
                    cursor = &items[0];
                    depth += 1;
                }
                other => {
                    assert_eq!(
                        *other,
                        Object::Null,
                        "the cap must substitute null once nesting exceeds MAX_PARSE_DEPTH"
                    );
                    break;
                }
            }
        }
        assert_eq!(
            depth,
            crate::parser::MAX_PARSE_DEPTH + 1,
            "materialize should recurse exactly through the depth cap before substituting null"
        );
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
    fn disconnect_resets_a_previously_recorded_parsed_offset() {
        // Mirrors `set_missing_resets_a_previously_recorded_parsed_offset`
        // for the same Parsed-Offset Contract clause: a handle a caller
        // keeps alive past its owning `Pdf`'s drop must not keep reporting
        // its former body's source position once it reads as null.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        handle.set_parsed_offset_if_unset(100);
        assert_eq!(handle.get_parsed_offset(), 100);

        handle.disconnect();

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
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

#[cfg(test)]
mod rounded_accessor_tests {
    use super::*;

    #[test]
    fn boolean_handle_round_trips_its_value() {
        assert_eq!(ObjectHandle::boolean(true).as_boolean(), Some(true));
        assert_eq!(ObjectHandle::boolean(false).as_boolean(), Some(false));
        assert_eq!(ObjectHandle::integer(1).as_boolean(), None);
    }

    #[test]
    fn as_real_accepts_both_real_and_real_literal_like_object_does() {
        // Mirrors Object::as_real's own `Real(v) | RealLiteral { value: v, .. }`
        // arm (object.rs:348-353) — a real-literal value is still "a real"
        // for callers that don't care about the source spelling.
        assert_eq!(ObjectHandle::real(1.5).as_real(), Some(1.5));
        assert_eq!(
            ObjectHandle::real_literal(0.4, b".4".to_vec()).as_real(),
            Some(0.4)
        );
        assert_eq!(ObjectHandle::integer(1).as_real(), None);
    }

    #[test]
    fn name_and_string_handles_round_trip_their_bytes() {
        assert_eq!(
            ObjectHandle::name(b"Type".to_vec()).as_name(),
            Some(b"Type".to_vec())
        );
        assert_eq!(
            ObjectHandle::string(b"hi".to_vec()).as_string(),
            Some(b"hi".to_vec())
        );
        assert!(ObjectHandle::name(b"Type".to_vec()).as_string().is_none());
        assert!(ObjectHandle::string(b"hi".to_vec()).as_name().is_none());
    }

    #[test]
    fn as_reference_reads_a_resolved_indirect_redirect_but_not_a_plain_value() {
        // ObjectValue::Reference is what an indirect handle resolves to when
        // its own body is itself a bare reference (Pdf::set_object-driven
        // redirect/collapse chains — see ObjectValue::Reference's own doc).
        let redirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        redirect.set_resolved(ObjectValue::Reference(ObjectRef::new(9, 0)));
        assert_eq!(redirect.as_reference(), Some(ObjectRef::new(9, 0)));
        assert_eq!(ObjectHandle::integer(1).as_reference(), None);
    }

    #[test]
    fn rounded_accessors_return_none_for_an_indirect_handle_before_resolution() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        assert_eq!(handle.as_boolean(), None);
        assert_eq!(handle.as_real(), None);
        assert!(handle.as_name().is_none());
        assert!(handle.as_string().is_none());
        assert_eq!(handle.as_reference(), None);
    }
}

#[cfg(test)]
mod token_value_tests {
    use super::*;

    #[test]
    fn operator_handle_round_trips_its_bytes() {
        let handle = ObjectHandle::operator(b"q".to_vec());
        assert_eq!(handle.as_operator(), Some(b"q".to_vec()));
        assert!(handle.as_inline_image().is_none());
    }

    #[test]
    fn inline_image_handle_round_trips_its_bytes() {
        let handle = ObjectHandle::inline_image(b"\x00\x01raw".to_vec());
        assert_eq!(handle.as_inline_image(), Some(b"\x00\x01raw".to_vec()));
        assert!(handle.as_operator().is_none());
    }

    #[test]
    fn operator_and_inline_image_materialize_to_the_matching_object_variant() {
        assert_eq!(
            ObjectHandle::operator(b"Do".to_vec()).materialize(),
            Object::Operator(b"Do".to_vec())
        );
        assert_eq!(
            ObjectHandle::inline_image(b"data".to_vec()).materialize(),
            Object::InlineImage(b"data".to_vec())
        );
    }
}

#[cfg(test)]
mod is_resolved_visibility_tests {
    use super::*;

    #[test]
    fn is_resolved_is_usable_the_same_way_a_pub_fn_is() {
        // This test doesn't exercise new behavior (resolution_state_tests
        // already covers is_resolved's semantics exhaustively) — it exists
        // only to keep a compile-time witness that `is_resolved` stays
        // `pub`, the same way the rest of this module's public surface has
        // a direct caller in-tree. Real external verification happens in
        // Task 7 (zero-consumer-diff gate does not apply to this file
        // itself, so a positive compile check here is the useful signal).
        let handle = ObjectHandle::integer(1);
        let _: bool = ObjectHandle::is_resolved(&handle);
    }
}

#[cfg(test)]
mod type_code_tests {
    use super::*;

    #[test]
    fn direct_scalar_and_container_type_codes_match_qpdf_ordinals() {
        // Ordinals and strings verified directly against the pinned qpdf
        // 11.9.0 source: `include/qpdf/Constants.h:108-127`
        // (`qpdf_object_type_e`) for the numbers, and each type's own
        // `libqpdf/QPDF_*.cc` `QPDFValue(::ot_*, "...")` constructor for the
        // name string (e.g. `libqpdf/QPDF_InlineImage.cc:6` for the
        // hyphenated `"inline-image"`).
        let cases: &[(ObjectHandle, u8, &str)] = &[
            (ObjectHandle::null(), 2, "null"),
            (ObjectHandle::boolean(true), 3, "boolean"),
            (ObjectHandle::integer(1), 4, "integer"),
            (ObjectHandle::real(1.5), 5, "real"),
            (ObjectHandle::real_literal(0.4, b".4".to_vec()), 5, "real"),
            (ObjectHandle::string(b"s".to_vec()), 6, "string"),
            (ObjectHandle::name(b"N".to_vec()), 7, "name"),
            (ObjectHandle::array(vec![]), 8, "array"),
            (ObjectHandle::dictionary(vec![]), 9, "dictionary"),
            (ObjectHandle::operator(b"q".to_vec()), 11, "operator"),
            (
                ObjectHandle::inline_image(b"d".to_vec()),
                12,
                "inline-image",
            ),
        ];
        for (handle, code, name) in cases {
            assert_eq!(handle.type_code(), *code, "{name}");
            assert_eq!(handle.type_name(), *name);
        }
    }

    #[test]
    fn stream_handle_type_code_is_stream() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            dict,
            data: Vec::new(),
        });
        assert_eq!(stream.type_code(), 10);
        assert_eq!(stream.type_name(), "stream");
    }

    #[test]
    fn not_yet_resolved_indirect_handle_reports_unresolved_without_resolving() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert_eq!(handle.type_code(), 13, "ot_unresolved");
        assert_eq!(handle.type_name(), "unresolved");
    }

    #[test]
    fn destroyed_indirect_handle_reports_destroyed() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(1));
        handle.disconnect();
        assert_eq!(handle.type_code(), 14, "ot_destroyed");
        assert_eq!(handle.type_name(), "destroyed");
    }

    #[test]
    fn missing_indirect_handle_reports_null_not_a_distinct_missing_code() {
        // qpdf has no separate "missing" ot_* code — a dangling/broken
        // reference presents as ot_null, matching set_missing's own
        // documented is_null()==true contract.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_missing();
        assert_eq!(handle.type_code(), 2, "ot_null");
        assert_eq!(handle.type_name(), "null");
    }

    #[test]
    fn resolved_indirect_handle_reports_its_real_value_type() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        assert_eq!(handle.type_code(), 4, "ot_integer");
        assert_eq!(handle.type_name(), "integer");
    }

    #[test]
    fn resolved_to_a_reference_indirect_handle_reports_unresolved() {
        // `ObjectValue::Reference` is a real, reachable resolution state,
        // not a speculative one: `Pdf::set_object` (`reader.rs:1184-1239`)
        // is public, `Object::Reference` is a public variant, and
        // `set_object` writes exactly this state via
        // `handle.set_resolved(value)` (`reader.rs:1207-1210`) whenever the
        // lifted value is itself a bare reference
        // (`reader.rs:1877`'s `Object::Reference(object_ref) =>
        // ObjectValue::Reference(*object_ref)` arm) — e.g.
        // `pdf.set_object(holder, Object::Reference(target))` to redirect a
        // holder chain in place, exactly as `ref_chain.rs`'s own test
        // fixture does. `resolve_object_handle` itself can never produce
        // this state (a top-level bare reference never comes from a
        // file/ObjStm parse — `parser.rs`'s `top_level_no_reference`
        // integerizes it instead, matching qpdf — and `set_object` always
        // resolves the same canonical handle it writes into the legacy
        // cache, so `resolve_object_handle`'s own `is_resolved` early-return
        // guards against ever re-deriving this value itself), but the state
        // is still reachable from any public accessor call on the handle
        // `Pdf::set_object` resolved directly. This test calls the same
        // `set_resolved` method `set_object` itself calls, to exercise the
        // state without pulling `Pdf` into this single-file slice.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Reference(ObjectRef::new(9, 0)));
        assert_eq!(handle.type_code(), 13, "ot_unresolved");
        assert_eq!(handle.type_name(), "unresolved");
        // The contradiction this method's own doc calls out: the value
        // itself is known (is_resolved() is true) even though its type
        // code reports the same ordinal as a handle whose value is not
        // known at all.
        assert!(handle.is_resolved());
    }
}

#[cfg(test)]
mod unparse_tests {
    use super::*;

    #[test]
    fn unparse_resolved_covers_every_teardown_arm_for_a_nested_array_dict_and_stream() {
        // Codex Review on PR #603 (discussion_r3689896128) found that the
        // recursive materialization walk backing unparse/unparse_resolved
        // had no protection against a deeply nested tree's per-frame stack
        // cost. Two recursion points were fixed: construction
        // (unparse_materialize, wrapped in stacker::maybe_grow) and the
        // resulting materialized `Object` tree's own teardown right after
        // (unparse_drop_iteratively, an explicit-stack walk replacing
        // Object's ordinary recursive Drop).
        //
        // This test intentionally does NOT probe an extreme depth. A
        // depth large enough to discriminate "protected" from
        // "unprotected" on every CI runner turned out not to exist: a
        // depth safe on this author's local machine (4,000) still
        // stack-overflowed on macOS/ARM/Windows CI runners, because
        // `Object::write_pdf` -- called on the materialized tree between
        // the two now-protected walks -- is *itself* an unprotected
        // recursive serializer living in object.rs, outside this slice's
        // file allowlist (only object_handle.rs may change). Fixing that
        // third recursion point is out of scope here; it is tracked
        // together with the other pre-existing unprotected-recursion
        // concerns in flpdf-egzr.3.5. Until it lands, no caller of
        // unparse_resolved has arbitrary-depth safety, so this test
        // stays at a shallow, portable depth and exercises every
        // container arm (Array, Dictionary, Stream) that
        // unparse_drop_iteratively and drain_dictionary_onto handle,
        // rather than chasing a depth number.
        let stream = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        stream.set_resolved(ObjectValue::Stream {
            dict: ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(0))]),
            data: Vec::new(),
        });
        let inner_dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::null()),
            (b"B".to_vec(), ObjectHandle::integer(2)),
        ]);
        let array = ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            inner_dict,
            stream,
            ObjectHandle::array(vec![ObjectHandle::name(b"Nested".to_vec())]),
        ]);

        assert_eq!(
            array.unparse_resolved(),
            b"[ 1 << /B 2 >> 9 0 R [ /Nested ] ]"
        );
    }

    #[test]
    fn direct_scalar_unparses_like_object_write_pdf() {
        assert_eq!(ObjectHandle::integer(7).unparse(), b"7");
        assert_eq!(ObjectHandle::boolean(true).unparse(), b"true");
        assert_eq!(ObjectHandle::name(b"Type".to_vec()).unparse(), b"/Type");
    }

    #[test]
    fn indirect_handle_unparse_is_always_the_reference_form_even_before_resolution() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        assert_eq!(handle.unparse(), b"7 2 R");
    }

    #[test]
    fn indirect_handle_unparse_resolved_falls_back_to_null_before_resolution() {
        // No hidden I/O: an unresolved indirect handle's value is not
        // known, so unparse_resolved reports the same as materialize()'s
        // own documented null fallback rather than triggering resolution.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        assert_eq!(handle.unparse_resolved(), b"null");
    }

    #[test]
    fn resolved_indirect_handle_unparse_resolved_shows_the_real_value() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        handle.set_resolved(ObjectValue::Integer(42));
        assert_eq!(handle.unparse(), b"7 2 R");
        assert_eq!(handle.unparse_resolved(), b"42");
    }

    #[test]
    fn stream_value_unparse_resolved_still_reports_the_reference_form() {
        // QPDF_Stream::unparse() (libqpdf/QPDF_Stream.cc:173-178) always
        // returns its own "N G R" — mirrored here rather than inlining the
        // stream's dictionary/data.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(0))]);
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        handle.set_resolved(ObjectValue::Stream {
            dict,
            data: Vec::new(),
        });
        assert_eq!(handle.unparse(), b"9 0 R");
        assert_eq!(handle.unparse_resolved(), b"9 0 R");
    }

    #[test]
    fn direct_array_unparse_writes_indirect_children_as_references_not_recursed() {
        let child = ObjectHandle::new_indirect_unresolved(ObjectRef::new(5, 0), 0);
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), child]);
        assert_eq!(array.unparse(), b"[ 1 5 0 R ]");
    }

    #[test]
    fn a_direct_stream_value_unparse_resolved_inlines_rather_than_referencing() {
        // A *direct* Stream `ObjectValue` is not the common case (no public
        // `ObjectHandle::stream(..)` factory exists; production reader code
        // installs real stream values via `set_resolved` on an indirect
        // handle), but it IS reachable through the public API: a nested
        // `Object::Stream` passed to `Pdf::set_object` (e.g. inside an
        // `Object::Array`) is lifted via `reader.rs`'s `lift_bounded`'s
        // direct-value arm into `ObjectHandle::from_value`, producing
        // exactly this shape. Real qpdf has no equivalent state -- a stream
        // is only ever a *newly allocated indirect* `QPDFObjectHandle`
        // (`QPDFObjectHandle::newStream`) -- so `QPDF_Stream::unparse()`'s
        // reference-form guarantee has nothing to say about this case, and
        // there is no qpdf byte-parity oracle to match here. This test
        // pins down that the fallback path (materialize + `Object::write_pdf`)
        // handles this shape by inlining the dictionary and data the same
        // way `Object::write_pdf` already does for `Object::Stream`, rather
        // than fabricating a meaningless reference for a value that was
        // never assigned an object number/generation.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            dict,
            data: b"ab".to_vec(),
        });
        assert_eq!(
            handle.unparse_resolved(),
            b"<< /Length 2 >>\nstream\nab\nendstream"
        );
    }

    #[test]
    fn destroyed_indirect_handle_unparse_is_unaffected_but_unparse_resolved_falls_back_to_null() {
        // qpdf's own `unparse()` never dereferences an indirect handle at
        // all (`libqpdf/QPDFObjectHandle.cc:1574-1584`: the isIndirect()
        // branch returns "N G R" directly without calling
        // `unparseResolved()`), so a destroyed handle's `unparse()` does
        // not throw either -- no divergence there. `unparseResolved()`
        // does dereference, though, and qpdf's `QPDF_Destroyed::unparse()`
        // (`libqpdf/QPDF_Destroyed.cc:24-29`) throws `std::logic_error`
        // once it gets there. This method has no exception channel to
        // mirror that with, so -- as documented on `unparse_resolved`
        // itself -- it presents the same `null` fallback this file's other
        // value accessors give a destroyed handle, rather than panicking.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        handle.disconnect();

        assert_eq!(handle.unparse(), b"1 0 R");
        assert_eq!(handle.unparse_resolved(), b"null");
    }

    #[test]
    fn resolved_to_a_reference_indirect_handle_unparse_and_unparse_resolved_diverge() {
        // Mirrors `type_code_tests::resolved_to_a_reference_indirect_handle_reports_unresolved`'s
        // own state: a handle `Pdf::set_object` redirected in place to
        // another object (see `ObjectValue::Reference`'s own doc).
        // `unparse()` never dereferences an indirect handle (see this
        // module's `destroyed_...` test above), so it reports the
        // redirecting handle's own "N G R" -- not the target's.
        // `unparse_resolved()` does read the resolved value, which is
        // itself a bare reference, so it reports the *target's* "N G R"
        // instead of chasing to the target's own concrete value (e.g.
        // `42`). This is a real gap, not a documented design choice: this
        // crate's own `flpdf-qtest-tools::driver::Handle` already has an
        // established, tested contract for exactly this redirect scenario
        // (`reference_chain_resolves_but_unparse_retains_the_first_reference`,
        // `driver/handle.rs:678-696`) where the equivalent accessor *does*
        // chase to the target's terminal value while `unparse()` keeps
        // reporting the first reference's own identity -- this method does
        // not yet replicate that. Chasing needs `Pdf` (`ObjectValue::Reference`
        // stores only a bare `ObjectRef`, not a handle link), so it cannot be
        // implemented from this file alone; tracked as flpdf-l3kz, and wired
        // as a hard dependency of flpdf-egzr.3.2.3 (the slice that migrates
        // `driver::Handle` itself onto this API and must not regress that
        // test).
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Reference(ObjectRef::new(9, 0)));

        assert_eq!(handle.unparse(), b"1 0 R");
        assert_eq!(handle.unparse_resolved(), b"9 0 R");
    }

    #[test]
    fn unparse_resolved_omits_a_direct_null_dictionary_entry() {
        // qpdf's QPDF_Dictionary::unparse() (libqpdf/QPDF_Dictionary.cc:59-69)
        // skips any entry whose value isNull(), matching the PDF-spec
        // equivalence between an explicit null value and a missing key.
        let dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::null()),
            (b"B".to_vec(), ObjectHandle::integer(1)),
        ]);
        assert_eq!(dict.unparse_resolved(), b"<< /B 1 >>");
    }

    #[test]
    fn unparse_resolved_omits_an_already_resolved_null_indirect_dictionary_entry() {
        // The same qpdf rule applies to an indirect child, since qpdf's
        // isNull() dereferences before checking -- but only when this
        // child's value is already known (is_resolved()), never by forcing
        // new resolution (see the "keeps a not-yet-resolved" test below).
        let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        missing.set_missing();
        let dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), missing),
            (b"B".to_vec(), ObjectHandle::integer(1)),
        ]);
        assert_eq!(dict.unparse_resolved(), b"<< /B 1 >>");
    }

    #[test]
    fn unparse_resolved_keeps_a_not_yet_resolved_indirect_dictionary_entry() {
        // Divergence from qpdf, which would resolve the child to check its
        // nullness (QPDFObjectHandle::isNull() dereferences,
        // libqpdf/QPDFObjectHandle.cc:353-356); this port never performs
        // hidden resolution (see unparse_resolved's own doc), so an entry
        // whose nullness is not yet known is conservatively kept rather
        // than guessed away.
        let unresolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), unresolved)]);
        assert_eq!(dict.unparse_resolved(), b"<< /A 9 0 R >>");
    }

    #[test]
    fn unparse_resolved_omits_nulls_in_a_nested_dictionary_inside_an_array() {
        let inner = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);
        let array = ObjectHandle::array(vec![inner]);
        assert_eq!(array.unparse_resolved(), b"[ << >> ]");
    }

    #[test]
    fn unparse_resolved_does_not_omit_null_array_elements() {
        // Only dictionary keys are omitted for a null value; array elements
        // keep their position (QPDF_Array::unparse(),
        // libqpdf/QPDF_Array.cc:123-140, explicitly fills gaps with the
        // literal "null" token rather than skipping them).
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::null()]);
        assert_eq!(array.unparse_resolved(), b"[ 1 null ]");
    }
}

#[cfg(test)]
mod mutation_tests {
    use super::*;

    #[test]
    fn object_value_clone_preserves_scalar_content() {
        let value = ObjectValue::Integer(42);
        let cloned = value.clone();
        assert!(matches!(cloned, ObjectValue::Integer(42)));
    }

    #[test]
    fn object_value_clone_of_a_dictionary_shares_child_identity() {
        let child = ObjectHandle::integer(7);
        let dict = ObjectValue::Dictionary([(b"K".to_vec(), child.clone())].into_iter().collect());
        let cloned = dict.clone();
        let ObjectValue::Dictionary(entries) = cloned else {
            panic!("expected dictionary"); // cov:ignore: unreachable in a passing run
        };
        assert!(entries.get(b"K".as_slice()).unwrap().ptr_eq(&child));
    }

    #[test]
    fn get_key_returns_a_live_child_handle_without_snapshotting_the_dictionary() {
        let child = ObjectHandle::integer(1);
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), child.clone())]);
        let fetched = dict.get_key(b"A");
        assert!(fetched.ptr_eq(&child));
    }

    #[test]
    fn get_key_on_a_missing_key_returns_a_direct_null_handle() {
        let dict = ObjectHandle::dictionary(vec![]);
        assert!(dict.get_key(b"Missing").is_null());
    }

    #[test]
    fn get_key_on_a_non_dictionary_handle_returns_a_direct_null_handle() {
        let scalar = ObjectHandle::integer(5);
        assert!(scalar.get_key(b"A").is_null());
    }

    #[test]
    fn replace_key_mutates_the_live_dictionary_in_place() {
        let dict = ObjectHandle::dictionary(vec![]);
        let clone = dict.clone();
        dict.replace_key(b"A", ObjectHandle::integer(9));
        assert_eq!(clone.get_key(b"A").as_integer(), Some(9));
    }

    #[test]
    fn replace_key_overwrites_an_existing_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        dict.replace_key(b"A", ObjectHandle::integer(2));
        assert_eq!(dict.get_key(b"A").as_integer(), Some(2));
    }

    #[test]
    fn replace_key_on_a_non_dictionary_handle_is_a_no_op() {
        let scalar = ObjectHandle::integer(1);
        scalar.replace_key(b"A", ObjectHandle::integer(2));
        assert_eq!(scalar.as_integer(), Some(1));
    }

    #[test]
    fn replace_array_item_preserves_identity_and_rejects_invalid_slots() {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let replacement = ObjectHandle::dictionary(vec![]);
        let retained = replacement.clone();

        assert!(array.replace_array_item(0, replacement));
        retained.replace_key(b"K", ObjectHandle::integer(9));
        let inserted = array.as_array().expect("array")[0].clone();
        assert_eq!(inserted.get_key(b"K").as_integer(), Some(9));

        assert!(!array.replace_array_item(1, ObjectHandle::integer(2)));
        assert!(!ObjectHandle::integer(1).replace_array_item(0, ObjectHandle::integer(2)));
        assert!(!array.replace_array_item(0, array.clone()));
    }

    #[test]
    fn replace_key_rejects_inserting_a_direct_dictionary_into_itself() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let self_clone = dict.clone();
        dict.replace_key(b"Self", self_clone);
        assert!(dict.get_key(b"Self").is_null());
        // The rest of the dictionary is untouched by the rejected insert.
        assert_eq!(dict.get_key(b"A").as_integer(), Some(1));
    }

    #[test]
    fn replace_key_allows_an_indirect_handle_to_reference_itself() {
        // Unlike a direct self-insertion, an indirect handle referencing
        // itself is not a direct cycle -- every recursive walker already
        // stops at the indirect boundary, so this must remain a normal
        // insert rather than being rejected as a no-op.
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1);
        indirect.set_resolved(ObjectValue::Dictionary(Default::default()));
        indirect.replace_key(b"Self", indirect.clone());
        assert!(indirect.get_key(b"Self").is_indirect());
    }

    #[test]
    fn remove_key_deletes_a_present_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        dict.remove_key(b"A");
        assert!(dict.get_key(b"A").is_null());
    }

    #[test]
    fn remove_key_on_a_missing_key_is_a_no_op() {
        let dict = ObjectHandle::dictionary(vec![]);
        dict.remove_key(b"Missing");
        assert!(dict.get_key(b"Missing").is_null());
    }

    #[test]
    fn remove_key_on_a_non_dictionary_handle_is_a_no_op() {
        let scalar = ObjectHandle::integer(1);
        scalar.remove_key(b"A");
        assert_eq!(scalar.as_integer(), Some(1));
    }

    #[test]
    fn shallow_copy_is_always_direct_even_from_an_indirect_source() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        indirect.set_resolved(ObjectValue::Dictionary(Default::default()));
        let copy = indirect.shallow_copy();
        assert!(copy.is_direct());
    }

    #[test]
    fn shallow_copy_mutation_does_not_affect_the_source() {
        let original = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let copy = original.shallow_copy();
        copy.replace_key(b"A", ObjectHandle::integer(2));
        assert_eq!(original.get_key(b"A").as_integer(), Some(1));
        assert_eq!(copy.get_key(b"A").as_integer(), Some(2));
    }

    // Despite the name, qpdf's shallowCopy() recursively copies through
    // every *direct* array/dictionary descendant, stopping only at an
    // *indirect* child (kept shared) -- see shallow_copy's own doc comment
    // for the qpdf citations. These two tests pin that distinction.

    #[test]
    fn shallow_copy_of_a_direct_dictionary_child_produces_an_independent_copy() {
        let original = ObjectHandle::dictionary(vec![(
            b"A".to_vec(),
            ObjectHandle::dictionary(vec![(b"Inner".to_vec(), ObjectHandle::integer(1))]),
        )]);
        let copy = original.shallow_copy();
        copy.get_key(b"A")
            .replace_key(b"Inner", ObjectHandle::integer(2));
        assert_eq!(
            original.get_key(b"A").get_key(b"Inner").as_integer(),
            Some(1)
        );
        assert_eq!(copy.get_key(b"A").get_key(b"Inner").as_integer(), Some(2));
    }

    #[test]
    fn shallow_copy_of_an_indirect_dictionary_child_keeps_shared_identity() {
        let child = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        child.set_resolved(ObjectValue::Integer(1));
        let original = ObjectHandle::dictionary(vec![(b"A".to_vec(), child.clone())]);
        let copy = original.shallow_copy();
        assert!(copy.get_key(b"A").ptr_eq(&child));
    }

    #[test]
    fn shallow_copy_of_a_non_container_clones_the_scalar_value() {
        let original = ObjectHandle::integer(5);
        let copy = original.shallow_copy();
        assert!(!copy.ptr_eq(&original));
        assert_eq!(copy.as_integer(), Some(5));
    }

    #[test]
    fn has_key_distinguishes_a_present_null_value_from_a_missing_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);
        assert!(dict.has_key(b"A"));
        assert!(!dict.has_key(b"Missing"));
    }

    #[test]
    fn has_key_on_a_non_dictionary_handle_is_false() {
        let scalar = ObjectHandle::integer(1);
        assert!(!scalar.has_key(b"A"));
    }

    #[test]
    fn merge_resources_is_a_no_op_unless_both_sides_are_dictionaries() {
        let scalar = ObjectHandle::integer(1);
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        scalar.merge_resources(&dict, None);
        dict.merge_resources(&scalar, None);
        assert_eq!(dict.get_key(b"A").as_integer(), Some(1));
        assert!(dict.get_key(b"B").is_null());
    }

    #[test]
    fn merge_resources_installs_a_private_copy_of_a_top_level_key_self_lacks() {
        let source_sub = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(1))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), source_sub.clone())]);
        let dest = ObjectHandle::dictionary(vec![]);
        dest.merge_resources(&other, None);
        let installed = dest.get_key(b"Font");
        assert_eq!(installed.get_key(b"F1").as_integer(), Some(1));
        assert!(!installed.ptr_eq(&source_sub)); // privatized, not shared
    }

    #[test]
    fn merge_resources_adds_a_new_inner_key_without_a_conflicts_map() {
        let this_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(1))]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F2".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        dest.merge_resources(&other, None);
        let font = dest.get_key(b"Font");
        assert_eq!(font.get_key(b"F1").as_integer(), Some(1));
        assert_eq!(font.get_key(b"F2").as_integer(), Some(2));
    }

    #[test]
    fn merge_resources_leaves_a_colliding_inner_key_untouched_without_a_conflicts_map() {
        let this_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(1))]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font =
            ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(99))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        dest.merge_resources(&other, None);
        assert_eq!(dest.get_key(b"Font").get_key(b"F1").as_integer(), Some(1));
    }

    #[test]
    fn merge_resources_reuses_an_existing_key_for_the_same_indirect_object() {
        let shared = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        shared.set_resolved(ObjectValue::Name(b"Shared".to_vec()));
        let this_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), shared.clone())]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        // dest already has F1 -> shared. other also wants F1, but pointing at
        // the same shared object identity -- reuse F1 verbatim, no conflict
        // entry (existing_key == key), no new key minted.
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), shared.clone())]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();
        dest.merge_resources(&other, Some(&mut conflicts));
        assert!(conflicts.is_empty());
        assert!(dest.get_key(b"Font").get_key(b"F1").ptr_eq(&shared));
    }

    #[test]
    fn merge_resources_reuse_records_a_conflict_when_the_reused_name_differs() {
        let shared = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        shared.set_resolved(ObjectValue::Name(b"Shared".to_vec()));
        // dest already has this same shared object, but under a DIFFERENT
        // key (F2) than what other asks for (F1) -- reuse F2, and DO record
        // the rename since the reused name differs from the requested one.
        let this_font = ObjectHandle::dictionary(vec![
            (b"F2".to_vec(), shared.clone()),
            (b"F1".to_vec(), ObjectHandle::integer(1)),
        ]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), shared.clone())]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();
        dest.merge_resources(&other, Some(&mut conflicts));
        assert_eq!(
            conflicts
                .get(b"Font".as_slice())
                .unwrap()
                .get(b"F1".as_slice()),
            Some(&b"F2".to_vec())
        );
        // F1 keeps its own original (unrelated) value; nothing overwrote it.
        assert_eq!(dest.get_key(b"Font").get_key(b"F1").as_integer(), Some(1));
    }

    #[test]
    fn merge_resources_mints_a_fresh_name_for_a_genuine_conflict() {
        let this_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(1))]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();
        dest.merge_resources(&other, Some(&mut conflicts));
        let new_name = conflicts
            .get(b"Font".as_slice())
            .and_then(|m| m.get(b"F1".as_slice()))
            .expect("F1 conflict recorded");
        assert_eq!(new_name, b"F1_1");
        assert_eq!(dest.get_key(b"Font").get_key(b"F1").as_integer(), Some(1));
        assert_eq!(
            dest.get_key(b"Font").get_key(new_name).as_integer(),
            Some(2)
        );
    }

    #[test]
    fn merge_resources_privatizes_an_indirect_existing_sub_dictionary() {
        let indirect_font = ObjectHandle::new_indirect_unresolved(ObjectRef::new(3, 0), -1);
        indirect_font.set_resolved(ObjectValue::Dictionary(
            [(b"F1".to_vec(), ObjectHandle::integer(1))]
                .into_iter()
                .collect(),
        ));
        let shared_dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), indirect_font.clone())]);
        let another_holder =
            ObjectHandle::dictionary(vec![(b"Font".to_vec(), indirect_font.clone())]);
        let other_font = ObjectHandle::dictionary(vec![(b"F2".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        shared_dest.merge_resources(&other, None);
        // shared_dest's own /Font is now a private direct copy...
        assert!(shared_dest.get_key(b"Font").is_direct());
        assert_eq!(
            shared_dest.get_key(b"Font").get_key(b"F2").as_integer(),
            Some(2)
        );
        // ...and the other holder's /Font (and the original indirect object)
        // is untouched.
        assert!(another_holder.get_key(b"Font").ptr_eq(&indirect_font));
        assert!(indirect_font.get_key(b"F2").is_null());
    }

    #[test]
    fn merge_resources_unions_scalar_array_items_by_unparsed_text() {
        let dest = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
        )]);
        let other = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"PDF".to_vec()),
                ObjectHandle::name(b"Text".to_vec()),
            ]),
        )]);
        dest.merge_resources(&other, None);
        let items = dest.get_key(b"ProcSet").as_array().unwrap();
        let names: Vec<_> = items.iter().map(|i| i.as_name().unwrap()).collect();
        assert_eq!(names, vec![b"PDF".to_vec(), b"Text".to_vec()]);
    }

    #[test]
    fn merge_resources_leaves_mismatched_or_non_container_rtype_shapes_untouched() {
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), ObjectHandle::integer(1))]);
        let other = ObjectHandle::dictionary(vec![(
            b"Font".to_vec(),
            ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(2))]),
        )]);
        dest.merge_resources(&other, None);
        assert_eq!(dest.get_key(b"Font").as_integer(), Some(1));
    }

    #[test]
    fn replace_stream_data_updates_data_and_length() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            dict: dict.clone(),
            data: b"old".to_vec(),
        });
        stream.replace_stream_data(b"new data".to_vec(), None, None);
        assert_eq!(stream.as_stream_data(), Some(b"new data".to_vec()));
        assert_eq!(dict.get_key(b"Length").as_integer(), Some(8));
    }

    #[test]
    fn replace_stream_data_sets_filter_and_decode_parms_when_given() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            dict: dict.clone(),
            data: b"old".to_vec(),
        });
        let filter = ObjectHandle::name(b"FlateDecode".to_vec());
        let parms =
            ObjectHandle::dictionary(vec![(b"Predictor".to_vec(), ObjectHandle::integer(12))]);
        stream.replace_stream_data(b"x".to_vec(), Some(filter.clone()), Some(parms.clone()));
        assert!(dict.get_key(b"Filter").ptr_eq(&filter));
        assert!(dict.get_key(b"DecodeParms").ptr_eq(&parms));
    }

    #[test]
    fn replace_stream_data_leaves_filter_untouched_when_not_given() {
        let dict = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            dict: dict.clone(),
            data: b"old".to_vec(),
        });
        stream.replace_stream_data(b"new".to_vec(), None, None);
        assert_eq!(
            dict.get_key(b"Filter").as_name(),
            Some(b"FlateDecode".to_vec())
        );
    }

    #[test]
    fn replace_stream_data_on_a_non_stream_handle_is_a_no_op() {
        let scalar = ObjectHandle::integer(1);
        scalar.replace_stream_data(b"x".to_vec(), None, None);
        assert_eq!(scalar.as_integer(), Some(1));
    }

    // --- Coverage closers: paths the tests above never happened to reach ---

    #[test]
    fn replace_key_and_remove_key_mutate_a_resolved_indirect_handle() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        indirect.set_resolved(ObjectValue::Dictionary(Default::default()));
        indirect.replace_key(b"A", ObjectHandle::integer(1));
        assert_eq!(indirect.get_key(b"A").as_integer(), Some(1));
        indirect.remove_key(b"A");
        assert!(indirect.get_key(b"A").is_null());
    }

    #[test]
    fn resolving_an_indirect_dictionary_records_its_direct_child_owner() {
        // This fails if resolution leaves a direct child detached from the
        // canonical indirect object that contains it. Pdf's incremental
        // writer then has no local owner to schedule after the child mutates.
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let child = ObjectHandle::dictionary(vec![]);

        owner.set_resolved(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Child".to_vec(), child.clone()),
        ])));

        assert_eq!(child.containing_object_refs(), vec![owner_ref]);
    }

    #[test]
    fn an_indirect_handle_has_no_direct_containment_owner() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1);

        assert!(handle.containing_object_refs_for_pdf(1).is_empty());
    }

    #[test]
    fn replacing_a_contained_direct_value_propagates_its_owner_to_new_children() {
        // A preserved direct stream dictionary is replaced in place by
        // Pdf::set_object. New direct descendants must inherit the same
        // incremental-write owner rather than requiring a later graph scan.
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let direct = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Direct".to_vec(), direct.clone()),
        ])));
        let child = ObjectHandle::integer(42);

        direct.replace_direct_value(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Child".to_vec(), child.clone()),
        ])));

        assert_eq!(child.containing_object_refs(), vec![owner_ref]);
    }

    #[test]
    fn associating_direct_owners_stops_at_an_indirect_child() {
        // Direct containment ends at indirect identity. Propagating owner 7
        // through this boundary would incorrectly make object 9's payload a
        // direct child of object 7.
        let owner = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1);
        let direct = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Direct".to_vec(), direct.clone()),
        ])));
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);

        direct.replace_key(b"Indirect", indirect.clone());

        assert!(direct.get_key(b"Indirect").is_same_object_as(&indirect));
        assert!(indirect.containing_object_refs().is_empty());
    }

    #[test]
    fn replace_key_and_remove_key_are_no_ops_on_an_unresolved_indirect_handle() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        indirect.replace_key(b"A", ObjectHandle::integer(1)); // must not panic
        indirect.remove_key(b"A"); // must not panic
        assert!(indirect.get_key(b"A").is_null());
    }

    #[test]
    fn shallow_copy_of_an_unresolved_indirect_handle_is_a_direct_null() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        let copy = indirect.shallow_copy();
        assert!(copy.is_direct());
        assert!(copy.is_null());
    }

    #[test]
    fn shallow_copy_of_an_array_recurses_through_direct_elements() {
        let inner = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let original = ObjectHandle::array(vec![inner]);
        let copy = original.shallow_copy();
        let copy_inner = copy.as_array().unwrap()[0].clone();
        assert!(!copy_inner.ptr_eq(&original.as_array().unwrap()[0]));
        assert_eq!(
            copy.as_array().unwrap()[0].as_array().unwrap()[0].as_integer(),
            Some(1)
        );
    }

    #[test]
    fn shallow_copy_of_a_resolved_indirect_stream_gives_the_copy_its_own_dictionary() {
        // Regression test: the copy's dict must not be Rc-shared with the
        // source's, or mutating the copy via replace_stream_data would
        // silently corrupt the source stream's /Length/Filter/DecodeParms.
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        indirect.set_resolved(ObjectValue::Stream {
            dict: ObjectHandle::dictionary(vec![]),
            data: b"old".to_vec(),
        });
        let copy = indirect.shallow_copy();
        copy.replace_stream_data(b"new data".to_vec(), None, None);

        assert_eq!(copy.as_stream_data(), Some(b"new data".to_vec()));
        assert_eq!(
            copy.as_stream_dict()
                .unwrap()
                .get_key(b"Length")
                .as_integer(),
            Some(8)
        );
        assert_eq!(indirect.as_stream_data(), Some(b"old".to_vec()));
        assert!(indirect
            .as_stream_dict()
            .unwrap()
            .get_key(b"Length")
            .is_null());
    }

    #[test]
    fn merge_resources_installs_an_already_indirect_new_key_without_shallow_copying() {
        let shared = ObjectHandle::new_indirect_unresolved(ObjectRef::new(5, 0), -1);
        shared.set_resolved(ObjectValue::Integer(1));
        let this_font = ObjectHandle::dictionary(vec![]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), shared.clone())]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        dest.merge_resources(&other, None);
        assert!(dest.get_key(b"Font").get_key(b"F1").ptr_eq(&shared));
    }

    #[test]
    fn merge_resources_array_union_skips_a_non_scalar_item() {
        let dest =
            ObjectHandle::dictionary(vec![(b"ProcSet".to_vec(), ObjectHandle::array(vec![]))]);
        let other = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::dictionary(vec![])]),
        )]);
        dest.merge_resources(&other, None);
        assert!(dest.get_key(b"ProcSet").as_array().unwrap().is_empty());
    }

    #[test]
    fn is_scalar_covers_every_disjunct() {
        assert!(is_scalar(&ObjectHandle::boolean(true)));
        assert!(is_scalar(&ObjectHandle::integer(1)));
        assert!(is_scalar(&ObjectHandle::name(b"N".to_vec())));
        assert!(is_scalar(&ObjectHandle::null()));
        assert!(is_scalar(&ObjectHandle::real(1.0)));
        assert!(is_scalar(&ObjectHandle::string(b"S".to_vec())));
        assert!(!is_scalar(&ObjectHandle::array(vec![])));
    }

    #[test]
    fn merge_resources_mints_a_second_unique_name_when_the_first_candidate_is_taken() {
        // this_val (the Font sub-dict itself) has a nested dictionary-valued
        // entry ("Widths") whose own key happens to be "F1_1" --
        // get_resource_names is called ON this_val (see merge_resources's
        // own doc comment on why it is this level, not dest's), so its
        // "grandchildren" pool picks this up, forcing unique_resource_name
        // past its first candidate.
        let this_font = ObjectHandle::dictionary(vec![
            (b"F1".to_vec(), ObjectHandle::integer(1)),
            (
                b"Widths".to_vec(),
                ObjectHandle::dictionary(vec![(b"F1_1".to_vec(), ObjectHandle::integer(0))]),
            ),
        ]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();
        dest.merge_resources(&other, Some(&mut conflicts));
        let new_name = conflicts
            .get(b"Font".as_slice())
            .and_then(|m| m.get(b"F1".as_slice()))
            .expect("F1 conflict recorded");
        assert_eq!(new_name, b"F1_2");
        assert_eq!(
            dest.get_key(b"Font").get_key(new_name).as_integer(),
            Some(2)
        );
    }
}
