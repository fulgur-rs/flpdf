//! qpdf correspondence: QPDF's central document container, direct document-state accessors, and teardown (`include/qpdf/QPDF.hh:1438-1518`; `libqpdf/QPDF.cc:215-232,2323-2358,2647-2651`).

use crate::acroform_document_helper::AcroFormCache;
use crate::cache::ObjectCache;
use crate::encryption::state::{EncryptionInspectionState, EncryptionState};
use crate::pages::repair::PreparedPages;
use crate::reader::resolver::ResolverHandle;
use crate::reader::InputSourceControl;
use crate::{Error, ObjectHandle, ObjectRef, Result, XrefForm};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::rc::Rc;

/// Provenance for a legacy object-stream member that has already been
/// materialized.
///
/// `parent_ref` identifies the direct object-stream container named by the
/// type-2 xref field1. `parent_index` is the raw type-2 field2 retained as
/// provenance; it is not a positional member selected through an `/Extends`
/// chain. `source_stream`/`source_index` preserve the live xref identity so
/// resolution-time xref reconstruction can distinguish a still-valid
/// compressed member from a stale mapping.
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
/// ([`crate::pages`], [`crate::outline_object_helper`], [`crate::PdfWriter`])
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
/// let catalog = pdf.root_handle()?;
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
    /// Optional qpdf-style file-source lifetime controller. Generic readers
    /// (memory buffers and caller-owned streams) leave this absent; the
    /// file-open factory installs it for `QPDFJob::handle_page_specs`.
    pub(crate) input_source_control: Option<InputSourceControl>,
    pub(crate) version: String,
    /// Whether qpdf's enhanced `QPDF::getRoot` checks are enabled for a
    /// document check (`QPDF::JobSetter::setCheckMode`,
    /// `libqpdf/QPDFJob.cc:745-752`).
    pub(crate) check_mode: bool,
    pub(crate) trailer: ObjectHandle,
    pub(crate) last_xref_form: XrefForm,
    /// qpdf's xref-parser-owned `first_xref_item_offset` used by the
    /// linearization `/T` check; zero preserves qpdf's initialized default
    /// when no parsed xref section contains object 0.
    pub(crate) first_xref_item_offset: u64,
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
    /// qpdf's per-source AcroForm helper cache (`QPDFJob::get_afdh_for_qpdf`,
    /// `QPDFJob.cc:1847-1856`). It stores only canonical ObjectHandle
    /// identities, so sequential helper facades can share it without a
    /// self-referential borrow of this Pdf. Transient page-selection helpers
    /// use their own cache instead.
    pub(crate) acroform_cache: Rc<RefCell<Option<AcroFormCache>>>,
    /// Optional replacement used by the JSON importer while it is building a
    /// new document trailer. Ordinary parsed documents use `trailer` directly.
    pub(crate) trailer_handle_memo: Option<ObjectHandle>,
    /// Canonical `/Root` handle after the first root lookup.
    pub(crate) root_handle_memo: Option<ObjectHandle>,
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
    /// path in the raw value cache.
    pub(crate) handle_mutated_object_refs: BTreeSet<ObjectRef>,
    /// Valid indirect references discovered while preparing qpdf JSON whose
    /// exact object generation has no live xref/cache target.
    pub(crate) qpdf_dangling_refs: BTreeSet<ObjectRef>,
    /// Valid indirect references parsed from every classic trailer and xref
    /// stream dictionary in the source `/Prev` chain.
    pub(crate) qpdf_trailer_references: BTreeSet<ObjectRef>,
    /// Historical xref-stream object identities promoted into the canonical
    /// resolver cache while following the source `/Prev` chain. The parsed
    /// values themselves live on their [`ObjectHandle`]s; this set only keeps
    /// the qpdf JSON preparation/mutation boundary aware of cache-only refs.
    pub(crate) qpdf_parsed_xref_stream_refs: BTreeSet<ObjectRef>,
    /// Objects removed through [`Self::delete_object`]. qpdf's object cache
    /// removal is persistent across repeated JSON preparation; keep this
    /// separate from immutable source/trailer discovery so those seeds cannot
    /// resurrect an explicitly removed reference.
    pub(crate) qpdf_removed_refs: BTreeSet<ObjectRef>,
    /// Monotonic observation matching qpdf's `everCalledGetAllPages()`.
    pub(crate) ever_called_get_all_pages: bool,
    /// qpdf's `m->all_pages` cache (`QPDF_pages.cc:39-75`). The prepared
    /// root and leaf identities stay together because page consumers need the
    /// repaired root as well as the ordered page list. An empty qpdf page
    /// vector is not cached by qpdf (it is its cache sentinel), so this is
    /// populated only for a non-empty page list.
    pub(crate) page_list_cache: Option<PreparedPages>,
    pub(crate) encryption: Rc<RefCell<Option<EncryptionState>>>,
    /// qpdf's parsed encryption parameters retained for read-only inspection,
    /// including the partial state visible after a bad password.
    pub(crate) encryption_inspection: Rc<RefCell<Option<EncryptionInspectionState>>>,
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
    /// Set qpdf's `ClosedFileInputSource::stayOpen` policy when this document
    /// was opened through the file-source factory. Non-file readers have no
    /// close/reopen controller and therefore remain unchanged.
    pub(crate) fn set_input_source_stay_open(&self, value: bool) {
        if let Some(control) = &self.input_source_control {
            control.set_stay_open(value);
        }
    }

    /// PDF version header as written in the first line of the file (e.g. `"1.7"`).
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Return the logical bytes of the input source, excluding any leading
    /// material skipped by the PDF header parser.
    ///
    /// This is a crate-visible source seam for qpdf consumers that inspect
    /// physical offsets against the same document input, notably
    /// `QPDF::checkLinearization` (`libqpdf/QPDF_linearization.cc:84-245`).
    pub(crate) fn source_bytes(&self) -> crate::Result<Vec<u8>> {
        self.resolver.source_bytes()
    }

    /// Return the qpdf-logical offset from which the most recent source read
    /// started (`InputSource::getLastOffset()`). This is used by linearization
    /// damage warnings to reproduce qpdf's source-location context.
    pub(crate) fn source_last_offset(&self) -> u64 {
        self.resolver.last_offset()
    }

    /// Configure this document as a qpdf source whose lazy stream data must
    /// be materialized when it is copied into another document. This is
    /// qpdf's `QPDF::setImmediateCopyFrom` (`include/qpdf/QPDF.hh:242-257`):
    /// the flag belongs to the source document, not the destination, and one
    /// materialized buffer is then shared by every copied stream.
    pub fn set_immediate_copy_from(&self, value: bool) {
        self.resolver.set_immediate_copy_from(value);
    }

    /// Run a writer-owned operation with qpdf's PCLm stream-length boundary
    /// selected on this document's resolver.
    ///
    /// qpdf's PCLm writer passes the complete length recovered by an
    /// `endstream` scan into its encrypted stream pipeline
    /// (`libqpdf/QPDFWriter.cc:2068-2098,2928-3005`; `libqpdf/QPDF.cc:1482-1524`).
    /// Other stream consumers retain flpdf's existing recovery-framing policy,
    /// so this mode is scoped to the PCLm writer and restored before return.
    pub(crate) fn with_pclm_stream_data<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let previous = self.resolver.set_pclm_mode(true);
        let result = operation(self);
        self.resolver.set_pclm_mode(previous);
        result
    }

    /// Whether this document has been asked to enumerate its complete page tree.
    ///
    /// This is monotonic for the lifetime of the [`Pdf`] and mirrors qpdf's
    /// `everCalledGetAllPages()` observation used by JSON v2 metadata.
    pub fn ever_called_get_all_pages(&self) -> bool {
        self.ever_called_get_all_pages
    }

    /// Rebuild qpdf's document-owned page-list cache after direct `/Pages`
    /// tree manipulation.
    ///
    /// This is qpdf's `QPDF::updateAllPagesCache`
    /// (`include/qpdf/QPDF.hh:695-704`). Page-specific mutation APIs update
    /// the cache themselves, but callers that edit page-tree handles directly
    /// must invoke this boundary before asking for the refreshed page list.
    pub fn update_all_pages_cache(&mut self) -> Result<()> {
        self.invalidate_page_list_cache();
        let _ = crate::PageDocumentHelper::new(self).get_all_pages()?;
        Ok(())
    }

    pub(crate) fn mark_get_all_pages_called(&mut self) {
        self.ever_called_get_all_pages = true;
    }

    pub(crate) fn cached_page_list(&self) -> Option<PreparedPages> {
        self.page_list_cache.clone()
    }

    pub(crate) fn cache_page_list(&mut self, prepared: &PreparedPages) {
        if !prepared.pages.is_empty() {
            self.page_list_cache = Some(prepared.clone());
        }
    }

    /// Invalidate qpdf's page-list cache after a page-tree mutation.
    pub(crate) fn invalidate_page_list_cache(&mut self) {
        self.page_list_cache = None;
    }

    /// Adobe extension level from the catalog's `/Extensions /ADBE
    /// /ExtensionLevel`, resolving direct and indirect references at each
    /// step. Returns `None` when any link in that chain is absent or is not
    /// the expected type. Only the `/ADBE` developer prefix is honoured,
    /// matching qpdf's `--check` version banner and the extension level qpdf
    /// accumulates into its `max_input_version`.
    ///
    /// The trailer's `/Root` may itself be a direct Catalog dictionary;
    /// qpdf's `QPDF::getRoot` accepts that shape via `getKey` rather than
    /// requiring an indirect object (`libqpdf/QPDF.cc:2329-2367`).
    pub fn adobe_extension_level(&mut self) -> Option<i64> {
        let catalog = self.trailer_key_handle(b"Root");
        self.resolve(&catalog).ok()?;
        let extensions = catalog.try_get_key(b"/Extensions").ok()?;
        self.resolve(&extensions).ok()?;
        extensions.try_as_dictionary().ok()?.as_ref()?;
        let adbe = extensions.try_get_key(b"/ADBE").ok()?;
        self.resolve(&adbe).ok()?;
        adbe.try_as_dictionary().ok()?.as_ref()?;
        let level = adbe.try_get_key(b"/ExtensionLevel").ok()?;
        self.resolve(&level).ok()?;
        level.try_as_integer().ok().flatten()
    }

    /// The live trailer dictionary as an [`ObjectHandle`].
    pub fn trailer(&mut self) -> ObjectHandle {
        if let Some(handle) = &self.trailer_handle_memo {
            return handle.clone();
        }
        let handle = self.trailer.clone();
        self.trailer_handle_memo = Some(handle.clone());
        handle
    }

    /// Return the live value for a trailer key, or a contextless null handle
    /// when the key is absent.
    pub fn trailer_key_handle(&mut self, key: &[u8]) -> ObjectHandle {
        let trailer = self.trailer();
        let mut name = Vec::with_capacity(key.len() + 1);
        name.push(b'/');
        name.extend_from_slice(key);
        let handle = trailer
            .try_get_key(&name)
            .unwrap_or_else(|_| ObjectHandle::null());
        if key == b"Root" && !handle.is_null() {
            self.root_handle_memo = Some(handle.clone());
        }
        handle
    }

    /// `/Root` as listed in the trailer, when present.
    pub fn root_ref(&self) -> Option<ObjectRef> {
        self.trailer_handle_memo
            .as_ref()
            .unwrap_or(&self.trailer)
            .try_get_key(b"/Root")
            .ok()
            .and_then(|handle| handle.object_ref())
    }

    /// Return the live catalog handle after applying qpdf's `QPDF::getRoot`
    /// dictionary gate (`libqpdf/QPDF.cc:2329-2368`). The trailer value may
    /// be an indirect reference, so resolve it through the canonical handle
    /// graph before checking its type. A missing, dangling, or non-dictionary
    /// `/Root` is a document-level error rather than a missing-key fallback.
    ///
    /// Reads `/Root` fresh from the live trailer on every call, matching
    /// [`Self::root_ref`]'s identical reasoning: a caller may replace `/Root`
    /// through the live handle returned by [`Self::trailer`], or install a
    /// new trailer via `update_from_json()`, after an earlier call already
    /// populated `root_handle_memo` — trusting that memo unconditionally
    /// would keep returning the old catalog after such a replacement.
    pub fn root_handle(&mut self) -> Result<ObjectHandle> {
        let candidate = self
            .trailer_handle_memo
            .as_ref()
            .unwrap_or(&self.trailer)
            .try_get_key(b"/Root")
            .unwrap_or_else(|_| ObjectHandle::null());
        if !candidate.is_null() {
            self.root_handle_memo = Some(candidate.clone());
        }
        self.resolve(&candidate)?;
        let root = candidate;
        if root.as_dictionary().is_none() {
            return Err(Error::System("unable to find /Root dictionary".into()));
        }
        if self.check_mode
            && !root
                .try_get_key(b"/Type")?
                .try_is_name_and_equals(b"Catalog")?
        {
            // qpdf's check mode warns and repairs an invalid Catalog type in
            // `QPDF::getRoot` (`libqpdf/QPDF.cc:2354-2366`). The replacement
            // is on the live handle so later inspection branches observe the
            // same repaired Catalog.
            self.resolver
                .push_warning("catalog /Type entry missing or invalid")?;
            root.replace_key(b"/Type", ObjectHandle::name(b"Catalog".to_vec()))?;
            self.mark_object_handle_dirty(&root)?;
        }
        Ok(root)
    }

    /// Enable or disable qpdf's enhanced root checks for a document check.
    pub(crate) fn set_check_mode(&mut self, enabled: bool) {
        self.check_mode = enabled;
    }
}
