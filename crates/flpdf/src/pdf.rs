//! qpdf correspondence: QPDF's central document container, direct document-state accessors, and teardown (`include/qpdf/QPDF.hh:1438-1518`; `libqpdf/QPDF.cc:215-232,2323-2358,2647-2651`).

use crate::cache::ObjectCache;
use crate::reader::resolver::ResolverHandle;
use crate::reader::EncryptionState;
use crate::{Dictionary, Object, ObjectHandle, ObjectRef, XrefForm};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::rc::Rc;

/// Provenance for a legacy object-stream member that has already been
/// materialized.
///
/// `parent_ref`/`parent_index` identify the actual member stream after an
/// `/Extends` chain is followed. `source_stream`/`source_index` preserve the
/// live xref identity that led to that parent, so resolution-time xref
/// reconstruction can distinguish a still-valid compressed member from a
/// stale mapping without flattening the chain provenance.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompressedMemberProvenance {
    pub(crate) parent_ref: ObjectRef,
    pub(crate) parent_index: u32,
    pub(crate) source_stream: u32,
    pub(crate) source_index: u32,
}

/// Lazily parsed PDF document handle.
///
/// `Pdf` is the core type of the crate. Opening a document only reads the cross-reference
/// table and the trailer; individual objects are parsed on first access via
/// [`Pdf::resolve`]. The same handle is what every higher-level helper
/// ([`crate::pages`], [`crate::outline`], [`crate::fonts`], [`crate::PdfWriter`])
/// consumes.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{ObjectRef, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// println!("version {}", pdf.version());
/// let catalog = pdf.resolve(pdf.root_ref().expect("root"))?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Why `R: 'static`
///
/// A document hands each [`ObjectHandle`] it vends a weak link back to its own
/// resolver, so that dereferencing a nested reference will not require the
/// `Pdf` in scope. [`ObjectHandle`] has no lifetime parameter — it is a plain
/// `'static` type held in the document's registry and handed to callers — so
/// the trait object that link points at is `dyn DocumentResolver + 'static`,
/// and everything reachable from it, the input source included, must be
/// `'static` too.
///
/// The practical consequence is that `R` cannot borrow: `Cursor<&[u8]>` is
/// rejected, while `Cursor<Vec<u8>>`, `Cursor<Arc<[u8]>>`, and
/// `BufReader<File>` are fine. For in-memory input use [`Pdf::open_mem`],
/// which shares an `Arc<[u8]>` with the caller, or [`Pdf::open_mem_owned`],
/// which takes a `Vec<u8>` outright. Neither copies.
pub struct Pdf<R: Read + Seek + 'static> {
    /// Stable per-document identity used by qpdf-style foreign object copiers.
    pub(crate) unique_id: u64,
    /// The canonical resolver and the state it owns — the input source, the
    /// header offset, and the cross-reference table among them. See
    /// [`crate::reader::resolver::ResolverCore`] for the full field list and its qpdf
    /// correspondence.
    ///
    /// Held behind an `Rc` because every [`ObjectHandle`] this document vends
    /// carries a `Weak` to it, so a nested reference can be dereferenced with
    /// no `&mut Pdf` in scope. `Pdf` holds the only strong reference, so a
    /// surviving handle can never keep a dropped document's input source
    /// alive.
    pub(crate) resolver: Rc<ResolverHandle<R>>,
    pub(crate) version: String,
    pub(crate) trailer: Dictionary,
    pub(crate) last_xref_form: XrefForm,
    pub(crate) cache: ObjectCache,
    // The canonical indirect-object handle registry that used to live here is
    // now `ResolverCore::object_cache`, reached through `self.resolver`. It
    // had to move: `DocumentResolver::resolve_indirect` takes `&self` and
    // must mint a canonical handle for every nested `N G R` it parses, and a
    // registry it cannot reach would mean two maps, divergent identity, and
    // reference cycles `Pdf::drop` could no longer break.
    //
    // `ObjectHandle`'s Rc<RefCell<..>> identity (see object_handle.rs) makes
    // `Pdf<R>` lose the `Send`/`Sync` auto traits it previously had for any
    // `R: Send`/`Sync`. This is an accepted, intentional consequence of that
    // deviation, not a regression to fix — qpdf's own `QPDF` is likewise not
    // thread-safe for concurrent access to one document.
    /// qpdf's `m->object_copiers[source unique_id].object_map` equivalent.
    pub(crate) foreign_object_maps: BTreeMap<u64, BTreeMap<ObjectRef, ObjectRef>>,
    /// qpdf's `m->object_copiers[source unique_id].visiting` equivalent
    /// (`include/qpdf/QPDF.hh:891-897`). qpdf never rolls back
    /// `ObjCopier::object_map`/`visiting` when `copyForeignObject` fails
    /// partway (`libqpdf/QPDF.cc:2019-2093`): a `reserveObjects` traversal
    /// failure leaves the ancestor chain's refs still in `visiting`, and the
    /// *next* call for that same source checks this exact field before doing
    /// any work, throwing rather than silently treating the earlier
    /// failure's partial reservations as complete (`QPDF.cc:2066-2069`). Kept
    /// separate from [`Self::foreign_object_maps`], rather than folded into
    /// one bundled per-source struct, so map persistence and failure poisoning
    /// remain independently tracked by the canonical `copy_foreign_object`
    /// port.
    pub(crate) foreign_object_visiting: BTreeMap<u64, BTreeSet<ObjectRef>>,
    /// Canonical trailer handle (`QPDF::getTrailer`-equivalent identity):
    /// repeated [`Pdf::trailer_handle`] calls return the same shared handle
    /// rather than re-deriving a fresh one from `self.trailer` each time.
    /// Populated lazily on first request.
    pub(crate) trailer_handle_memo: Option<ObjectHandle>,
    /// `qpdf-cutover-delete(flpdf-25kg.3.3)`: delete with
    /// [`Pdf::resolve_borrowed`] after its callers move to canonical handle
    /// accessors. Do not use this memo from the new resolver path.
    ///
    /// Memoized [`Object`] materialization of an already-resolved
    /// [`ObjectHandle`] (`ObjectHandle::materialize`), keyed by `ObjectRef`.
    /// This is [`Pdf::resolve_borrowed`]'s own cache, distinct from
    /// `self.cache`: after the native-parse cutover, `self.cache` is not
    /// guaranteed to hold a value that agrees with what the handle graph
    /// would materialize (e.g. [`Pdf::set_object`] on an excessively deep
    /// value writes an authoritative override here without a corresponding
    /// handle-graph update — see that method's own comment). Populated
    /// lazily by [`Pdf::resolve_borrowed`]; invalidated (removed, never
    /// re-inserted with a stale value) by [`Pdf::set_object`] and
    /// [`Pdf::delete_object`] so the next resolve re-derives from the
    /// updated handle.
    pub(crate) legacy_materialized_memo: BTreeMap<ObjectRef, Object>,
    /// Entries in [`Self::legacy_materialized_memo`] that are authoritative
    /// caller-supplied replacements which still need to be lifted into the
    /// canonical handle graph. Compatibility snapshots populated by
    /// [`Pdf::resolve_borrowed`] are deliberately not included: reconciling
    /// those must not materialize a lazy source stream just to compare it.
    pub(crate) legacy_materialized_replacement_refs: BTreeSet<ObjectRef>,
    pub(crate) compressed_member_parents: BTreeMap<ObjectRef, CompressedMemberProvenance>,
    /// Every uncompressed object offset, sorted ascending and deduplicated. Used
    /// to bound a single object read to the start of the next object in the file
    /// (objects do not overlap in a well-formed PDF), so resolving one object
    /// cannot read/parse the whole remaining file — which would make resolving
    /// many objects quadratic, a CPU DoS on a crafted (e.g. repaired) document.
    pub(crate) sorted_object_offsets: Vec<u64>,
    /// Whether the legacy cache and object-boundary snapshot already reflect
    /// the resolver's reconstructed xref. Open-time recovery initializes all
    /// three from the same recovered table; resolution-time recovery flips
    /// this lazily before the next legacy read.
    pub(crate) legacy_resolution_state_synced: bool,
    /// Remaining read-to-end fallbacks allowed when a bounded object window does
    /// not contain a complete object (a corrupt offset pointing inside another
    /// object, or a header-like line recorded inside stream data during repair).
    /// Each fallback may scan to EOF, so the count is capped: a handful of bad
    /// boundaries in an otherwise valid file still resolve, but a document full
    /// of objects whose bodies run to EOF cannot revive the quadratic cost.
    pub(crate) resolution_fallbacks_remaining: u32,
    pub(crate) dirty_object_refs: BTreeSet<ObjectRef>,
    /// Dirty objects whose live ObjectHandle graph was changed directly, so
    /// the legacy object cache may no longer agree with it. `set_object`
    /// updates both representations and retains the stream zero-copy fast
    /// path in `resolve_borrowed`.
    pub(crate) handle_mutated_object_refs: BTreeSet<ObjectRef>,
    /// Exact source framing EOLs removed while a line-anchored `endstream`
    /// scan remained authoritative. Rewriting restores this private metadata
    /// before applying the selected stream policy, matching qpdf's recovered
    /// raw stream bytes for missing, invalid, and unresolved `/Length` values.
    pub(crate) recovered_stream_eols: BTreeMap<ObjectRef, crate::parser::RecoveredStreamEol>,
    /// Streams whose cached representation has already accounted for source
    /// framing. This includes actual decryption and selective explicit
    /// `/Crypt` removal. Recovered framing must not be appended later in either
    /// case: ciphertext framing is not plaintext, while explicit-filter
    /// framing is consumed while transforming the declared chain. Metadata and
    /// document-level Identity streams remain source-represented and absent.
    pub(crate) transformed_stream_refs: BTreeSet<ObjectRef>,
    /// Valid indirect references discovered while preparing qpdf JSON whose
    /// exact object generation has no live xref/cache target.
    pub(crate) qpdf_dangling_refs: BTreeSet<ObjectRef>,
    /// Valid indirect references parsed from every classic trailer and xref
    /// stream dictionary in the source `/Prev` chain.
    pub(crate) qpdf_trailer_references: BTreeSet<ObjectRef>,
    /// Xref stream objects parsed while following the source `/Prev` chain.
    /// Kept outside the public cache so superseded/free streams remain visible
    /// only through qpdf's raw object view.
    pub(crate) qpdf_parsed_xref_streams: BTreeMap<ObjectRef, Object>,
    /// Objects removed through [`Self::delete_object`]. qpdf's object cache
    /// removal is persistent across repeated JSON preparation; keep this
    /// separate from immutable source/trailer discovery so those seeds cannot
    /// resurrect an explicitly removed reference.
    pub(crate) qpdf_removed_refs: BTreeSet<ObjectRef>,
    /// Monotonic observation matching qpdf's `everCalledGetAllPages()`.
    pub(crate) ever_called_get_all_pages: bool,
    pub(crate) encryption: Rc<RefCell<Option<EncryptionState>>>,
}

impl<R: Read + Seek> Drop for Pdf<R> {
    // A resolved indirect handle's value can embed other indirect handles
    // sharing `handle_registry`'s own canonical `Rc` identity (array/dict/
    // stream-dict children). Two objects that reference each other (e.g. a
    // `/Pages` node and a page's `/Parent`, common in real PDFs) therefore
    // form a strong reference cycle once both are resolved, which plain
    // `Rc` drop never collects on its own.
    //
    // Mirrors qpdf's own teardown: `QPDF::~QPDF()` walks its object cache
    // and disconnects every resolved object, replacing it with
    // `QPDF_Destroyed()`, specifically to break cycles like this one
    // (`libqpdf/QPDF.cc`, `QPDF::~QPDF`). Disconnecting every registry
    // entry here — the sole owner of the canonical `Rc`s — before the
    // registry itself drops ensures no lingering cycle keeps a document's
    // object graph (and any reachable stream buffers) alive past `self`.
    fn drop(&mut self) {
        self.resolver.disconnect_all();
    }
}

impl<R: Read + Seek> Pdf<R> {
    /// PDF version header as written in the first line of the file (e.g. `"1.7"`).
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Configure this document as a qpdf source whose lazy stream data must
    /// be materialized when it is copied into another document. This is
    /// qpdf's `QPDF::setImmediateCopyFrom` (`include/qpdf/QPDF.hh:242-257`):
    /// the flag belongs to the source document, not the destination, and one
    /// materialized buffer is then shared by every copied stream.
    pub fn set_immediate_copy_from(&self, value: bool) {
        self.resolver.set_immediate_copy_from(value);
    }

    /// Whether this document has been asked to enumerate its complete page tree.
    ///
    /// This is monotonic for the lifetime of the [`Pdf`] and mirrors qpdf's
    /// `everCalledGetAllPages()` observation used by JSON v2 metadata.
    pub fn ever_called_get_all_pages(&self) -> bool {
        self.ever_called_get_all_pages
    }

    pub(crate) fn mark_get_all_pages_called(&mut self) {
        self.ever_called_get_all_pages = true;
    }

    /// Adobe extension level from the catalog's `/Extensions /ADBE
    /// /ExtensionLevel`, resolving indirect references at each step. Returns
    /// `None` when any link in that chain is absent or is not the expected
    /// type. Only the `/ADBE` developer prefix is honoured, matching qpdf's
    /// `--check` version banner and the extension level qpdf accumulates into
    /// its `max_input_version`.
    pub fn adobe_extension_level(&mut self) -> Option<i64> {
        let root_ref = self.trailer().get_ref("Root")?;
        let catalog = self.resolve(root_ref).ok()?;
        let extensions = resolve_object_value(self, catalog.as_dict()?.get("Extensions")?.clone())?;
        let adbe = resolve_object_value(self, extensions.as_dict()?.get("ADBE")?.clone())?;
        let level = resolve_object_value(self, adbe.as_dict()?.get("ExtensionLevel")?.clone())?;
        level.as_integer()
    }

    /// The trailer dictionary (or the dictionary attached to the trailing xref stream
    /// for cross-reference-stream documents). This is where you'd reach for `/Root`,
    /// `/Info`, `/Size`, `/ID`, etc.
    pub fn trailer(&self) -> &Dictionary {
        &self.trailer
    }

    // Degrade to a null handle rather than propagating the legacy materializer's
    // depth error or panicking: the trailer is always fully parsed already, so
    // the handle bridge accepts the same `parser::MAX_PARSE_DEPTH` bound as the
    // parser itself. A value beyond that accepted bound is structurally
    // unusable here, just as `resolve`/`resolve_borrowed` present an unusable
    // reference as `Object::Null` rather than erroring.
    //
    // Memoized in `self.trailer_handle_memo`, the same way `handle_registry`
    // memoizes indirect handles: `self.trailer` is set once at construction
    // and never reassigned afterward, so a lazily-cached handle here has no
    // invalidation to worry about, and repeated calls return the same shared
    // identity (`QPDF::getTrailer`) instead of a fresh `Rc` (and fresh direct
    // children, e.g. `/ID`) on every call.
    /// The trailer dictionary as an [`ObjectHandle`].
    ///
    /// The trailer is always a direct, in-memory dictionary — it is never
    /// itself an indirect object per the PDF spec — so the returned handle is
    /// always direct. A trailer whose literal (non-indirect) nesting exceeds
    /// the parser's accepted bound yields a null handle instead — note this
    /// degrades the *entire* trailer, so a caller that only cares about one
    /// key and cannot tolerate an unrelated sibling entry's nesting erasing it
    /// should use [`Pdf::trailer_key_handle`] instead. Repeated calls return
    /// the same shared handle.
    pub fn trailer_handle(&mut self) -> ObjectHandle {
        if let Some(handle) = &self.trailer_handle_memo {
            return handle.clone();
        }
        let trailer = Object::Dictionary(self.trailer.clone());
        let handle = self
            .lift_to_handle_bounded(&trailer, 0, crate::parser::MAX_PARSE_DEPTH)
            .unwrap_or_else(|_| ObjectHandle::null());
        self.trailer_handle_memo = Some(handle.clone());
        handle
    }

    /// `key`'s value in the trailer dictionary, as an [`ObjectHandle`] —
    /// unlike `Pdf::trailer_handle().get_key(key)`, this lifts only `key`'s
    /// own value, so an unrelated sibling trailer entry whose literal nesting
    /// exceeds the crate's inline-object-nesting bound cannot degrade this
    /// result to null the way it degrades [`Pdf::trailer_handle`]'s whole-
    /// trailer walk. A bare reference (`/Key 1 0 R`) becomes a genuine
    /// indirect handle sharing the canonical `handle_registry` identity
    /// (matching how a dictionary *child* reference lifts, not `lift`'s own
    /// top-level `ObjectValue::Reference` shape — a trailer value is read,
    /// never `Pdf::set_object`-redirected in place). Returns a direct null
    /// handle for a missing key or a lift failure on `key`'s own value
    /// (matching [`ObjectHandle::get_key`]'s own "missing key" contract).
    /// Not memoized — unlike the whole trailer, a single key's handle is
    /// cheap enough to relift on every call, and every caller today needs at
    /// most one key per `Pdf`.
    pub fn trailer_key_handle(&mut self, key: &[u8]) -> ObjectHandle {
        let Some(value) = self.trailer.get(key).cloned() else {
            return ObjectHandle::null();
        };
        // `parser::MAX_PARSE_DEPTH`, not `lift`'s default `MAX_INLINE_DEPTH`:
        // the trailer was already parsed successfully at the looser
        // `MAX_PARSE_DEPTH` bound, so a value nested between the two would
        // otherwise degrade to null *here* while the legacy `resolve_chain`/
        // `resolve_borrowed` path (`MAX_PARSE_DEPTH`-bounded) still returns
        // it — the same divergence `resolve_object_handle`'s own call to
        // `lift_bounded` documents and avoids for the analogous
        // compressed-member case.
        self.lift_to_handle_bounded(&value, 0, crate::parser::MAX_PARSE_DEPTH)
            .unwrap_or_else(|_| ObjectHandle::null())
    }

    /// `/Root` as listed in the trailer, when present.
    pub fn root_ref(&self) -> Option<ObjectRef> {
        self.trailer.get_ref("Root")
    }
}

// Resolve `value` one level: follow an `Object::Reference` through `pdf`,
// or return a non-reference value unchanged.
fn resolve_object_value<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> Option<Object> {
    match value {
        Object::Reference(reference) => pdf.resolve(reference).ok(),
        other => Some(other),
    }
}
