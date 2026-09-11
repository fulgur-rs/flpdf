//! qpdf correspondence: QPDF.cc xref loading and repair.
//!
//! The xref loader follows qpdf 11.9.0's
//! `QPDF::read_xref`/`read_xrefStream`/`processXRefStream` ordering
//! (`libqpdf/QPDF.cc:626-710,846-1148`): the xref stream is parsed as a live
//! `ObjectHandle` graph, `/Type`/`/W`/`/Index`/`/Size` are inspected, and only
//! then is the encoded payload passed to the handle-native filter pipeline.
//! `Pdf::open` supplies the already-created `ResolverHandle` as the canonical
//! owner, so active xref-stream, hybrid, `/Prev`, and reconstruction-candidate
//! reads use qpdf's live `readObjectAtOffset`/`readStream` route. The
//! short-lived `BootstrapHandleDocument` remains only for the owner-less
//! standalone xref loader and its reconstruction-only bounded-read tests.
//!
//! qpdf keeps its shared `QPDF::Members::file` input source and does not read
//! PDF objects until they are needed (`include/qpdf/QPDF.hh:67-97,1453-1457`,
//! `libqpdf/QPDF.cc:245-275`). The bootstrap owner follows that boundary:
//! handle identity and diagnostics are available during xref parsing, while
//! its `Rc<[u8]>` source snapshot is initialized only for an actual indirect
//! object or stream-length resolution.
//!
//! qpdf's `xref_offset == 0` check (`libqpdf/QPDF.cc:450-452`) throws
//! `damagedPDF("can't find startxref")` immediately and never calls
//! `read_xref` at all, whether the zero came from a missing/malformed
//! `startxref` or a syntactically valid `startxref` that explicitly names
//! offset 0. `load_xref_state_from_bytes` preserves that boundary and enters
//! the line-scan recovery directly, so an object at logical offset 0 cannot
//! enter the canonical cache as a speculative xref read before
//! `reconstruct_xref` chooses the effective occurrence
//! (`libqpdf/QPDF.cc:450-469,516-531`).
use crate::object_handle::{DocumentResolver, ObjectValue};
use crate::parser::{
    parse_qpdf_direct_object_handle_with_diagnostics,
    parse_qpdf_file_object_handle_with_diagnostics, HandleResolver, ParserDiagnostic,
};
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::qutil::{qpdf_string_to_int_checked, QpdfIntParse};
use crate::reader::file_object::{
    finish_file_object_handle, parse_file_object_handle_syntax, parse_file_object_header,
    FileObjectDiagnostic, FileObjectDiagnosticKind, HandleFileObjectRead, RecoveryPolicy,
    ResolvedStreamLength,
};
use crate::reader::resolver::ResolverHandle;
use crate::tokenizer::{Token, TokenType, Tokenizer};
use crate::writer::DecodeLevel;
use crate::{
    Diagnostics, Error, ObjectHandle, ObjectRef, QpdfErrorCode, QpdfExc, Result, XrefEntry,
};
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashSet};
#[cfg(test)]
use std::io::SeekFrom;
use std::io::{Read, Seek};
use std::rc::{Rc, Weak};

// The bootstrap resolver can re-enter once per indirect reference in a
// stream's /Length or xref metadata. Keep the stack-growth values aligned with
// the post-open resolver's measured frame layout.
const XREF_STACK_RED_ZONE: usize = 128 * 1024;
const XREF_STACK_GROWTH_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct LoadedXref {
    pub version: String,
    pub startxref: u64,
    pub entries: BTreeMap<ObjectRef, XrefEntry>,
    pub trailer: ObjectHandle,
    pub last_xref_form: XrefForm,
    pub repair_diagnostics: Diagnostics,
}

/// The document owner available while an xref section is parsed.
///
/// qpdf constructs `QPDF::Members` before `QPDF::parse`, so a classic trailer,
/// xref stream, and every indirect child they discover are inserted into the
/// same `obj_cache` that later resolution uses. This trait exposes the live
/// offset-read and warning boundaries needed to keep that ownership intact;
/// the owner-less standalone loader retains its separate bootstrap context.
pub(crate) trait CanonicalTrailerOwner {
    fn indirect_handle(&self, object_ref: ObjectRef) -> ObjectHandle;
    fn direct_handle(&self, value: ObjectValue) -> ObjectHandle;
    fn install_xref_entries(&self, entries: BTreeMap<ObjectRef, XrefEntry>);
    fn set_header_offset(&self, offset: usize);
    /// Enter the document's parse guard, mirroring qpdf's `QPDF::readTrailer`
    /// constructing its parser with `this` as the context
    /// (`libqpdf/QPDF.cc:1317`), which is what arms `QPDF::ParseGuard`.
    fn begin_parse(&self) -> crate::Result<()>;
    /// Leave that guard, mirroring `ParseGuard`'s destructor.
    fn end_parse(&self);
    #[allow(dead_code)]
    fn read_object_at_offset(
        &self,
        offset: u64,
        expected: ObjectRef,
        description: Option<Vec<u8>>,
    ) -> Result<(ObjectHandle, Option<u64>)>;
    fn read_xref_stream_at_offset(
        &self,
        offset: u64,
        description: Option<Vec<u8>>,
    ) -> Result<(ObjectHandle, Option<u64>)>;
    fn push_warning(&self, warning: QpdfExc) -> Result<()>;
    fn repair_diagnostics(&self) -> Diagnostics;
}

impl<R: Read + Seek + 'static> CanonicalTrailerOwner for ResolverHandle<R> {
    fn begin_parse(&self) -> crate::Result<()> {
        self.in_parse(true)
    }

    fn end_parse(&self) {
        // Mirrors `ChildHandles`: `begin_parse` set the flag, so the symmetric
        // "already false" branch cannot be observed from here.
        let _ = self.in_parse(false);
    }

    fn indirect_handle(&self, object_ref: ObjectRef) -> ObjectHandle {
        self.get_object_handle(object_ref)
    }

    fn direct_handle(&self, value: ObjectValue) -> ObjectHandle {
        self.parsed_direct_object_handle(value)
    }

    fn install_xref_entries(&self, entries: BTreeMap<ObjectRef, XrefEntry>) {
        self.install_source_xref_entries(entries);
    }

    fn set_header_offset(&self, offset: usize) {
        ResolverHandle::set_header_offset(self, offset);
    }

    fn read_object_at_offset(
        &self,
        offset: u64,
        expected: ObjectRef,
        description: Option<Vec<u8>>,
    ) -> Result<(ObjectHandle, Option<u64>)> {
        self.resolve_at_offset_with_optional_description(offset, expected, description)
    }

    fn read_xref_stream_at_offset(
        &self,
        offset: u64,
        description: Option<Vec<u8>>,
    ) -> Result<(ObjectHandle, Option<u64>)> {
        self.resolve_xref_stream_at_offset(offset, description)
    }

    fn repair_diagnostics(&self) -> Diagnostics {
        ResolverHandle::repair_diagnostics(self)
    }

    fn push_warning(&self, warning: QpdfExc) -> Result<()> {
        self.push_qpdf_warning(warning)
    }
}

/// Deliver diagnostics produced by the xref reader through the document's
/// qpdf warning sink. Owner-less loading keeps the same diagnostics local for
/// its public snapshot API; canonical loading has one live owner and must not
/// hand a second logger channel to `engine.rs`.
fn deliver_canonical_diagnostics(
    owner: Option<&dyn CanonicalTrailerOwner>,
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    let Some(owner) = owner else {
        return Ok(());
    };
    let batch = diagnostics.drain();
    for warning in batch.entries() {
        owner.push_warning(warning.clone())?;
    }
    Ok(())
}

fn with_xref_open_diagnostics(
    error: Error,
    local: Diagnostics,
    owner: Option<&dyn CanonicalTrailerOwner>,
) -> Error {
    if let Some(owner) = owner {
        return Error::with_open_diagnostics(error, owner.repair_diagnostics());
    }
    Error::with_open_diagnostics(error, local)
}

#[derive(Debug, Default)]
struct BootstrapHandleState {
    handles: BTreeMap<ObjectRef, ObjectHandle>,
    resolving: BTreeSet<ObjectRef>,
    resolved_object_streams: BTreeSet<u32>,
    diagnostics: Diagnostics,
    reconstruction_trigger: Option<(u64, String)>,
}

#[derive(Debug, Default)]
pub(crate) struct BootstrapCache {
    /// qpdf's one-operation cache and resolution state. Keeping this state
    /// apart from the owner document avoids making each context a new resolver
    /// identity while retaining the document strongly below.
    handle_state: Rc<RefCell<BootstrapHandleState>>,
    handle_document: Option<Rc<BootstrapHandleDocument>>,
    handle_document_owners: Vec<Rc<BootstrapHandleDocument>>,
}

impl Drop for BootstrapCache {
    /// Bootstrap parsing builds the same strong object-reference cycles as the
    /// live document cache, but it runs before `Pdf` owns a `ResolverHandle`
    /// that could perform the normal qpdf-style disconnect walk. Break those
    /// cycles while the temporary cache is still being destroyed, mirroring
    /// `QPDF::~QPDF()`'s replacement of cached values with
    /// `QPDF_Destroyed()` (`libqpdf/QPDF.cc:215-235`).
    fn drop(&mut self) {
        let mut states: Vec<(Rc<RefCell<BootstrapHandleState>>, usize)> = Vec::new();
        let mut add_state = |state: Rc<RefCell<BootstrapHandleState>>| {
            if let Some((_, references)) = states
                .iter_mut()
                .find(|(candidate, _)| Rc::ptr_eq(candidate, &state))
            {
                *references += 1;
            } else {
                states.push((state, 1));
            }
        };
        add_state(Rc::clone(&self.handle_state));
        if let Some(document) = &self.handle_document {
            add_state(Rc::clone(&document.state));
        }
        for document in &self.handle_document_owners {
            add_state(Rc::clone(&document.state));
        }

        for (state, internal_references) in states {
            // A merge can leave an older BootstrapCache pointing at the same
            // state as the cache that superseded it. Only the final owner may
            // disconnect the handles; otherwise it invalidates the newer
            // cache's trailer before it can be rebound.
            if Rc::strong_count(&state) != internal_references + 1 {
                continue;
            }
            let handles: Vec<_> = state.borrow().handles.values().cloned().collect();
            for handle in handles {
                handle.disconnect_and_destroy();
            }
        }
    }
}

type SharedBootstrapCache = Rc<RefCell<BootstrapCache>>;

fn empty_bootstrap_cache() -> SharedBootstrapCache {
    Rc::new(RefCell::new(BootstrapCache::default()))
}

#[derive(Debug, Clone)]
pub(crate) struct LoadedXrefState {
    pub(crate) loaded: LoadedXref,
    /// qpdf's raw `m->xref_table`, retained separately from the valid
    /// `ObjectRef` view so inspection can preserve signed object/generation
    /// identity (`QPDF.cc:1149-1184`).
    pub(crate) raw_entries: BTreeMap<QpdfObjGen, XrefEntry>,
    /// qpdf's `m->first_xref_item_offset`, populated while reading the xref
    /// section and consumed later by `checkLinearizationInternal`.
    pub(crate) first_xref_item_offset: u64,
    /// Byte position qpdf's `readTrailer` records for a classic trailer
    /// keyword. `None` identifies an xref-stream dictionary, whose `/Prev`
    /// diagnostics have a different qpdf description and remain on that
    /// existing path.
    pub(crate) classic_trailer_offset: Option<usize>,
    /// A bootstrap `/Size` lookup can request qpdf-style xref reconstruction
    /// while the classic trailer is being validated. Keep that trigger on the
    /// partially loaded state so the outer loader can reconstruct and then
    /// continue with the post-chain `/Size` consistency check, rather than
    /// treating the resolver handoff as a terminal xref parse failure.
    pub(crate) pending_reconstruction_trigger: Option<(u64, String)>,
    pub(crate) trailer_references: BTreeSet<ObjectRef>,
    pub(crate) parsed_xref_streams: BTreeMap<ObjectRef, ObjectHandle>,
    /// Owner-less objects resolved while reading xref streams stay available
    /// to post-chain trailer validation, matching the standalone loader's
    /// temporary cache. Canonical-owner loading leaves this `None` because
    /// qpdf keeps those objects in the document's one object cache.
    pub(crate) bootstrap_cache: Option<SharedBootstrapCache>,
    pub(crate) header_offset: usize,
    /// True when open-time xref recovery via linear scan already ran.
    ///
    /// qpdf `m->reconstructed_xref` (`QPDF.cc:524`) is set inside
    /// `reconstruct_xref` which runs both at open time (`:464`) and during
    /// object resolution (`:1617`). Carrying this into the resolver lets it
    /// initialize `ResolverCore::reconstructed_xref` correctly so that a second
    /// full reconstruction scan is not performed when an object from an
    /// already-recovered table later fails to parse.
    pub(crate) already_reconstructed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefForm {
    Table,
    Stream,
}

#[derive(Debug, Clone, Copy)]
enum ParsedXrefEntry {
    Live {
        object_ref: QpdfObjGen,
        entry: XrefEntry,
    },
    Free {
        object_ref: QpdfObjGen,
    },
}

#[derive(Debug, Default)]
struct XrefRegistration {
    /// qpdf's raw `m->xref_table`, retained through registration so an invalid
    /// generation still participates in exact-key/free-row precedence before
    /// the valid indirect-reference boundary is applied.
    raw_entries: BTreeMap<QpdfObjGen, XrefEntry>,
    /// Effective live rows after the qpdf raw identity has crossed the parsed
    /// indirect-reference boundary. Existing resolver consumers intentionally
    /// remain on this valid `ObjectRef` view.
    entries: BTreeMap<ObjectRef, XrefEntry>,
    /// A construction-only, object-number-wide free-row filter. A normal
    /// qpdf `read_xref` registration retains it through `/Size` validation,
    /// then clears it (`QPDF.cc:686-708`). `reconstruct_xref` instead clears
    /// its line-scan filter immediately at `QPDF.cc:575`, before the optional
    /// candidate xref-stream re-read at `:576-607`; that re-read gets a fresh
    /// registration with its own normal lifetime. It deliberately never
    /// crosses into `ResolverCore`: canonical cache/xref replacement and
    /// removal are a separate `Pdf` mutation boundary.
    deleted_objects: BTreeSet<u32>,
}

impl XrefRegistration {
    /// qpdf `insertXrefEntry`: a deleted object number suppresses every later
    /// live registration, while an exact object-generation collision is
    /// first-wins because sections are read newest to oldest.
    fn insert_xref_entry(&mut self, key: QpdfObjGen, entry: XrefEntry) {
        let Some(object_number) = u32::try_from(key.get_obj()).ok() else {
            return;
        };
        if self.deleted_objects.contains(&object_number) {
            return;
        }
        // qpdf uses `try_emplace` and returns when the row already exists
        // (`QPDF.cc:1164-1168`), so the first section read wins the value as
        // well as the key.
        match self.raw_entries.entry(key) {
            std::collections::btree_map::Entry::Occupied(_) => return,
            std::collections::btree_map::Entry::Vacant(slot) => slot.insert(entry),
        };
        if let Some(object_ref) = key.to_object_ref() {
            self.entries.entry(object_ref).or_insert(entry);
        }
    }

    /// qpdf `insertFreeXrefEntry`: free rows are represented only by the
    /// object-number tombstone, and a matching exact live generation wins.
    fn insert_free_xref_entry(&mut self, key: QpdfObjGen) {
        let Some(object_number) = u32::try_from(key.get_obj()).ok() else {
            return;
        };
        if !self.raw_entries.contains_key(&key) {
            self.deleted_objects.insert(object_number);
        }
    }

    /// The highest object number in the raw table, which is what qpdf's
    /// `/Size` check reads (`QPDF.cc:691-693`).
    fn highest_raw_object_number(&self) -> i64 {
        self.raw_entries
            .keys()
            .map(|key| key.get_obj())
            .max()
            .unwrap_or(0)
    }

    fn contains_raw_key(&self, key: QpdfObjGen) -> bool {
        self.raw_entries.contains_key(&key)
    }

    fn replace_effective_entries(&mut self, entries: BTreeMap<ObjectRef, XrefEntry>) -> Result<()> {
        self.raw_entries = entries
            .iter()
            .map(|(object_ref, entry)| Ok((QpdfObjGen::try_from_object_ref(*object_ref)?, *entry)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        self.entries = entries;
        Ok(())
    }

    fn snapshot(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        self.entries.clone()
    }

    fn raw_snapshot(&self) -> BTreeMap<QpdfObjGen, XrefEntry> {
        self.raw_entries.clone()
    }
}

fn seed_candidate_reentry_registration(
    line_scan_entries: &BTreeMap<ObjectRef, XrefEntry>,
    preexisting_raw_entries: Option<&BTreeMap<QpdfObjGen, XrefEntry>>,
) -> Result<XrefRegistration> {
    let mut registration = XrefRegistration::default();
    registration.replace_effective_entries(line_scan_entries.clone())?;
    if let Some(raw_entries) = preexisting_raw_entries {
        // qpdf removes all type-1 rows before reconstruction, then repopulates
        // them from the line scan. Only the surviving type-0/type-2 rows from
        // the previous table may seed the candidate re-entry.
        for (&object_ref, &entry) in raw_entries {
            if !matches!(entry, XrefEntry::Uncompressed { .. }) {
                registration.raw_entries.entry(object_ref).or_insert(entry);
            }
        }
    }
    Ok(registration)
}

/// Keep only the effective highest-generation row for each object number
/// after the complete cross-reference chain has been registered.
///
/// This is QPDF::read_xref's post-chain loop at QPDF.cc:710-718, discarding
/// the same way QPDF::removeObject does at QPDF.cc:1996-2005: erase the
/// xref-table row (`entries`) and, for a discarded generation that was
/// already read and cached while walking the chain, the object-cache entry
/// too. `parsed_xref_streams` is flpdf's pre-`Pdf`-construction stand-in for
/// `m->obj_cache` (see `install_parsed_xref_stream_handles`'s doc) --
/// leaving a discarded xref-stream object there would let it resurface as a
/// live handle even though its xref row is gone.
fn discard_lower_generations(
    raw_entries: &mut BTreeMap<QpdfObjGen, XrefEntry>,
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    parsed_xref_streams: &mut BTreeMap<ObjectRef, ObjectHandle>,
) {
    let mut previous: Option<QpdfObjGen> = None;
    let mut lower_generations = Vec::new();
    for &object_ref in raw_entries.keys() {
        if let Some(previous_ref) = previous {
            if previous_ref.get_obj() == object_ref.get_obj() && object_ref.get_obj() > 0 {
                lower_generations.push(previous_ref);
            }
        }
        previous = Some(object_ref);
    }
    for object_ref in lower_generations {
        raw_entries.remove(&object_ref);
        if let Some(object_ref) = object_ref.to_object_ref() {
            entries.remove(&object_ref);
            parsed_xref_streams.remove(&object_ref);
        }
    }
}

/// The `QPDF::Members` settings the cross-reference loader consults, carried
/// together the way qpdf keeps them on `m` rather than as parallel arguments.
#[derive(Debug, Clone, Default)]
pub(crate) struct XrefLoadOptions {
    /// qpdf `m->attempt_recovery`: run the reconstruction pass when the strict
    /// parse fails.
    pub(crate) allow_repair: bool,
    /// qpdf `m->ignore_xref_streams` (`QPDF::setIgnoreXRefStreams`): never read
    /// a cross-reference stream.
    pub(crate) ignore_xref_streams: bool,
    /// qpdf's input source name, retained in every warning's QPDFExc.
    pub(crate) description: Vec<u8>,
}

fn damaged_warning(
    filename: &[u8],
    object: impl AsRef<[u8]>,
    message: impl Into<String>,
    offset: Option<u64>,
) -> QpdfExc {
    QpdfExc::new(
        QpdfErrorCode::DamagedPdf,
        filename,
        object,
        offset
            .map(|value| i64::try_from(value).unwrap_or(i64::MAX))
            .unwrap_or(0),
        message.into().into_bytes(),
    )
}

/// The owner-less context used by the standalone xref loader while an xref
/// section is being read.
///
/// `Pdf::open` constructs its canonical `ResolverCore` before invoking the
/// loader and therefore does not enter this context. The three construction
/// sites retained here mirror the qpdf call-order contexts needed by the
/// standalone reconstruction implementation: reconstruction's complete
/// line-scan table, the current classic/hybrid section, and a section reached
/// through `/Prev`.
#[derive(Debug, Clone, Copy)]
enum XrefReadContextSpec<'a> {
    ActiveSection,
    ActiveSectionWithCache {
        bootstrap_cache: &'a SharedBootstrapCache,
    },
    Reconstruction {
        line_scan_entries: &'a BTreeMap<ObjectRef, XrefEntry>,
        reference_offsets: &'a Rc<[u64]>,
    },
    ReconstructionWithCache {
        line_scan_entries: &'a BTreeMap<ObjectRef, XrefEntry>,
        reference_offsets: &'a Rc<[u64]>,
        bootstrap_cache: &'a SharedBootstrapCache,
    },
}

fn context_spec_without_bootstrap_cache<'a>(
    context_spec: XrefReadContextSpec<'a>,
) -> XrefReadContextSpec<'a> {
    match context_spec {
        XrefReadContextSpec::ActiveSection | XrefReadContextSpec::ActiveSectionWithCache { .. } => {
            XrefReadContextSpec::ActiveSection
        }
        XrefReadContextSpec::Reconstruction {
            line_scan_entries,
            reference_offsets,
        }
        | XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries,
            reference_offsets,
            ..
        } => XrefReadContextSpec::Reconstruction {
            line_scan_entries,
            reference_offsets,
        },
    }
}

fn context_spec_with_bootstrap_cache<'a>(
    context_spec: XrefReadContextSpec<'a>,
    bootstrap_cache: &'a SharedBootstrapCache,
) -> XrefReadContextSpec<'a> {
    match context_spec {
        XrefReadContextSpec::ActiveSection | XrefReadContextSpec::ActiveSectionWithCache { .. } => {
            XrefReadContextSpec::ActiveSectionWithCache { bootstrap_cache }
        }
        XrefReadContextSpec::Reconstruction {
            line_scan_entries,
            reference_offsets,
        }
        | XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries,
            reference_offsets,
            ..
        } => XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries,
            reference_offsets,
            bootstrap_cache,
        },
    }
}

/// The qpdf description passed to `readObjectAtOffset` for a bootstrap object.
/// Ordinary resolution uses an empty description, while
/// `QPDF::read_xrefStream` passes `"xref stream"` so file-object warnings carry
/// the same prefix (`QPDF.cc:949-963,1298-1313`).
#[derive(Debug, Clone, Copy)]
enum XrefObjectDescription {
    Ordinary,
    XrefStream,
    ObjStmMember {
        stream_number: u32,
        object_ref: ObjectRef,
    },
}

impl XrefObjectDescription {
    fn warning_prefix(self) -> &'static str {
        match self {
            Self::Ordinary => "",
            Self::XrefStream => "xref stream: ",
            Self::ObjStmMember { .. } => "", // cov:ignore: ObjStmMember is used only by direct member parsing, never by the file-object warning-prefix boundary
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum XrefEntryLookup<'a> {
    Registration(&'a BTreeMap<ObjectRef, XrefEntry>),
    Reconstruction {
        line_scan_entries: &'a BTreeMap<ObjectRef, XrefEntry>,
        registration_entries: &'a BTreeMap<ObjectRef, XrefEntry>,
    },
}

impl XrefEntryLookup<'_> {
    fn get(&self, object_ref: &ObjectRef) -> Option<XrefEntry> {
        let entry = match self {
            Self::Registration(entries) => entries.get(object_ref),
            Self::Reconstruction {
                line_scan_entries,
                registration_entries,
            } => line_scan_entries
                .get(object_ref)
                .or_else(|| registration_entries.get(object_ref)),
        };
        entry
            .filter(|entry| !matches!(entry, XrefEntry::Free { .. }))
            .copied()
    }

    fn owned_entries(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        let mut entries = BTreeMap::new();
        match self {
            Self::Registration(source) => {
                entries.extend(source.iter().map(|(key, value)| (*key, *value)))
            }
            Self::Reconstruction {
                line_scan_entries,
                registration_entries,
            } => {
                entries.extend(
                    registration_entries
                        .iter()
                        .map(|(key, value)| (*key, *value)),
                );
                entries.extend(line_scan_entries.iter().map(|(key, value)| (*key, *value)));
            }
        }
        entries.retain(|object_ref, entry| {
            !matches!(entry, XrefEntry::Free { .. }) && self.get(object_ref).is_some()
        });
        entries
    }
}

fn reconstructed_reference_offsets(entries: &BTreeMap<ObjectRef, XrefEntry>) -> Rc<[u64]> {
    let mut offsets: Vec<u64> = entries
        .values()
        .filter_map(|entry| match entry {
            XrefEntry::Uncompressed { offset } => Some(*offset),
            XrefEntry::Compressed { .. } | XrefEntry::Free { .. } => None,
        })
        .collect();
    offsets.sort_unstable();
    offsets.dedup();
    Rc::from(offsets)
}

/// The owner-less document context used by standalone xref loading and its
/// tests. The normal `Pdf::open` route supplies [`CanonicalTrailerOwner`]
/// instead, so it never constructs this pre-`Pdf` graph for xref objects. This
/// context still gives every parsed direct child the same weak
/// `DocumentResolver` and gives every `N G R` the same handle slot within one
/// standalone xref-loading operation.
struct BootstrapHandleDocument {
    /// The static resolver only retains a source snapshot after a bootstrap
    /// operation actually needs to read an indirect object. Direct parser
    /// values and shared diagnostics do not require the full input buffer.
    bytes: OnceCell<Rc<[u8]>>,
    entry_lookup: RefCell<BTreeMap<ObjectRef, XrefEntry>>,
    /// Sorted reconstructed object offsets used only to bound bootstrap
    /// reference reads during recovery. Normal active-section reads leave
    /// this unset and retain qpdf's unbounded source view.
    reference_offsets: RefCell<Option<Rc<[u64]>>>,
    options: XrefLoadOptions,
    state: Rc<RefCell<BootstrapHandleState>>,
    resolver: RefCell<Option<Weak<dyn DocumentResolver>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReferenceReadWindow {
    end: usize,
    fallback_end: usize,
}

/// A single attempt to read an uncompressed bootstrap object at a bounded
/// `end`, with its diagnostics held back (rather than pushed immediately)
/// until the caller decides whether this attempt or a wider retry wins.
struct UncompressedObjectRead {
    value: ObjectValue,
    parsed_offset: i64,
    end_before_space: i64,
    end_after_space: i64,
    /// Whether `complete_handle_stream` fell back to
    /// `recover_stream_boundary`'s heuristic `endstream`/`endobj` search
    /// (signaled by `FileObjectDiagnosticKind::AttemptingStreamLengthRecovery`)
    /// rather than validating the declared `/Length` boundary exactly. A
    /// truncated window can make that heuristic search accept a terminator
    /// -- empty or not -- that only exists because the real one lies beyond
    /// `end`, so this is the caller's signal to retry through the wider
    /// fallback window rather than trusting this attempt's result.
    used_heuristic_recovery: bool,
    diagnostics: Vec<QpdfExc>,
}

impl std::fmt::Debug for BootstrapHandleDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapHandleDocument")
            .field("cache_len", &self.state.borrow().handles.len())
            .field("resolving", &self.state.borrow().resolving)
            .finish()
    }
}

impl BootstrapHandleDocument {
    fn new_with_state(
        bytes: Option<&[u8]>,
        entry_lookup: XrefEntryLookup<'_>,
        options: XrefLoadOptions,
        state: Rc<RefCell<BootstrapHandleState>>,
    ) -> Rc<Self> {
        let source_bytes = OnceCell::new();
        if let Some(bytes) = bytes {
            let _ = source_bytes.set(Rc::from(bytes));
        }
        let document = Rc::new(Self {
            bytes: source_bytes,
            entry_lookup: RefCell::new(entry_lookup.owned_entries()),
            reference_offsets: RefCell::new(None),
            options: options.clone(),
            state,
            resolver: RefCell::new(None),
        });
        let resolver: Rc<dyn DocumentResolver> = document.clone();
        *document.resolver.borrow_mut() = Some(Rc::downgrade(&resolver));
        document
    }

    fn ensure_source_bytes(&self, bytes: &[u8]) {
        let _ = self.bytes.get_or_init(|| Rc::from(bytes));
    }

    fn refresh_entry_lookup(&self, entry_lookup: XrefEntryLookup<'_>) {
        *self.entry_lookup.borrow_mut() = entry_lookup.owned_entries();
    }

    fn set_reference_offsets(&self, reference_offsets: Option<Rc<[u64]>>) {
        *self.reference_offsets.borrow_mut() = reference_offsets;
    }

    fn reference_read_window(&self, offset: u64, source_len: usize) -> ReferenceReadWindow {
        let Some(offsets) = self.reference_offsets.borrow().as_ref().cloned() else {
            return ReferenceReadWindow {
                end: source_len,
                fallback_end: source_len,
            };
        };
        let next_offset_index = offsets.partition_point(|candidate| *candidate <= offset);
        let end = offsets
            .get(next_offset_index)
            .and_then(|offset| usize::try_from(*offset).ok())
            .unwrap_or(source_len)
            .min(source_len);
        let fallback_index = next_offset_index
            .saturating_add(XREF_RECONSTRUCTION_FALLBACK_SPAN)
            .min(offsets.len());
        let fallback_end = offsets
            .get(fallback_index)
            .and_then(|offset| usize::try_from(*offset).ok())
            .unwrap_or(source_len)
            .min(source_len);
        ReferenceReadWindow { end, fallback_end }
    }

    fn resolver_weak(&self) -> Weak<dyn DocumentResolver> {
        self.resolver
            .borrow()
            .clone()
            .expect("bootstrap document resolver is installed before parsing")
    }

    fn handle_for_reference(&self, object_ref: ObjectRef) -> ObjectHandle {
        if let Some(handle) = self.state.borrow().handles.get(&object_ref).cloned() {
            return handle;
        }
        let handle = ObjectHandle::new_indirect_with_resolver(object_ref, self.resolver_weak());
        self.state
            .borrow_mut()
            .handles
            .insert(object_ref, handle.clone());
        handle
    }

    fn push_warning(&self, message: impl Into<String>, offset: Option<u64>) {
        // cov:ignore-start: bootstrap diagnostics are emitted only by the recoverable xref paths, which are covered through their public loader
        self.state.borrow_mut().diagnostics.push(damaged_warning(
            &self.options.description,
            b"",
            message,
            offset,
        ));
        // cov:ignore-end
    }

    fn push_diagnostic(&self, diagnostic: QpdfExc) {
        self.state.borrow_mut().diagnostics.push(diagnostic);
    }

    fn object_policy(&self) -> RecoveryPolicy {
        if self.options.allow_repair {
            RecoveryPolicy::Bounded
        } else {
            RecoveryPolicy::RequireEndstream
        }
    }

    fn read_file_object(
        &self,
        input: &[u8],
        absolute_offset: u64,
        policy: RecoveryPolicy,
        description: XrefObjectDescription,
        source_bytes: &[u8],
    ) -> Result<HandleFileObjectRead> {
        let mut parser = BootstrapHandleParser {
            document: self,
            description,
        };
        let pending = parse_file_object_handle_syntax(input, &mut parser)?;
        let resolved_length = pending.indirect_length_ref().map(|object_ref| {
            self.ensure_source_bytes(source_bytes);
            stacker::maybe_grow(XREF_STACK_RED_ZONE, XREF_STACK_GROWTH_SIZE, || {
                self.resolve_length(object_ref)
            })
        });
        let completed = finish_file_object_handle(input, pending, resolved_length, policy)?;
        let absolute_offset = i64::try_from(absolute_offset).unwrap_or(i64::MAX);
        let rebase = |relative: i64| {
            if relative < 0 {
                -1
            } else {
                absolute_offset.saturating_add(relative)
            }
        };
        let (end_before_space, end_after_space) = completed.object.end_offsets();
        let end_before_space = rebase(end_before_space);
        let end_after_space = if end_before_space < 0 {
            rebase(end_after_space)
        } else {
            let scan_start = usize::try_from(end_before_space).unwrap_or(usize::MAX);
            let tail = source_bytes.get(scan_start..).unwrap_or_default();
            match tail.iter().position(|byte| !byte.is_ascii_whitespace()) {
                Some(index) => i64::try_from(scan_start.saturating_add(index)).unwrap_or(i64::MAX),
                None => return Err(Error::parse(source_bytes.len(), "EOF after endobj")),
            }
        };
        completed
            .object
            .set_end_offsets(end_before_space, end_after_space);
        let _ = description;
        Ok(completed)
    }

    // Mirrors qpdf's own three-way `/Length` classification (an integer, a
    // resolved null, or anything else) rather than collapsing "resolved but
    // not an integer" into the same Missing case as "genuinely absent" --
    // the raw parser's own `resolve_stream_length` (this file, further down)
    // keeps the same three-way split and must report the same
    // "/Length key in stream dictionary is not an integer" diagnostic for a
    // hybrid xref stream whose /Length resolves through the active classic
    // table to a non-integer object.
    fn resolve_length(&self, object_ref: ObjectRef) -> ResolvedStreamLength {
        let handle = self.handle_for_reference(object_ref);
        match handle.try_as_integer() {
            Ok(Some(value)) => ResolvedStreamLength::Integer(value),
            Ok(None) => {
                if handle.try_is_null().unwrap_or(true) {
                    ResolvedStreamLength::Missing
                } else {
                    ResolvedStreamLength::Invalid
                }
            }
            // A reconstruction trigger lets `resolve_indirect` propagate a
            // parse error so the caller can rebuild the xref table. Treat a
            // failed indirect length as missing for this framing attempt,
            // preserving the stream-boundary recovery fallback.
            Err(_) => ResolvedStreamLength::Missing,
        }
    }

    // qpdf-deviation-start: qpdf 11.9.0 reads referenced bootstrap objects to EOF; reconstruction-only windows bound flpdf recovery work
    fn read_uncompressed_object(
        &self,
        object_ref: ObjectRef,
        offset: u64,
    ) -> Result<(ObjectValue, i64, i64, i64)> {
        let start = usize::try_from(offset)
            .ok()
            .ok_or_else(|| Error::parse(0, "object offset does not fit usize"))?;
        let source_bytes = Rc::clone(self.bytes.get().ok_or_else(|| {
            Error::Internal("bootstrap resolver source bytes were not initialized".to_owned())
        })?);
        let window = self.reference_read_window(offset, source_bytes.len());
        let narrow = self.read_uncompressed_object_with_end(
            object_ref,
            offset,
            start,
            window.end,
            &source_bytes,
        );
        // A truncated window can also make `read_uncompressed_object_with_end`
        // *succeed* with a bogus result instead of failing: under
        // `RecoveryPolicy::Bounded`, a stream whose real `endstream`/`endobj`
        // terminator lies beyond `window.end` makes `complete_handle_stream`
        // fall back to `recover_stream_boundary`'s heuristic search, which can
        // accept a terminator -- empty recovered data, or a real but earlier
        // `endobj`/`endstream` match, either way short of the true boundary --
        // purely because the window cut off what lies past it. Retry through
        // the wider fallback window whenever that heuristic path was taken,
        // not only when the narrow attempt's result happened to come back
        // empty or an outright `Err`.
        let should_retry = window.fallback_end > window.end
            && match &narrow {
                Err(Error::Parse { .. }) => true,
                Ok(read) => read.used_heuristic_recovery,
                Err(_) => false, // cov:ignore: the only non-Parse error this attempt can return is the already-defensive "did not produce a direct value" Internal error below, which the parser's own contract makes unreachable
            };
        let accepted = if should_retry {
            self.read_uncompressed_object_with_end(
                object_ref,
                offset,
                start,
                window.fallback_end,
                &source_bytes,
            )
        } else {
            narrow
        }?;
        // Discard whichever attempt was not accepted rather than pushing its
        // diagnostics too: a document qpdf could read with a single unbounded
        // pass must not surface warnings from a speculative narrow read this
        // window-bounding mechanism alone introduced.
        for diagnostic in accepted.diagnostics {
            self.push_diagnostic(diagnostic);
        }
        Ok((
            accepted.value,
            accepted.parsed_offset,
            accepted.end_before_space,
            accepted.end_after_space,
        ))
    }

    fn read_uncompressed_object_with_end(
        &self,
        object_ref: ObjectRef,
        offset: u64,
        start: usize,
        end: usize,
        source_bytes: &[u8],
    ) -> Result<UncompressedObjectRead> {
        let input = source_bytes
            .get(start..end)
            .ok_or_else(|| Error::parse(start, "object is beyond the end of the file"))?;
        let policy = self.object_policy();
        let actual_object_ref =
            parse_file_object_header(input).map_err(|error| error.rebase_offset(start))?;
        if actual_object_ref.number == 0 {
            return Err(Error::parse(start, "object with ID 0"));
        }
        if actual_object_ref != object_ref {
            let message = format!(
                "expected {} {} obj",
                object_ref.number, object_ref.generation
            );
            if self.options.allow_repair || policy == RecoveryPolicy::Bounded {
                let mut state = self.state.borrow_mut();
                let trigger = &mut state.reconstruction_trigger;
                if trigger.is_none() {
                    *trigger = Some((offset, message.clone()));
                }
                return Err(Error::parse(start, message));
            }
            self.push_warning(
                format!(
                    "(object {} {}, offset {}): {}",
                    object_ref.number, object_ref.generation, offset, message
                ),
                Some(offset),
            );
        }
        // Rebase by `start`, matching the header parse immediately above:
        // `input` is already the offset-relative tail slice, so an error
        // surfaced from parsing its body (e.g. "trailing bytes after
        // object") reports an offset relative to `input`, not the file,
        // unless rebased here.
        let mut completed = self
            .read_file_object(
                input,
                offset,
                policy,
                XrefObjectDescription::Ordinary,
                source_bytes,
            )
            .map_err(|error| error.rebase_offset(start))?;
        let used_heuristic_recovery = completed.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.kind,
                FileObjectDiagnosticKind::AttemptingStreamLengthRecovery
            )
        });
        let diagnostics = completed
            .diagnostics
            .iter()
            .map(|diagnostic| {
                xref_file_object_diagnostic(
                    XrefObjectDescription::Ordinary,
                    completed.object_ref,
                    offset,
                    &self.options.description,
                    diagnostic.clone(),
                )
            })
            .collect();
        let parsed_offset = completed.object.get_parsed_offset();
        let (end_before_space, end_after_space) = completed.object.end_offsets();
        let _ = completed.remove_included_recovery_eol_for_decryption();
        // cov:ignore-start: the handle parser guarantees an exclusively owned direct top-level value
        let value = completed.object.into_direct_value().ok_or_else(|| {
            Error::Internal(format!(
                "bootstrap object {} {} did not produce a direct value",
                object_ref.number, object_ref.generation
            ))
        })?;
        // cov:ignore-end
        Ok(UncompressedObjectRead {
            value: value.0,
            parsed_offset,
            end_before_space,
            end_after_space,
            used_heuristic_recovery,
            diagnostics,
        })
    }
    // qpdf-deviation-end

    fn resolve_objects_in_stream(&self, stream_number: u32) -> Result<()> {
        if !self
            .state
            .borrow_mut()
            .resolved_object_streams
            .insert(stream_number)
        {
            return Ok(());
        }

        let stream_handle = self.handle_for_reference(ObjectRef::new(stream_number, 0));
        stream_handle.try_dereference()?;
        let (stream_end_before_space, stream_end_after_space) = stream_handle.end_offsets();
        let stream_dict = stream_handle.as_stream_dict().ok_or_else(|| {
            Error::parse(
                0,
                format!("supposed object stream {stream_number} is not a stream"),
            )
        })?;
        if !stream_dict.try_is_dictionary_of_type(b"ObjStm", b"")? {
            self.push_warning(
                format!("supposed object stream {stream_number} has wrong type"),
                None,
            );
        }
        let object_count = self.handle_integer(&stream_dict, b"/N", "object stream /N")?;
        let first = self.handle_integer(&stream_dict, b"/First", "object stream /First")?;
        // qpdf calls getStreamData(qpdf_dl_specialized) here, which routes
        // through the stream's canonical filter pipeline rather than a
        // separate whole-buffer decoder (`QPDF.cc:1792`).
        let decoded = stream_handle.get_stream_data(DecodeLevel::Specialized)?;

        let mut tokenizer = Tokenizer::new(&decoded);
        let mut members = BTreeMap::new();
        for _ in 0..object_count {
            let object_number = u32::try_from(tokenizer.next_object_stream_integer()?)
                .map_err(|_| Error::parse(0, "object stream object number is invalid"))?;
            let object_offset = usize::try_from(tokenizer.next_object_stream_integer()?)
                .map_err(|_| Error::parse(0, "object stream object offset is invalid"))?;
            members.insert(object_number, object_offset);
        }

        for (object_number, object_offset) in members {
            let object_ref = ObjectRef::new(object_number, 0);
            let entry = self.entry_lookup.borrow().get(&object_ref).copied();
            if !matches!(entry, Some(XrefEntry::Compressed { stream, .. }) if stream == stream_number)
            {
                if entry.is_none() {
                    // qpdf's `m->xref_table[og]` inserts a default type-0
                    // entry when an ObjStm header names an absent member
                    // (`QPDF.cc:1821-1828`).
                    self.entry_lookup
                        .borrow_mut()
                        .insert(object_ref, XrefEntry::Free { next: 0 });
                }
                continue;
            }
            let member_start = first
                .checked_add(object_offset)
                .ok_or_else(|| Error::parse(0, "object stream member offset overflow"))?;
            // qpdf seeks to the member offset even when it is past the
            // decoded payload, then lets readObjectInStream report the normal
            // EOF/empty-object result (`QPDF.cc:1825-1828`).
            let diagnostic_start = member_start.min(decoded.len());
            let member_data = decoded.get(diagnostic_start..).unwrap_or_default();
            let mut parser = BootstrapHandleParser {
                document: self,
                description: XrefObjectDescription::ObjStmMember {
                    stream_number,
                    object_ref,
                },
            };
            let (value, parsed_offset, diagnostics) =
                match parse_qpdf_direct_object_handle_with_diagnostics(
                    member_data,
                    i64::try_from(diagnostic_start).unwrap_or(i64::MAX),
                    Some(i64::try_from(diagnostic_start).unwrap_or(i64::MAX)),
                    &mut parser,
                ) {
                    Ok(parsed) => parsed,
                    // Mirror `XrefReadContext::resolve_objects_in_stream`'s own
                    // member-context wrapping: a raw parse error must carry the
                    // same "object stream N (object M 0, offset ...)" identity
                    // and rebased offset a successfully-parsed member's
                    // diagnostics already get below, not the raw member-relative
                    // offset/message.
                    Err(error) => {
                        return Err(match error.rebase_offset(diagnostic_start) {
                        Error::Parse { offset, message } => Error::parse(
                            offset,
                            format!(
                                "object stream {stream_number} (object {} 0, offset {offset}): {message}",
                                object_ref.number
                            ),
                        ),
                        other => other, // cov:ignore: byte-backed direct parser errors are parse errors
                    });
                    }
                };
            let malformed = !diagnostics.is_empty() && matches!(&value, ObjectValue::Null);
            for diagnostic in diagnostics {
                let offset = diagnostic_start.saturating_add(diagnostic.relative_offset);
                self.push_warning(
                    format!(
                        "object stream {stream_number} (object {} 0, offset {offset}): {}",
                        object_ref.number, diagnostic.message
                    ),
                    Some(offset as u64),
                );
            }
            // qpdf's `resolveObjectsInStream` always parses the member and
            // `updateCache` overwrites an existing cache slot
            // (`libqpdf/QPDF.cc:1821-1828,1843-1857`). A reentrant lookup
            // during stream-dictionary resolution can have temporarily
            // resolved this handle to null, so do not let that provisional
            // value suppress the real member parse.
            let member_handle = self.handle_for_reference(object_ref);
            member_handle.set_resolved(value);
            if !malformed {
                member_handle.set_parsed_offset_if_unset(parsed_offset);
                member_handle.set_end_offsets(stream_end_before_space, stream_end_after_space);
                if parsed_offset >= 0 && !member_handle.is_null() {
                    member_handle.set_description(
                        self.object_description_template(stream_number, object_ref),
                        parsed_offset,
                    );
                }
            }
        }
        Ok(())
    }

    fn object_description_template(&self, stream_number: u32, object_ref: ObjectRef) -> Vec<u8> {
        // qpdf renders an ObjStm member's warning from three pieces: the
        // decoded InputSource name (`<file> object stream N`, built at
        // `libqpdf/QPDF.cc:1796`), the parser's own description string
        // (`object M 0`, `QPDF.cc:1451-1459`) and the parsed offset --
        // `QPDFParser::warn` passes all three into `QPDFExc`
        // (`libqpdf/QPDFParser.cc:509-513`). flpdf's description template
        // carries that whole rendered prefix, with `$PO` standing in for the
        // offset (`crates/flpdf/src/object_handle.rs:940`), so it has to keep
        // the input description and the stream number. This is the same shape
        // the canonical reader produces
        // (`reader/resolver.rs::object_stream_description_template`).
        let mut description = self.options.description.clone();
        description.extend_from_slice(
            format!(
                " object stream {stream_number}, object {} {} at offset $PO",
                object_ref.number, object_ref.generation
            )
            .as_bytes(),
        );
        description
    }

    fn handle_integer(&self, dictionary: &ObjectHandle, key: &[u8], label: &str) -> Result<usize> {
        let value = dictionary.try_get_key(key)?.try_as_integer()?;
        let value = value.ok_or_else(|| Error::parse(0, format!("{label} is not an integer")))?;
        usize::try_from(value).map_err(|_| Error::parse(0, format!("{label} is invalid")))
    }
}

struct BootstrapHandleParser<'document> {
    document: &'document BootstrapHandleDocument,
    description: XrefObjectDescription,
}

struct CanonicalTrailerParser<'document> {
    owner: &'document dyn CanonicalTrailerOwner,
}

impl HandleResolver for CanonicalTrailerParser<'_> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.owner.indirect_handle(object_ref)
    }

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        self.owner.direct_handle(value)
    }

    fn description_template(&self) -> Option<Vec<u8>> {
        None
    }

    fn begin_parse(&self) -> Result<()> {
        // The classic trailer is parsed by the document-owned resolver, and
        // qpdf holds the guard there: `QPDF::readTrailer` builds its parser
        // with `this` (`libqpdf/QPDF.cc:1317`), exactly as `readObject` and
        // `readObjectInStream` do.
        self.owner.begin_parse()
    }

    fn end_parse(&self) {
        self.owner.end_parse();
    }
}

impl HandleResolver for BootstrapHandleParser<'_> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.document.handle_for_reference(object_ref)
    }

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        ObjectHandle::from_parsed_value_with_resolver(value, self.document.resolver_weak())
    }

    fn description_template(&self) -> Option<Vec<u8>> {
        Some(match self.description {
            XrefObjectDescription::Ordinary => b"object $OG".to_vec(),
            XrefObjectDescription::XrefStream => b"xref stream: object $OG".to_vec(),
            XrefObjectDescription::ObjStmMember {
                stream_number,
                object_ref,
            } => self
                .document
                .object_description_template(stream_number, object_ref),
        })
    }
}

impl DocumentResolver for BootstrapHandleDocument {
    fn input_description(&self) -> Vec<u8> {
        self.options.description.clone()
    }

    fn warn(&self, warning: QpdfExc) -> Result<()> {
        // qpdf's QPDFObjectHandle::objectWarning/typeWarning boundary hands
        // an already-formed exception to QPDF::warn, which appends it to the
        // document warning collection (QPDF.cc:487-494). The owner-less
        // bootstrap has no logger of its own; its state diagnostics are the
        // standalone equivalent and are replayed by the xref loader.
        self.push_diagnostic(warning);
        Ok(())
    }

    fn warn_stream_data(&self, warning: QpdfExc) -> Result<()> {
        // QPDF_Stream::warn uses the stream parsed offset and lets QPDF::warn
        // collect the warning without aborting decoding
        // (QPDF_Stream.cc:695-698, QPDF.cc:487-494). Keep this separate
        // trait boundary so ObjectHandle::stream_data_warning can preserve
        // the input description and offset while retaining the decoded
        // ObjStm member.
        self.push_diagnostic(warning);
        Ok(())
    }

    // qpdf-deviation: qpdf has no stack-growth policy; grow the Rust bootstrap resolver stack instead of imposing an arbitrary depth cap
    fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
        stacker::maybe_grow(XREF_STACK_RED_ZONE, XREF_STACK_GROWTH_SIZE, || {
            self.resolve_indirect_inner(object_ref, handle)
        })
    }
}

impl BootstrapHandleDocument {
    #[inline(never)]
    fn resolve_indirect_inner(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
        if handle.is_resolved() {
            return Ok(());
        }
        if !self.state.borrow_mut().resolving.insert(object_ref) {
            self.push_warning(
                format!(
                    "loop detected resolving object {} {}",
                    object_ref.number, object_ref.generation
                ),
                None,
            );
            handle.set_resolved(ObjectValue::Null);
            return Ok(());
        }

        // Snapshot the row before the `match`: a temporary `Ref` produced in
        // a scrutinee lives for the whole match expression, and the compressed
        // arm below reaches `resolve_objects_in_stream`, which inserts qpdf's
        // default free row for a header member absent from the table
        // (`QPDF.cc:1821-1828`) through `entry_lookup.borrow_mut()`.
        let entry = self.entry_lookup.borrow().get(&object_ref).copied();
        let result = match entry {
            Some(XrefEntry::Uncompressed { offset }) if offset != 0 => {
                self.read_uncompressed_object(object_ref, offset)
            }
            Some(XrefEntry::Uncompressed { .. }) => {
                self.push_warning("object has offset 0", Some(0));
                Ok((ObjectValue::Null, -1, -1, -1))
            }
            // qpdf's `resolve` wraps `resolveObjectsInStream` in the same
            // try/catch as the type-1 branch above (`QPDF.cc:1719-1734`): a
            // malformed object stream (e.g. non-integer `/N` or `/First`,
            // `QPDF.cc:1782-1784`) is caught and warned, then falls through
            // to `updateCache(og, QPDF_Null::create(), -1, -1)`
            // (`QPDF.cc:1744-1747`) instead of aborting resolution. Route
            // through `result`/the `Err` arm below rather than `?` so that
            // catch-and-null fallback, and the `resolving` guard removal
            // just past this match, both still run on this path.
            Some(XrefEntry::Compressed { stream, .. }) => {
                match self.resolve_objects_in_stream(stream) {
                    Ok(()) => {
                        if handle.is_resolved() {
                            self.state.borrow_mut().resolving.remove(&object_ref);
                            return Ok(());
                        }
                        Ok((ObjectValue::Null, -1, -1, -1))
                    }
                    Err(error) => Err(error),
                }
            }
            Some(XrefEntry::Free { .. }) | None => Ok((ObjectValue::Null, -1, -1, -1)),
        };
        self.state.borrow_mut().resolving.remove(&object_ref);

        match result {
            Ok((value, parsed_offset, end_before_space, end_after_space)) => {
                handle.set_resolved(value);
                handle.set_parsed_offset_if_unset(parsed_offset);
                handle.set_end_offsets(end_before_space, end_after_space);
            }
            Err(error) => {
                // A header-generation mismatch is qpdf's reconstruction
                // trigger. Keep the requested handle unresolved until the
                // caller rebuilds the xref table; caching null here would
                // prevent the post-reconstruction `/Size` lookup from
                // resolving the same indirect value against the repaired
                // entries.
                if self.state.borrow().reconstruction_trigger.is_some() {
                    return Err(error); // cov:ignore: LLVM maps the tested reconstruction handoff return to the condition edge
                } // cov:ignore: LLVM maps the tested reconstruction handoff return to this closing branch edge
                  // cov:ignore-start: bootstrap byte reads expose only qpdf damage and parse failures; these transport fallbacks are defensive
                let warning = match error {
                    Error::QpdfExc(warning) => warning,
                    Error::Parse { offset, message } => QpdfExc::new(
                        QpdfErrorCode::DamagedPdf,
                        &self.options.description,
                        format!("object {} {}", object_ref.number, object_ref.generation),
                        i64::try_from(offset).unwrap_or(i64::MAX),
                        message,
                    ),
                    Error::SystemBytes(message) => QpdfExc::new(
                        QpdfErrorCode::DamagedPdf,
                        &self.options.description,
                        format!("object {} {}", object_ref.number, object_ref.generation),
                        0,
                        message,
                    ),
                    Error::System(message) | Error::Internal(message) => QpdfExc::new(
                        QpdfErrorCode::DamagedPdf,
                        &self.options.description,
                        format!("object {} {}", object_ref.number, object_ref.generation),
                        0,
                        message,
                    ),
                    Error::Io(error) => QpdfExc::new(
                        QpdfErrorCode::DamagedPdf,
                        &self.options.description,
                        format!("object {} {}", object_ref.number, object_ref.generation),
                        0,
                        error.to_string(),
                    ),
                    other => return Err(other),
                };
                // cov:ignore-end
                self.push_diagnostic(warning);
                handle.set_resolved(ObjectValue::Null);
            }
        }
        Ok(())
    }
}

/// A short-lived view over the shared handle cache used while a cross-reference
/// section is being loaded. The cache is already canonical; this wrapper only
/// retains the old load-phase commit points so the section/recovery code can
/// keep its qpdf ordering without introducing a second value model.
#[derive(Debug, Clone)]
struct XrefHandleCache {
    shared: SharedBootstrapCache,
}

impl XrefHandleCache {
    fn get(&self, object_ref: &ObjectRef) -> Option<ObjectHandle> {
        self.shared
            .borrow()
            .handle_state
            .borrow()
            .handles
            .get(object_ref)
            .cloned()
            .filter(|handle| handle.is_resolved())
    }

    fn insert(&self, object_ref: ObjectRef, handle: ObjectHandle) {
        self.shared
            .borrow_mut()
            .handle_state
            .borrow_mut()
            .handles
            .insert(object_ref, handle);
    }

    fn commit(&mut self) {}

    fn shared(&self) -> SharedBootstrapCache {
        Rc::clone(&self.shared)
    }
}

/// Handle-only bootstrap context for qpdf's pre-`Pdf` xref reads.
struct XrefReadContext<'bytes> {
    bytes: &'bytes [u8],
    document: Rc<BootstrapHandleDocument>,
    cache: XrefHandleCache,
    diagnostics: Diagnostics,
    handle_diagnostics_len: usize,
}

struct XrefDetachedHandles;

impl HandleResolver for XrefDetachedHandles {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        ObjectHandle::new_indirect_unresolved(object_ref, -1)
    }

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        ObjectHandle::from_value(value)
    }
}

impl<'bytes> XrefReadContext<'bytes> {
    fn new(
        bytes: &'bytes [u8],
        spec: XrefReadContextSpec<'_>,
        registration: &XrefRegistration,
        options: XrefLoadOptions,
    ) -> Self {
        let (entry_lookup, shared, reference_offsets) = match spec {
            XrefReadContextSpec::ActiveSection => (
                XrefEntryLookup::Registration(&registration.entries),
                empty_bootstrap_cache(),
                None,
            ),
            XrefReadContextSpec::ActiveSectionWithCache { bootstrap_cache } => (
                XrefEntryLookup::Registration(&registration.entries),
                Rc::clone(bootstrap_cache),
                None,
            ),
            XrefReadContextSpec::Reconstruction {
                line_scan_entries,
                reference_offsets,
            } => (
                XrefEntryLookup::Reconstruction {
                    line_scan_entries,
                    registration_entries: &registration.entries,
                },
                empty_bootstrap_cache(),
                Some(Rc::clone(reference_offsets)),
            ),
            XrefReadContextSpec::ReconstructionWithCache {
                line_scan_entries,
                reference_offsets,
                bootstrap_cache,
            } => (
                XrefEntryLookup::Reconstruction {
                    line_scan_entries,
                    registration_entries: &registration.entries,
                },
                Rc::clone(bootstrap_cache),
                Some(Rc::clone(reference_offsets)),
            ),
        };
        let document = {
            let mut cache = shared.borrow_mut();
            if let Some(document) = cache.handle_document.as_ref() {
                document.refresh_entry_lookup(entry_lookup);
                Rc::clone(document)
            } else {
                let document = BootstrapHandleDocument::new_with_state(
                    None,
                    entry_lookup,
                    options.clone(),
                    Rc::clone(&cache.handle_state),
                );
                cache.handle_document = Some(Rc::clone(&document));
                document
            }
        };
        document.set_reference_offsets(reference_offsets);
        let handle_diagnostics_len = shared
            .borrow()
            .handle_state
            .borrow()
            .diagnostics
            .entries()
            .len();
        Self {
            bytes,
            document,
            cache: XrefHandleCache { shared },
            diagnostics: Diagnostics::default(),
            handle_diagnostics_len,
        }
    }

    fn read_file_object_handle(
        &mut self,
        input: &[u8],
        absolute_offset: u64,
        policy: RecoveryPolicy,
        description: XrefObjectDescription,
    ) -> Result<HandleFileObjectRead> {
        let result =
            self.document
                .read_file_object(input, absolute_offset, policy, description, self.bytes);
        self.sync_handle_diagnostics();
        result
    }

    fn resolve_dictionary_value(
        &mut self,
        dictionary: &ObjectHandle,
        key: &str,
    ) -> Option<ObjectHandle> {
        let mut name = Vec::with_capacity(key.len() + 1);
        name.push(b'/');
        name.extend_from_slice(key.as_bytes());
        self.ensure_source_for_resolution(dictionary);
        let value = dictionary.try_get_key(&name).ok()?;
        self.ensure_source_for_resolution(&value);
        let _ = value.try_dereference();
        self.sync_handle_diagnostics();
        Some(value)
    }

    fn ensure_source_for_resolution(&self, handle: &ObjectHandle) {
        if handle.object_ref().is_some() && !handle.is_resolved() {
            self.document.ensure_source_bytes(self.bytes);
        }
    }

    fn sync_handle_diagnostics(&mut self) {
        let shared = self.cache.shared.borrow();
        let state = shared.handle_state.borrow();
        for diagnostic in state
            .diagnostics
            .entries()
            .iter()
            .skip(self.handle_diagnostics_len)
        {
            self.diagnostics.push(diagnostic.clone());
        }
        self.handle_diagnostics_len = state.diagnostics.entries().len();
    }

    fn append_diagnostics_to(&mut self, diagnostics: &mut Diagnostics) {
        self.sync_handle_diagnostics();
        for diagnostic in self.diagnostics.entries() {
            diagnostics.push(diagnostic.clone());
        }
    }

    fn take_reconstruction_trigger(&self) -> Option<Error> {
        let shared = self.cache.shared.borrow();
        let trigger = shared
            .handle_state
            .borrow_mut()
            .reconstruction_trigger
            .take()
            .map(|(offset, message)| Error::parse(offset as usize, message));
        trigger
    }
}

trait XrefObjectContext {
    fn ensure_source_for_resolution(&self, handle: &ObjectHandle);
    fn resolve_dictionary_value(
        &mut self,
        dictionary: &ObjectHandle,
        key: &str,
    ) -> Option<ObjectHandle>;
    /// The xref stream's fully filter-decoded entry table.
    ///
    /// qpdf reads this in one call, `xref_obj.getStreamData(qpdf_dl_specialized)`
    /// (`libqpdf/QPDF.cc:1051`), over `QPDFObjectHandle::getStreamData`
    /// (`QPDFObjectHandle.cc:1289-1292`). Both the bootstrap and
    /// canonical-owner contexts use the resolver-backed handle's canonical
    /// `get_stream_data` pipe; bootstrap retains its bounded source-read
    /// policy in the resolver rather than a second materialized decoder.
    fn decoded_xref_stream_data(
        &mut self,
        object_ref: ObjectRef,
        stream_dict: &ObjectHandle,
        object: &ObjectHandle,
        xref_pos: usize,
    ) -> Result<Vec<u8>>;
    fn sync_handle_diagnostics(&mut self);
    fn append_diagnostics_to(&mut self, diagnostics: &mut Diagnostics);
    fn take_reconstruction_trigger(&mut self) -> Option<Error>;
    fn push_diagnostic(&mut self, diagnostic: QpdfExc);
    fn description(&self) -> &[u8];
}

impl XrefObjectContext for XrefReadContext<'_> {
    fn ensure_source_for_resolution(&self, handle: &ObjectHandle) {
        Self::ensure_source_for_resolution(self, handle);
    }

    fn resolve_dictionary_value(
        &mut self,
        dictionary: &ObjectHandle,
        key: &str,
    ) -> Option<ObjectHandle> {
        Self::resolve_dictionary_value(self, dictionary, key)
    }

    fn decoded_xref_stream_data(
        &mut self,
        _object_ref: ObjectRef,
        _stream_dict: &ObjectHandle,
        object: &ObjectHandle,
        _xref_pos: usize,
    ) -> Result<Vec<u8>> {
        let data = object.get_stream_data(DecodeLevel::Specialized)?;
        Ok((*data).clone())
    }

    fn sync_handle_diagnostics(&mut self) {
        Self::sync_handle_diagnostics(self);
    }

    fn append_diagnostics_to(&mut self, diagnostics: &mut Diagnostics) {
        Self::append_diagnostics_to(self, diagnostics);
    }

    fn take_reconstruction_trigger(&mut self) -> Option<Error> {
        Self::take_reconstruction_trigger(self)
    }

    fn push_diagnostic(&mut self, diagnostic: QpdfExc) {
        self.diagnostics.push(diagnostic);
    }

    fn description(&self) -> &[u8] {
        &self.document.options.description
    }
}

struct CanonicalXrefContext<'owner> {
    owner: &'owner dyn CanonicalTrailerOwner,
    description: Vec<u8>,
    diagnostics: Diagnostics,
    owner_diagnostics_start: usize,
    owner_diagnostics_synced: usize,
}

impl<'owner> CanonicalXrefContext<'owner> {
    fn new(owner: &'owner dyn CanonicalTrailerOwner, description: Vec<u8>) -> Self {
        let owner_diagnostics_start = owner.repair_diagnostics().entries().len();
        Self {
            owner,
            description,
            diagnostics: Diagnostics::default(),
            owner_diagnostics_start,
            owner_diagnostics_synced: 0,
        }
    }

    /// Track how far the owner's diagnostics have advanced, without copying
    /// them into this context.
    ///
    /// The document is the single emitter for its own warnings:
    /// `push_qpdf_warning` logs them and records them on the document, and
    /// `engine.rs` installs this context's collection onto that same
    /// document afterwards. Mirroring them here unconditionally would
    /// deliver one repair twice, where qpdf delivers it once because it
    /// reconstructs once per document (`libqpdf/QPDF.cc:518-522`). The
    /// counter is still advanced so a future consumer can tell what the
    /// owner added during this read. A caller wrapping this context's use in
    /// a caller delivers the local diagnostics through the same owner after
    /// the qpdf call-order boundary, so the two mechanisms do not double-count
    /// each other.
    fn sync_owner_diagnostics(&mut self) {
        self.owner_diagnostics_synced = self
            .owner
            .repair_diagnostics()
            .entries()
            .len()
            .saturating_sub(self.owner_diagnostics_start);
    }
}

impl XrefObjectContext for CanonicalXrefContext<'_> {
    fn ensure_source_for_resolution(&self, _handle: &ObjectHandle) {}

    fn resolve_dictionary_value(
        &mut self,
        dictionary: &ObjectHandle,
        key: &str,
    ) -> Option<ObjectHandle> {
        let mut name = Vec::with_capacity(key.len() + 1);
        name.push(b'/');
        name.extend_from_slice(key.as_bytes());
        let value = match dictionary.try_get_key(&name) {
            Ok(value) => value,
            Err(_) => {
                self.sync_owner_diagnostics();
                return None;
            }
        };
        let _ = value.try_dereference();
        self.sync_owner_diagnostics();
        Some(value)
    }

    fn decoded_xref_stream_data(
        &mut self,
        _object_ref: ObjectRef,
        _stream_dict: &ObjectHandle,
        object: &ObjectHandle,
        _xref_pos: usize,
    ) -> Result<Vec<u8>> {
        let data = object.get_stream_data(DecodeLevel::Specialized);
        self.sync_owner_diagnostics();
        Ok((*data?).clone())
    }

    fn sync_handle_diagnostics(&mut self) {
        self.sync_owner_diagnostics();
    }

    fn append_diagnostics_to(&mut self, diagnostics: &mut Diagnostics) {
        // Only this context's own diagnostics. The owner's warnings are
        // delivered directly by the canonical document sink, so mirroring
        // them here would report one repair twice.
        for diagnostic in self.diagnostics.entries() {
            diagnostics.push(diagnostic.clone());
        }
    }

    fn take_reconstruction_trigger(&mut self) -> Option<Error> {
        None
    }

    fn push_diagnostic(&mut self, diagnostic: QpdfExc) {
        self.diagnostics.push(diagnostic);
    }

    fn description(&self) -> &[u8] {
        &self.description
    }
}

#[cfg(test)]
fn detach_bootstrap_handle(source: &ObjectHandle) -> Result<ObjectHandle> {
    if let Some(object_ref) = source.object_ref() {
        return Ok(ObjectHandle::new_indirect_unresolved(
            object_ref,
            source.get_parsed_offset(),
        ));
    }

    source.try_dereference()?;
    let value = source
        .with_value(|value| value.cloned())
        .ok_or_else(|| Error::Internal("bootstrap trailer has no value".to_owned()))?;
    let value = match value {
        ObjectValue::Array(children) => ObjectValue::Array(
            children
                .iter()
                .map(detach_bootstrap_handle)
                .collect::<Result<Vec<_>>>()?,
        ),
        ObjectValue::Dictionary(entries) => ObjectValue::Dictionary(
            entries
                .into_iter()
                .map(|(key, child)| Ok((key, detach_bootstrap_handle(&child)?)))
                .collect::<Result<BTreeMap<_, _>>>()?,
        ),
        ObjectValue::Stream {
            stream_dict,
            stream_data,
            stream_provider,
            filter_on_write,
            stream_length,
        } => ObjectValue::Stream {
            stream_dict: detach_bootstrap_handle(&stream_dict)?,
            stream_data,
            stream_provider,
            filter_on_write,
            stream_length,
        },
        other => other,
    };
    Ok(ObjectHandle::from_value(value))
}

#[cfg(test)]
pub(crate) fn load_xref_state_with_options<R: Read + Seek>(
    reader: &mut R,
    options: XrefLoadOptions,
) -> Result<LoadedXrefState> {
    let mut source_bytes = Vec::new();
    reader.seek(SeekFrom::Start(0))?;
    reader.read_to_end(&mut source_bytes)?;

    load_xref_state_from_bytes(&source_bytes, options, None)
}

/// Load xref state from bytes that were read by the document's own input
/// source.  When `canonical_trailer_owner` is present, the initial classic
/// trailer is parsed directly into that owner instead of being rebuilt through
/// the short-lived bootstrap cache.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_xref_state_from_bytes(
    source_bytes: &[u8],
    options: XrefLoadOptions,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<LoadedXrefState> {
    let allow_repair = options.allow_repair;

    let mut initial_diagnostics = Diagnostics::default();
    // qpdf's QPDF::parse always calls findHeader before it considers
    // attempt_recovery (`QPDF.cc:429-438`). A missing or malformed header is a
    // warning, not a strict-open terminal parse error; qpdf falls back to PDF
    // 1.2 and continues to find/read startxref. Keep that warning in the same
    // document collection for both repair modes.
    let (version, header_offset) = match find_qpdf_header(source_bytes) {
        Some((offset, version)) => (version, offset),
        None => {
            initial_diagnostics.push(damaged_warning(
                &options.description,
                b"",
                "can't find PDF header",
                None,
            ));
            ("1.2".to_string(), 0)
        }
    };
    if let Some(owner) = canonical_trailer_owner {
        owner.set_header_offset(header_offset);
    }
    deliver_canonical_diagnostics(canonical_trailer_owner, &mut initial_diagnostics)?;
    let bytes = &source_bytes[header_offset..];
    let mut parse_errors = Vec::new();
    // qpdf-deviation-start: qpdf's xref_offset == 0 check
    // (`QPDF.cc:450-452`) throws damagedPDF("can't find startxref")
    // immediately and never calls read_xref at all -- whether xref_offset is
    // 0 because startxref itself could not be parsed, or because a
    // syntactically valid `startxref` explicitly names offset 0.
    // parse_startxref's Ok(0) case (an explicit zero) and the Err fallback
    // below (a missing/malformed startxref) both leave `startxref == 0`
    // here. Below, the canonical-owner + repair-mode combination now skips
    // the retry entirely (matching qpdf exactly, see the comment there); the
    // remaining owner-less recovery path is entered only after this qpdf
    // guard has classified the zero offset as the `can't find startxref`
    // trigger; no route reads a speculative xref at logical offset zero.
    let startxref = match parse_startxref(bytes) {
        Ok(offset) => offset,
        Err(error) if allow_repair => {
            parse_errors.push(error);
            0
        }
        Err(error) => return Err(error),
    };
    let xref_pos = match usize::try_from(startxref) {
        Ok(xref_pos) => xref_pos,
        // cov:ignore-start: converting the u64 startxref offset can overflow
        // only on a 32-bit target; the supported CI target is 64-bit.
        Err(_) if allow_repair => {
            parse_errors.push(Error::parse(0, "startxref does not fit usize"));
            0
        }
        // cov:ignore-end
        Err(_) => return Err(Error::parse(0, "startxref does not fit usize")), // cov:ignore: the same u64-to-usize overflow is unrepresentable on the supported target
    };

    if startxref == 0 {
        if !allow_repair {
            return Err(Error::parse(0, "can't find startxref"));
        }
        let trigger = parse_errors
            .into_iter()
            .next()
            .unwrap_or_else(|| Error::parse(0, "can't find startxref"));
        let mut recovered = recover_xref_from_linear_scan(
            bytes,
            version,
            startxref,
            trigger,
            None,
            None,
            None,
            None,
            options.clone(),
            initial_diagnostics,
            None,
            canonical_trailer_owner,
        )?;
        recovered.header_offset = header_offset;
        return Ok(recovered);
    }

    let mut registration = XrefRegistration::default();
    let mut initial_parse_diagnostics = Diagnostics::default();
    let mut observed_first_xref_item_offset = None;
    let initial_bootstrap_cache = canonical_trailer_owner
        .is_none()
        .then(empty_bootstrap_cache);
    // Unlike the owner-less bootstrap path this retry otherwise shares, a
    // canonical owner cannot safely attempt it at all --
    // `CanonicalTrailerOwner::indirect_handle`/`read_xref_stream_at_offset`
    // resolve through the live document, and any warning that resolution
    // raises is pushed immediately (`ResolverHandle::push_qpdf_warning`,
    // matching qpdf's own `warn()` mutating `m->warnings` as it is called,
    // `QPDF.cc:1250-1258`), not buffered for later discard the way this
    // function's own `initial_parse_diagnostics` is. Skip the attempt
    // entirely for this owner so it matches qpdf's real control flow
    // (`QPDF.cc:450-452`: never call `read_xref` when the offset is zero)
    // instead of leaking a warning qpdf never produces. `allow_repair` is
    // required in the guard because a non-repair open can only reach
    // `startxref == 0` via an explicit `startxref 0` in the file (the
    // Err-from-parse_startxref arm above returns immediately when
    // `!allow_repair`); that narrower case is unverified against qpdf and
    // left to the unfixed bootstrap-shaped retry below.
    let initial_context_spec = initial_bootstrap_cache
        .as_ref()
        .map_or(XrefReadContextSpec::ActiveSection, |bootstrap_cache| {
            XrefReadContextSpec::ActiveSectionWithCache { bootstrap_cache }
        });
    let initial_parse_result =
        if allow_repair && startxref == 0 && canonical_trailer_owner.is_some() {
            Err(Error::parse(0, "xref not found"))
        } else {
            parse_xref_from_start_with_owner(
                bytes,
                xref_pos,
                startxref,
                &version,
                options.clone(),
                &mut registration,
                Some(&mut initial_parse_diagnostics),
                initial_context_spec,
                Some(&mut observed_first_xref_item_offset),
                true,
                canonical_trailer_owner,
            )
        };
    let mut loaded = match initial_parse_result
    // qpdf-deviation-end
    {
        Ok(loaded) => loaded,
        Err(error) if allow_repair => {
            // Report the first recorded failure; this parse error is only the
            // trigger when the startxref stage itself succeeded.
            let trigger = parse_errors.into_iter().next().unwrap_or_else(|| {
                if startxref == 0 {
                    Error::parse(0, "xref not found")
                } else {
                    error
                }
            });
            // qpdf rejects an xref offset of zero before calling read_xref
            // (`QPDF.cc:450-452`), so diagnostics from flpdf's marked
            // offset-zero retry have no qpdf counterpart. Keep diagnostics
            // from a real non-zero xref read, but discard this detour's
            // warnings before candidate recovery.
            if startxref != 0 {
                for diagnostic in initial_parse_diagnostics.entries() {
                    initial_diagnostics.push(diagnostic.clone());
                }
                deliver_canonical_diagnostics(
                    canonical_trailer_owner,
                    &mut initial_diagnostics,
                )?; // cov:ignore: this branch only propagates a canonical warning-sink failure from a failed nonzero-startxref parse; the sink boundary is covered by Pdf open failure tests
            }
            let preexisting_entries = (startxref != 0).then_some(&registration.entries);
            let preexisting_raw_entries = (startxref != 0).then_some(&registration.raw_entries);
            let preexisting_bootstrap_cache = if startxref != 0 {
                initial_bootstrap_cache.as_ref()
            } else {
                None // cov:ignore: startxref == 0 returns through the reconstruction handoff above
            };
            let mut recovered = recover_xref_from_linear_scan(
                bytes,
                version,
                startxref,
                trigger,
                None,
                preexisting_entries,
                preexisting_raw_entries,
                preexisting_bootstrap_cache,
                options.clone(),
                initial_diagnostics,
                observed_first_xref_item_offset,
                canonical_trailer_owner,
            )?;
            recovered.header_offset = header_offset;
            return Ok(recovered);
        }
        Err(error) => return Err(error),
    };
    prepend_repair_diagnostics(&mut loaded.loaded.repair_diagnostics, initial_diagnostics);
    deliver_canonical_diagnostics(
        canonical_trailer_owner,
        &mut loaded.loaded.repair_diagnostics,
    )?; // cov:ignore: this is the defensive logger-failure edge after a successful initial xref parse; the same live sink is covered at the Pdf open boundary

    if let Some((offset, message)) = loaded.pending_reconstruction_trigger.take() {
        let trigger = Error::parse(offset as usize, message);
        let diagnostics = std::mem::take(&mut loaded.loaded.repair_diagnostics);
        let recovered = recover_xref_from_linear_scan(
            bytes,
            version.clone(),
            startxref,
            trigger,
            Some(&loaded.loaded.trailer),
            Some(&registration.entries),
            Some(&registration.raw_entries),
            loaded.bootstrap_cache.as_ref(),
            options.clone(),
            diagnostics,
            Some(loaded.first_xref_item_offset),
            canonical_trailer_owner,
        )?; // cov:ignore: a pending trigger always carries a parsed fallback trailer
        let deleted_objects = std::mem::take(&mut registration.deleted_objects);
        loaded = merge_recovered_qpdf_state(recovered, loaded, &deleted_objects);
        registration.replace_effective_entries(loaded.loaded.entries.clone())?;
        registration.deleted_objects.clear();
    }

    let mut previous_parse_diagnostics = Diagnostics::default();
    if let Err(error) = merge_previous_xref_sections_with_observer(
        bytes,
        &version,
        &mut loaded,
        options.clone(),
        &mut registration,
        Some(&mut previous_parse_diagnostics),
        XrefReadContextSpec::ActiveSection,
        Some(&mut observed_first_xref_item_offset),
        canonical_trailer_owner,
    ) {
        if allow_repair {
            let deleted_objects = std::mem::take(&mut registration.deleted_objects);
            let trigger = parse_errors.into_iter().next().unwrap_or(error);
            // cov:ignore-start: post-chain /Size reconstruction is superseded by the canonical classic-trailer recovery handoff
            let recovered = recover_xref_from_linear_scan(
                bytes,
                version,
                startxref,
                trigger,
                Some(&loaded.loaded.trailer),
                Some(&registration.entries),
                Some(&registration.raw_entries),
                loaded.bootstrap_cache.as_ref(),
                options.clone(),
                previous_parse_diagnostics,
                observed_first_xref_item_offset,
                canonical_trailer_owner,
            )?;
            let mut recovered = merge_recovered_qpdf_state(recovered, loaded, &deleted_objects);
            recovered.header_offset = header_offset;
            return Ok(recovered);
            // cov:ignore-end
        }
        return Err(error);
    }

    loaded.loaded.entries = registration.snapshot();
    loaded.raw_entries = registration.raw_snapshot();
    // qpdf's post-chain `m->trailer.getKey("/Size").getIntValueAsInt()`
    // dereferences indirect `/Size` values through the completed active xref
    // table before applying the consistency warning (`QPDF.cc:689-704`).
    // Keep `/Size` in the xref-loading responsibility boundary. The canonical
    // `Pdf::open` route resolves it through the already-created resolver;
    // owner-less standalone loading uses its retained bootstrap context.
    let (resolved_size, size_reconstruction_trigger) = if let Some(owner) = canonical_trailer_owner
    {
        owner.install_xref_entries(registration.snapshot());
        let mut context = CanonicalXrefContext::new(owner, options.description.clone());
        let value = context.resolve_dictionary_value(&loaded.loaded.trailer, "Size");
        let trigger = context.take_reconstruction_trigger();
        context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
        (value, trigger)
    } else {
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSectionWithCache {
                bootstrap_cache: loaded
                    .bootstrap_cache
                    .as_ref()
                    .expect("owner-less xref state has a bootstrap cache"),
            },
            &registration,
            options.clone(),
        );
        let value = context.resolve_dictionary_value(&loaded.loaded.trailer, "Size");
        let trigger = context.take_reconstruction_trigger();
        if trigger.is_none() {
            context.cache.commit();
        }
        context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
        (value, trigger)
    };

    // qpdf's ordinary post-chain `/Size` lookup calls `resolve`, whose
    // `readObjectAtOffset(true, ...)` can reconstruct the xref table when the
    // active entry points at a different object header (`QPDF.cc:1605-1623`).
    // This is the same top-level `reconstruct_xref` responsibility as an
    // initial xref failure, not a size-validation warning: consume the
    // deferred trigger before comparing `/Size`, and run the line-scan
    // recovery with the already-established trailer (`QPDF.cc:516-575`).
    // cov:ignore-start: post-chain /Size reconstruction is superseded by the canonical classic-trailer recovery handoff
    if let Some(error) = size_reconstruction_trigger {
        // The trigger is only recorded by a bounded (repair-mode) read; keep
        // this path as the single qpdf-style reconstruction handoff.
        let diagnostics = std::mem::take(&mut loaded.loaded.repair_diagnostics);
        let recovered = recover_xref_from_linear_scan(
            bytes,
            version.clone(),
            startxref,
            error,
            Some(&loaded.loaded.trailer),
            None, // cov:ignore: an existing fallback trailer suppresses candidate re-entry, so no prior candidate state is consumed here
            None, // cov:ignore: no prior raw registration is consumed here
            loaded.bootstrap_cache.as_ref(), // cov:ignore: the post-chain /Size trigger is superseded by the canonical classic-trailer validation handoff before this defensive path
            options.clone(),
            diagnostics,
            None,
            canonical_trailer_owner,
        )?; // cov:ignore: recover_xref_entries has no fallible branch; retain defensive propagation
        let deleted_objects = std::mem::take(&mut registration.deleted_objects);
        let mut recovered = merge_recovered_qpdf_state(recovered, loaded, &deleted_objects);
        recovered.header_offset = header_offset;

        // qpdf continues the original read_xref call after
        // readObjectAtOffset(true, ...) reconstructs the table: its
        // m->trailer.getKey("/Size").getIntValueAsInt() at QPDF.cc:689
        // therefore resolves the value against the newly reconstructed xref
        // before the :697-704 size consistency warning. Re-run that one
        // post-reconstruction lookup through the same owner that read the
        // recovered xref table; the owner-less route retains its reconstruction
        // context while `Pdf::open` uses the canonical resolver.
        let (recovered_size, recovered_size_diagnostics) = {
            let recovered_reference_offsets =
                reconstructed_reference_offsets(&recovered.loaded.entries);
            let reconstruction_registration = XrefRegistration::default();
            if let Some(owner) = canonical_trailer_owner {
                owner.install_xref_entries(recovered.loaded.entries.clone());
                let mut context = CanonicalXrefContext::new(owner, options.description.clone());
                let value = context.resolve_dictionary_value(&recovered.loaded.trailer, "Size");
                let mut diagnostics = Diagnostics::default();
                context.append_diagnostics_to(&mut diagnostics);
                (value, diagnostics)
            } else {
                let mut context = XrefReadContext::new(
                    bytes,
                    XrefReadContextSpec::ReconstructionWithCache {
                        line_scan_entries: &recovered.loaded.entries,
                        reference_offsets: &recovered_reference_offsets,
                        bootstrap_cache: recovered
                            .bootstrap_cache
                            .as_ref()
                            .expect("owner-less recovered xref state has a bootstrap cache"),
                    },
                    &reconstruction_registration,
                    options.clone(),
                );
                let value = context.resolve_dictionary_value(&recovered.loaded.trailer, "Size");
                context.cache.commit();
                (value, context.diagnostics.clone())
            }
        };
        for diagnostic in recovered_size_diagnostics.entries() {
            recovered.loaded.repair_diagnostics.push(diagnostic.clone());
        }
        append_xref_size_warning_for(
            recovered_size.as_ref(),
            highest_object_number(recovered.loaded.entries.keys().map(|key| key.number)),
            &BTreeSet::new(),
            &options.description,
            &mut recovered.loaded.repair_diagnostics,
        );
        deliver_canonical_diagnostics(
            canonical_trailer_owner,
            &mut recovered.loaded.repair_diagnostics,
        )?;
        return Ok(recovered);
    }
    // cov:ignore-end

    append_xref_size_warning_for(
        resolved_size.as_ref(),
        registration.highest_raw_object_number(),
        &registration.deleted_objects,
        &options.description,
        &mut loaded.loaded.repair_diagnostics,
    );
    deliver_canonical_diagnostics(
        canonical_trailer_owner,
        &mut loaded.loaded.repair_diagnostics,
    )?; // cov:ignore: this is the defensive logger-failure edge after ordinary /Size validation; the same live sink is covered at the Pdf open boundary
        // This is the ordinary `read_xref` lifetime: qpdf keeps
        // `m->deleted_objects` through `/Size` validation, then clears it
        // (`QPDF.cc:686-708`). `reconstruct_xref` has a distinct line-scan
        // lifetime and clears before candidate re-read (`:516-575`, `:576-607`).
        // The set implements only registration suppression (`:1187-1210`), never
        // resolver or mutation history, and must not cross the xref-loader boundary.
    registration.deleted_objects.clear(); // cov:ignore: ordinary post-chain cleanup is subsumed by the canonical recovery handoff

    // cov:ignore-start: parse_errors are drained by the earlier qpdf recovery handoff before ordinary completion
    if let Some(error) = parse_errors.into_iter().next() {
        push_repair_diagnostics(
            &mut loaded.loaded.repair_diagnostics,
            &error,
            startxref,
            &options.description,
        );
        deliver_canonical_diagnostics(
            canonical_trailer_owner,
            &mut loaded.loaded.repair_diagnostics,
        )?; // cov:ignore: canonical trailer diagnostics are already exercised; this line only propagates an injected logger failure after the classic read
    }
    // cov:ignore-end

    discard_lower_generations(
        &mut loaded.raw_entries,
        &mut loaded.loaded.entries,
        &mut loaded.parsed_xref_streams,
    );
    loaded.header_offset = header_offset;
    Ok(loaded)
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn parse_xref_from_start(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: &str,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    first_xref_item_offset_sink: Option<&mut Option<u64>>,
    validate_current_classic_trailer: bool,
) -> Result<LoadedXrefState> {
    parse_xref_from_start_with_owner(
        bytes,
        xref_pos,
        startxref,
        version,
        options,
        registration,
        error_diagnostics_sink,
        context_spec,
        first_xref_item_offset_sink,
        validate_current_classic_trailer,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_xref_from_start_with_owner(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: &str,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    first_xref_item_offset_sink: Option<&mut Option<u64>>,
    validate_current_classic_trailer: bool,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<LoadedXrefState> {
    parse_xref_from_start_with_owner_and_build_diagnostics(
        bytes,
        xref_pos,
        startxref,
        version,
        options,
        registration,
        error_diagnostics_sink,
        context_spec,
        first_xref_item_offset_sink,
        validate_current_classic_trailer,
        canonical_trailer_owner,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_xref_from_start_with_owner_and_build_diagnostics(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: &str,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    mut error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    first_xref_item_offset_sink: Option<&mut Option<u64>>,
    validate_current_classic_trailer: bool,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
    hybrid_build_diagnostics_sink: Option<&mut Diagnostics>,
) -> Result<LoadedXrefState> {
    let mut classic_xref_pos = xref_pos;
    while bytes
        .get(classic_xref_pos)
        .is_some_and(|byte| is_pdf_space(*byte))
    {
        classic_xref_pos += 1;
    }
    let is_classic_xref = bytes.get(classic_xref_pos..).is_some_and(|tail| {
        tail.starts_with(b"xref") && tail.get(4).is_some_and(|byte| is_pdf_space(*byte))
    });
    if is_classic_xref {
        // qpdf counts `skip` from the keyword it just read but adds it to the
        // *original* offset: `read_xrefTable(xref_offset + skip)`
        // (`QPDF.cc:670-676`). Any whitespace it skipped first is therefore
        // counted twice, and the table read starts that many bytes early --
        // inside the keyword -- which is why qpdf reports `xref syntax
        // invalid` and reconstructs instead of reading such a file. Starting
        // after the keyword would silently accept what qpdf rejects.
        let mut skip = 4;
        while bytes
            .get(classic_xref_pos + skip)
            .is_some_and(|byte| is_pdf_space(*byte))
        {
            skip += 1;
        }
        // qpdf warns before it reads the table (`QPDF.cc:663-665`), so the
        // warning has to survive a table read that then fails -- which the
        // offset above makes likely for exactly these files.
        let whitespace_warning = (classic_xref_pos != xref_pos).then(|| {
            damaged_warning(
                &options.description,
                b"",
                "extraneous whitespace seen before xref",
                None,
            )
        });
        let mut cursor = ByteCursor::new(bytes, xref_pos + skip);
        let table = parse_xref_table(
            &mut cursor,
            bytes,
            first_xref_item_offset_sink,
            &options.description,
        );
        let (entries, trailer_start, mut table_diagnostics, first_xref_item_offset) = match table {
            Ok(table) => table,
            Err(error) => {
                if let (Some(warning), Some(sink)) = (
                    whitespace_warning.clone(),
                    error_diagnostics_sink.as_deref_mut(),
                ) {
                    sink.push(warning);
                }
                // The canonical owner keeps its diagnostics through
                // `push_warning`, not through the caller's sink.
                if let Some(warning) = whitespace_warning {
                    let mut pending = Diagnostics::default();
                    pending.push(warning);
                    deliver_canonical_diagnostics(canonical_trailer_owner, &mut pending)?;
                }
                return Err(error);
            }
        };
        if let Some(warning) = whitespace_warning {
            table_diagnostics.insert(0, warning);
        }
        let mut deferred_free = Vec::new();
        for entry in entries {
            match entry {
                ParsedXrefEntry::Live { object_ref, entry } => {
                    registration.insert_xref_entry(object_ref, entry);
                }
                ParsedXrefEntry::Free { object_ref } => deferred_free.push(object_ref),
            }
        }
        let mut trailer_context = canonical_trailer_owner
            .is_none()
            .then(|| XrefReadContext::new(bytes, context_spec, registration, options.clone()));
        // The initial classic subsection is already known when qpdf calls
        // readTrailer, so parser-created indirect children belong to the
        // document's one obj_cache. The owner-less standalone loader is the
        // only path that still creates a bootstrap cache below.
        if let Some(owner) = canonical_trailer_owner {
            owner.install_xref_entries(registration.snapshot());
        }
        let (trailer, trailer_parser_diagnostics) = if let Some(owner) = canonical_trailer_owner {
            let mut trailer_parser = CanonicalTrailerParser { owner };
            read_trailer(
                bytes,
                trailer_start,
                &options.description,
                &mut trailer_parser,
            )?
        } else {
            let mut trailer_parser = BootstrapHandleParser {
                document: &trailer_context
                    .as_ref()
                    .expect("owner-less classic trailer has a bootstrap context")
                    .document,
                description: XrefObjectDescription::Ordinary,
            };
            read_trailer(
                bytes,
                trailer_start,
                &options.description,
                &mut trailer_parser,
            )?
        };
        let mut trailer_diags = table_diagnostics;
        trailer_diags.extend(trailer_parser_diagnostics);
        if !trailer.try_is_dictionary()? {
            // qpdf delivers parser warnings even when readTrailer returns a
            // non-dictionary, then throws the terminal exception
            // (`QPDF.cc:565-568,894-905`). Do not let this early return skip
            // the warning batch.
            if let Some(owner) = canonical_trailer_owner {
                for diagnostic in &trailer_diags {
                    owner.push_warning(diagnostic.clone())?;
                }
            } else if let Some(sink) = error_diagnostics_sink.as_deref_mut() {
                for diagnostic in &trailer_diags {
                    sink.push(diagnostic.clone());
                }
            }
            return Err(Error::QpdfExc(QpdfExc::new(
                QpdfErrorCode::DamagedPdf,
                &options.description,
                b"",
                i64::try_from(trailer_start).unwrap_or(i64::MAX),
                b"expected trailer dictionary",
            )));
        }
        let mut bootstrap_diagnostics = Diagnostics::default();
        if let Some(context) = trailer_context.as_mut() {
            context.append_diagnostics_to(&mut bootstrap_diagnostics);
        }
        // cov:ignore-start: the trailer parser does not dereference its
        // indirect children, so this pre-Pdf bootstrap diagnostic sink is
        // empty for every reachable trailer shape.
        if let Some(sink) = error_diagnostics_sink.as_deref_mut() {
            for diagnostic in bootstrap_diagnostics.entries() {
                sink.push(diagnostic.clone());
            }
        }
        // cov:ignore-end
        let bootstrap_cache = trailer_context
            .as_ref()
            .map(|context| context.cache.shared());
        let trailer_references = collect_trailer_references(&trailer);
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: version.to_string(),
                startxref,
                entries: registration.snapshot(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            raw_entries: registration.raw_snapshot(),
            first_xref_item_offset,
            classic_trailer_offset: Some(trailer_start),
            pending_reconstruction_trigger: None,
            trailer_references,
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache,
            header_offset: 0,
            already_reconstructed: false,
        };
        for diagnostic in trailer_diags {
            loaded.loaded.repair_diagnostics.push(diagnostic);
        }
        // cov:ignore-start: the same trailer parse above cannot add bootstrap
        // diagnostics without dereferencing a child, which it never does.
        for diagnostic in bootstrap_diagnostics.entries() {
            loaded.loaded.repair_diagnostics.push(diagnostic.clone());
        }
        // cov:ignore-end
        deliver_canonical_diagnostics(
            canonical_trailer_owner,
            &mut loaded.loaded.repair_diagnostics,
        )?; // cov:ignore: this only propagates an injected logger failure after classic trailer parsing; the live sink is covered at the Pdf open boundary
        if validate_current_classic_trailer {
            let validation = if let Some(owner) = canonical_trailer_owner {
                let mut context = CanonicalXrefContext::new(owner, options.description.clone());
                let validation =
                    validate_classic_trailer(&mut context, &loaded.loaded.trailer, trailer_start);
                context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
                validation
            } else {
                validate_classic_trailer(
                    trailer_context
                        .as_mut()
                        .expect("owner-less classic trailer has a bootstrap context"),
                    &loaded.loaded.trailer,
                    trailer_start,
                )
            };
            match validation {
                Ok(ClassicTrailerValidation::Valid) => {}
                Ok(ClassicTrailerValidation::NeedsReconstruction(error)) => {
                    let Error::Parse { offset, message } = error else {
                        // cov:ignore-start: only Error::Parse creates a bootstrap reconstruction trigger
                        unreachable!("classic trailer reconstruction trigger is a parse error")
                        // cov:ignore-end
                    };
                    loaded.pending_reconstruction_trigger = Some((offset as u64, message));
                }
                Err(error) => {
                    if let Some(sink) = error_diagnostics_sink.as_deref_mut() {
                        for diagnostic in loaded.loaded.repair_diagnostics.entries() {
                            sink.push(diagnostic.clone());
                        }
                    }
                    return Err(error);
                }
            }
            deliver_canonical_diagnostics(
                canonical_trailer_owner,
                &mut loaded.loaded.repair_diagnostics,
            )?; // cov:ignore: this is the defensive logger-failure edge after classic trailer validation; the sink boundary is covered by Pdf open failure tests
        }
        merge_xref_stream_from_classic_trailer_with_build_diagnostics(
            bytes,
            xref_pos,
            &mut loaded,
            options.clone(),
            registration,
            error_diagnostics_sink,
            context_spec,
            canonical_trailer_owner,
            hybrid_build_diagnostics_sink,
        )?;
        deliver_canonical_diagnostics(
            canonical_trailer_owner,
            &mut loaded.loaded.repair_diagnostics,
        )?; // cov:ignore: this is the defensive logger-failure edge after hybrid builder delivery; the sink boundary is covered by Pdf open failure tests
        for object_ref in deferred_free {
            registration.insert_free_xref_entry(object_ref);
        }
        loaded.loaded.entries = registration.snapshot();
        loaded.raw_entries = registration.raw_snapshot();
        return Ok(loaded);
    }

    parse_xref_stream(
        bytes,
        xref_pos,
        startxref,
        version.to_string(),
        options.clone(),
        registration,
        error_diagnostics_sink,
        context_spec,
        canonical_trailer_owner,
    )
}

/// Validate the first classic trailer exactly where qpdf's
/// `QPDF::read_xrefTable` does (`QPDF.cc:902-912`). The trailer offset is the
/// position immediately after the `trailer` keyword, which is the location
/// `QPDF::readTrailer` restores on its `InputSource` before constructing the
/// `QPDFExc` (`QPDF.cc:1313-1327`).
enum ClassicTrailerValidation {
    Valid,
    NeedsReconstruction(Error),
}

fn validate_classic_trailer(
    context: &mut dyn XrefObjectContext,
    trailer: &ObjectHandle,
    trailer_offset: usize,
) -> Result<ClassicTrailerValidation> {
    // qpdf's QPDF_Dictionary::hasKey checks the dictionary's raw child slot
    // before the subsequent getKey("/Size").isInteger() call resolves that
    // child.  Do not use ObjectHandle::try_has_key here: its public
    // qpdf-compatible visible-key operation resolves a child while deciding
    // whether it is null, which changes the recovery handoff for an indirect
    // /Size whose stale xref row points at another object.
    let size = trailer
        .as_dictionary()
        .and_then(|entries| entries.get(b"/Size".as_slice()).cloned());
    let Some(size) = size else {
        return Err(Error::parse(
            trailer_offset,
            "trailer dictionary lacks /Size key",
        ));
    };
    if size.is_null() {
        return Err(Error::parse(
            trailer_offset,
            "trailer dictionary lacks /Size key",
        ));
    }

    context.ensure_source_for_resolution(&size);
    let is_integer = size.try_is_integer();
    context.sync_handle_diagnostics();
    if let Some(error) = context.take_reconstruction_trigger() {
        return Ok(ClassicTrailerValidation::NeedsReconstruction(error));
    }
    if !is_integer? {
        return Err(Error::parse(
            trailer_offset,
            "/Size key in trailer dictionary is not an integer",
        ));
    }
    Ok(ClassicTrailerValidation::Valid)
}

fn merge_bootstrap_handle_state_prefer_source(
    destination: &Rc<RefCell<BootstrapHandleState>>,
    source: &Rc<RefCell<BootstrapHandleState>>,
) {
    if Rc::ptr_eq(destination, source) {
        return;
    }
    let destination = destination.borrow();
    let mut source = source.borrow_mut();
    let mut handles = destination.handles.clone();
    handles.extend(source.handles.clone());
    source.handles = handles;
    source
        .resolving
        .extend(destination.resolving.iter().copied());
    source
        .resolved_object_streams
        .extend(destination.resolved_object_streams.iter().copied());
    let mut diagnostics = destination.diagnostics.clone();
    for diagnostic in source.diagnostics.entries() {
        diagnostics.push(diagnostic.clone());
    }
    source.diagnostics = diagnostics;
    if source.reconstruction_trigger.is_none() {
        source.reconstruction_trigger = destination.reconstruction_trigger.clone();
    }
}

fn merge_bootstrap_cache_prefer_source(
    destination: &mut Option<SharedBootstrapCache>,
    source: &Option<SharedBootstrapCache>,
) {
    let (Some(destination), Some(source)) = (destination.as_ref(), source.as_ref()) else {
        if destination.is_none() {
            *destination = source.as_ref().map(Rc::clone);
        }
        return;
    };
    if Rc::ptr_eq(destination, source) {
        return;
    }
    let source = source.borrow();
    let mut destination = destination.borrow_mut();
    destination
        .handle_document_owners
        .extend(source.handle_document_owners.iter().cloned());
    if let Some(source_document) = source.handle_document.as_ref() {
        if let Some(destination_document) = destination.handle_document.clone() {
            if !Rc::ptr_eq(&destination_document, source_document) {
                destination
                    .handle_document_owners
                    .push(destination_document);
            }
        }
        merge_bootstrap_handle_state_prefer_source(&destination.handle_state, &source.handle_state);
        destination.handle_state = Rc::clone(&source.handle_state);
        destination.handle_document = Some(Rc::clone(source_document));
    } else {
        merge_bootstrap_handle_state_prefer_source(&destination.handle_state, &source.handle_state);
    }
}

/// Read the optional hybrid-reference stream named by a classic trailer's
/// `/XRefStm`. This is the `QPDF::read_xrefTable` branch at QPDF.cc:915-927:
/// it reads the stream before the table's deferred free entries and deliberately
/// discards the stream trailer's `/Prev` continuation.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn merge_xref_stream_from_classic_trailer(
    bytes: &[u8],
    classic_xref_pos: usize,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<()> {
    merge_xref_stream_from_classic_trailer_with_build_diagnostics(
        bytes,
        classic_xref_pos,
        loaded,
        options,
        registration,
        error_diagnostics_sink,
        context_spec,
        canonical_trailer_owner,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn merge_xref_stream_from_classic_trailer_with_build_diagnostics(
    bytes: &[u8],
    classic_xref_pos: usize,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    mut error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
    hybrid_build_diagnostics_sink: Option<&mut Diagnostics>,
) -> Result<()> {
    let has_xref_stream_key = loaded
        .loaded
        .trailer
        .as_dictionary()
        .is_some_and(|entries| entries.contains_key(b"/XRefStm".as_slice()));
    if !has_xref_stream_key {
        return Ok(());
    }

    // qpdf's ignore gate precedes both the integer check and read_xrefStream.
    // Do not rely on parse_xref_stream's internal gate: at this call site qpdf
    // succeeds without inspecting an ignored, malformed `/XRefStm` value.
    if options.ignore_xref_streams {
        return Ok(());
    }

    let hybrid_context_spec = if canonical_trailer_owner.is_some() {
        context_spec_without_bootstrap_cache(context_spec)
    } else {
        match context_spec {
            XrefReadContextSpec::ActiveSection | XrefReadContextSpec::Reconstruction { .. } => {
                let bootstrap_cache = loaded
                    .bootstrap_cache
                    .as_ref()
                    .expect("owner-less xref state has a bootstrap cache");
                context_spec_with_bootstrap_cache(context_spec, bootstrap_cache)
            }
            XrefReadContextSpec::ActiveSectionWithCache { .. }
            | XrefReadContextSpec::ReconstructionWithCache { .. } => context_spec,
        }
    };
    let xref_stream_value = if let Some(owner) = canonical_trailer_owner {
        let mut context = CanonicalXrefContext::new(owner, options.description.clone());
        let value = context.resolve_dictionary_value(&loaded.loaded.trailer, "XRefStm");
        context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
        value
    } else {
        let mut context =
            XrefReadContext::new(bytes, hybrid_context_spec, registration, options.clone());
        let value = context.resolve_dictionary_value(&loaded.loaded.trailer, "XRefStm");
        if let Some(error) = context.take_reconstruction_trigger() {
            context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
            if let Some(sink) = error_diagnostics_sink.as_mut() {
                for diagnostic in loaded.loaded.repair_diagnostics.entries() {
                    sink.push(diagnostic.clone());
                }
            }
            return Err(error);
        }
        context.cache.commit();
        context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
        value
    };
    let Some(xref_stream_offset) = // cov:ignore: LLVM maps the covered hybrid-offset let-else binding to its continuation edge
        xref_stream_value.and_then(|value| value.try_as_integer().ok().flatten())
    else {
        if let Some(sink) = error_diagnostics_sink.as_mut() {
            for diagnostic in loaded.loaded.repair_diagnostics.entries() {
                sink.push(diagnostic.clone());
            }
        }
        return Err(Error::parse(classic_xref_pos, "invalid /XRefStm"));
    };
    let xref_stream_pos = match usize::try_from(xref_stream_offset) {
        Ok(xref_stream_pos) => xref_stream_pos,
        Err(_) => {
            // qpdf passes the signed integer to InputSource::seek; a negative
            // value therefore fails as an invalid seek rather than as malformed
            // `/XRefStm` syntax.
            let error = Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("xref stream offset {xref_stream_offset} is before the file start"),
            ));
            if let Some(sink) = error_diagnostics_sink.as_mut() {
                for diagnostic in loaded.loaded.repair_diagnostics.entries() {
                    sink.push(diagnostic.clone());
                }
            }
            return Err(error);
        }
    };

    // The hybrid stream contributes entries and raw-object discovery state, but
    // its own trailer is not the current trailer and its `/Prev` is ignored.
    let mut hybrid_error_diagnostics = Diagnostics::default();
    let hybrid = match parse_xref_stream(
        bytes,
        xref_stream_pos,
        xref_stream_pos as u64,
        loaded.loaded.version.clone(),
        options.clone(),
        registration,
        Some(&mut hybrid_error_diagnostics),
        hybrid_context_spec,
        canonical_trailer_owner,
    ) {
        Ok(hybrid) => hybrid,
        Err(error) => {
            if let Some(sink) = error_diagnostics_sink.as_mut() {
                for diagnostic in loaded.loaded.repair_diagnostics.entries() {
                    sink.push(diagnostic.clone());
                }
                for diagnostic in hybrid_error_diagnostics.entries() {
                    sink.push(diagnostic.clone());
                }
            }
            return Err(error);
        }
    };
    if let Some(sink) = hybrid_build_diagnostics_sink {
        for diagnostic in hybrid.loaded.repair_diagnostics.entries() {
            sink.push(diagnostic.clone());
        }
    } else {
        for diagnostic in hybrid.loaded.repair_diagnostics.entries() {
            loaded.loaded.repair_diagnostics.push(diagnostic.clone());
        }
    }
    if hybrid.first_xref_item_offset != 0 {
        loaded.first_xref_item_offset = hybrid.first_xref_item_offset;
    }
    loaded
        .trailer_references
        .extend(hybrid.trailer_references.iter().copied());
    loaded
        .parsed_xref_streams
        .extend(hybrid.parsed_xref_streams);
    merge_bootstrap_cache_prefer_source(&mut loaded.bootstrap_cache, &hybrid.bootstrap_cache);

    loaded.loaded.entries = registration.snapshot();
    loaded.raw_entries = registration.raw_snapshot();
    deliver_canonical_diagnostics(
        canonical_trailer_owner,
        &mut loaded.loaded.repair_diagnostics,
    )?; // cov:ignore: this is the defensive logger-failure edge after a /Prev section merge; the sink boundary is covered by Pdf open failure tests

    Ok(())
}

/// `error_diagnostics_sink` is forwarded to each `/Prev` section's own
/// `parse_xref_from_start` call so a section that needs repair (e.g.
/// stream-length recovery) but then fails its own later validation still
/// hands that already-recorded warning to the sink before this function's
/// `?` propagates the error -- qpdf's `read_xref`'s `/Prev` walk
/// (`QPDF.cc:678`) calls the same `read_xrefStream` for every section in the
/// chain, top-level or not, so a section's own read warns unconditionally
/// regardless of position in the chain (empirically confirmed against qpdf
/// 11.9.0: a `/Prev` target needing repair whose own `/W` then fails
/// validation still shows the repair warning, twice -- discovery and
/// re-entry -- before the terminal error).
#[allow(clippy::too_many_arguments)]
fn merge_previous_xref_sections(
    bytes: &[u8],
    version: &str,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<()> {
    merge_previous_xref_sections_with_observer(
        bytes,
        version,
        loaded,
        options.clone(),
        registration,
        error_diagnostics_sink,
        context_spec,
        None,
        canonical_trailer_owner,
    )
}

#[allow(clippy::too_many_arguments)]
fn merge_previous_xref_sections_with_observer(
    bytes: &[u8],
    version: &str,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    mut error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    mut first_xref_item_offset_sink: Option<&mut Option<u64>>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<()> {
    let mut visited = HashSet::new();
    if loaded.loaded.startxref != 0 {
        visited.insert(loaded.loaded.startxref);
    }
    let section_context_spec = if canonical_trailer_owner.is_some() {
        context_spec_without_bootstrap_cache(context_spec)
    } else {
        match context_spec {
            XrefReadContextSpec::ActiveSection | XrefReadContextSpec::Reconstruction { .. } => {
                let bootstrap_cache = loaded
                    .bootstrap_cache
                    .as_ref()
                    .expect("owner-less xref state has a bootstrap cache");
                context_spec_with_bootstrap_cache(context_spec, bootstrap_cache)
            }
            XrefReadContextSpec::ActiveSectionWithCache { .. }
            | XrefReadContextSpec::ReconstructionWithCache { .. } => context_spec,
        }
    };
    let previous_offset_result = resolve_previous_xref_offset(
        bytes,
        options.clone(),
        registration,
        section_context_spec,
        &loaded.loaded.trailer,
        loaded.classic_trailer_offset,
        canonical_trailer_owner,
    );
    let (mut previous_offset, previous_diagnostics, reconstruction_trigger) =
        previous_offset_result?;
    let mut previous_diagnostics = previous_diagnostics;
    deliver_canonical_diagnostics(canonical_trailer_owner, &mut previous_diagnostics)?;
    for diagnostic in previous_diagnostics.entries() {
        loaded.loaded.repair_diagnostics.push(diagnostic.clone());
    }
    if let Some(error) = reconstruction_trigger {
        return Err(error);
    }

    while let Some(offset) = previous_offset {
        let previous_pos = usize::try_from(offset)
            .map_err(|_| Error::parse(0, "xref /Prev does not fit usize"))?;

        if !visited.insert(offset) {
            return Err(Error::parse(0, "loop detected following xref tables"));
        }

        let mut previous_error_diagnostics = Diagnostics::default();
        let mut previous_build_diagnostics = Diagnostics::default();
        let previous_result = parse_xref_from_start_with_owner_and_build_diagnostics(
            bytes,
            previous_pos,
            offset,
            version,
            options.clone(),
            registration,
            Some(&mut previous_error_diagnostics),
            section_context_spec,
            first_xref_item_offset_sink.as_deref_mut(),
            false,
            canonical_trailer_owner,
            Some(&mut previous_build_diagnostics),
        );
        let previous = match previous_result {
            Ok(mut previous) => {
                if previous.classic_trailer_offset.is_some() {
                    for diagnostic in previous.loaded.repair_diagnostics.entries() {
                        loaded.loaded.repair_diagnostics.push(diagnostic.clone());
                    }
                    for diagnostic in previous_build_diagnostics.entries() {
                        loaded.loaded.repair_diagnostics.push(diagnostic.clone());
                    }
                } else {
                    for diagnostic in previous.loaded.repair_diagnostics.entries() {
                        loaded.loaded.repair_diagnostics.push(diagnostic.clone());
                    }
                }
                deliver_canonical_diagnostics(
                    canonical_trailer_owner,
                    &mut previous.loaded.repair_diagnostics,
                )?; // cov:ignore: this only propagates an injected logger failure after a prior /Prev section; the sink boundary is covered by Pdf open failure tests
                deliver_canonical_diagnostics(
                    canonical_trailer_owner,
                    &mut previous_build_diagnostics,
                )?; // cov:ignore: this only propagates an injected logger failure after a /Prev builder diagnostic; the sink boundary is covered by Pdf open failure tests
                previous
            }
            Err(error) => {
                let classic_section = bytes
                    .get(previous_pos..)
                    .is_some_and(|tail| tail.starts_with(b"xref"));
                let append_ordered = |target: &mut Diagnostics| {
                    if classic_section {
                        for diagnostic in previous_error_diagnostics.entries() {
                            target.push(diagnostic.clone());
                        }
                    }
                    if !classic_section {
                        for diagnostic in previous_error_diagnostics.entries() {
                            target.push(diagnostic.clone());
                        }
                    }
                };
                if let Some(sink) = error_diagnostics_sink.as_deref_mut() {
                    append_ordered(sink);
                } else {
                    append_ordered(&mut loaded.loaded.repair_diagnostics);
                }
                return Err(error);
            }
        };
        if previous.first_xref_item_offset != 0 {
            loaded.first_xref_item_offset = previous.first_xref_item_offset;
        }
        loaded
            .trailer_references
            .extend(previous.trailer_references.iter().copied());
        for (object_ref, object) in previous.parsed_xref_streams {
            let newer_live = matches!(
                loaded.loaded.entries.get(&object_ref),
                Some(XrefEntry::Uncompressed { .. } | XrefEntry::Compressed { .. })
            );
            if !newer_live {
                loaded
                    .parsed_xref_streams
                    .entry(object_ref)
                    .or_insert(object);
            }
        }
        let hop_offset_result = resolve_previous_xref_offset(
            bytes,
            options.clone(),
            registration,
            section_context_spec,
            &previous.loaded.trailer,
            previous.classic_trailer_offset,
            canonical_trailer_owner,
        );
        let (next_previous_offset, previous_diagnostics, reconstruction_trigger) =
            hop_offset_result?;
        let mut previous_diagnostics = previous_diagnostics;
        deliver_canonical_diagnostics(canonical_trailer_owner, &mut previous_diagnostics)?;
        for diagnostic in previous_diagnostics.entries() {
            loaded.loaded.repair_diagnostics.push(diagnostic.clone());
        }
        if let Some(error) = reconstruction_trigger {
            return Err(error);
        }
        previous_offset = next_previous_offset;
    }

    loaded.loaded.entries = registration.snapshot();
    loaded.raw_entries = registration.raw_snapshot();

    Ok(())
}

fn resolve_previous_xref_offset(
    bytes: &[u8],
    options: XrefLoadOptions,
    registration: &XrefRegistration,
    context_spec: XrefReadContextSpec<'_>,
    trailer: &ObjectHandle,
    trailer_offset: Option<usize>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<(Option<u64>, Diagnostics, Option<Error>)> {
    if let Some(owner) = canonical_trailer_owner {
        let mut context = CanonicalXrefContext::new(owner, options.description.clone());
        return resolve_previous_xref_offset_with_context(&mut context, trailer, trailer_offset);
    }
    let mut context = XrefReadContext::new(bytes, context_spec, registration, options);
    let result = resolve_previous_xref_offset_with_context(&mut context, trailer, trailer_offset);
    context.cache.commit();
    result
}

fn resolve_previous_xref_offset_with_context(
    context: &mut dyn XrefObjectContext,
    trailer: &ObjectHandle,
    trailer_offset: Option<usize>,
) -> Result<(Option<u64>, Diagnostics, Option<Error>)> {
    context.ensure_source_for_resolution(trailer);
    if let Some(previous) = trailer
        .as_dictionary()
        .and_then(|entries| entries.get(b"/Prev".as_slice()).cloned())
    {
        context.ensure_source_for_resolution(&previous);
    }
    let has_previous = trailer.try_has_key(b"/Prev")?;
    context.sync_handle_diagnostics();
    let previous = has_previous
        .then(|| context.resolve_dictionary_value(trailer, "Prev"))
        .flatten();
    let (offset, validation_error) = match previous {
        Some(offset) => match parse_non_negative_u64_handle(&offset, "/Prev") {
            Ok(offset) => (Some(offset).filter(|&offset| offset != 0), None),
            Err(_) if trailer_offset.is_some() => (
                None,
                Some(Error::parse(
                    trailer_offset.expect("classic trailer offset is present"),
                    "/Prev key in trailer dictionary is not an integer",
                )),
            ),
            Err(_) => (None, None),
        },
        None => (None, None),
    };
    let reconstruction_trigger = context.take_reconstruction_trigger();
    let mut diagnostics = Diagnostics::default();
    context.append_diagnostics_to(&mut diagnostics);
    Ok((
        offset,
        diagnostics,
        reconstruction_trigger.or(validation_error),
    ))
}

fn collect_trailer_references(trailer: &ObjectHandle) -> BTreeSet<ObjectRef> {
    let mut references = BTreeSet::new();
    let mut stack = vec![trailer.clone()];
    while let Some(handle) = stack.pop() {
        if let Some(object_ref) = handle.object_ref() {
            references.insert(object_ref);
            continue;
        }
        if let Some(children) = handle.as_array() {
            stack.extend(children);
        } else if let Some(entries) = handle.as_dictionary() {
            stack.extend(entries.into_values());
        } else if let Some(stream_dict) = handle.as_stream_dict() {
            stack.push(stream_dict);
        }
    }
    references
}

/// `max_live` is the highest object number in qpdf's **raw** `m->xref_table`
/// (`QPDF.cc:691-693` reads `rbegin()->first.getObj()`), so a live row whose
/// generation cannot cross the valid `ObjectRef` boundary still counts here.
fn highest_object_number(numbers: impl Iterator<Item = u32>) -> i64 {
    numbers.map(i64::from).max().unwrap_or(0)
}

fn append_xref_size_warning_for(
    size: Option<&ObjectHandle>,
    max_live: i64,
    deleted_objects: &BTreeSet<u32>,
    filename: &[u8],
    repair_diagnostics: &mut Diagnostics,
) {
    let Some(size) = size.and_then(|value| value.try_as_integer().ok().flatten()) else {
        return;
    };
    let max_deleted = i64::from(deleted_objects.iter().copied().max().unwrap_or(0));
    let max_object = max_live.max(max_deleted);

    if size < 1 || size - 1 != max_object {
        repair_diagnostics.push(damaged_warning(
            filename,
            b"",
            format!(
                "reported number of objects ({size}) is not one plus the highest object number ({max_object})"
            ),
            None,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn recover_xref_from_linear_scan(
    bytes: &[u8],
    version: String,
    startxref: u64,
    trigger_error: Error,
    fallback_trailer: Option<&ObjectHandle>,
    preexisting_entries: Option<&BTreeMap<ObjectRef, XrefEntry>>,
    preexisting_raw_entries: Option<&BTreeMap<QpdfObjGen, XrefEntry>>,
    preexisting_bootstrap_cache: Option<&SharedBootstrapCache>,
    options: XrefLoadOptions,
    mut repair_diagnostics: Diagnostics,
    observed_first_xref_item_offset: Option<u64>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<LoadedXrefState> {
    // qpdf mutates `m->first_xref_item_offset` while reading object 0's row,
    // before a later row can throw, and `reconstruct_xref` preserves that
    // member across the exception (`QPDF.cc:846-869, 626-708`). Rust's
    // `Result::Err` cannot carry the successful prefix, so the xref reader
    // supplies this explicit side channel instead of recovering through a
    // sentinel value.
    push_repair_diagnostics(
        &mut repair_diagnostics,
        &trigger_error,
        startxref,
        &options.description,
    );
    deliver_canonical_diagnostics(canonical_trailer_owner, &mut repair_diagnostics)?;

    let recovered = recover_xref_entries_with_owner(
        bytes,
        fallback_trailer.is_none(),
        &options.description,
        canonical_trailer_owner,
    )
    .map_err(|error| {
        // cov:ignore-start: defensive open-failure wrapper after a line-scan parser error; the live sink boundary is covered by Pdf open failure tests
        with_xref_open_diagnostics(error, repair_diagnostics.clone(), canonical_trailer_owner)
    })?;
    // cov:ignore-end
    let mut entries = recovered.entries;
    // qpdf removes only type-1 rows before its reconstruction scan
    // (`QPDF.cc:516-575`). A failed xref-stream insertion can leave a default
    // type-0 row in the table, and compressed rows survive as well. Carry those
    // non-uncompressed rows into the candidate re-entry while allowing the
    // line scan's reconstructed type-1 rows to take precedence.
    if let Some(preexisting_entries) = preexisting_entries {
        for (&object_ref, &entry) in preexisting_entries {
            if !matches!(entry, XrefEntry::Uncompressed { .. }) {
                entries.entry(object_ref).or_insert(entry);
            }
        }
    }
    for diagnostic in recovered.trailer_diagnostics {
        repair_diagnostics.push(diagnostic);
    }
    deliver_canonical_diagnostics(canonical_trailer_owner, &mut repair_diagnostics)?;
    let mut parsed_xref_streams = BTreeMap::new();
    let mut extra_trailer_references = BTreeSet::new();
    let mut bootstrap_cache = preexisting_bootstrap_cache.map(Rc::clone).or_else(|| {
        canonical_trailer_owner
            .is_none()
            .then(empty_bootstrap_cache)
    });

    // qpdf's `reconstruct_xref` (`QPDF.cc:564-616`) gates BOTH its `trailer`
    // keyword scan (`!m->trailer.isInitialized() && t1.isWord("trailer")`)
    // and its `/Type /XRef` candidate search (`if
    // (!m->trailer.isInitialized())`) on the trailer not already being
    // known. `fallback_trailer` -- the trailer from a successfully parsed
    // newest revision whose `/Prev` chain later broke -- models exactly
    // that already-initialized state: qpdf never looks at a stray candidate
    // elsewhere in the file when the correct trailer is already in hand, so
    // neither trailer capture in `recover_xref_entries` nor the candidate search
    // runs at all in that case. `startxref` (the position that produced
    // `fallback_trailer`)
    // is already valid then too, so it needs no adjustment; it is only
    // rewritten to the candidate's own verified re-entry point when the
    // candidate path is what actually recovered the trailer. `last_xref_form`
    // is left as a placeholder (`Table`) in the `fallback_trailer` case: the
    // caller (`load_xref_state_with_options`) always overwrites it via
    // `merge_recovered_qpdf_state` with the already-successfully-parsed
    // revision's own real form once this returns.
    let mut candidate_xref_reentered = false;
    let (trailer, recovered_startxref, recovered_form, recovered_first_xref_item_offset) =
        if let Some(trailer) = fallback_trailer {
            (trailer.clone(), startxref, XrefForm::Table, 0)
        } else {
            match recovered.trailer {
                Some(trailer) => (trailer, startxref, XrefForm::Table, 0),
                None => match recover_trailer_from_xref_stream_candidate(
                    bytes,
                    &version,
                    options.clone(),
                    &mut entries,
                    &mut parsed_xref_streams,
                    &mut repair_diagnostics,
                    &mut extra_trailer_references,
                    preexisting_raw_entries,
                    preexisting_bootstrap_cache,
                    canonical_trailer_owner,
                ) {
                    Ok((
                        trailer,
                        max_offset,
                        form,
                        _deleted_objects,
                        first_xref_item_offset,
                        candidate_bootstrap_cache,
                    )) => {
                        // Candidate re-entry has already consumed its local
                        // tombstones while filtering `entries`; never retain
                        // them past this recovery operation.
                        candidate_xref_reentered = true;
                        bootstrap_cache = candidate_bootstrap_cache;
                        (trailer, max_offset, form, first_xref_item_offset)
                    }
                    Err(candidate_error) => {
                        return Err(with_xref_open_diagnostics(
                            candidate_error,
                            repair_diagnostics,
                            canonical_trailer_owner,
                        ));
                    }
                },
            }
        };
    let recovered_first_xref_item_offset =
        observed_first_xref_item_offset.unwrap_or(recovered_first_xref_item_offset);

    let mut trailer_references = collect_trailer_references(&trailer);
    trailer_references.extend(extra_trailer_references);
    if let Some(owner) = canonical_trailer_owner {
        owner.install_xref_entries(entries.clone());
    }
    // `XrefRegistration` uses a free entry only as the private placeholder
    // left by qpdf's failed unknown-type insertion. The effective reader xref
    // table never exposes free rows; remove the placeholder at
    // the recovery boundary after candidate re-entry has consumed it.
    entries.retain(|_, entry| !matches!(entry, XrefEntry::Free { .. }));

    deliver_canonical_diagnostics(canonical_trailer_owner, &mut repair_diagnostics)?;

    let mut raw_entries = entries
        .iter()
        .map(|(object_ref, entry)| Ok((QpdfObjGen::try_from_object_ref(*object_ref)?, *entry)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    // qpdf keeps non-type-1 raw rows that were inserted before a failed xref
    // stream entry decode (notably the object-0 type-0 placeholder) while
    // reconstruction replaces only the uncompressed rows
    // (`QPDF.cc:516-575`). Retain those exact raw identities for the later
    // resolver census; the effective ObjectRef map must still omit them.
    if let Some(preexisting_raw_entries) = preexisting_raw_entries {
        for (&object_gen, &entry) in preexisting_raw_entries {
            if !matches!(entry, XrefEntry::Uncompressed { .. }) {
                raw_entries.entry(object_gen).or_insert(entry);
            }
        }
    }
    // qpdf's candidate path re-enters read_xref(max_offset) after the
    // reconstruction line scan (QPDF.cc:576-607). That nested read_xref
    // performs its own post-chain generation pruning (QPDF.cc:710-718),
    // whereas a plain reconstruct_xref return preserves every valid
    // line-scan generation. Keep this call scoped to the candidate re-entry
    // rather than applying normal read_xref cleanup to all recovery results.
    if candidate_xref_reentered {
        discard_lower_generations(&mut raw_entries, &mut entries, &mut parsed_xref_streams);
    }
    Ok(LoadedXrefState {
        loaded: LoadedXref {
            version,
            startxref: recovered_startxref,
            entries,
            trailer,
            last_xref_form: recovered_form,
            repair_diagnostics,
        },
        raw_entries,
        first_xref_item_offset: recovered_first_xref_item_offset,
        classic_trailer_offset: None,
        pending_reconstruction_trigger: None,
        trailer_references,
        parsed_xref_streams,
        bootstrap_cache,
        header_offset: 0,
        already_reconstructed: true,
    })
}

fn prepend_repair_diagnostics(target: &mut Diagnostics, initial: Diagnostics) {
    if initial.entries().is_empty() {
        return;
    }
    let existing = std::mem::take(target);
    *target = initial;
    for diagnostic in existing.entries() {
        target.push(diagnostic.clone());
    }
}

fn merge_recovered_qpdf_state(
    mut recovered: LoadedXrefState,
    mut accumulated: LoadedXrefState,
    accumulated_deleted_objects: &BTreeSet<u32>,
) -> LoadedXrefState {
    let mut repair_diagnostics = std::mem::take(&mut accumulated.loaded.repair_diagnostics);
    for diagnostic in recovered.loaded.repair_diagnostics.entries() {
        repair_diagnostics.push(diagnostic.clone());
    }
    recovered.loaded.repair_diagnostics = repair_diagnostics;
    // `recover_xref_from_linear_scan` is only ever called with a
    // `fallback_trailer` from this merge's caller, and that always wins the
    // trailer (see its own doc comment) -- so `accumulated`'s xref form,
    // the already-successfully-parsed newest revision's real one, is always
    // the correct value here, not `recovered`'s `Table` placeholder.
    recovered.loaded.last_xref_form = accumulated.loaded.last_xref_form;
    if recovered.first_xref_item_offset == 0 {
        recovered.first_xref_item_offset = accumulated.first_xref_item_offset;
    }
    recovered.classic_trailer_offset = accumulated
        .classic_trailer_offset
        .or(recovered.classic_trailer_offset);
    // qpdf `reconstruct_xref` removes existing type-1 entries before scanning,
    // and `insertReconstructedXrefEntry` suppresses object numbers in that
    // scan's local filter (`QPDF.cc:516-575`, `:1194-1210`). It clears the
    // scan filter at `:575`, before any candidate xref-stream re-read
    // (`:576-607`). Consume the accumulated filter only to apply that scan's
    // merge effect; a candidate re-read owns a fresh registration. This is not
    // `replaceObject`/`removeObject` cache mutation history.
    recovered
        .loaded
        .entries
        .retain(|object_ref, _| !accumulated_deleted_objects.contains(&object_ref.number));
    recovered.raw_entries.retain(|object_ref, _| {
        u32::try_from(object_ref.get_obj())
            .map(|number| !accumulated_deleted_objects.contains(&number))
            .unwrap_or(true)
    });
    for (&object_ref, &entry) in &accumulated.raw_entries {
        if !matches!(entry, XrefEntry::Uncompressed { .. }) {
            recovered.raw_entries.entry(object_ref).or_insert(entry);
        }
    }
    recovered
        .trailer_references
        .extend(accumulated.trailer_references);
    // `BTreeMap::append` keeps recovered-only streams while replacing
    // collisions with values from `accumulated`. The latter came from the
    // successfully parsed latest-to-oldest /Prev prefix, so it is qpdf's
    // authoritative nearest cached generation.
    recovered
        .parsed_xref_streams
        .append(&mut accumulated.parsed_xref_streams);
    // The accumulated state is the newer parsed xref prefix, so its shared
    // bootstrap objects supersede any same-reference value from recovery.
    merge_bootstrap_cache_prefer_source(
        &mut recovered.bootstrap_cache,
        &accumulated.bootstrap_cache,
    );
    recovered
}

/// Recover uncompressed object offsets and, when requested, the first valid
/// trailer dictionary by replaying qpdf's `reconstruct_xref`
/// (`libqpdf/QPDF.cc`, qpdf 11.9.0): scan the file line by line, and on each line
/// whose first token sequence is `int int obj`, record the object at the offset of
/// its *number* token. A first-token `trailer` candidate is parsed in the same
/// forward scan; malformed or non-dictionary candidates are ignored so scanning
/// can continue. Only the first valid trailer is retained (`QPDF::setTrailer`
/// refuses subsequent assignments). Object bodies are never parsed, and the last
/// occurrence of an object in the file wins (`insertReconstructedXrefEntry`
/// overwrites). Inspecting at most three short tokens per object-header line —
/// never re-parsing a body to end-of-file — keeps the entry scan linear in the
/// file size.
///
/// qpdf records only uncompressed (type-1) entries during reconstruction and
/// declines to look inside object streams (`reconstruct_xref` trailing comment in
/// `QPDF.cc:532-575, 618-623`). A real xref-stream candidate is still re-entered separately by
/// [`recover_trailer_from_xref_stream_candidate`].
pub(crate) struct RecoveredXref {
    pub(crate) entries: BTreeMap<ObjectRef, XrefEntry>,
    pub(crate) trailer: Option<ObjectHandle>,
    pub(crate) trailer_diagnostics: Vec<QpdfExc>,
}

pub(crate) fn recover_xref_entries(
    bytes: &[u8],
    capture_trailer: bool,
    filename: &[u8],
) -> Result<RecoveredXref> {
    recover_xref_entries_with_owner(bytes, capture_trailer, filename, None)
}

fn recover_xref_entries_with_owner(
    bytes: &[u8],
    capture_trailer: bool,
    filename: &[u8],
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<RecoveredXref> {
    let mut entries = BTreeMap::new();
    let mut trailer = None;
    let mut trailer_diagnostics = Vec::new();
    let mut line_start = 0usize;
    while line_start < bytes.len() {
        let next_line_start = next_line_start(bytes, line_start);
        if let Some(first_token) = read_scan_token(bytes, line_start, next_line_start) {
            if capture_trailer && trailer.is_none() && first_token.is_word_value(b"trailer") {
                let (candidate, diagnostics) = parse_trailer_candidate(
                    bytes,
                    first_token.end,
                    filename,
                    canonical_trailer_owner,
                );
                trailer = candidate;
                trailer_diagnostics.extend(diagnostics);
            } else if let Some((object_ref, offset)) =
                scan_object_header_after_first_token(bytes, &first_token)?
            {
                entries.insert(object_ref, XrefEntry::Uncompressed { offset });
            }
        }
        line_start = next_line_start;
    }

    Ok(RecoveredXref {
        entries,
        trailer,
        trailer_diagnostics,
    })
}

/// How many further offset-*positions* (not bytes) a truncated reconstruction
/// read window may extend into on a fallback retry, independent of every other
/// entry's own retries. Bounding by position rather than sharing a global
/// retry budget across the whole scan means no fixed number of unrelated,
/// individually-truncated entries earlier in the (ascending object-number)
/// scan can ever deny a later, genuine candidate or referenced object its own
/// retry. qpdf's per-object recovery has no shared budget at all. Total work
/// stays O(file size): at most this many additional offsets are examined per
/// retry, and only entries within this many positions of the end of the
/// offset-sorted list can ever have their retry reach all the way to EOF.
const XREF_RECONSTRUCTION_FALLBACK_SPAN: usize = 64;

/// qpdf's second trailer-recovery fallback (`reconstruct_xref`, `QPDF.cc:577-608`,
/// qpdf 11.9.0): entered only when the line scan found no usable trailer dictionary.
/// Walk the reconstructed type-1 entries in ascending object order looking for
/// one that is a `/Type /XRef` stream with a positive offset. `setTrailer`
/// only ever takes effect once, so the *first* candidate encountered supplies
/// the trailer dictionary while `max_offset` keeps tracking the true maximum
/// offset across all of them for the re-entry below — the winning trailer and
/// the winning re-entry point are not necessarily the same candidate. If a
/// candidate exists, re-parse the real cross-reference stream chain starting
/// at `max_offset` (mirroring `read_xref`) and merge its entries into
/// `entries`, keeping the line scan's own entries where both agree by object
/// *number* (qpdf's `insertXrefEntry`/`insertFreeXrefEntry`, `QPDF.cc:1149-1206`,
/// both key priority off the number alone). A candidate that fails to decode
/// becomes "error decoding candidate xref stream while recovering damaged
/// file"; no candidate at all becomes "unable to find trailer dictionary while
/// recovering damaged file". The candidate re-read uses its own fresh
/// `XrefRegistration`: like normal `read_xref`, it uses its free-row filter
/// for `/Size` before clear (`QPDF.cc:686-708`), while the reconstruction
/// line-scan filter was already cleared at `:575`. The returned filter is
/// consumed only by this immediate candidate merge; it is never resolver or
/// mutation state.
type RecoveredXrefStream = (
    ObjectHandle,
    u64,
    XrefForm,
    BTreeSet<u32>,
    u64,
    Option<SharedBootstrapCache>,
);

#[allow(clippy::too_many_arguments)]
fn recover_trailer_from_xref_stream_candidate(
    bytes: &[u8],
    version: &str,
    options: XrefLoadOptions,
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    parsed_xref_streams: &mut BTreeMap<ObjectRef, ObjectHandle>,
    repair_diagnostics: &mut Diagnostics,
    trailer_references: &mut BTreeSet<ObjectRef>,
    preexisting_raw_entries: Option<&BTreeMap<QpdfObjGen, XrefEntry>>,
    preexisting_bootstrap_cache: Option<&SharedBootstrapCache>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<RecoveredXrefStream> {
    // All bootstrap contexts below resolve against this same line-scan map
    // until the candidate chain has been merged. Build the sorted offset
    // index once and pass cheap Rc clones through each context instead of
    // sorting the full map again for every `/Prev` lookup.
    let reference_offsets = reconstructed_reference_offsets(entries);
    let (candidate, discovery_diagnostics) = find_xref_stream_trailer_candidate(
        bytes,
        entries,
        options.clone(),
        &reference_offsets,
        preexisting_bootstrap_cache,
        canonical_trailer_owner,
    );
    // qpdf's candidate search resolves every type-1 entry unconditionally
    // (`getObjectByObjGen(iter.first)` runs before the `isStreamOfType`
    // check, `QPDF.cc:585-589`), warning immediately as each is read, in
    // ascending-object-number scan order -- independent of whether that
    // entry ends up being the winning candidate, and independent of
    // whether any candidate is found at all. Surface them all here,
    // unconditionally, before anything else this function might do.
    for diagnostic in discovery_diagnostics.entries() {
        repair_diagnostics.push(diagnostic.clone());
    }
    deliver_canonical_diagnostics(canonical_trailer_owner, repair_diagnostics)?;
    let Some(candidate) = candidate else {
        return Err(Error::parse(
            0,
            "unable to find trailer dictionary while recovering damaged file",
        ));
    };
    let max_offset = candidate.max_offset;

    // The winning candidate's own re-entry below reads it a second,
    // independent time (`read_xrefStream` -> `readObjectAtOffset`,
    // `QPDF.cc:956`, does not consult the object cache the way
    // `getObjectByObjGen` does) -- empirically confirmed against qpdf
    // 11.9.0: a candidate needing stream-length repair warns twice, once
    // plainly during discovery (already pushed above) and once labeled
    // distinctly during re-entry (via `reentry.loaded.repair_diagnostics`
    // below), not once deduplicated. The re-entry's own read can also fail
    // outright after that stream-length repair succeeds -- e.g. a malformed
    // `/W`/`/Index`/`/Size` or truncated entry data (`processXRefStream`,
    // `QPDF.cc:960-1128`) -- but qpdf's `readObjectAtOffset` call (956)
    // happens first and unconditionally, so its repair warning is not rolled
    // back by that later failure (`warn()` mutates `m->warnings` immediately,
    // independent of whatever exception `processXRefStream` throws next;
    // empirically confirmed against qpdf 11.9.0 with a malformed-`/W`
    // candidate: its "recovered stream length" warning still precedes the
    // terminal "error decoding candidate xref stream..." message). The
    // `reentry_error_diagnostics` is a separate sink for
    // `parse_xref_stream_with_canonical_owner`'s own build diagnostic (e.g.
    // "wrong size" for a malformed `/W`), written only when the build fails
    // after the read already succeeded. The canonical sink has already
    // delivered the read warning, so appending this buffer keeps that order.
    //
    // The candidate's own re-entry gets a fresh `XrefRegistration`, scoped to
    // just this call and its `/Prev` chain -- qpdf's `insertXrefEntry`/
    // `insertFreeXrefEntry` priority is local to `read_xref`'s own walk of
    // that one revision chain, not shared with the line scan's entries.
    // qpdf re-enters `read_xref` against the xref table produced by the
    // reconstruction scan. Seed registration with that table so an entry
    // already present there is skipped before its type is validated.
    let mut reentry_registration =
        seed_candidate_reentry_registration(entries, preexisting_raw_entries)?;
    // A build failure inside `parse_xref_stream_with_canonical_owner` (e.g.
    // a malformed `/W`) writes its own diagnostic to a scratch buffer. The
    // canonical owner has already delivered any warning raised while reading
    // the candidate object, so append the build diagnostic afterwards.
    let mut reentry_error_diagnostics = Diagnostics::default();
    let candidate_context_spec = if canonical_trailer_owner.is_some() {
        XrefReadContextSpec::Reconstruction {
            line_scan_entries: entries,
            reference_offsets: &reference_offsets,
        }
    } else {
        XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries: entries,
            reference_offsets: &reference_offsets,
            bootstrap_cache: candidate
                .bootstrap_cache
                .as_ref()
                .expect("owner-less candidate has a bootstrap cache"),
        }
    };
    let reentry_result = parse_xref_from_start_with_owner(
        bytes,
        max_offset as usize,
        max_offset,
        version,
        options.clone(),
        &mut reentry_registration,
        Some(&mut reentry_error_diagnostics),
        candidate_context_spec,
        None,
        false,
        canonical_trailer_owner,
    );
    // Append after the live candidate read: this is only non-empty when the build step itself
    // fails after the (already-reconciled) read succeeded, and that build
    // failure happens strictly after the read in qpdf's own call order
    // (`QPDF.cc:956` before `:960-1128`).
    for diagnostic in reentry_error_diagnostics.entries() {
        repair_diagnostics.push(diagnostic.clone());
    }
    deliver_canonical_diagnostics(canonical_trailer_owner, repair_diagnostics)?;
    let mut reentry = match reentry_result {
        Ok(reentry) => reentry,
        Err(_) => {
            // qpdf's message is exactly this, with no nested detail appended
            // (`libqpdf/QPDF.cc:604`); the inner failure is what led here, not
            // part of the public text.
            return Err(Error::QpdfExc(QpdfExc::new(
                QpdfErrorCode::DamagedPdf,
                &options.description,
                b"",
                0,
                b"error decoding candidate xref stream while recovering damaged file",
            )));
        }
    };
    // qpdf appends the candidate's warning when its re-entry reads the
    // candidate object, before `read_xref` follows `/Prev`. Preserve that
    // order even when a later `/Prev` section fails. Keep the count so the
    // successful path does not append the candidate diagnostics twice.
    let candidate_diagnostic_count = reentry.loaded.repair_diagnostics.entries().len();
    for diagnostic in reentry.loaded.repair_diagnostics.entries() {
        repair_diagnostics.push(diagnostic.clone());
    }
    deliver_canonical_diagnostics(canonical_trailer_owner, repair_diagnostics)?;
    // Buffer diagnostics from a failing `/Prev` section. The merge helper
    // otherwise writes them directly to the outer accumulator before it
    // returns, which would place them ahead of the candidate diagnostics.
    let mut previous_failure_diagnostics = Diagnostics::default();
    if merge_previous_xref_sections(
        bytes,
        version,
        &mut reentry,
        options.clone(),
        &mut reentry_registration,
        Some(&mut previous_failure_diagnostics),
        candidate_context_spec,
        canonical_trailer_owner,
    )
    .is_err()
    {
        // Preserve diagnostics from any earlier `/Prev` sections that merged
        // successfully, then the diagnostics from the section that failed.
        for diagnostic in reentry
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .skip(candidate_diagnostic_count)
        {
            repair_diagnostics.push(diagnostic.clone());
        }
        for diagnostic in previous_failure_diagnostics.entries() {
            repair_diagnostics.push(diagnostic.clone());
        }
        deliver_canonical_diagnostics(canonical_trailer_owner, repair_diagnostics)?; // cov:ignore: this is the defensive logger-failure edge on a failed candidate /Prev merge; normal candidate delivery is covered by qpdf differential tests
                                                                                     // cov:ignore-start: qtest candidate-recovery failures exercise this terminal qpdf exception through the external corpus
        return Err(Error::QpdfExc(QpdfExc::new(
            QpdfErrorCode::DamagedPdf,
            &options.description,
            b"",
            0,
            b"error decoding candidate xref stream while recovering damaged file",
        )));
        // cov:ignore-end
    }

    let first_xref_item_offset = reentry.first_xref_item_offset;
    let deleted_objects = reentry_registration.deleted_objects.clone();

    // `reentry.loaded.entries` is already the live-only snapshot of
    // `reentry_registration` (free rows never get a map entry, matching
    // `XrefRegistration::insert_free_xref_entry`). Live-entry priority is
    // exact-`ObjectRef` keyed, matching qpdf's `insertXrefEntry`
    // (`QPDF.cc:1149-1181`): `m->xref_table.try_emplace(QPDFObjGen(obj, f2))`
    // only disregards the candidate's entry when the line scan already
    // populated that *same* (number, generation) pair -- an obsolete
    // generation's own leftover entry from the line scan must not suppress
    // the candidate's entry for a distinct generation of the same object
    // number. Free rows remain local tombstones in the candidate's own
    // `XrefRegistration`, matching `insertFreeXrefEntry`.
    for (object_ref, xref_entry) in reentry.loaded.entries {
        entries.entry(object_ref).or_insert(xref_entry);
    }
    entries.retain(|object_ref, _| !deleted_objects.contains(&object_ref.number));
    parsed_xref_streams.extend(reentry.parsed_xref_streams);
    trailer_references.extend(reentry.trailer_references);
    // The candidate re-entry (and any `/Prev` chain it follows) can itself
    // emit repair warnings (e.g. stream-length recovery); propagate the
    // successfully merged `/Prev` diagnostics here. The candidate's own
    // diagnostics were appended before the merge above.
    for diagnostic in reentry
        .loaded
        .repair_diagnostics
        .entries()
        .iter()
        .skip(candidate_diagnostic_count)
    {
        repair_diagnostics.push(diagnostic.clone());
    }
    deliver_canonical_diagnostics(canonical_trailer_owner, repair_diagnostics)?;
    // qpdf's post-chain `m->trailer.getKey("/Size").getIntValueAsInt()`
    // dereferences an indirect `/Size` through the reconstructed table
    // (`QPDF.cc:697`). Resolve it through the same canonical owner used while
    // re-entering the candidate; the owner-less loader retains its bootstrap
    // context instead of inspecting the raw reference.
    let resolved_size = if let Some(owner) = canonical_trailer_owner {
        owner.install_xref_entries(entries.clone());
        let mut context = CanonicalXrefContext::new(owner, options.description.clone());
        let value = context.resolve_dictionary_value(&candidate.trailer, "Size");
        context.append_diagnostics_to(repair_diagnostics);
        value
    } else {
        let merged_reference_offsets = reconstructed_reference_offsets(entries);
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ReconstructionWithCache {
                line_scan_entries: entries,
                reference_offsets: &merged_reference_offsets,
                bootstrap_cache: candidate
                    .bootstrap_cache
                    .as_ref()
                    .expect("owner-less candidate has a bootstrap cache"),
            },
            &reentry_registration,
            options.clone(),
        );
        let value = context.resolve_dictionary_value(&candidate.trailer, "Size");
        context.cache.commit();
        context.append_diagnostics_to(repair_diagnostics);
        value
    };
    append_xref_size_warning_for(
        resolved_size.as_ref(),
        highest_object_number(entries.keys().map(|key| key.number)),
        &deleted_objects,
        &options.description,
        repair_diagnostics,
    );
    deliver_canonical_diagnostics(canonical_trailer_owner, repair_diagnostics)?;

    Ok((
        candidate.trailer,
        max_offset,
        reentry.loaded.last_xref_form,
        deleted_objects,
        first_xref_item_offset,
        candidate.bootstrap_cache,
    ))
}

/// The `/Type /XRef` candidate this file's line-scanned entries point at:
/// its dictionary (which may or may not be the winning trailer -- see
/// [`find_xref_stream_trailer_candidate`]'s doc) and its true maximum
/// offset (the re-entry point).
struct XrefStreamCandidate {
    trailer: ObjectHandle,
    max_offset: u64,
    /// The owner-less reconstruction pass resolves and caches every type-1
    /// object while discovering candidates. Reuse that cache for the later
    /// post-chain `/Size` lookup so a repair warning is not emitted again;
    /// canonical-owner discovery uses the document cache and leaves this
    /// field `None`.
    bootstrap_cache: Option<SharedBootstrapCache>,
}

/// Find the trailer dictionary and re-entry offset for
/// [`recover_trailer_from_xref_stream_candidate`], alongside the repair
/// diagnostics recorded while resolving *every* stream object encountered
/// along the way (any type, not just `/Type /XRef` -- qpdf's
/// `getObjectByObjGen(iter.first)` runs before the `isStreamOfType` check,
/// `QPDF.cc:585-589`, so it reads, and can warn about, any type-1 stream,
/// matched or not). Returns `(None, _)` when no reconstructed type-1 entry
/// is a `/Type /XRef` stream -- the diagnostics are still meaningful in
/// that case, so the caller must not discard them just because no
/// candidate was found.
///
/// Candidates are visited in `entries`'s own ascending-object-number order
/// (`BTreeMap<ObjectRef, _>`, matching qpdf's `std::map<QPDFObjGen, _>`
/// iteration order for `m->xref_table`) -- *not* ascending offset order. The
/// two are not interchangeable: object numbers need not correlate with file
/// position, and the "first candidate wins the trailer" quirk this mirrors
/// depends specifically on object-number order. Diagnostics are collected
/// in this same scan order, matching qpdf's own warning sequence (each
/// object warns, if it needs to, exactly when discovery resolves it).
///
/// A separate offset-sorted index bounds each candidate's parse window to
/// the offset of its byte-adjacent neighbor, retrying with a wider window
/// when that truncates a real object (a header-like line recorded inside an
/// object's own payload can become a bogus next offset). That retry extends
/// by [`XREF_RECONSTRUCTION_FALLBACK_SPAN`] further offset-*positions* rather
/// than sharing one global attempt-count budget across the whole scan:
/// a shared budget lets enough earlier, unrelated truncated entries deny a
/// later, genuine candidate its own retry, which qpdf's per-object recovery
/// has no equivalent of.
fn find_xref_stream_trailer_candidate(
    bytes: &[u8],
    entries: &BTreeMap<ObjectRef, XrefEntry>,
    options: XrefLoadOptions,
    reference_offsets: &Rc<[u64]>,
    preexisting_bootstrap_cache: Option<&SharedBootstrapCache>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> (Option<XrefStreamCandidate>, Diagnostics) {
    if let Some(owner) = canonical_trailer_owner {
        return find_xref_stream_trailer_candidate_canonical(entries, options, owner);
    }
    let mut max_offset = 0u64;
    let mut trailer: Option<ObjectHandle> = None;
    let mut discovery_diagnostics = Diagnostics::default();
    let empty_registration = XrefRegistration::default();
    let context_spec = match preexisting_bootstrap_cache {
        Some(bootstrap_cache) => XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries: entries,
            reference_offsets,
            bootstrap_cache,
        },
        None => XrefReadContextSpec::Reconstruction {
            line_scan_entries: entries,
            reference_offsets,
        },
    };
    let mut context = XrefReadContext::new(bytes, context_spec, &empty_registration, options);
    let mut emitted_diagnostics = 0usize;
    for (&object_ref, entry) in entries {
        let XrefEntry::Uncompressed { offset } = *entry else {
            continue;
        };
        let start = offset as usize;
        let next_offset_index = reference_offsets.partition_point(|&candidate| candidate <= offset);
        let window_end = reference_offsets
            .get(next_offset_index)
            .map_or(bytes.len(), |&next| next as usize);
        // `find_xref_stream_trailer_candidate` only ever runs after
        // `recover_xref_from_linear_scan` has already committed to repair
        // mode, matching qpdf's `attempt_recovery` (true by default) being
        // active throughout `reconstruct_xref`, including candidate
        // discovery's own object reads (`getObjectByObjGen` -> `readStream`,
        // `QPDF.cc:1391`). `Bounded` mirrors that: a directly-resolvable but
        // mismatched `/Length` still falls through to stream-boundary
        // recovery here, instead of being rejected outright.
        let parsed = if let Some(cached) = context.cache.get(&object_ref) {
            Some(cached)
        } else {
            let narrow =
                read_xref_candidate(&mut context, bytes, start, window_end, offset, object_ref);
            // Retry through the wider window when the bounded read failed or
            // only recovered a null. The narrow attempt is dropped together
            // with its diagnostics so the accepted read alone speaks for the
            // object, as `read_uncompressed_object` does for its own retry:
            // qpdf reads each candidate once, to EOF (`QPDF.cc:580-607`), and
            // never sees the window boundary that produced them.
            let retry =
                window_end < bytes.len() && narrow.as_ref().is_none_or(is_recovered_null_candidate);
            let accepted = if retry {
                let wide_index = next_offset_index
                    .saturating_add(XREF_RECONSTRUCTION_FALLBACK_SPAN)
                    .min(reference_offsets.len());
                let wide_end = reference_offsets
                    .get(wide_index)
                    .map_or(bytes.len(), |&next| next as usize);
                read_xref_candidate(&mut context, bytes, start, wide_end, offset, object_ref)
            } else {
                narrow
            };
            accepted.and_then(|completed| {
                commit_xref_candidate(&mut context, completed, offset, object_ref)
            })
        };
        append_new_context_diagnostics(
            &context,
            &mut discovery_diagnostics,
            &mut emitted_diagnostics,
        );
        let Some(object) = parsed else {
            continue;
        };
        // qpdf's `getObjectByObjGen` (`QPDF.cc:585`) resolves the object
        // before `isStreamOfType` (`QPDF.cc:587`) is even checked, so a
        // non-stream object's own read warnings (e.g. "expected endobj",
        // `QPDF.cc:1352-1355`) are collected above regardless of whether it
        // turns out to be a stream at all.
        let Some(stream_dict) = object.as_stream_dict() else {
            continue;
        };
        if !is_xref_stream_dict(&mut context, &stream_dict) {
            append_new_context_diagnostics(
                &context,
                &mut discovery_diagnostics,
                &mut emitted_diagnostics,
            );
            continue;
        }
        append_new_context_diagnostics(
            &context,
            &mut discovery_diagnostics,
            &mut emitted_diagnostics,
        );
        if offset > max_offset {
            max_offset = offset;
            if trailer.is_none() {
                trailer = Some(stream_dict);
            }
        }
    }

    context.cache.commit();
    let candidate = trailer.map(|dict| XrefStreamCandidate {
        trailer: dict,
        max_offset,
        bootstrap_cache: Some(context.cache.shared()),
    });
    append_new_context_diagnostics(
        &context,
        &mut discovery_diagnostics,
        &mut emitted_diagnostics,
    );
    (candidate, discovery_diagnostics)
}

fn find_xref_stream_trailer_candidate_canonical(
    entries: &BTreeMap<ObjectRef, XrefEntry>,
    options: XrefLoadOptions,
    owner: &dyn CanonicalTrailerOwner,
) -> (Option<XrefStreamCandidate>, Diagnostics) {
    owner.install_xref_entries(entries.clone());
    let mut context = CanonicalXrefContext::new(owner, options.description);
    let mut max_offset = 0u64;
    let mut trailer = None;
    // qpdf's candidate search resolves every type-1 entry unconditionally,
    // warning immediately as each is read (`QPDF.cc:585-589`). The canonical
    // owner is the sole warning sink, so those reads are delivered directly;
    // only diagnostics created by the xref context itself remain local until
    // the caller flushes this returned batch.
    for (&object_ref, entry) in entries {
        let XrefEntry::Uncompressed { offset } = *entry else {
            continue;
        };
        let object = owner.indirect_handle(object_ref);
        let _ = object.try_dereference();
        context.sync_handle_diagnostics();
        let Some(stream_dict) = object.as_stream_dict() else {
            continue;
        };
        if !is_xref_stream_dict(&mut context, &stream_dict) {
            continue;
        }
        context.sync_handle_diagnostics();
        if offset > max_offset {
            max_offset = offset;
            if trailer.is_none() {
                trailer = Some(stream_dict);
            }
        } // cov:ignore: LLVM maps the covered canonical candidate offset branch to its closing brace
    }
    let mut diagnostics = Diagnostics::default();
    context.append_diagnostics_to(&mut diagnostics);
    let candidate = trailer.map(|trailer| XrefStreamCandidate {
        trailer,
        max_offset,
        bootstrap_cache: None,
    });
    (candidate, diagnostics)
}

/// Read one reconstruction candidate through `start..end` without touching
/// the shared context: diagnostics and the canonical cache are committed by
/// [`commit_xref_candidate`] only for the attempt
/// [`find_xref_stream_trailer_candidate`] finally accepts.
fn read_xref_candidate(
    context: &mut XrefReadContext,
    bytes: &[u8],
    start: usize,
    end: usize,
    offset: u64,
    object_ref: ObjectRef,
) -> Option<HandleFileObjectRead> {
    let input = bytes.get(start..end)?;
    let mut completed = context
        .read_file_object_handle(
            input,
            offset,
            RecoveryPolicy::Bounded,
            XrefObjectDescription::Ordinary,
        )
        .ok()?;
    if completed.object_ref != object_ref {
        return None;
    }
    let _ = completed.remove_included_recovery_eol_for_decryption();
    Some(completed)
}

/// A bounded reconstruction read can recover an incomplete container as a
/// null handle while retaining tokenizer/EOF diagnostics. That cannot be a
/// /Type /XRef candidate, but the caller may recover the real object by
/// retrying with the wider offset window.
fn is_recovered_null_candidate(completed: &HandleFileObjectRead) -> bool {
    completed.object.is_null() && !completed.diagnostics.is_empty()
}

/// Commit the accepted candidate read: record its diagnostics and cache the
/// parsed handle. A recovered null is cached as well -- qpdf's `resolve`
/// caches the null it substitutes for a failed read (`QPDF.cc:1738-1748`),
/// so a later reference to the object neither re-reads it nor repeats its
/// diagnostics; the caller's `/Type /XRef` check is what excludes it as a
/// candidate.
fn commit_xref_candidate(
    context: &mut XrefReadContext,
    completed: HandleFileObjectRead,
    offset: u64,
    object_ref: ObjectRef,
) -> Option<ObjectHandle> {
    for diagnostic in completed.diagnostics {
        context.diagnostics.push(xref_file_object_diagnostic(
            XrefObjectDescription::Ordinary,
            object_ref,
            offset,
            &context.document.options.description,
            diagnostic,
        ));
    }
    let canonical = context.document.handle_for_reference(object_ref);
    let value = completed.object.into_direct_value()?.0;
    canonical.set_resolved(value);
    context.cache.insert(object_ref, canonical.clone());
    Some(canonical)
}

fn append_new_context_diagnostics(
    context: &XrefReadContext,
    diagnostics: &mut Diagnostics,
    emitted: &mut usize,
) {
    for diagnostic in context.diagnostics.entries().iter().skip(*emitted) {
        diagnostics.push(diagnostic.clone());
    }
    *emitted = context.diagnostics.entries().len();
}

fn is_xref_stream_dict(context: &mut dyn XrefObjectContext, dict: &ObjectHandle) -> bool {
    context
        .resolve_dictionary_value(dict, "Type")
        .and_then(|value| value.try_as_name().ok())
        .flatten()
        .is_some_and(|name| name.as_slice() == b"XRef")
}

/// Push the qpdf-compatible repair warning sequence onto `diagnostics`.
///
/// qpdf (`reconstruct_xref` in `QPDF.cc`, observed with qpdf 11.9.0)
/// emits the same three warnings regardless of how the damaged
/// cross-reference data is ultimately recovered: `file is damaged`, the error
/// that triggered recovery, and `Attempting to reconstruct cross-reference
/// table`. `trigger_error` is the first failure that initiated recovery;
/// subsequent failures from the retry-at-offset-0 detour are not reported
/// because qpdf has no such detour and they have no counterpart on its
/// stderr. The triggering error's warning carries that error's own byte
/// offset when available (falling back to the `startxref` offset); the
/// surrounding warnings carry no offset, matching qpdf, which reports them
/// at offset 0 and suppresses the display.
fn push_repair_diagnostics(
    diagnostics: &mut Diagnostics,
    trigger_error: &Error,
    startxref: u64,
    filename: &[u8],
) {
    diagnostics.push(QpdfExc::new(
        QpdfErrorCode::DamagedPdf,
        filename,
        b"",
        0,
        b"file is damaged",
    ));
    if let Error::QpdfExc(warning) = trigger_error {
        diagnostics.push(warning.clone());
        diagnostics.push(QpdfExc::new(
            QpdfErrorCode::DamagedPdf,
            filename,
            b"",
            0,
            b"Attempting to reconstruct cross-reference table",
        ));
        return;
    }
    let (object, message, offset) = match trigger_error {
        Error::Parse { offset: 0, message } if message == "xref not found" => {
            (Vec::new(), b"can't find startxref".to_vec(), 0)
        }
        Error::Parse { message, .. } if message == "can't find startxref" => {
            (Vec::new(), message.as_bytes().to_vec(), 0)
        }
        Error::Parse { message, .. } if message == "loop detected following xref tables" => {
            (Vec::new(), message.as_bytes().to_vec(), 0)
        }
        Error::Parse { offset, message } if is_classic_trailer_validation_message(message) => (
            b"trailer".to_vec(),
            message.as_bytes().to_vec(),
            *offset as i64,
        ),
        Error::Parse { offset, message }
            if message.starts_with("unknown xref stream entry type ") =>
        {
            (
                b"xref stream".to_vec(),
                message.as_bytes().to_vec(),
                *offset as i64,
            )
        }
        Error::Parse { offset, message } => {
            (Vec::new(), message.as_bytes().to_vec(), *offset as i64)
        }
        // qpdf's outer `parse` catches non-QPDF exceptions raised by
        // `read_xref` and turns them into a damaged-PDF exception with the
        // fixed `error reading xref: ` prefix and offset zero
        // (`QPDF.cc:450-464`). Keep the inner I/O message rather than the
        // public Error display, which would add flpdf's `I/O error: ` label.
        Error::Io(error) => (
            Vec::new(),
            format!("error reading xref: {error}").into_bytes(),
            0,
        ),
        // cov:ignore-start: non-qpdf transport variants are normalized before reconstruction diagnostics are built
        Error::SystemBytes(message) => (
            Vec::new(),
            [b"error reading xref: ".as_slice(), message].concat(),
            0,
        ),
        Error::System(message) | Error::Internal(message) | Error::Unsupported(message) => (
            Vec::new(),
            format!("error reading xref: {message}").into_bytes(),
            0,
        ),
        // cov:ignore-end
        // cov:ignore-start: the caller guards reconstruction failures to qpdf damage or parse variants
        _ => (
            Vec::new(),
            trigger_error.raw_message().unwrap_or_default().to_vec(),
            startxref as i64,
        ),
        // cov:ignore-end
    };
    diagnostics.push(QpdfExc::new(
        QpdfErrorCode::DamagedPdf,
        filename,
        object,
        offset,
        message,
    ));
    diagnostics.push(QpdfExc::new(
        QpdfErrorCode::DamagedPdf,
        filename,
        b"",
        0,
        b"Attempting to reconstruct cross-reference table",
    ));
}

fn is_classic_trailer_validation_message(message: &str) -> bool {
    matches!(
        message,
        "trailer dictionary lacks /Size key"
            | "/Size key in trailer dictionary is not an integer"
            | "/Prev key in trailer dictionary is not an integer"
    )
}

/// Read a trailer through the same parser boundary used by qpdf's
/// `QPDF::readTrailer` (`QPDF.cc:1312-1328`). The parser-created handle stays
/// owned by the supplied resolver, while the post-parse lookahead uses qpdf's
/// `readToken` contract (`allow_bad = true`) only to detect an unexpected
/// `stream` keyword.
fn read_trailer(
    input: &[u8],
    start: usize,
    filename: &[u8],
    resolver: &mut dyn HandleResolver,
) -> Result<(ObjectHandle, Vec<QpdfExc>)> {
    let slice = input
        .get(start..)
        .ok_or_else(|| Error::parse(start, "trailer is not a dictionary"))?;
    let parsed = parse_qpdf_file_object_handle_with_diagnostics(
        slice,
        i64::try_from(start).unwrap_or(i64::MAX),
        None,
        resolver,
    )
    .map_err(|error| error.rebase_offset(start))?;
    let mut diagnostics = trailer_diagnostics(start, parsed.diagnostics, filename, Some(slice));
    if let Some(empty_offset) = parsed.empty_offset {
        diagnostics.push(trailer_warning(
            filename,
            "empty object treated as null",
            Some(start.saturating_add(empty_offset) as u64),
        ));
    } else if parsed.value.try_is_dictionary()? {
        let mut tokenizer = Tokenizer::new(slice);
        tokenizer.allow_eof();
        tokenizer
            .set_position(parsed.next_offset)
            .map_err(|error| error.rebase_offset(start))?;
        let token = tokenizer
            .read_token(true, 0)
            .map_err(|error| error.rebase_offset(start))?;
        if token.is_word_value(b"stream") {
            diagnostics.push(trailer_warning(
                filename,
                "stream keyword found in trailer",
                Some(start.saturating_add(tokenizer.position()) as u64),
            ));
        }
    }
    Ok((parsed.value, diagnostics))
}

fn parse_trailer_candidate(
    bytes: &[u8],
    start: usize,
    filename: &[u8],
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> (Option<ObjectHandle>, Vec<QpdfExc>) {
    if bytes.get(start..).is_none() {
        return (None, Vec::new());
    }
    // qpdf's reconstruct_xref calls readTrailer() unconditionally and only
    // rejects a non-dictionary result afterward ("Oh well.  It was worth a
    // try.", `QPDF.cc:566-568`); any warning the parser already raised while
    // building that rejected candidate still reaches `m->warnings`. Extract
    // diagnostics regardless of whether the parse ultimately produced a
    // dictionary, a different object, or an error.
    let result = if let Some(owner) = canonical_trailer_owner {
        let mut resolver = CanonicalTrailerParser { owner };
        read_trailer(bytes, start, filename, &mut resolver)
    } else {
        let mut resolver = XrefDetachedHandles;
        read_trailer(bytes, start, filename, &mut resolver)
    };
    let (trailer, diagnostics) = match result {
        Ok((handle, diagnostics)) => (
            handle
                .try_is_dictionary()
                .ok()
                .filter(|is_dict| *is_dict)
                .map(|_| handle),
            diagnostics,
        ),
        Err(_) => (None, Vec::new()), // cov:ignore: the context-aware parser recovers malformed trailer tokens as null with diagnostics
    };
    (trailer, diagnostics)
}

/// Format qpdf's `readTrailer()` parser diagnostics with its
/// `object_description = "trailer"` attribution (`QPDF::readTrailer`,
/// `QPDF.cc:1313-1317`; `QPDFExc::createWhat`, `QPDFExc.cc:18-49`):
/// `(trailer, offset N): <message>`, where `N` is the absolute file offset
/// qpdf's `frame->offset` (`QPDFParser.hh:38-44`) would report -- `start`
/// (the trailer parser's own byte-slice origin) plus the parser's
/// slice-relative offset.
fn trailer_diagnostics(
    start: usize,
    diagnostics: Vec<ParserDiagnostic>,
    filename: &[u8],
    source: Option<&[u8]>,
) -> Vec<QpdfExc> {
    fn invalid_hex_byte(source: &[u8], offset: usize) -> Option<u8> {
        let mut position = offset;
        if source.get(position) == Some(&b'<') {
            position = position.saturating_add(1);
            while let Some(&byte) = source.get(position) {
                if byte == b'>' {
                    return None;
                }
                if byte.is_ascii_hexdigit()
                    || matches!(
                        byte,
                        b'\0' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r' | b' '
                    )
                {
                    position = position.saturating_add(1);
                    continue;
                }
                return Some(byte);
            }
            return None;
        }
        source.get(position).copied()
    }

    diagnostics
        .into_iter()
        .map(|diagnostic| {
            let offset = (start as u64).saturating_add(diagnostic.relative_offset as u64);
            let message = if diagnostic.message.starts_with("invalid character (") {
                source
                    .and_then(|source| invalid_hex_byte(source, diagnostic.relative_offset))
                    .map(|byte| {
                        let mut message = b"invalid character (".to_vec();
                        message.push(byte);
                        message.extend_from_slice(b") in hexstring");
                        message
                    })
                    .unwrap_or_else(|| diagnostic.message.into_bytes())
            } else {
                diagnostic.message.into_bytes()
            };
            QpdfExc::new(
                QpdfErrorCode::DamagedPdf,
                filename,
                b"trailer",
                i64::try_from(offset).unwrap_or(i64::MAX),
                message,
            )
        })
        .collect()
}

fn trailer_warning(filename: &[u8], message: impl Into<String>, offset: Option<u64>) -> QpdfExc {
    QpdfExc::new(
        QpdfErrorCode::DamagedPdf,
        filename,
        b"trailer",
        offset
            .map(|value| i64::try_from(value).unwrap_or(i64::MAX))
            .unwrap_or(0),
        message.into().into_bytes(),
    )
}

/// Return the offset just past the next end-of-line at or after `from`, or
/// `bytes.len()` when no further end-of-line exists. A run of consecutive
/// `\r`/`\n` bytes is treated as a single line terminator (mirroring qpdf's
/// `findAndSkipNextEOL`, which collapses `\r\n` and blank lines). When
/// `from < bytes.len()` the result is always strictly greater than `from`, so
/// the line scan in [`recover_xref_entries`] always makes progress.
fn next_line_start(bytes: &[u8], from: usize) -> usize {
    let mut pos = from;
    while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
        pos += 1;
    }
    // Skip the run of end-of-line bytes so blank lines do not become their own
    // iterations; this keeps the scan linear by advancing `line_start` past the
    // whole run that a forward token read would otherwise re-scan. When no
    // end-of-line exists this loop is a no-op and `pos` is already `bytes.len()`.
    while pos < bytes.len() && matches!(bytes[pos], b'\n' | b'\r') {
        pos += 1;
    }
    pos
}

const XREF_RECONSTRUCTION_MAX_TOKEN_LEN: usize = 100;

/// Read the next qpdf token whose start lies in `[from, limit)`. The bounded
/// prefix keeps whitespace/comment-only line floods linear. A token beginning
/// on the line is still complete because `next_line_start` includes its EOL
/// delimiter. Later header tokens use the full input so they may span lines,
/// matching qpdf's reconstruction loop.
fn read_scan_token(bytes: &[u8], from: usize, limit: usize) -> Option<Token> {
    let bounded = bytes.get(..limit)?;
    let mut tokenizer = Tokenizer::new(bounded);
    tokenizer.allow_eof();
    tokenizer.set_position(from).ok()?;
    let token = tokenizer
        .read_token(true, XREF_RECONSTRUCTION_MAX_TOKEN_LEN)
        .ok()?;
    (token.token_type != TokenType::Eof && token.start < limit).then_some(token)
}

fn parse_scan_integer(token: &Token) -> Result<i32> {
    let text = std::str::from_utf8(&token.value).unwrap_or_default();
    match qpdf_string_to_int_checked(text) {
        QpdfIntParse::Value(value) => Ok(value),
        QpdfIntParse::NoDigits => Ok(0),
        QpdfIntParse::Overflow(message) => Err(Error::SystemBytes(message.into_bytes())),
    }
}

/// If the already-read first token opens an `int int obj` token sequence,
/// return the recovered object and the offset of its number token.
///
/// Mirrors qpdf's `reconstruct_xref` per-line logic: the first token must begin
/// on this line (otherwise the line records nothing — qpdf's
/// `token_start >= next_line_start` guard, here enforced by bounding the first
/// token read to `next_line_start`), the second and third tokens may spill onto
/// following lines, and the object/generation must satisfy qpdf's
/// `insertReconstructedXrefEntry` guards (`obj > 0`, `0 <= gen < 65535`).
fn scan_object_header_after_first_token(
    bytes: &[u8],
    number_token: &Token,
) -> Result<Option<(ObjectRef, u64)>> {
    let Some(gen_token) = read_scan_token(bytes, number_token.end, bytes.len()) else {
        return Ok(None);
    };
    if !gen_token.is_integer() {
        return Ok(None);
    }

    let Some(obj_token) = read_scan_token(bytes, gen_token.end, bytes.len()) else {
        return Ok(None);
    };
    if !obj_token.is_word_value(b"obj") {
        return Ok(None);
    }

    let obj = parse_scan_integer(number_token)?;
    let gen = parse_scan_integer(&gen_token)?;

    // qpdf's `insertReconstructedXrefEntry` guards (`obj > 0`, `0 <= gen < 65535`).
    if obj <= 0 || !(0..65535).contains(&gen) {
        return Ok(None);
    }
    let number = u32::try_from(obj).expect("positive qpdf int fits u32");
    let generation = u16::try_from(gen).expect("qpdf generation guard fits u16");
    Ok(Some((
        ObjectRef::new(number, generation),
        number_token.start as u64,
    )))
}

fn parse_xref_table(
    cursor: &mut ByteCursor<'_>,
    bytes: &[u8],
    mut first_xref_item_offset_sink: Option<&mut Option<u64>>,
    filename: &[u8],
) -> Result<(Vec<ParsedXrefEntry>, usize, Vec<QpdfExc>, u64)> {
    let mut entries = Vec::new();
    let mut first_xref_item_offset = 0;
    let mut table_diagnostics = Vec::new();
    loop {
        cursor.skip_ws();
        let section_start = cursor.pos;
        if cursor.peek_word(b"trailer") {
            let _ = cursor.read_token()?;
            break;
        }
        let header_start = cursor.pos;
        let header = cursor.read_bytes(50);
        let (first, count, header_bytes) =
            parse_xref_first_line_with_bytes(&header).ok_or_else(|| {
                Error::QpdfExc(QpdfExc::new(
                    QpdfErrorCode::DamagedPdf,
                    filename,
                    b"xref table",
                    i64::try_from(section_start).unwrap_or(i64::MAX),
                    b"xref syntax invalid",
                ))
            })?;
        // qpdf reads a fixed 50-byte buffer and then seeks to the number of
        // bytes consumed by `parse_xrefFirst` (`QPDF.cc:725-767`), so the
        // subsection header may span physical lines. Do not leave the cursor
        // at the end of the speculative buffer.
        cursor.pos = header_start.saturating_add(header_bytes);
        if first == 0 && count > 0 {
            first_xref_item_offset = cursor.pos as u64;
            if let Some(sink) = first_xref_item_offset_sink.as_deref_mut() {
                *sink = Some(first_xref_item_offset);
            }
        }
        for index in 0..count {
            let entry_offset = cursor.pos;
            let line = cursor.read_line(30);
            let (offset, generation, in_use, invalid) =
                parse_xref_entry_line(&line).ok_or_else(|| {
                    Error::QpdfExc(QpdfExc::new(
                        QpdfErrorCode::DamagedPdf,
                        filename,
                        b"xref table",
                        i64::try_from(entry_offset).unwrap_or(i64::MAX),
                        format!("invalid xref entry (obj={})", first + index).into_bytes(),
                    ))
                })?;
            if invalid {
                table_diagnostics.push(damaged_warning(
                    filename,
                    b"xref table",
                    "accepting invalid xref table entry",
                    Some(entry_offset as u64),
                ));
            }
            // cov:ignore-start: object numbers are narrowed to i32 immediately
            // below, so a u32 addition overflow cannot be reached by a valid
            // parsed xref row.
            let object_number = first.checked_add(index).ok_or_else(|| {
                Error::QpdfExc(QpdfExc::new(
                    QpdfErrorCode::DamagedPdf,
                    filename,
                    b"xref table",
                    i64::try_from(entry_offset).unwrap_or(i64::MAX),
                    b"invalid xref entry",
                ))
            })?;
            // cov:ignore-end
            let object_ref = QpdfObjGen::new(
                i32::try_from(object_number)
                    .map_err(|_| Error::parse(0, "object number does not fit i32"))?,
                generation,
            );
            match in_use {
                b'f' => {
                    let _next = offset;
                    entries.push(ParsedXrefEntry::Free { object_ref });
                }
                b'n' => {
                    entries.push(ParsedXrefEntry::Live {
                        object_ref,
                        entry: XrefEntry::Uncompressed { offset },
                    });
                }
                // cov:ignore-start: parse_xref_entry_line accepts only the
                // two in-use markers handled above.
                _ => {
                    return Err(Error::QpdfExc(QpdfExc::new(
                        QpdfErrorCode::DamagedPdf,
                        filename,
                        b"xref table",
                        i64::try_from(entry_offset).unwrap_or(i64::MAX),
                        format!("invalid xref entry (obj={object_number})").into_bytes(),
                    )))
                } // cov:ignore-end
            }
        }
    }

    let trailer_start = cursor.pos;
    let _ = bytes;
    Ok((
        entries,
        trailer_start,
        table_diagnostics,
        first_xref_item_offset,
    ))
}

/// `QUtil::is_space` (`include/qpdf/QUtil.hh:497-501`). NUL is deliberately
/// absent: `parse_xrefEntry` relies on `is_space('\0')` being false to stop at
/// the end of its buffer (`QPDF.cc:775-782`).
fn is_pdf_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r' | b' ')
}

/// The tokenizer's `is_delimiter` (`libqpdf/QPDFTokenizer.cc:16-23`), which is
/// what ends a keyword. `readToken(...).isWord("trailer")` (`QPDF.cc:889`)
/// therefore accepts `trailer<<` as readily as `trailer\n<<`.
fn is_pdf_delimiter(byte: u8) -> bool {
    is_pdf_space(byte)
        || matches!(
            byte,
            b'/' | b'(' | b')' | b'{' | b'}' | b'<' | b'>' | b'[' | b']' | b'%' | b'\0'
        )
}

#[cfg(test)]
fn parse_xref_first_line(line: &[u8]) -> Option<(u32, u32)> {
    parse_xref_first_line_with_bytes(line).map(|(first, count, _)| (first, count))
}

fn parse_xref_first_line_with_bytes(line: &[u8]) -> Option<(u32, u32, usize)> {
    let mut pos = 0;
    while line.get(pos).copied().is_some_and(is_pdf_space) {
        pos += 1;
    }
    let first_start = pos;
    while line.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    if pos == first_start || !line.get(pos).copied().is_some_and(is_pdf_space) {
        return None;
    }
    let first_end = pos;
    while line.get(pos).copied().is_some_and(is_pdf_space) {
        pos += 1;
    }
    let count_start = pos;
    while line.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    if pos == count_start {
        return None;
    }
    let first = std::str::from_utf8(&line[first_start..first_end])
        .ok()?
        .parse::<u64>()
        .ok()?;
    let count = std::str::from_utf8(&line[count_start..pos])
        .ok()?
        .parse::<u64>()
        .ok()?;
    while line.get(pos).copied().is_some_and(is_pdf_space) {
        pos += 1;
    }
    Some((u32::try_from(first).ok()?, u32::try_from(count).ok()?, pos))
}

fn parse_xref_entry_line(line: &[u8]) -> Option<(u64, i32, u8, bool)> {
    let mut pos = 0;
    let mut invalid = false;
    while line.get(pos).copied().is_some_and(is_pdf_space) {
        invalid = true;
        pos += 1;
    }
    let first_start = pos;
    while line.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    if pos == first_start || !line.get(pos).copied().is_some_and(is_pdf_space) {
        return None;
    }
    if line.get(pos + 1).copied().is_some_and(is_pdf_space) {
        invalid = true;
    }
    while line.get(pos).copied().is_some_and(is_pdf_space) {
        pos += 1;
    }
    let second_start = pos;
    while line.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    if pos == second_start || !line.get(pos).copied().is_some_and(is_pdf_space) {
        return None;
    }
    if line.get(pos + 1).copied().is_some_and(is_pdf_space) {
        invalid = true;
    }
    while line.get(pos).copied().is_some_and(is_pdf_space) {
        pos += 1;
    }
    let in_use = *line.get(pos)?;
    if !matches!(in_use, b'f' | b'n') {
        return None;
    }
    let first_text = std::str::from_utf8(&line[first_start..second_start])
        .ok()?
        .trim();
    let second_text = std::str::from_utf8(&line[second_start..pos]).ok()?.trim();
    invalid |= first_text.len() != 10 || second_text.len() != 5;
    Some((
        first_text.parse::<u64>().ok()?,
        second_text.parse::<i32>().ok()?,
        in_use,
        invalid,
    ))
}

#[allow(clippy::too_many_arguments)]
fn parse_xref_stream(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: String,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    canonical_trailer_owner: Option<&dyn CanonicalTrailerOwner>,
) -> Result<LoadedXrefState> {
    // qpdf's `read_xrefStream` wraps its whole body in
    // `if (!m->ignore_xref_streams)` and otherwise falls straight through to
    // `throw damagedPDF("", xref_offset, "xref not found")` — the offset is
    // never read, so this precedes the end-of-file check below. The same error
    // is what a non-stream object at the offset produces.
    if options.ignore_xref_streams {
        return Err(Error::parse(xref_pos, "xref not found"));
    }
    if let Some(owner) = canonical_trailer_owner {
        return parse_xref_stream_with_canonical_owner(
            xref_pos,
            startxref,
            version,
            options,
            registration,
            owner,
        );
    }
    let allow_repair = options.allow_repair;
    let tail = bytes
        .get(xref_pos..)
        .filter(|slice| !slice.is_empty())
        .ok_or_else(|| Error::parse(xref_pos, "xref stream offset is beyond end of file"))?;
    let policy = if allow_repair {
        RecoveryPolicy::Bounded
    } else {
        RecoveryPolicy::RequireEndstream
    };
    let mut repair_diagnostics = Diagnostics::default();
    // Keep the borrowed bootstrap context alive only while the xref stream
    // object and its dictionary are being decoded. qpdf mutates the shared
    // xref table after this read, so the Rust borrow must end before the
    // cumulative registration receives these entries.
    let (build_result, reconstruction_trigger, bootstrap_cache) = {
        let mut context = XrefReadContext::new(bytes, context_spec, registration, options);
        let mut handle_completed = match context.read_file_object_handle(
            tail,
            xref_pos as u64,
            policy,
            XrefObjectDescription::XrefStream,
        ) {
            Ok(completed) => completed,
            Err(error) => {
                context.append_diagnostics_to(&mut repair_diagnostics);
                if let Some(sink) = error_diagnostics_sink {
                    for diagnostic in repair_diagnostics.entries() {
                        sink.push(diagnostic.clone());
                    }
                }
                return Err(error.rebase_offset(xref_pos));
            }
        };
        // Xref streams are not encrypted, but filter decoding still requires
        // the logical payload rather than qpdf's raw recovery EOL.
        let _recovered_handle_eol = handle_completed.remove_included_recovery_eol_for_decryption();
        let stream_data_offset = handle_completed
            .stream_data_offset
            .map(|offset| xref_pos.saturating_add(offset));
        let handle_object = handle_completed.object;
        let object_ref = handle_completed.object_ref;
        // Push through `context.diagnostics` -- not directly into
        // `repair_diagnostics` -- so these framing diagnostics land AFTER
        // whatever `read_file_object_handle`'s own `sync_handle_diagnostics`
        // call already synced there (for example a warning raised while
        // resolving this stream's indirect `/Length` target, which runs
        // before `finish_file_object_handle` produces these diagnostics).
        // `context.append_diagnostics_to` below drains `context.diagnostics`
        // into `repair_diagnostics` in that same, qpdf-matching temporal
        // order; pushing straight into `repair_diagnostics` here would
        // report the recovery notice before the resolution warning that
        // caused it.
        for diagnostic in &handle_completed.diagnostics {
            context.diagnostics.push(xref_file_object_diagnostic(
                // cov:ignore: stream framing diagnostics are synchronized by the canonical finalization route
                XrefObjectDescription::XrefStream,
                object_ref,
                xref_pos as u64,
                &context.document.options.description, // cov:ignore: stream framing diagnostics are synchronized by the canonical finalization path
                diagnostic.clone(),
            ));
        }
        // qpdf's own read of this object (`readObjectAtOffset`, `QPDF.cc:956`)
        // happens before `processXRefStream` validates `/Type`, `/W`, `/Index`,
        // `/Size`, or the entry data (`QPDF.cc:960-1128`); any repair warning
        // already recorded above (e.g. stream-length recovery) is `warn()`-style
        // member state, not rolled back by a later validation failure in the
        // same call (empirically confirmed against qpdf 11.9.0: a candidate
        // needing repair whose `/W` then fails validation still shows the
        // repair warning before the terminal error). The shared builder keeps
        // that validation and diagnostic ordering identical for bootstrap and
        // canonical-owner reads.
        let build_result = build_xref_stream(
            &mut context,
            xref_pos,
            XrefStreamObjectRead {
                object_ref,
                object: handle_object.clone(),
                stream_data_offset,
            },
            registration,
        )
        .map(
            |(trailer, entries, trailer_references, has_first_xref_item)| {
                (
                    object_ref,
                    handle_object.clone(),
                    trailer,
                    entries,
                    trailer_references,
                    has_first_xref_item,
                )
            },
        );
        let reconstruction_trigger = context.take_reconstruction_trigger();
        context.append_diagnostics_to(&mut repair_diagnostics);
        context.cache.commit();
        let bootstrap_cache = context.cache.shared();
        (build_result, reconstruction_trigger, bootstrap_cache)
    };

    let (object_ref, handle_object, trailer, entries, trailer_references, has_first_xref_item) =
        match build_result {
            Ok(built) => built,
            Err(error) => {
                let error = reconstruction_trigger.unwrap_or(error);
                if let Some(sink) = error_diagnostics_sink {
                    for diagnostic in repair_diagnostics.entries() {
                        sink.push(diagnostic.clone());
                    }
                } // cov:ignore: diagnostic forwarding closes only on a sink-backed xref build failure
                return Err(error);
            }
        };

    for entry in entries {
        match entry {
            ParsedXrefEntry::Live { object_ref, entry } => {
                registration.insert_xref_entry(object_ref, entry);
            }
            ParsedXrefEntry::Free { object_ref } => {
                registration.insert_free_xref_entry(object_ref);
            }
        }
    }
    let parsed_xref_streams = BTreeMap::from([(object_ref, handle_object)]);
    let state = LoadedXrefState {
        loaded: LoadedXref {
            version,
            startxref,
            entries: registration.snapshot(),
            trailer,
            last_xref_form: XrefForm::Stream,
            repair_diagnostics,
        },
        raw_entries: registration.raw_snapshot(),
        first_xref_item_offset: if has_first_xref_item {
            xref_pos as u64
        } else {
            0
        },
        classic_trailer_offset: None,
        pending_reconstruction_trigger: None,
        trailer_references,
        parsed_xref_streams,
        bootstrap_cache: Some(bootstrap_cache),
        header_offset: 0,
        already_reconstructed: false,
    };

    if let Some(error) = reconstruction_trigger {
        if let Some(sink) = error_diagnostics_sink {
            for diagnostic in state.loaded.repair_diagnostics.entries() {
                sink.push(diagnostic.clone());
            }
        }
        return Err(error);
    }

    Ok(state)
}

fn xref_file_object_diagnostic(
    description: XrefObjectDescription,
    object_ref: ObjectRef,
    offset: u64,
    filename: &[u8],
    diagnostic: FileObjectDiagnostic,
) -> QpdfExc {
    let object = format!(
        "{}object {} {}",
        description.warning_prefix(),
        object_ref.number,
        object_ref.generation
    );
    QpdfExc::new(
        QpdfErrorCode::DamagedPdf,
        filename,
        object,
        i64::try_from(offset.saturating_add(diagnostic.relative_offset as u64)).unwrap_or(i64::MAX),
        diagnostic.kind.message().as_bytes(),
    )
}

type XrefWidths = (usize, usize, usize);

type XrefStreamBuild = (
    ObjectHandle,
    Vec<ParsedXrefEntry>,
    BTreeSet<ObjectRef>,
    bool,
);

struct XrefStreamObjectRead {
    object_ref: ObjectRef,
    object: ObjectHandle,
    stream_data_offset: Option<usize>,
}

fn build_xref_stream(
    context: &mut dyn XrefObjectContext,
    xref_pos: usize,
    object: XrefStreamObjectRead,
    registration: &mut XrefRegistration,
) -> Result<XrefStreamBuild> {
    let XrefStreamObjectRead {
        object_ref,
        object: handle_object,
        stream_data_offset,
    } = object;
    let handle_stream_dict = handle_object
        .as_stream_dict()
        .ok_or_else(|| Error::parse(xref_pos, "xref not found"))?;
    // QPDF::read_xrefStream accepts an xref stream only when
    // `isStreamOfType("/XRef")` succeeds. The shared parser owns this
    // check for both direct startxref streams and classic-trailer
    // `/XRefStm` targets.
    if !is_xref_stream_handle(context, &handle_object)? {
        return Err(Error::parse(xref_pos, "xref not found"));
    }

    let trailer = handle_stream_dict.clone();
    let size_value = handle_stream_dict
        .as_dictionary()
        .and_then(|entries| entries.get(b"/Size".as_slice()).cloned())
        .ok_or(Error::Missing("XRef stream /Size"))?;
    context.ensure_source_for_resolution(&size_value);
    let size = parse_non_negative_u64_handle(&size_value, "/Size")?;
    let size = u32::try_from(size).map_err(|_| Error::parse(0, "/Size does not fit u32"))?;

    let widths = parse_xref_widths_handle(context, &handle_stream_dict)?;
    // qpdf validates the summed `/W` entry size before reading `/Index` or
    // asking the stream for decoded bytes (`QPDF.cc:986-1018`). A zero sum is
    // a `damagedPDF("xref stream", xref_offset, message)` exception, not an
    // internal parser error, so preserve its description and offset through
    // recovery (`QPDF.cc:1003-1005`).
    let entry_size = widths
        .0
        .checked_add(widths.1)
        .and_then(|size| size.checked_add(widths.2))
        .ok_or_else(|| Error::parse(xref_pos, "xref stream entry size overflow"))?;
    if entry_size == 0 {
        return Err(Error::QpdfExc(QpdfExc::new(
            QpdfErrorCode::DamagedPdf,
            context.description(),
            b"xref stream",
            i64::try_from(xref_pos).unwrap_or(i64::MAX),
            b"Cross-reference stream's /W indicates entry size of 0",
        )));
    }
    let index = parse_xref_index_handle(context, &handle_stream_dict, size)?;
    let ranges = build_xref_ranges(index)?;
    let has_first_xref_item = ranges.iter().any(|&(start, count)| start == 0 && count > 0);
    // The bootstrap context's decoder inspects `/Filter`/`/DecodeParms`
    // itself and needs any indirect value staged first; the canonical-owner
    // context's `ensure_source_for_resolution` is a no-op since its resolver
    // reads indirect values live through `get_stream_data`.
    let stream_dictionary = handle_stream_dict.as_dictionary();
    if let Some(filter) = stream_dictionary
        .as_ref()
        .and_then(|dictionary| dictionary.get(b"/Filter".as_slice()))
    {
        context.ensure_source_for_resolution(filter);
    }
    if let Some(decode_parms) = stream_dictionary
        .as_ref()
        .and_then(|dictionary| dictionary.get(b"/DecodeParms".as_slice()))
    {
        context.ensure_source_for_resolution(decode_parms);
    }
    let stream_data = context.decoded_xref_stream_data(
        object_ref,
        &handle_stream_dict,
        &handle_object,
        xref_pos,
    )?;
    let expected_size = ranges.iter().try_fold(0usize, |total, &(_, count)| {
        let count = usize::try_from(count)
            .map_err(|_| Error::parse(xref_pos, "xref stream entry count overflow"))?;
        let range_size = entry_size.checked_mul(count).ok_or_else(|| {
            // cov:ignore-start: 32-bit oversized xref stream arithmetic requires an unrepresentable input
            Error::parse(xref_pos, "xref stream data size calculation overflow")
        })?; // cov:ignore-end
        total.checked_add(range_size).ok_or_else(|| {
            // cov:ignore-start: 32-bit oversized xref stream arithmetic requires an unrepresentable input
            Error::parse(xref_pos, "xref stream data size calculation overflow")
        }) // cov:ignore-end
    })?;
    if stream_data.len() < expected_size {
        return Err(Error::parse(
            xref_pos,
            format!(
                "Cross-reference stream data has the wrong size; expected = {expected_size}; actual = {}",
                stream_data.len()
            ),
        ));
    }
    if stream_data.len() > expected_size {
        // qpdf calls warn() as soon as the decoded payload size is known,
        // before parsing the entries (`QPDF.cc:1051-1065`), so a later
        // entry-decoding error must not discard this diagnostic.
        context.push_diagnostic(QpdfExc::new(
            QpdfErrorCode::DamagedPdf,
            context.description(),
            b"xref stream",
            i64::try_from(xref_pos).unwrap_or(i64::MAX),
            format!(
                "Cross-reference stream data has the wrong size; expected = {expected_size}; actual = {}",
                stream_data.len()
            )
            .into_bytes(),
        ));
    }
    let mut cursor = ByteCursor::new(&stream_data, 0);
    let entries = parse_xref_entries(
        &mut cursor,
        &ranges,
        widths,
        stream_data_offset,
        registration,
    )?;
    let trailer_references = collect_trailer_references(&trailer);

    Ok((trailer, entries, trailer_references, has_first_xref_item))
}

fn parse_xref_stream_with_canonical_owner(
    xref_pos: usize,
    startxref: u64,
    version: String,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    owner: &dyn CanonicalTrailerOwner,
) -> Result<LoadedXrefState> {
    owner.install_xref_entries(registration.snapshot());
    let mut context = CanonicalXrefContext::new(owner, options.description.clone());
    let (handle_object, _) =
        match owner.read_xref_stream_at_offset(xref_pos as u64, Some(b"xref stream".to_vec())) {
            Ok(read) => read,
            Err(error) => {
                let mut diagnostics = Diagnostics::default();
                context.append_diagnostics_to(&mut diagnostics);
                deliver_canonical_diagnostics(Some(owner), &mut diagnostics)?;
                // `QPDF::read_xrefStream` catches the QPDFExc raised by
                // `readObjectAtOffset` and then reports its own
                // `damagedPDF(xref_offset, "xref not found")` below
                // (`QPDF.cc:956-969`). Header/body parse failures are the Rust
                // equivalent of that caught QPDFExc; transport and warning-sink
                // failures must retain their original error class.
                return Err(match error {
                    Error::Parse { .. } => Error::parse(xref_pos, "xref not found"),
                    other => other,
                });
            }
        };
    context.sync_handle_diagnostics();
    let object_ref = handle_object
        .object_ref()
        .ok_or_else(|| Error::parse(xref_pos, "xref stream object is not indirect"))?;
    let stream_data_offset = handle_object
        .as_stream_dict()
        .and_then(|_| usize::try_from(handle_object.get_parsed_offset()).ok());
    let build_result = build_xref_stream(
        &mut context,
        xref_pos,
        XrefStreamObjectRead {
            object_ref,
            object: handle_object.clone(),
            stream_data_offset,
        },
        registration,
    );
    let reconstruction_trigger = context.take_reconstruction_trigger();
    let mut diagnostics = Diagnostics::default();
    context.append_diagnostics_to(&mut diagnostics);
    deliver_canonical_diagnostics(Some(owner), &mut diagnostics)?;
    let (trailer, entries, trailer_references, has_first_xref_item) = match build_result {
        Ok(build) => build,
        Err(error) => {
            let error = reconstruction_trigger.unwrap_or(error);
            return Err(error);
        }
    };
    for entry in entries {
        match entry {
            ParsedXrefEntry::Live { object_ref, entry } => {
                registration.insert_xref_entry(object_ref, entry);
            }
            ParsedXrefEntry::Free { object_ref } => {
                registration.insert_free_xref_entry(object_ref);
            }
        }
    }
    owner.install_xref_entries(registration.snapshot());
    let state = LoadedXrefState {
        loaded: LoadedXref {
            version,
            startxref,
            entries: registration.snapshot(),
            trailer,
            last_xref_form: XrefForm::Stream,
            repair_diagnostics: diagnostics,
        },
        raw_entries: registration.raw_snapshot(),
        first_xref_item_offset: if has_first_xref_item {
            xref_pos as u64
        } else {
            0
        },
        classic_trailer_offset: None,
        pending_reconstruction_trigger: None,
        trailer_references,
        // Keep the stream handle as qpdf obj_cache provenance. The final Pdf
        // constructor skips effective xref rows, but marks historical/free
        // rows as non-live while retaining them in the complete canonical cache view.
        parsed_xref_streams: BTreeMap::from([(object_ref, handle_object)]),
        bootstrap_cache: None,
        header_offset: 0,
        already_reconstructed: false,
    };
    // cov:ignore-start: CanonicalXrefContext never defers reconstruction; the live resolver performs any read-time recovery internally.
    if let Some(error) = reconstruction_trigger {
        return Err(error);
    }
    // cov:ignore-end
    Ok(state)
}

// qpdf 11.9.0's QPDF::processXRefStream rejects each /W value above
// sizeof(qpdf_offset_t) before summing the entry size (libqpdf/QPDF.cc:986-1003).
// qpdf_offset_t is a long long (include/qpdf/Types.h:31), so use the
// corresponding fixed-width Rust type rather than a platform-sized usize.
const MAX_XREF_FIELD_WIDTH: usize = std::mem::size_of::<i64>();

fn is_xref_stream_handle(
    context: &mut dyn XrefObjectContext,
    stream: &ObjectHandle,
) -> Result<bool> {
    let Some(stream_dict) = stream.as_stream_dict() else {
        return Ok(false);
    };
    context.ensure_source_for_resolution(&stream_dict);
    let type_value = stream_dict.try_get_key(b"/Type")?;
    context.ensure_source_for_resolution(&type_value);
    Ok(type_value.try_as_name()?.as_deref() == Some(b"XRef"))
}

fn parse_xref_widths_handle(
    context: &mut dyn XrefObjectContext,
    stream_dict: &ObjectHandle,
) -> Result<XrefWidths> {
    context.ensure_source_for_resolution(stream_dict);
    let value = stream_dict
        .as_dictionary()
        .and_then(|entries| entries.get(b"/W".as_slice()).cloned())
        .ok_or(Error::Missing("XRef stream /W"))?;
    context.ensure_source_for_resolution(&value);
    let values = value
        .try_as_array()?
        .ok_or_else(|| Error::parse(0, "/W must be array"))?;
    if values.len() != 3 {
        return Err(Error::parse(0, "/W must contain three integers"));
    }

    for value in &values {
        context.ensure_source_for_resolution(value);
    }
    let w0 = parse_usize(parse_non_negative_u64_handle(&values[0], "/W[0]")?, "/W[0]")?;
    let w1 = parse_usize(parse_non_negative_u64_handle(&values[1], "/W[1]")?, "/W[1]")?;
    let w2 = parse_usize(parse_non_negative_u64_handle(&values[2], "/W[2]")?, "/W[2]")?;
    if w0 > MAX_XREF_FIELD_WIDTH || w1 > MAX_XREF_FIELD_WIDTH || w2 > MAX_XREF_FIELD_WIDTH {
        return Err(Error::parse(
            0,
            "Cross-reference stream's /W contains impossibly large values",
        ));
    }
    Ok((w0, w1, w2))
}

fn parse_xref_index_handle(
    context: &mut dyn XrefObjectContext,
    stream_dict: &ObjectHandle,
    size: u32,
) -> Result<Vec<u32>> {
    context.ensure_source_for_resolution(stream_dict);
    let value = stream_dict.try_get_key(b"/Index")?;
    context.ensure_source_for_resolution(&value);
    if value.try_is_null()? {
        return Ok(vec![0, size]);
    }
    let values = value
        .try_as_array()?
        .ok_or_else(|| Error::parse(0, "/Index must be array"))?;
    if values.len() % 2 != 0 {
        return Err(Error::parse(
            0,
            "/Index must contain an even number of integers",
        ));
    }
    values
        .iter()
        .map(|value| {
            context.ensure_source_for_resolution(value);
            parse_non_negative_u64_handle(value, "/Index").and_then(|integer| {
                integer
                    .try_into()
                    .map_err(|_| Error::parse(0, "xref /Index value must fit u32"))
            })
        })
        .collect()
}

fn parse_non_negative_u64_handle(value: &ObjectHandle, name: &str) -> Result<u64> {
    let integer = value
        .try_as_integer()?
        .ok_or_else(|| Error::parse(0, format!("{name} is not integer")))?;
    if integer < 0 {
        return Err(Error::parse(0, format!("{name} is negative")));
    }
    Ok(integer as u64)
}

fn build_xref_ranges(index: Vec<u32>) -> Result<Vec<(u32, u32)>> {
    let mut ranges = Vec::with_capacity(index.len() / 2);
    for chunk in index.chunks_exact(2) {
        if chunk[1] == 0 {
            continue;
        }
        ranges.push((chunk[0], chunk[1]));
    }
    Ok(ranges)
}

fn parse_xref_entries(
    cursor: &mut ByteCursor<'_>,
    ranges: &[(u32, u32)],
    widths: XrefWidths,
    stream_data_offset: Option<usize>,
    registration: &mut XrefRegistration,
) -> Result<Vec<ParsedXrefEntry>> {
    let (w0, w1, w2) = widths;
    let entry_width = w0 + w1 + w2;
    if entry_width == 0 {
        return Err(Error::parse(0, "invalid cross-reference stream widths"));
    }

    let mut entries = Vec::new();
    for &(start, count) in ranges {
        let start =
            usize::try_from(start).map_err(|_| Error::parse(0, "object number too large"))?;
        let count = usize::try_from(count).map_err(|_| Error::parse(0, "range count too large"))?;

        for index in 0..count {
            if cursor.pos + entry_width > cursor.bytes.len() {
                return Err(Error::parse(cursor.pos, "xref stream data truncated"));
            }

            let object_type = if w0 == 0 {
                1
            } else {
                let value = cursor.read_be_u64(w0)?;
                u8::try_from(value).map_err(|_| {
                    Error::parse(cursor.pos, "xref stream object type does not fit u8")
                })?
            };
            let field1 = if w1 == 0 { 0 } else { cursor.read_be_u64(w1)? };
            let field2 = if w2 == 0 { 0 } else { cursor.read_be_u64(w2)? };

            let object_number = (start + index) as u32;
            let object_ref = QpdfObjGen::new(
                i32::try_from(object_number)
                    .map_err(|_| Error::parse(0, "object number does not fit i32"))?,
                match object_type {
                    0 | 2 => 0,
                    _ => i32::try_from(field2)
                        .map_err(|_| Error::parse(0, "generation does not fit i32"))?,
                },
            );
            // qpdf's insertXrefEntry checks deleted object numbers and the
            // exact object-generation slot before it switches on the entry
            // type (`QPDF.cc:1158-1169`). This matters on a failed first
            // pass: try_emplace leaves a default type-0 slot behind, so the
            // reconstruction re-entry skips the same malformed entry.
            if registration.deleted_objects.contains(&object_number)
                || registration.contains_raw_key(object_ref)
            {
                continue;
            }
            match object_type {
                0 => {
                    let _next = field1;
                    let _generation = field2;
                    registration.insert_free_xref_entry(object_ref);
                    entries.push(ParsedXrefEntry::Free { object_ref });
                }
                1 => {
                    let entry = XrefEntry::Uncompressed { offset: field1 };
                    registration.insert_xref_entry(object_ref, entry);
                    entries.push(ParsedXrefEntry::Live { object_ref, entry });
                }
                2 => {
                    let stream = u32::try_from(field1).map_err(|_| {
                        Error::parse(0, "xref stream object number does not fit u32")
                    })?;
                    let index = u32::try_from(field2)
                        .map_err(|_| Error::parse(0, "xref stream index does not fit u32"))?;
                    let entry = XrefEntry::Compressed { stream, index };
                    registration.insert_xref_entry(object_ref, entry);
                    entries.push(ParsedXrefEntry::Live { object_ref, entry });
                }
                _ => {
                    // qpdf's `try_emplace` has already inserted its default
                    // type-0 entry before the unknown-type exception. Keep
                    // the same partial registration for reconstruction.
                    registration.insert_xref_entry(object_ref, XrefEntry::Free { next: 0 });
                    // qpdf reports this through `damagedPDF("xref stream",
                    // ...)`, which uses the input's last read offset
                    // (`QPDF.cc:2625-2628`): `pipeStreamData` reads the whole
                    // payload with one `read` from its start
                    // (`QPDF.cc:2496-2498`), so the offset is the stream
                    // payload start regardless of which entry is malformed.
                    let offset = stream_data_offset.unwrap_or_default();
                    return Err(Error::parse(
                        offset,
                        format!("unknown xref stream entry type {object_type}"),
                    ));
                }
            }
        }
    }

    Ok(entries)
}

fn parse_usize(value: u64, name: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::parse(0, format!("{name} does not fit usize")))
}

fn find_qpdf_header(bytes: &[u8]) -> Option<(usize, String)> {
    let search_end = bytes.len().min(1024);
    (0..search_end).find_map(|offset| {
        if !bytes[offset..].starts_with(b"%PDF-") {
            return None;
        }
        parse_qpdf_header_version(&bytes[offset..]).map(|version| (offset, version))
    })
}

fn parse_qpdf_header_version(bytes: &[u8]) -> Option<String> {
    // `QPDF::findHeader` calls `readLine(1024)`, so a dot/version component
    // beyond that candidate-local window must not make an otherwise invalid
    // candidate valid.
    let line = &bytes[..bytes.len().min(1024)];
    let line_end = line
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')
        .unwrap_or(line.len());
    let version = line.get(5..line_end)?;
    let major_end = version
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(version.len());
    let minor_start = major_end.checked_add(1)?;
    if major_end == 0
        || version.get(major_end) != Some(&b'.')
        || !version.get(minor_start).is_some_and(u8::is_ascii_digit)
    {
        return None;
    }
    let minor_len = version[minor_start..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let version_end = minor_start + minor_len;
    Some(
        std::str::from_utf8(&version[..version_end])
            .expect("validated PDF version contains only ASCII digits and a dot")
            .to_string(),
    )
}

fn parse_startxref(bytes: &[u8]) -> Result<u64> {
    let marker = b"startxref";
    // qpdf's QPDF::parse searches for the marker only in the final 1054
    // bytes of the file (`QPDF.cc:440-464`). This preserves its recovery
    // boundary for files with stale startxref markers in an earlier update.
    let search_start = bytes.len().saturating_sub(1054);
    let Some(relative_pos) = bytes[search_start..]
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        return Err(Error::parse(0, "can't find startxref"));
    };
    let pos = search_start + relative_pos;

    let mut cursor = ByteCursor::new(bytes, pos + marker.len());
    let token = cursor.read_token()?;
    if !token.is_integer() {
        // qpdf's findStartxref only accepts the marker when its following
        // token is an integer. A malformed value therefore falls through to
        // the same damagedPDF("can't find startxref") path as an absent
        // marker (`libqpdf/QPDF.cc:413-453`), rather than exposing the
        // tokenizer's integer-type diagnostic.
        return Err(Error::parse(0, "can't find startxref"));
    }
    let text = std::str::from_utf8(&token.value)
        .map_err(|_| Error::parse(token.start, "number is not utf-8"))?;
    text.parse::<u64>()
        .map_err(|_| Error::parse(token.start, "invalid unsigned integer"))
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }

    fn skip_ws(&mut self) {
        while self.bytes.get(self.pos).copied().is_some_and(is_pdf_space) {
            self.pos += 1;
        }
    }

    fn read_token(&mut self) -> Result<Token> {
        let mut tokenizer = Tokenizer::new(self.bytes);
        tokenizer.allow_eof();
        tokenizer.set_position(self.pos)?;
        let token = tokenizer.read_token(false, 0)?;
        self.pos = tokenizer.position();
        Ok(token)
    }

    fn peek_word(&self, word: &[u8]) -> bool {
        self.bytes
            .get(self.pos..)
            .is_some_and(|tail| tail.starts_with(word))
            && self
                .bytes
                .get(self.pos + word.len())
                .is_none_or(|byte| is_pdf_delimiter(*byte))
    }

    fn read_be_u64(&mut self, width: usize) -> Result<u64> {
        if self.pos + width > self.bytes.len() {
            return Err(Error::parse(self.pos, "unexpected end of stream field"));
        }

        let mut value = 0u64;
        for _ in 0..width {
            value = (value << 8) | u64::from(self.bytes[self.pos]);
            self.pos += 1;
        }
        Ok(value)
    }

    fn read_line(&mut self, max_len: usize) -> Vec<u8> {
        let start = self.pos;
        let mut end = start;
        while end < self.bytes.len() && !matches!(self.bytes[end], b'\n' | b'\r') {
            end += 1;
        }
        let line_end = (start + max_len).min(end);
        let line = self.bytes[start..line_end].to_vec();
        self.pos = end;
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            self.pos += 1;
        }
        line
    }

    fn read_bytes(&mut self, max_len: usize) -> Vec<u8> {
        let start = self.pos;
        let end = start.saturating_add(max_len).min(self.bytes.len());
        self.pos = end;
        self.bytes[start..end].to_vec()
    }
}

#[cfg(test)]
mod final_handle_tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    use std::time::{Duration, Instant};

    fn load_xref_snapshot<R: Read + Seek>(
        reader: &mut R,
        allow_repair: bool,
    ) -> Result<LoadedXref> {
        let mut state = load_xref_state_with_options(
            reader,
            XrefLoadOptions {
                allow_repair,
                ..XrefLoadOptions::default()
            },
        )?;
        state.loaded.trailer = detach_bootstrap_handle(&state.loaded.trailer)?;
        Ok(state.loaded)
    }

    fn malformed_candidate_fixture(count: usize) -> Vec<u8> {
        let mut bytes = b"%PDF-1.5\n".to_vec();
        for index in 1..=count {
            let target = count + index;
            bytes.extend_from_slice(
                format!(
                    "{index} 0 obj\n<< /Type {target} 0 R /Length 0 /W [1 1 1] /Size 0 >>\nstream\nendstream\nendobj\n"
                )
                .as_bytes(),
            );
        }
        for index in 1..=count {
            let target = count + index;
            // Each generated target must end its own line: the reconstruction
            // line scan only recognizes an "N G obj" header as the *first*
            // token of a physical line, so a trailing space here (rather than
            // a newline) would concatenate the next target's header onto this
            // one's unterminated literal, leaving every target after the
            // first unreachable by the line scan.
            bytes.extend_from_slice(
                format!("{target} 0 obj\n(unterminated target {target}\n").as_bytes(),
            );
        }
        bytes.extend_from_slice(b"\nstartxref\n999999\n%%EOF\n");
        bytes
    }

    fn chained_indirect_length_bootstrap_fixture(
        links: u32,
    ) -> (Vec<u8>, BTreeMap<ObjectRef, XrefEntry>) {
        let mut bytes = Vec::from(*b"%PDF-1.4\n");
        let mut entries = BTreeMap::new();
        for number in 1..=links {
            entries.insert(
                ObjectRef::new(number, 0),
                XrefEntry::Uncompressed {
                    offset: bytes.len() as u64,
                },
            );
            bytes.extend_from_slice(
                format!(
                    "{number} 0 obj\n<< /Length {} 0 R >>\nstream\n\nendstream\nendobj\n",
                    number + 1
                )
                .as_bytes(),
            );
        }
        entries.insert(
            ObjectRef::new(links + 1, 0),
            XrefEntry::Uncompressed {
                offset: bytes.len() as u64,
            },
        );
        bytes.extend_from_slice(format!("{} 0 obj\n0\nendobj\n", links + 1).as_bytes());
        (bytes, entries)
    }

    #[test]
    fn bootstrap_long_indirect_length_chain_grows_the_stack_instead_of_aborting() {
        #[cfg(windows)]
        let stack_size = 32 * 1024 * 1024;
        #[cfg(not(windows))]
        let stack_size = 256 * 1024;
        std::thread::Builder::new()
            .stack_size(stack_size)
            .spawn(|| {
                let (bytes, entries) = chained_indirect_length_bootstrap_fixture(4_000);
                let mut context = XrefReadContext::new(
                    &bytes,
                    XrefReadContextSpec::ActiveSection,
                    &XrefRegistration {
                        entries,
                        ..XrefRegistration::default()
                    },
                    XrefLoadOptions::default(),
                );
                let indirect = context.document.handle_for_reference(ObjectRef::new(1, 0));
                let dictionary = ObjectHandle::dictionary(vec![(b"/Size".to_vec(), indirect)]);

                let value = context
                    .resolve_dictionary_value(&dictionary, "Size")
                    .expect("the chained reference must be resolved");
                assert!(value.is_resolved());
                assert!(value.as_stream_dict().is_some());
            })
            .expect("spawn")
            .join()
            .expect("a 4,000-link bootstrap chain must not overflow a small stack");
    }

    #[test]
    fn bootstrap_objstm_uses_the_canonical_member_parser_and_metadata() {
        let member_body = b"<< /Child 3 0 R >>";
        let first = b"2 0 ".len();
        let mut objstm_data = b"2 0 ".to_vec();
        objstm_data.extend_from_slice(member_body);
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let stream_offset = bytes.len() as u64;
        let stream_header = format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
            objstm_data.len()
        );
        bytes.extend_from_slice(stream_header.as_bytes());
        bytes.extend_from_slice(&objstm_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n3 0 obj\nnull\nendobj\n");

        let mut entries = BTreeMap::new();
        entries.insert(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 4,
                index: 0,
            },
        );
        entries.insert(
            ObjectRef::new(3, 0),
            XrefEntry::Uncompressed {
                offset: (bytes.len() - b"3 0 obj\nnull\nendobj\n".len()) as u64,
            },
        );
        entries.insert(
            ObjectRef::new(4, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        let document = BootstrapHandleDocument::new_with_state(
            Some(&bytes),
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        );

        document
            .resolve_objects_in_stream(4)
            .expect("ObjStm members resolve through the bootstrap owner");
        let member = document.handle_for_reference(ObjectRef::new(2, 0));
        assert!(member.is_resolved());
        // qpdf renders the member warning from the decoded InputSource name
        // (`<file> object stream N`, `libqpdf/QPDF.cc:1796`), the parser's
        // `object M 0` description (`:1451-1459`) and the parsed offset,
        // combined by `QPDFParser::warn` (`libqpdf/QPDFParser.cc:509-513`).
        // flpdf's template carries that whole prefix, so the stream number and
        // the input description stay in it.
        assert_eq!(
            member.description(),
            b" object stream 4, object 2 0 at offset 6"
        );
        let child = member
            .try_get_key(b"/Child")
            .expect("member dictionary child");
        assert_eq!(child.object_ref(), Some(ObjectRef::new(3, 0)));

        let source_stream = document.handle_for_reference(ObjectRef::new(4, 0));
        assert!(source_stream.end_offsets().0 >= 0);
        assert_eq!(member.end_offsets(), source_stream.end_offsets());
    }

    #[test]
    fn bootstrap_objstm_propagates_member_description_to_nested_direct_values() {
        let member_ref = ObjectRef::new(7, 0);
        let document = bootstrap_objstm_document(
            1,
            b"7 0 ",
            b"<< /Nested [ (text) << /Leaf (text) >> ] >>",
            BTreeMap::from([(
                member_ref,
                XrefEntry::Compressed {
                    stream: 4,
                    index: 0,
                },
            )]),
        );

        document
            .resolve_objects_in_stream(4)
            .expect("the ObjStm member resolves");
        let member = document.handle_for_reference(member_ref);
        let nested = member
            .try_get_key(b"/Nested")
            .expect("nested direct value is present");
        // With the bootstrap document's warning sink in place
        // (`flpdf-92r5`), a type mismatch behaves as qpdf does: the accessor
        // warns and returns qpdf's fallback rather than failing
        // (`QPDF_Stream::warn` -> `QPDF::warn`, which records without
        // throwing, `libqpdf/QPDF_Stream.cc:695-698` and
        // `libqpdf/QPDF.cc:487-494`). The member context therefore has to be
        // checked on the recorded warnings, not on an error value.
        assert_eq!(
            nested
                .try_get_array_item(0)
                .expect("nested array item is present")
                .try_get_int_value()
                .expect("qpdf warns and falls back instead of failing"),
            0
        );
        assert_eq!(
            nested
                .try_get_array_item(1)
                .expect("nested dictionary item is present")
                .try_get_key(b"/Leaf")
                .expect("nested dictionary leaf is present")
                .try_get_int_value()
                .expect("qpdf warns and falls back instead of failing"),
            0
        );
        let state = document.state.borrow();
        let contextual = state
            .diagnostics
            .entries()
            .iter()
            .filter(|warning| {
                let object = String::from_utf8_lossy(warning.get_object()).to_string();
                object.contains("object 7 0") && object.contains("object stream 4")
            })
            .count();
        let recorded = format!("{:?}", state.diagnostics);
        assert!(
            contextual >= 2,
            "both nested values must warn with the ObjStm member context: {recorded}"
        );
    }

    fn bootstrap_objstm_document(
        object_count: usize,
        header: &[u8],
        member_body: &[u8],
        mut entries: BTreeMap<ObjectRef, XrefEntry>,
    ) -> Rc<BootstrapHandleDocument> {
        let mut objstm_data = header.to_vec();
        objstm_data.extend_from_slice(member_body);
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "4 0 obj\n<< /Type /ObjStm /N {object_count} /First {} /Length {} >>\nstream\n",
                header.len(),
                objstm_data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&objstm_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n%tail\n");
        entries.insert(
            ObjectRef::new(4, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        BootstrapHandleDocument::new_with_state(
            Some(&bytes),
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        )
    }

    #[test]
    fn bootstrap_objstm_wraps_a_direct_member_parse_error() {
        let member_ref = ObjectRef::new(7, 0);
        let document = bootstrap_objstm_document(
            1,
            b"7 0 ",
            b"[ 2147483648 0 R ]",
            BTreeMap::from([(
                member_ref,
                XrefEntry::Compressed {
                    stream: 4,
                    index: 0,
                },
            )]),
        );

        let error = document
            .resolve_objects_in_stream(4)
            .expect_err("a malformed direct member must retain its parse error");
        assert!(matches!(
            error,
            Error::Parse { message, .. }
                if message.contains("object stream 4 (object 7 0, offset")
        ));
    }

    #[test]
    fn bootstrap_objstm_delivers_member_diagnostics_and_null_recovery() {
        let member_ref = ObjectRef::new(7, 0);
        let warning_document = bootstrap_objstm_document(
            1,
            b"7 0 ",
            b"<< /A#zB 1 >>",
            BTreeMap::from([(
                member_ref,
                XrefEntry::Compressed {
                    stream: 4,
                    index: 0,
                },
            )]),
        );
        warning_document
            .resolve_objects_in_stream(4)
            .expect("a recoverable member warning must not abort resolution");
        assert!(warning_document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("object stream 4 (object 7 0, offset")));

        let empty_document = bootstrap_objstm_document(
            1,
            b"7 0 ",
            b"endobj",
            BTreeMap::from([(
                member_ref,
                XrefEntry::Compressed {
                    stream: 4,
                    index: 0,
                },
            )]),
        );
        empty_document
            .resolve_objects_in_stream(4)
            .expect("an empty member follows qpdf's null recovery");
        let empty_member = empty_document.handle_for_reference(member_ref);
        assert!(empty_member.is_null());
        assert_eq!(empty_member.end_offsets(), (-1, -1));
        assert!(empty_document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("empty object treated as null")));
    }

    #[test]
    fn bootstrap_objstm_keeps_member_after_a_recoverable_stream_warning() {
        let member_ref = ObjectRef::new(7, 0);
        let decoded = b"7 0 << /Value 42 >>";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(decoded)
            .expect("the ObjStm payload compresses");
        let mut compressed = encoder.finish().expect("the zlib stream finishes");
        compressed.truncate(compressed.len().saturating_sub(4));

        let state = Rc::new(RefCell::new(BootstrapHandleState::default()));
        let document = BootstrapHandleDocument::new_with_state(
            None,
            XrefEntryLookup::Registration(&BTreeMap::from([
                (ObjectRef::new(4, 0), XrefEntry::Uncompressed { offset: 0 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: 4,
                        index: 0,
                    },
                ),
            ])),
            XrefLoadOptions {
                description: b"bootstrap-truncated.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            Rc::clone(&state),
        );
        let stream = document.handle_for_reference(ObjectRef::new(4, 0));
        stream.set_resolved(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
                (b"/N".to_vec(), ObjectHandle::integer(1)),
                (b"/First".to_vec(), ObjectHandle::integer(4)),
                (
                    b"/Length".to_vec(),
                    ObjectHandle::integer(i64::try_from(compressed.len()).unwrap()),
                ),
                (
                    b"/Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ),
            ]),
            stream_data: Some(Rc::new(compressed)),
            stream_provider: None,
            filter_on_write: true,
            stream_length: decoded.len(),
        });
        stream.set_parsed_offset_if_unset(90);

        document
            .resolve_objects_in_stream(4)
            .expect("a recoverable stream warning must not abort ObjStm resolution");
        assert_eq!(
            document
                .handle_for_reference(member_ref)
                .try_get_key(b"/Value")
                .expect("the member survives the stream warning")
                .as_integer(),
            Some(42)
        );
        let state = state.borrow();
        let warning = state
            .diagnostics
            .entries()
            .iter()
            .find(|warning| {
                warning.get_message_detail()
                    == b"input stream is complete but output may still be valid"
            })
            .expect("the recoverable zlib warning is collected");
        assert_eq!(warning.get_filename(), b"bootstrap-truncated.pdf");
        assert_eq!(warning.get_object(), b"");
        assert_eq!(warning.get_file_position(), 90);
    }

    #[test]
    fn bootstrap_stream_warning_without_a_parsed_offset_uses_the_object_warning_sink() {
        let state = Rc::new(RefCell::new(BootstrapHandleState::default()));
        let document = BootstrapHandleDocument::new_with_state(
            None,
            XrefEntryLookup::Registration(&BTreeMap::new()),
            XrefLoadOptions::default(),
            Rc::clone(&state),
        );
        let stream = document.handle_for_reference(ObjectRef::new(4, 0));
        stream.set_resolved(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::integer(1),
            )]),
            stream_data: Some(Rc::new(vec![0])),
            stream_provider: None,
            filter_on_write: true,
            stream_length: 1,
        });

        let error = stream
            .get_stream_data(DecodeLevel::Specialized)
            .expect_err("an unfilterable stream still returns qpdf's terminal error");
        assert!(matches!(
            error,
            Error::QpdfExc(ref warning)
                if warning.get_message_detail() == b"getStreamData called on unfilterable stream"
        ));
        assert!(state.borrow().diagnostics.entries().iter().any(|warning| {
            warning.get_message_detail() == b"stream filter type is not name or array"
        }));
    }

    #[test]
    fn bootstrap_offset_zero_and_unlisted_compressed_objects_follow_null_fallbacks() {
        let offset_zero_ref = ObjectRef::new(1, 0);
        let offset_zero_document = BootstrapHandleDocument::new_with_state(
            Some(b""),
            XrefEntryLookup::Registration(&BTreeMap::from([(
                offset_zero_ref,
                XrefEntry::Uncompressed { offset: 0 },
            )])),
            XrefLoadOptions::default(),
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        );
        let offset_zero = offset_zero_document.handle_for_reference(offset_zero_ref);
        offset_zero
            .try_dereference()
            .expect("offset zero resolves to qpdf's null fallback");
        assert!(offset_zero.is_null());

        let unlisted_ref = ObjectRef::new(7, 0);
        let unlisted_document = bootstrap_objstm_document(
            0,
            b"",
            b"",
            BTreeMap::from([(
                unlisted_ref,
                XrefEntry::Compressed {
                    stream: 4,
                    index: 0,
                },
            )]),
        );
        let unlisted = unlisted_document.handle_for_reference(unlisted_ref);
        unlisted
            .try_dereference()
            .expect("an unlisted compressed object resolves after a successful ObjStm scan");
        assert!(unlisted.is_null());
    }

    #[test]
    fn bootstrap_objstm_uses_effective_xref_and_records_absent_headers() {
        let member_body = b"<< /Value 1 >>";
        let header = b"7 0 8 0 9 0 ";
        let mut objstm_data = header.to_vec();
        objstm_data.extend_from_slice(member_body);
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let stream_offset = bytes.len() as u64;
        let stream_header = format!(
            "4 0 obj\n<< /Type /ObjStm /N 3 /First {} /Length {} >>\nstream\n",
            header.len(),
            objstm_data.len()
        );
        bytes.extend_from_slice(stream_header.as_bytes());
        bytes.extend_from_slice(&objstm_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n%tail\n");

        let stream_ref = ObjectRef::new(4, 0);
        let active_ref = ObjectRef::new(7, 0);
        let overridden_ref = ObjectRef::new(8, 0);
        let absent_ref = ObjectRef::new(9, 0);
        let entries = BTreeMap::from([
            (
                stream_ref,
                XrefEntry::Uncompressed {
                    offset: stream_offset,
                },
            ),
            (
                active_ref,
                XrefEntry::Compressed {
                    stream: stream_ref.number,
                    index: 0,
                },
            ),
            (overridden_ref, XrefEntry::Uncompressed { offset: 1 }),
        ]);
        let document = BootstrapHandleDocument::new_with_state(
            Some(&bytes),
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        );

        document
            .resolve_objects_in_stream(stream_ref.number)
            .expect("the active member resolves through the bootstrap owner");
        assert_eq!(
            document
                .handle_for_reference(active_ref)
                .try_get_key(b"/Value")
                .expect("active member dictionary")
                .as_integer(),
            Some(1)
        );
        assert!(
            !document.handle_for_reference(overridden_ref).is_resolved(),
            "an overridden member must not be populated from the ObjStm"
        );
        assert_eq!(
            document.entry_lookup.borrow().get(&absent_ref),
            Some(&XrefEntry::Free { next: 0 }),
            "an absent header member receives qpdf's default free xref row"
        );
    }

    #[test]
    fn bootstrap_compressed_resolution_records_absent_headers_without_panicking() {
        // Same shape as the test above, but reached through
        // `resolve_indirect_inner` instead of calling
        // `resolve_objects_in_stream` directly: the compressed member is
        // resolved lazily, so the absent-header row is inserted while the
        // caller is still inside the `match` that read the entry table.
        let member_body = b"<< /Value 1 >>";
        let header = b"7 0 9 0 ";
        let mut objstm_data = header.to_vec();
        objstm_data.extend_from_slice(member_body);
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let stream_offset = bytes.len() as u64;
        let stream_header = format!(
            "4 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} >>\nstream\n",
            header.len(),
            objstm_data.len()
        );
        bytes.extend_from_slice(stream_header.as_bytes());
        bytes.extend_from_slice(&objstm_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n%tail\n");

        let stream_ref = ObjectRef::new(4, 0);
        let active_ref = ObjectRef::new(7, 0);
        let absent_ref = ObjectRef::new(9, 0);
        let entries = BTreeMap::from([
            (
                stream_ref,
                XrefEntry::Uncompressed {
                    offset: stream_offset,
                },
            ),
            (
                active_ref,
                XrefEntry::Compressed {
                    stream: stream_ref.number,
                    index: 0,
                },
            ),
        ]);
        let document = BootstrapHandleDocument::new_with_state(
            Some(&bytes),
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        );

        assert_eq!(
            document
                .handle_for_reference(active_ref)
                .try_get_key(b"/Value")
                .expect("the compressed member resolves lazily")
                .as_integer(),
            Some(1)
        );
        assert_eq!(
            document.entry_lookup.borrow().get(&absent_ref),
            Some(&XrefEntry::Free { next: 0 }),
            "an absent header member still receives qpdf's default free xref row"
        );
    }

    #[test]
    fn bootstrap_objstm_does_not_use_the_legacy_whole_buffer_route() {
        let source = include_str!("xref.rs");
        let method = source
            .split("fn resolve_objects_in_stream(&self, stream_number: u32)")
            .nth(1)
            .and_then(|rest| rest.split("fn handle_integer").next())
            .expect("bootstrap ObjStm method");

        assert!(
            method.contains("get_stream_data(DecodeLevel::Specialized)"),
            "bootstrap ObjStm must use qpdf's specialized stream accessor"
        );
        assert!(
            method.contains("parse_qpdf_direct_object_handle_with_diagnostics"),
            "bootstrap ObjStm members must use the direct qpdf parser"
        );
        assert!(
            method.contains("next_object_stream_integer()"),
            "bootstrap ObjStm headers must use qpdf's allow_bad token consumer"
        );
        assert!(
            !method.contains("decode_stream_data_from_handle"),
            "bootstrap ObjStm must not use the whole-buffer decoder"
        );
        assert!(
            !method.contains("parse_qpdf_file_object_handle_with_diagnostics"),
            "bootstrap ObjStm members must not use file-object framing"
        );
    }

    #[test]
    fn bootstrap_document_construction_defers_the_source_snapshot() {
        let entries = BTreeMap::new();
        let document = BootstrapHandleDocument::new_with_state(
            None,
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        );

        assert!(
            document.bytes.get().is_none(),
            "constructing the bootstrap owner must not copy the complete input"
        );
    }

    /// Builds and recovers a [`malformed_candidate_fixture`] of `count`
    /// candidates, asserting the expected recovery failure, and returns the
    /// elapsed wall-clock time.
    fn timed_malformed_candidate_recovery(count: usize) -> Duration {
        let bytes = malformed_candidate_fixture(count);
        let started = Instant::now();
        let error = load_xref_snapshot(&mut std::io::Cursor::new(&bytes), true)
            .expect_err("the fixture has no trailer dictionary");
        assert!(error
            .to_string()
            .contains("unable to find trailer dictionary while recovering damaged file"));
        started.elapsed()
    }

    #[test]
    fn reconstruction_bounds_referenced_reads_for_malformed_candidates() {
        // Compare a 10x candidate-count increase's elapsed time against a
        // small baseline rather than asserting an absolute wall-clock
        // ceiling: an absolute deadline is indistinguishable from a slow or
        // instrumented CI runner (e.g. under `cargo llvm-cov`), while an
        // O(n^2) regression -- the failure mode this test guards against --
        // would scale roughly 100x over this 10x input increase, far past
        // any noise an absolute ceiling would otherwise need to tolerate.
        // The `+ 200ms` floor absorbs fixed overhead the ratio alone can't,
        // since `small` can be small enough for noise to dominate a pure
        // multiple.
        let small = timed_malformed_candidate_recovery(500);
        let large = timed_malformed_candidate_recovery(5_000);
        assert!(
            large < small * 20 + Duration::from_millis(200),
            "elapsed time scaled worse than the bounded-window guard allows: \
             small (500 candidates) = {small:?}, large (5,000 candidates) = {large:?}"
        );
    }

    #[test]
    fn reconstruction_reference_read_window_uses_adjacent_offset_and_fallback_positions() {
        let mut entries = BTreeMap::new();
        for index in 0..=70 {
            let object_ref = ObjectRef::new(index + 1, 0);
            entries.insert(
                object_ref,
                XrefEntry::Uncompressed {
                    offset: u64::from(index) * 10,
                },
            );
        }
        entries.insert(ObjectRef::new(100, 0), XrefEntry::Free { next: 0 });
        entries.insert(
            ObjectRef::new(101, 0),
            XrefEntry::Compressed {
                stream: 1,
                index: 0,
            },
        );
        let reference_offsets = reconstructed_reference_offsets(&entries);
        let context = XrefReadContext::new(
            &[0; 800],
            XrefReadContextSpec::Reconstruction {
                line_scan_entries: &entries,
                reference_offsets: &reference_offsets,
            },
            &XrefRegistration::default(),
            XrefLoadOptions::default(),
        );

        assert_eq!(
            context.document.reference_read_window(10, 800),
            ReferenceReadWindow {
                end: 20,
                fallback_end: 660,
            }
        );
        let shared = empty_bootstrap_cache();
        let cached_context = XrefReadContext::new(
            &[0; 800],
            XrefReadContextSpec::ReconstructionWithCache {
                line_scan_entries: &entries,
                reference_offsets: &reference_offsets,
                bootstrap_cache: &shared,
            },
            &XrefRegistration::default(),
            XrefLoadOptions::default(),
        );
        assert_eq!(
            cached_context.document.reference_read_window(10, 800),
            ReferenceReadWindow {
                end: 20,
                fallback_end: 660,
            }
        );
        let context_offsets = context
            .document
            .reference_offsets
            .borrow()
            .as_ref()
            .expect("reconstruction context must retain its offset index")
            .clone();
        let cached_offsets = cached_context
            .document
            .reference_offsets
            .borrow()
            .as_ref()
            .expect("cached reconstruction context must retain its offset index")
            .clone();
        assert!(Rc::ptr_eq(&context_offsets, &reference_offsets));
        assert!(Rc::ptr_eq(&cached_offsets, &reference_offsets));
    }

    #[test]
    fn active_reference_read_window_keeps_qpdf_unbounded_view() {
        let context = XrefReadContext::new(
            &[0; 800],
            XrefReadContextSpec::ActiveSection,
            &XrefRegistration::default(),
            XrefLoadOptions::default(),
        );

        assert_eq!(
            context.document.reference_read_window(10, 800),
            ReferenceReadWindow {
                end: 800,
                fallback_end: 800,
            }
        );
    }

    #[test]
    fn reconstruction_reference_read_retries_with_the_controlled_fallback_window() {
        let mut bytes = b"1 0 obj\n<< /Length 27 >>\nstream\n".to_vec();
        bytes.extend_from_slice(b"aaaa\n2 0 obj\nbbbbbbbbbbbbb\n");
        let stream_length = b"aaaa\n2 0 obj\nbbbbbbbbbbbbb\n".len();
        bytes.extend_from_slice(b"endstream\nendobj\n%%EOF\n");
        assert_eq!(stream_length, 27);

        let first_ref = ObjectRef::new(1, 0);
        let second_ref = ObjectRef::new(2, 0);
        let second_offset = b"1 0 obj\n<< /Length 27 >>\nstream\n".len() + b"aaaa\n".len();
        let mut entries = BTreeMap::new();
        entries.insert(first_ref, XrefEntry::Uncompressed { offset: 0 });
        entries.insert(
            second_ref,
            XrefEntry::Uncompressed {
                offset: second_offset as u64,
            },
        );
        let reference_offsets = reconstructed_reference_offsets(&entries);
        let context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::Reconstruction {
                line_scan_entries: &entries,
                reference_offsets: &reference_offsets,
            },
            &XrefRegistration::default(),
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );
        context.document.ensure_source_bytes(&bytes);

        let (value, _, _, _) = context
            .document
            .read_uncompressed_object(first_ref, 0)
            .expect("the fallback window must recover a valid stream");
        let ObjectValue::Stream { stream_data, .. } = value else {
            panic!("expected a recovered stream, got {value:?}"); // cov:ignore: the preceding expect already guarantees a Stream value from this fixture
        };
        // The narrow window truncates before `endstream`, which
        // `RecoveryPolicy::Bounded` accepts as a *successful* empty stream
        // rather than an `Err`, so a weaker `matches!(.., Stream { .. })`
        // check alone would pass even if the retry never widened the
        // window. Assert the full 27-byte payload was actually recovered.
        assert_eq!(
            stream_data.as_deref().map(Vec::len),
            Some(stream_length),
            "the fallback window must recover the stream's real content, not an empty placeholder"
        );
    }

    #[test]
    fn xref_candidate_retry_widens_a_literal_truncated_by_a_false_header() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let first_offset = bytes.len();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Length 4 /Info (prefix\n",
        );
        let false_header_offset = bytes.len();
        // Keep bytes after the final `endobj`: qpdf's `readObjectAtOffset`
        // throws `EOF after endobj` when only whitespace follows it
        // (`QPDF.cc:1652-1660`) and `resolve` turns that into a warned
        // null (`QPDF.cc:1738-1748`), so an object that ends the file is
        // never an xref-stream candidate in qpdf. With a trailing `%%EOF`
        // line qpdf 11.9.0 accepts this object as its candidate and only
        // fails later while decoding it.
        bytes.extend_from_slice(b"2 0 obj\nsuffix) >>\nstream\nabcd\nendstream\nendobj\n%%EOF\n");

        let first_ref = ObjectRef::new(1, 0);
        let false_ref = ObjectRef::new(2, 0);
        let mut entries = BTreeMap::new();
        entries.insert(
            first_ref,
            XrefEntry::Uncompressed {
                offset: first_offset as u64,
            },
        );
        entries.insert(
            false_ref,
            XrefEntry::Uncompressed {
                offset: false_header_offset as u64,
            },
        );
        let reference_offsets = reconstructed_reference_offsets(&entries);

        let (candidate, diagnostics) = find_xref_stream_trailer_candidate(
            &bytes,
            &entries,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &reference_offsets,
            None,
            None,
        );

        let candidate = candidate.expect("wide retry must recover the xref stream candidate");
        assert_eq!(candidate.max_offset, first_offset as u64);
        assert_eq!(
            candidate
                .trailer
                .try_get_key(b"/Type")
                .expect("candidate dictionary has a /Type key")
                .as_name(),
            Some(b"XRef".to_vec())
        );
        // qpdf reads the candidate once, to EOF, so the literal spanning the
        // false header never yields a warning for object 1; the narrow
        // attempt's tokenizer/EOF diagnostics must be dropped with it.
        assert!(
            diagnostics
                .entries()
                .iter()
                .all(|diagnostic| diagnostic.get_object() != b"object 1 0"),
            "the discarded narrow attempt's diagnostics must not leak once the \
             wider retry recovers the candidate: {diagnostics:?}"
        );
    }

    #[test]
    fn xref_candidate_scan_keeps_the_diagnostics_of_an_unretryable_recovered_null() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let first_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Info (unterminated");

        let first_ref = ObjectRef::new(1, 0);
        let mut entries = BTreeMap::new();
        entries.insert(
            first_ref,
            XrefEntry::Uncompressed {
                offset: first_offset as u64,
            },
        );
        let reference_offsets = reconstructed_reference_offsets(&entries);

        let (candidate, diagnostics) = find_xref_stream_trailer_candidate(
            &bytes,
            &entries,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &reference_offsets,
            None,
            None,
        );

        assert!(
            candidate.is_none(),
            "a null recovered from a truncated object is not an xref-stream candidate"
        );
        // The bounded window already reaches EOF, so no wider retry exists and
        // this read is the single to-EOF read qpdf itself performs
        // (`QPDF.cc:580-607`); its diagnostics are the accepted attempt's and
        // must be reported.
        assert!(
            diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.get_object() == b"object 1 0"),
            "the accepted recovered-null read must keep its diagnostics: {diagnostics:?}"
        );
    }

    #[test]
    fn xref_candidate_scan_commits_the_wide_retry_s_recovered_null_with_its_diagnostics() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let first_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /XRef /Info (prefix\n");
        let false_header_offset = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\nsuffix) /Deep ");
        bytes.extend(std::iter::repeat_n(
            b'[',
            crate::parser::MAX_PARSE_DEPTH + 1,
        ));
        bytes.extend_from_slice(b"\n%%EOF\n");

        let mut entries = BTreeMap::new();
        entries.insert(
            ObjectRef::new(1, 0),
            XrefEntry::Uncompressed {
                offset: first_offset as u64,
            },
        );
        entries.insert(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: false_header_offset as u64,
            },
        );
        let reference_offsets = reconstructed_reference_offsets(&entries);

        let (candidate, diagnostics) = find_xref_stream_trailer_candidate(
            &bytes,
            &entries,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &reference_offsets,
            None,
            None,
        );

        assert!(candidate.is_none());
        // The wider retry recovers only a null again (under
        // `RecoveryPolicy::Bounded` the over-deep array is recovered, not
        // rejected), and that accepted read is committed with its
        // diagnostics: qpdf warns once and caches the substituted null for a
        // failed read (`QPDF.cc:1738-1748`) instead of dropping the object.
        assert!(
            diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.get_object() == b"object 1 0"),
            "a wide retry that recovers a null must keep its diagnostics: {diagnostics:?}"
        );
    }

    #[test]
    fn reconstruction_reference_read_retries_a_narrow_window_that_recovers_nonempty_but_truncated_data(
    ) {
        // A narrow window's heuristic search can accept a *non-empty* but
        // still wrong terminator: an `endobj` keyword embedded inside the
        // real stream payload, found only because the window happens to end
        // right after it, well before the declared `/Length` boundary.
        let mut bytes = b"1 0 obj\n<< /Length 40 >>\nstream\n".to_vec();
        let header_len = bytes.len();
        let embedded_false_terminator = b"AAAAAAAAAendobj ";
        let mut payload = embedded_false_terminator.to_vec();
        payload.resize(40, b'B');
        assert_eq!(payload.len(), 40);
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");

        let first_ref = ObjectRef::new(1, 0);
        let second_ref = ObjectRef::new(2, 0);
        // Land the narrow window's end right after the embedded "endobj ",
        // inside the payload and well short of its real 40-byte length.
        let second_offset = header_len + embedded_false_terminator.len();
        let mut entries = BTreeMap::new();
        entries.insert(first_ref, XrefEntry::Uncompressed { offset: 0 });
        entries.insert(
            second_ref,
            XrefEntry::Uncompressed {
                offset: second_offset as u64,
            },
        );
        let reference_offsets = reconstructed_reference_offsets(&entries);
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::Reconstruction {
                line_scan_entries: &entries,
                reference_offsets: &reference_offsets,
            },
            &XrefRegistration::default(),
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );
        context.document.ensure_source_bytes(&bytes);

        let (value, _, _, _) = context
            .document
            .read_uncompressed_object(first_ref, 0)
            .expect("the fallback window must recover a valid stream");
        let ObjectValue::Stream { stream_data, .. } = value else {
            panic!("expected a recovered stream, got {value:?}"); // cov:ignore: the preceding expect already guarantees a Stream value from this fixture
        };
        assert_eq!(
            stream_data.as_deref().map(Vec::len),
            Some(payload.len()),
            "a narrow window's false, non-empty terminator match must not be \
             accepted over the wider window's real declared-length boundary"
        );

        let mut collected = Diagnostics::default();
        context.append_diagnostics_to(&mut collected);
        // The wider window resolves the declared 40-byte /Length exactly, so
        // a single unbounded qpdf-style read would produce no diagnostics at
        // all here. Any entry at all would mean the discarded narrow
        // attempt's "expected endstream"/"attempting to recover stream
        // length" warnings leaked through instead of being dropped with it.
        assert!(
            collected.entries().is_empty(),
            "the discarded narrow attempt's recovery diagnostics must not leak \
             once the wider retry recovers the stream cleanly: {collected:?}"
        );
    }

    #[test]
    fn bootstrap_read_refuses_to_invent_an_end_offset_at_eof() {
        // qpdf scans the real file for the first non-space byte after
        // `endobj` and raises `EOF after endobj` when the source runs out
        // first (`libqpdf/QPDF.cc:1651-1663`); the canonical reader mirrors
        // that in `ResolverHandle::object_end_offsets`. The bootstrap parser
        // only sees an object-relative window, so without the source-side
        // rescan the window's own end would masquerade as the first
        // following byte and be cached as this object's extent -- and then
        // propagated to every member of an object stream.
        let bytes = b"1 0 obj\n<< /Size 1 >>\nendobj\n \n".to_vec();
        let object_ref = ObjectRef::new(1, 0);
        let context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &XrefRegistration::default(),
            XrefLoadOptions::default(),
        );
        context.document.ensure_source_bytes(&bytes);

        let error = context
            .document
            .read_uncompressed_object(object_ref, 0)
            .expect_err("only whitespace follows endobj, so qpdf raises EOF after endobj");
        assert!(
            error.to_string().contains("EOF after endobj"),
            "expected qpdf's own EOF after endobj wording, got {error}"
        );
    }

    #[test]
    fn bootstrap_read_reports_the_first_byte_after_the_terminator() {
        // The counterpart of the case above: with a trailer following the
        // object, the recorded `end_after_space` is the trailer's own offset
        // rather than the read window's end.
        let mut bytes = b"1 0 obj\n<< /Size 1 >>\nendobj\n\n".to_vec();
        let trailer_offset = bytes.len();
        bytes.extend_from_slice(b"%%EOF\n");
        let object_ref = ObjectRef::new(1, 0);
        let context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &XrefRegistration::default(),
            XrefLoadOptions::default(),
        );
        context.document.ensure_source_bytes(&bytes);

        let (_, _, end_before_space, end_after_space) = context
            .document
            .read_uncompressed_object(object_ref, 0)
            .expect("the object is well formed and a trailer follows it");
        assert_eq!(
            end_before_space,
            i64::try_from(b"1 0 obj\n<< /Size 1 >>\nendobj".len()).unwrap()
        );
        assert_eq!(end_after_space, i64::try_from(trailer_offset).unwrap());
    }

    #[test]
    fn read_uncompressed_object_pushes_the_accepted_read_s_own_diagnostics() {
        // No `/Length` at all, so recovery is unavoidable even without any
        // window truncation (`ActiveSection` never bounds the window, so
        // there is nothing to retry here) -- this attempt's diagnostics are
        // the only ones that can ever exist, and they must still reach the
        // shared diagnostics list once accepted.
        let bytes = b"1 0 obj\n<< >>\nstream\nhello\nendstream\nendobj\n%%EOF\n".to_vec();
        let object_ref = ObjectRef::new(1, 0);
        let mut entries = BTreeMap::new();
        entries.insert(object_ref, XrefEntry::Uncompressed { offset: 0 });
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &XrefRegistration::default(),
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );
        context.document.ensure_source_bytes(&bytes);

        let (value, _, _, _) = context
            .document
            .read_uncompressed_object(object_ref, 0)
            .expect("a missing /Length recovers through the endstream scan");
        assert!(matches!(value, ObjectValue::Stream { .. }));

        let mut collected = Diagnostics::default();
        context.append_diagnostics_to(&mut collected);
        assert!(
            collected.entries().iter().any(|diagnostic| diagnostic
                .message_string()
                .contains("stream dictionary lacks /Length key")),
            "the accepted read's own diagnostics must reach the shared list: {collected:?}"
        );
    }

    #[test]
    fn direct_only_bootstrap_access_does_not_initialize_the_source_snapshot() {
        let registration = XrefRegistration::default();
        let mut context = XrefReadContext::new(
            b"source bytes are not needed",
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        let dictionary =
            ObjectHandle::dictionary(vec![(b"/Size".to_vec(), ObjectHandle::integer(7))]);

        let value = context
            .resolve_dictionary_value(&dictionary, "Size")
            .expect("direct dictionary value");

        assert_eq!(value.try_as_integer().unwrap(), Some(7));
        assert!(context.document.bytes.get().is_none());
        context.sync_handle_diagnostics();
        assert!(context.diagnostics.entries().is_empty());

        let input = b"1 0 obj\n<< /Root 2 0 R >>\nendobj\n%%EOF\n";
        let mut context = XrefReadContext::new(
            input,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        let parsed = context
            .read_file_object_handle(
                input,
                0,
                RecoveryPolicy::RequireEndstream,
                XrefObjectDescription::Ordinary,
            )
            .expect("a direct dictionary with an unused reference");
        assert!(parsed.object.as_dictionary().is_some());
        assert!(context.document.bytes.get().is_none());
    }

    #[test]
    fn indirect_bootstrap_resolution_initializes_the_source_snapshot_once() {
        let bytes = b"\n1 0 obj\n7\nendobj\n%%EOF\n";
        let object_ref = ObjectRef::new(1, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            QpdfObjGen::try_from_object_ref(object_ref).unwrap(),
            XrefEntry::Uncompressed { offset: 1 },
        );
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        let indirect = context.document.handle_for_reference(object_ref);
        let dictionary = ObjectHandle::dictionary(vec![(b"/Size".to_vec(), indirect)]);

        assert!(context.document.bytes.get().is_none());
        let value = context
            .resolve_dictionary_value(&dictionary, "Size")
            .expect("indirect dictionary value");
        assert_eq!(value.try_as_integer().unwrap(), Some(7));

        let snapshot = context.document.bytes.get().expect("source snapshot");
        let snapshot_ptr = Rc::as_ptr(snapshot);
        context.document.ensure_source_bytes(b"different bytes");
        assert_eq!(
            Rc::as_ptr(context.document.bytes.get().unwrap()),
            snapshot_ptr
        );
    }

    #[test]
    fn bootstrap_object_read_reports_an_uninitialized_source_snapshot() {
        let entries = BTreeMap::new();
        let document = BootstrapHandleDocument::new_with_state(
            None,
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        );

        let error = document
            .read_uncompressed_object(ObjectRef::new(1, 0), 1)
            .expect_err("a direct resolver call without a source must fail internally");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "bootstrap resolver source bytes were not initialized"
        ));
    }

    #[test]
    fn indirect_stream_length_initializes_the_source_snapshot_before_resolution() {
        let mut bytes = b"1 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n".to_vec();
        let object_two_offset = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n3\nendobj\n%%EOF\n");
        let object_one = ObjectRef::new(1, 0);
        let object_two = ObjectRef::new(2, 0);
        let mut entries = BTreeMap::new();
        entries.insert(
            object_two,
            XrefEntry::Uncompressed {
                offset: object_two_offset as u64,
            },
        );
        let document = BootstrapHandleDocument::new_with_state(
            None,
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        );

        assert!(document.bytes.get().is_none());
        let result = document
            .read_file_object(
                &bytes,
                0,
                RecoveryPolicy::RequireEndstream,
                XrefObjectDescription::Ordinary,
                &bytes,
            )
            .expect("indirect stream length should resolve");

        assert_eq!(result.object_ref, object_one);
        assert_eq!(result.object.as_stream_data().unwrap().as_slice(), b"abc");
        assert!(document.bytes.get().is_some());
    }

    fn classic_xref_with_trailer(trailer: &str) -> (Vec<u8>, usize) {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \ntrailer\n");
        bytes.extend_from_slice(trailer.as_bytes());
        bytes.extend_from_slice(b"\nstartxref\n");
        bytes.extend_from_slice(xref.to_string().as_bytes());
        bytes.extend_from_slice(b"\n%%EOF\n");
        (bytes, xref)
    }

    #[test]
    fn classic_read_trailer_reports_qpdf_stream_warning_once() {
        let (mut bytes, _) = classic_xref_with_trailer("<< /Size 1 >> stream");
        bytes.extend_from_slice(b"\n");
        let warning_offset = bytes
            .windows(b"stream".len())
            .rposition(|window| window == b"stream")
            .expect("stream token")
            + b"stream".len();
        let mut reader = std::io::Cursor::new(bytes);
        let state = load_xref_state_with_options(
            &mut reader,
            XrefLoadOptions {
                description: b"stream-trailer.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
        )
        .expect("a valid classic trailer with an extra stream token loads");
        let warnings: Vec<_> = state
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .filter(|warning| warning.get_message_detail() == b"stream keyword found in trailer")
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].get_object(), b"trailer");
        assert_eq!(warnings[0].get_file_position(), warning_offset as i64);
    }

    #[test]
    fn canonical_classic_read_trailer_reports_qpdf_stream_warning() {
        let (mut bytes, _) = classic_xref_with_trailer("<< /Size 1 >> stream");
        bytes.extend_from_slice(b"\n");
        let warning_offset = bytes
            .windows(b"stream".len())
            .rposition(|window| window == b"stream")
            .expect("stream token")
            + b"stream".len();
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 8);
        let _state = load_xref_state_from_bytes(
            &bytes,
            XrefLoadOptions {
                description: b"canonical-stream-trailer.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            Some(resolver.as_ref()),
        )
        .expect("the canonical owner must use the shared classic trailer route");
        let owner_diagnostics = resolver.repair_diagnostics();
        let warning = owner_diagnostics
            .entries()
            .iter()
            .find(|warning| warning.get_message_detail() == b"stream keyword found in trailer")
            .expect("canonical stream warning");
        assert_eq!(warning.get_object(), b"trailer");
        assert_eq!(warning.get_file_position(), warning_offset as i64);
    }

    #[test]
    fn canonical_xref_handoff_does_not_retain_a_bootstrap_cache() {
        let (bytes, _) = classic_xref_with_trailer("<< /Size 1 >>");
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 11);
        let canonical =
            load_xref_state_from_bytes(&bytes, XrefLoadOptions::default(), Some(resolver.as_ref()))
                .expect("canonical xref loading succeeds");
        assert!(
            canonical.bootstrap_cache.is_none(),
            "canonical state must leave cache ownership on ResolverHandle"
        );

        let ownerless = load_xref_state_with_options(
            &mut std::io::Cursor::new(bytes),
            XrefLoadOptions::default(),
        )
        .expect("owner-less xref loading succeeds");
        assert!(
            ownerless.bootstrap_cache.is_some(),
            "standalone xref loading must retain its temporary cache"
        );
    }

    #[test]
    fn context_conversion_preserves_reconstruction_inputs_across_owner_routes() {
        let entries = BTreeMap::new();
        let reference_offsets: Rc<[u64]> = Rc::from(Vec::<u64>::new().into_boxed_slice());
        let bootstrap_cache = empty_bootstrap_cache();
        let source = XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries: &entries,
            reference_offsets: &reference_offsets,
            bootstrap_cache: &bootstrap_cache,
        };

        let canonical = context_spec_without_bootstrap_cache(source);
        assert!(matches!(
            canonical,
            XrefReadContextSpec::Reconstruction {
                line_scan_entries,
                reference_offsets: offsets,
            } if std::ptr::eq(line_scan_entries, &entries)
                && Rc::ptr_eq(offsets, &reference_offsets)
        ));

        let ownerless = context_spec_with_bootstrap_cache(source, &bootstrap_cache);
        assert!(matches!(
            ownerless,
            XrefReadContextSpec::ReconstructionWithCache {
                line_scan_entries,
                reference_offsets: offsets,
                bootstrap_cache: cache,
            } if std::ptr::eq(line_scan_entries, &entries)
                && Rc::ptr_eq(offsets, &reference_offsets)
                && Rc::ptr_eq(cache, &bootstrap_cache)
        ));
    }

    #[test]
    fn canonical_nonzero_startxref_recovery_keeps_bootstrap_cache_absent() {
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n999\n%%EOF\n".to_vec();
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), true, 12);
        let state = load_xref_state_from_bytes(
            &bytes,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            Some(resolver.as_ref()),
        )
        .expect("canonical recovery should rebuild a nonzero malformed startxref");

        assert!(state.already_reconstructed);
        assert!(state.bootstrap_cache.is_none());
        assert_eq!(state.loaded.trailer.object_ref(), None);
    }

    #[test]
    fn canonical_owner_skips_the_offset_zero_retry_when_startxref_is_missing() {
        // No `startxref` at all, so `parse_startxref` fails and `startxref`
        // becomes 0. Object 1 sits at logical offset 0 and its body has a
        // stray token before `endobj`, which is exactly the shape that
        // makes a real read of it warn. qpdf's `xref_offset == 0` check
        // (`QPDF.cc:450-452`) never attempts this read at all; a canonical
        // owner must match that, unlike the owner-less bootstrap path
        // covered by `nonzero_xref_stream_decode_warning_is_kept_before_recovery`.
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nextra\nendobj\n2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\ntrailer\n<< /Size 3 /Root 1 0 R >>\n%%EOF\n".to_vec();
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), true, 10);
        let _state = load_xref_state_from_bytes(
            &bytes,
            XrefLoadOptions {
                allow_repair: true,
                description: b"canonical-offset-zero.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            Some(resolver.as_ref()),
        )
        .expect("the trailer keyword is found directly, without candidate re-entry");
        // A live canonical-owner read (unlike this function's own local
        // `repair_diagnostics` buffer) pushes onto the document's own
        // diagnostic collection immediately (`ResolverHandle::push_qpdf_warning`),
        // which is why the skipped retry's absence must be checked here, not
        // on `state.loaded.repair_diagnostics`.
        let owner_diagnostics = resolver.repair_diagnostics();
        assert!(
            !owner_diagnostics
                .entries()
                .iter()
                .any(|warning| warning.get_object().starts_with(b"xref stream: object")),
            "the skipped retry must not leak an object-1 warning tagged as an xref stream read"
        );
        let trio: Vec<_> = resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|warning| String::from_utf8_lossy(warning.what_bytes()).into_owned())
            .collect();
        assert_eq!(
            trio,
            vec![
                "canonical-offset-zero.pdf: file is damaged",
                "canonical-offset-zero.pdf: can't find startxref",
                "canonical-offset-zero.pdf: Attempting to reconstruct cross-reference table",
            ],
            "the retry must not run at all, leaving only the reconstruction trio"
        );
    }

    #[test]
    fn classic_trailer_parse_errors_propagate_through_both_owner_routes() {
        let (bytes, _) = classic_xref_with_trailer("<< /Size 999999999999999999999999 >>");
        let error = load_xref_state_with_options(
            &mut std::io::Cursor::new(bytes.clone()),
            XrefLoadOptions::default(),
        )
        .expect_err("an overflowing trailer integer must fail the owner-less parser");
        assert!(matches!(
            error,
            Error::System(message)
                if message == "overflow/underflow converting 999999999999999999999999 to 64-bit integer"
        ));

        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 9);
        let error =
            load_xref_state_from_bytes(&bytes, XrefLoadOptions::default(), Some(resolver.as_ref()))
                .expect_err("an overflowing trailer integer must fail the canonical parser");
        assert!(matches!(
            error,
            Error::System(message)
                if message == "overflow/underflow converting 999999999999999999999999 to 64-bit integer"
        ));
    }

    #[test]
    fn one_space_before_xref_still_reads_the_table() {
        // qpdf's double-counted `skip` moves the read one byte early per
        // skipped space, so a single space lands on the newline after the
        // keyword and the table still parses -- with the warning. Measured
        // with qpdf 11.9.0: one warning, no reconstruction.
        let mut bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(b" xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n");
        bytes.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R >>\n");
        let state = load_xref_state_with_options(
            &mut std::io::Cursor::new({
                let mut bytes = bytes.clone();
                bytes.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
                bytes
            }),
            XrefLoadOptions::default(),
        )
        .expect("one space still leaves the table readable");
        let messages: Vec<_> = state
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.get_message_detail().to_vec())
            .collect();
        assert_eq!(
            messages,
            vec![b"extraneous whitespace seen before xref".to_vec()],
            "{messages:?}"
        );
    }

    #[test]
    fn a_non_dictionary_trailer_delivers_its_warnings_on_both_owner_routes() {
        // qpdf delivers the parser warnings and then throws
        // (`QPDF.cc:565-568,894-905`). Without leading whitespace the table
        // parses far enough to reach the trailer, which is where that split
        // matters.
        let mut bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n 0000000009  00000  n \n");
        bytes.extend_from_slice(b"trailer\n42\n");

        let mut registration = XrefRegistration::default();
        let mut diagnostics = Diagnostics::default();
        let error = parse_xref_from_start(
            &bytes,
            xref,
            xref as u64,
            "1.7",
            XrefLoadOptions::default(),
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
            None,
            false,
        )
        .expect_err("a non-dictionary trailer is terminal");
        assert!(matches!(
            error,
            Error::QpdfExc(exception)
                if exception.get_message_detail() == b"expected trailer dictionary"
        ));
        assert!(diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.get_message_detail()
                == b"accepting invalid xref table entry"));

        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 3);
        let mut registration = XrefRegistration::default();
        parse_xref_from_start_with_owner(
            &bytes,
            xref,
            xref as u64,
            "1.7",
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
            false,
            Some(resolver.as_ref()),
        )
        .expect_err("the canonical owner sees the same terminal error");
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.get_message_detail()
                == b"accepting invalid xref table entry"));
    }

    #[test]
    fn reconstruction_skips_headers_its_guards_reject() {
        // `insertReconstructedXrefEntry` drops `obj <= 0` and a generation
        // outside `0..65535` without a warning (`QPDF.cc:1195-1198`). Measured
        // with qpdf 11.9.0 on a file of this shape: the scan reports only
        // 1/0, 2/0 and 3/0.
        let mut bytes = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        bytes.extend_from_slice(b"0 0 obj\n<< /Skipped (obj 0) >>\nendobj\n");
        bytes.extend_from_slice(b"5 65535 obj\n<< /Skipped (gen 65535) >>\nendobj\n");
        for (number, body) in [
            (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ] {
            bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        // A startxref past the end forces the line-scan reconstruction.
        bytes.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n999999\n%%EOF\n");

        let state = load_xref_state_with_options(
            &mut std::io::Cursor::new(bytes),
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        )
        .expect("reconstruction recovers the well-formed objects");
        let recovered: Vec<_> = state
            .loaded
            .entries
            .keys()
            .map(|object_ref| (object_ref.number, object_ref.generation))
            .collect();
        assert_eq!(recovered, vec![(1, 0), (2, 0), (3, 0)], "{recovered:?}");
    }

    #[test]
    fn recovery_scan_integer_preserves_qpdf_narrowing_error() {
        let token = Token::new(TokenType::Integer, b"3000000000".to_vec());
        let error = parse_scan_integer(&token).expect_err("oversized object header must fail");
        assert!(matches!(
            error,
            Error::SystemBytes(message)
                if message == b"integer out of range converting 3000000000 from a 8-byte signed type to a 4-byte signed type"
        ));
    }

    #[test]
    fn recovery_scan_integer_uses_qpdf_no_digit_zero() {
        let empty_numeric = Token::new(TokenType::Integer, b"-".to_vec());
        assert_eq!(parse_scan_integer(&empty_numeric).unwrap(), 0);
    }

    #[test]
    fn a_trailer_keyword_ends_at_any_delimiter() {
        // `readToken(m->file).isWord("trailer")` (`QPDF.cc:889`) ends the
        // keyword at any delimiter (`QPDFTokenizer.cc:16-23`), so `trailer<<`
        // is as valid as `trailer\n<<`. Requiring whitespace here sent the
        // line to the section-header parser instead, and qpdf reads
        // `resurrect-missing-page-arr.pdf` -- which is written that way --
        // without a single warning.
        for separator in ["", " ", "\n", "\r\n", "\t"] {
            let mut bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
            let xref = bytes.len();
            bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n");
            bytes.extend_from_slice(
                format!("trailer{separator}<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                    .as_bytes(),
            );

            let state = load_xref_state_with_options(
                &mut std::io::Cursor::new(bytes),
                XrefLoadOptions::default(),
            )
            .unwrap_or_else(|error| panic!("separator {separator:?} must parse: {error:?}"));
            let diagnostics = state.loaded.repair_diagnostics.entries().to_vec();
            assert!(
                diagnostics.is_empty(),
                "separator {separator:?} must not trigger recovery: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn a_vertical_tab_separates_classic_xref_fields() {
        // `QUtil::is_space` counts `\v` (`include/qpdf/QUtil.hh:497-501`), and
        // `parse_xrefEntry` skips fields with it like any other space.
        let mut bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000\x0b65535\x0bf \n0000000009 00000 n \n");
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );

        let state = load_xref_state_with_options(
            &mut std::io::Cursor::new(bytes),
            XrefLoadOptions::default(),
        )
        .expect("a vertical tab is a space to qpdf");
        let diagnostics = state.loaded.repair_diagnostics.entries().to_vec();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn a_section_header_needs_a_space_after_its_first_number() {
        // `parse_xrefFirst` gathers digits and then requires a space
        // (`QPDF.cc:735-745`). Until `trailer<<` stopped being mistaken for a
        // section header, that rejection was only ever reached by the mistake.
        assert_eq!(parse_xref_first_line(b"0 2"), Some((0, 2)));
        assert_eq!(parse_xref_first_line(b"0x 2"), None);
        assert_eq!(parse_xref_first_line(b"x 2"), None);
        // qpdf requires no space after the second number: it skips whatever
        // spaces follow and reports the bytes it consumed (`QPDF.cc:754-767`).
        assert_eq!(parse_xref_first_line(b"0 2x"), Some((0, 2)));
    }

    #[test]
    fn classic_xref_section_header_can_span_physical_lines_like_qpdf() {
        let mut cursor = ByteCursor::new(b"\n6\n2147483647", 0);
        let error = parse_xref_table(&mut cursor, b"", None, b"issue-335b.pdf")
            .expect_err("qpdf reaches the first missing xref row after the split header");
        assert!(matches!(
            error,
            Error::QpdfExc(warning)
                if warning.get_object() == b"xref table"
                    && warning.get_message_detail() == b"invalid xref entry (obj=6)"
        ));
    }

    #[test]
    fn xref_stream_index_rows_are_not_rejected_only_for_exceeding_size() {
        let mut cursor = ByteCursor::new(&[1, 0], 0);
        let mut registration = XrefRegistration::default();
        let entries =
            parse_xref_entries(&mut cursor, &[(35, 1)], (1, 0, 1), None, &mut registration)
                .expect("qpdf accepts an /Index row beyond the reported /Size");

        assert!(matches!(
            entries.as_slice(),
            [ParsedXrefEntry::Live {
                object_ref,
                entry: XrefEntry::Uncompressed { offset: 0 },
            }] if *object_ref == QpdfObjGen::new(35, 0)
        ));
    }

    #[test]
    fn the_space_and_delimiter_sets_match_qpdf() {
        // `QUtil::is_space` (`include/qpdf/QUtil.hh:497-501`) and the
        // tokenizer's `is_delimiter` (`libqpdf/QPDFTokenizer.cc:16-23`).
        for byte in *b"\t\n\x0b\x0c\r " {
            assert!(is_pdf_space(byte), "{byte:#04x} is a qpdf space");
            assert!(is_pdf_delimiter(byte), "{byte:#04x} ends a keyword");
        }
        // `parse_xrefEntry` relies on `is_space('\0')` being false to stop at
        // its buffer end (`QPDF.cc:775-782`), but NUL still ends a keyword.
        assert!(!is_pdf_space(0));
        for byte in *b"/(){}<>[]%\0" {
            assert!(is_pdf_delimiter(byte), "{byte:#04x} ends a keyword");
        }
        for byte in *b"a0-+.#" {
            assert!(!is_pdf_delimiter(byte), "{byte:#04x} continues a keyword");
        }
    }

    #[test]
    fn strict_classic_xref_invalid_entry_uses_qpdf_error_context() {
        let mut bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        let invalid_entry = bytes.len();
        bytes.extend_from_slice(b"000000000x 00000 n \n");
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );

        let error = load_xref_state_with_options(
            &mut std::io::Cursor::new(bytes),
            XrefLoadOptions {
                description: b"bad5.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
        )
        .expect_err("qpdf rejects the malformed classic xref entry in strict mode");

        assert!(matches!(
            error,
            Error::QpdfExc(exception)
                if exception.get_filename() == b"bad5.pdf"
                    && exception.get_object() == b"xref table"
                    && exception.get_file_position() == invalid_entry as i64
                    && exception.get_message_detail() == b"invalid xref entry (obj=1)"
        ));
    }

    #[test]
    fn classic_xref_leniency_covers_whitespace_and_diagnostic_boundaries() {
        assert_eq!(parse_xref_first_line(b"  0 1\n"), Some((0, 1)));
        let (_, _, _, invalid) = parse_xref_entry_line(b" 0000000000  65535  f \n")
            .expect("qpdf accepts a parseable but non-fixed-width xref row");
        assert!(invalid);
        assert!(parse_xref_entry_line(b"0000000000 00000 x\n").is_none());

        // Two spaces before the keyword: qpdf adds its `skip` to the offset it
        // started from, so the table read begins inside `xref` and fails
        // (`QPDF.cc:670-676`). Measured with qpdf 11.9.0 on a file of this
        // shape: `extraneous whitespace seen before xref`, then `file is
        // damaged`, `xref syntax invalid`, and reconstruction -- not a trailer
        // diagnostic.
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(b"  xref\n0 1\n 0000000000  65535  f \ntrailer\n42\n");
        let mut registration = XrefRegistration::default();
        let mut diagnostics = Diagnostics::default();
        let error = parse_xref_from_start(
            &bytes,
            xref,
            xref as u64,
            "1.4",
            XrefLoadOptions {
                description: b"bad-spacing.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
            None,
            false,
        )
        .expect_err("a non-dictionary trailer must retain parser diagnostics");

        assert!(matches!(
            error,
            Error::QpdfExc(exception)
                if exception.get_message_detail() == b"xref syntax invalid"
        ));
        let messages: Vec<_> = diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.get_message_detail())
            .collect();
        assert!(messages.contains(&b"extraneous whitespace seen before xref".as_slice()));
        // The lenient row is never reached here -- qpdf does not report it for
        // this file either. Its own coverage is the direct
        // `parse_xref_entry_line` assertion above and the whitespace-free
        // table below.
        assert!(!messages.contains(&b"accepting invalid xref table entry".as_slice()));

        let mut lenient = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let lenient_xref = lenient.len();
        lenient.extend_from_slice(
            b"xref\n0 2\n 0000000000  65535  f \n0000000009 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\n",
        );
        let state = load_xref_state_with_options(
            &mut std::io::Cursor::new({
                let mut bytes = lenient.clone();
                bytes.extend_from_slice(format!("startxref\n{lenient_xref}\n%%EOF\n").as_bytes());
                bytes
            }),
            XrefLoadOptions::default(),
        )
        .expect("qpdf accepts the lenient row and keeps reading");
        let lenient_messages: Vec<_> = state
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.get_message_detail().to_vec())
            .collect();
        assert!(
            lenient_messages
                .iter()
                .any(|message| message == b"accepting invalid xref table entry"),
            "{lenient_messages:?}"
        );

        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 11);
        let mut registration = XrefRegistration::default();
        let error = parse_xref_from_start_with_owner(
            &bytes,
            xref,
            xref as u64,
            "1.4",
            XrefLoadOptions {
                description: b"bad-spacing.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
            false,
            Some(resolver.as_ref()),
        )
        .expect_err("the canonical owner must retain the same diagnostics");
        assert!(matches!(
            error,
            Error::QpdfExc(exception)
                if exception.get_message_detail() == b"xref syntax invalid"
        ));
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.get_message_detail()
                == b"extraneous whitespace seen before xref"));
    }

    #[test]
    fn reconstructed_read_trailer_attributes_empty_candidate_to_trailer() {
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\nendobj\ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n";
        let loaded = load_xref_snapshot(&mut std::io::Cursor::new(bytes), true)
            .expect("the later dictionary candidate is recoverable");
        let warning = loaded
            .repair_diagnostics
            .entries()
            .iter()
            .find(|warning| warning.get_message_detail() == b"empty object treated as null")
            .expect("empty trailer warning");
        assert_eq!(warning.get_object(), b"trailer");
        assert_eq!(warning.get_file_position(), 53);
    }

    #[test]
    fn initial_prev_loop_does_not_reparse_the_initial_section() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 /Prev {xref} >> stream\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        let loaded = load_xref_snapshot(&mut std::io::Cursor::new(bytes), true)
            .expect("repair mode keeps the parsed trailer after reporting the loop");
        assert_eq!(
            loaded
                .repair_diagnostics
                .entries()
                .iter()
                .filter(|warning| warning.get_message_detail() == b"stream keyword found in trailer")
                .count(),
            1
        );
    }

    fn trailer_value_offset(bytes: &[u8]) -> usize {
        bytes
            .windows(b"trailer".len())
            .position(|window| window == b"trailer")
            .expect("classic fixture has a trailer keyword")
            + b"trailer".len()
    }

    fn loaded_state_with_trailer(trailer: ObjectHandle) -> LoadedXrefState {
        LoadedXrefState {
            loaded: LoadedXref {
                version: "1.4".to_owned(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            raw_entries: BTreeMap::new(),
            first_xref_item_offset: 0,
            classic_trailer_offset: None,
            pending_reconstruction_trigger: None,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: Some(empty_bootstrap_cache()),
            header_offset: 0,
            already_reconstructed: false,
        }
    }

    fn hybrid_xref_with_classic_and_live_warning() -> Vec<u8> {
        let mut bytes = hybrid_xref_with_indirect_filter();
        let endstream = b"endstream\nendobj";
        let endstream_start = bytes
            .windows(endstream.len())
            .position(|window| window == endstream)
            .expect("the hybrid xref stream has an endobj boundary");
        bytes.splice(
            endstream_start..endstream_start + endstream.len(),
            b"endstream\njunk\nendobj".iter().copied(),
        );
        let trailer_start = bytes
            .windows(b">>\nstartxref\n".len())
            .rposition(|window| window == b">>\nstartxref\n")
            .expect("the hybrid classic trailer has a startxref marker");
        bytes.splice(
            trailer_start..trailer_start + b">>\nstartxref\n".len(),
            b">> stream\nstartxref\n".iter().copied(),
        );
        bytes
    }

    fn hybrid_xref_with_classic_live_and_builder_warning() -> Vec<u8> {
        let mut bytes = hybrid_xref_with_classic_and_live_warning();
        let length_marker = b"/Length ";
        let length_start = bytes
            .windows(length_marker.len())
            .position(|window| window == length_marker)
            .expect("the hybrid xref stream has a declared length");
        let digits_start = length_start + length_marker.len();
        let digits_end = digits_start
            + bytes[digits_start..]
                .iter()
                .position(u8::is_ascii_whitespace)
                .expect("the hybrid xref length ends before the dictionary close");
        let declared_length = std::str::from_utf8(&bytes[digits_start..digits_end])
            .expect("the hybrid xref length is decimal")
            .parse::<usize>()
            .expect("the hybrid xref length fits usize");
        bytes.splice(
            digits_start..digits_end,
            (declared_length + 2).to_string().bytes(),
        );
        let endstream_start = bytes
            .windows(b">\nendstream\n".len())
            .position(|window| window == b">\nendstream\n")
            .expect("the hybrid xref stream has an endstream marker");
        bytes.splice(endstream_start..endstream_start, b"00".iter().copied());
        bytes
    }

    fn classic_xref_with_malformed_previous() -> (Vec<u8>, usize) {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let object_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let previous_xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R /Prev (bad) >>\n");
        let previous_trailer = trailer_value_offset(&bytes);

        let current_xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R /Prev {previous_xref} >>\n").as_bytes(),
        );
        bytes.extend_from_slice(format!("startxref\n{current_xref}\n%%EOF\n").as_bytes());
        (bytes, previous_trailer)
    }

    fn classic_xref_with_indirect_previous_in_older_section() -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let object_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let previous_xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R /Prev 2 0 R >>\n");

        let current_xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R /Prev {previous_xref} >>\n").as_bytes(),
        );
        bytes.extend_from_slice(format!("startxref\n{current_xref}\n%%EOF\n").as_bytes());
        bytes
    }

    fn assert_strict_classic_trailer_error(trailer: &str, message: &str) {
        let (bytes, _) = classic_xref_with_trailer(trailer);
        let offset = trailer_value_offset(&bytes);
        let mut reader = std::io::Cursor::new(bytes);
        let error = load_xref_snapshot(&mut reader, false)
            .expect_err("strict classic trailer validation must reject the fixture");

        assert!(matches!(
            error,
            Error::Parse {
                offset: actual_offset,
                message: actual_message,
            } if actual_offset == offset && actual_message == message
        ));
    }

    #[test]
    fn strict_classic_xref_rejects_missing_trailer_size() {
        assert_strict_classic_trailer_error(
            "<< /Root 1 0 R >>",
            "trailer dictionary lacks /Size key",
        );
    }

    #[test]
    fn strict_classic_xref_rejects_non_integer_trailer_size() {
        assert_strict_classic_trailer_error(
            "<< /Size (bad) /Root 1 0 R >>",
            "/Size key in trailer dictionary is not an integer",
        );
    }

    #[test]
    fn strict_classic_xref_rejects_null_trailer_size() {
        assert_strict_classic_trailer_error(
            "<< /Size null /Root 1 0 R >>",
            "trailer dictionary lacks /Size key",
        );
    }

    #[test]
    fn strict_classic_validation_forwards_trailer_parser_diagnostics() {
        let (bytes, xref) = classic_xref_with_trailer("<< /Root 1 0 R /Broken >>");
        let mut registration = XrefRegistration::default();
        let mut diagnostics = Diagnostics::default();
        let error = parse_xref_from_start(
            &bytes,
            xref,
            xref as u64,
            "1.4",
            XrefLoadOptions::default(),
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
            None,
            true,
        )
        .expect_err("missing /Size must remain a strict validation error");

        assert!(error
            .to_string()
            .contains("trailer dictionary lacks /Size key"));
        assert!(
            diagnostics.entries().iter().any(|diagnostic| diagnostic
                .message_string()
                .contains("dictionary ended prematurely")),
            "strict validation must forward parser diagnostics collected before the error"
        );

        let mut registration = XrefRegistration::default();
        let error = parse_xref_from_start(
            &bytes,
            xref,
            xref as u64,
            "1.4",
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
            true,
        )
        .expect_err("the same validation error must not require a diagnostic sink");
        assert!(error
            .to_string()
            .contains("trailer dictionary lacks /Size key"));
    }

    #[test]
    fn malformed_xref_stream_framing_error_is_forwarded() {
        let mut registration = XrefRegistration::default();
        let mut diagnostics = Diagnostics::default();
        let error = parse_xref_stream(
            b"not an indirect object",
            0,
            0,
            "1.5".to_owned(),
            XrefLoadOptions::default(),
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
            None,
        )
        .expect_err("a malformed xref-stream object header must fail framing");

        assert!(matches!(error, Error::Parse { .. }));
        assert!(diagnostics.entries().is_empty());
    }

    #[test]
    fn candidate_xref_stream_wrong_size_warning_survives_later_decode_failure() {
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Length 4 >>\nstream\nabcd\nendstream\nendobj\n%%EOF\n";
        let error = load_xref_snapshot(&mut std::io::Cursor::new(bytes), true)
            .expect_err("the malformed candidate must fail after warning");
        let (source, diagnostics) = error
            .open_failure()
            .expect("permissive candidate failure carries repair diagnostics");
        let Error::QpdfExc(source_warning) = source else {
            // cov:ignore: the preceding qpdf candidate assertion makes this defensive arm unreachable
            panic!("candidate recovery must preserve qpdf's structured terminal error");
            // cov:ignore: defensive variant mismatch is unreachable after the qpdf candidate assertion
        };
        assert_eq!(
            source_warning.message_string(),
            "error decoding candidate xref stream while recovering damaged file"
        );
        assert_eq!(source_warning.get_file_position(), 0);
        let messages: Vec<_> = diagnostics
            .entries()
            .iter()
            .map(|diagnostic| String::from_utf8_lossy(diagnostic.what_bytes()).into_owned())
            .collect();
        assert_eq!(
            messages,
            vec![
                "file is damaged",
                "can't find startxref",
                "Attempting to reconstruct cross-reference table",
                "xref stream, offset 9: Cross-reference stream data has the wrong size; expected = 2; actual = 4",
            ]
        );
    }

    #[test]
    fn nonzero_xref_stream_decode_warning_is_kept_before_recovery() {
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Length 4 >>\nstream\nabcd\nendstream\nendobj\nstartxref\n9\n%%EOF\n";
        let loaded = load_xref_snapshot(&mut std::io::Cursor::new(bytes), true)
            .expect("the reconstruction re-entry must skip the already-registered entry");
        let messages: Vec<_> = loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| String::from_utf8_lossy(diagnostic.what_bytes()).into_owned())
            .collect();
        assert_eq!(
            messages,
            vec![
                "xref stream, offset 9: Cross-reference stream data has the wrong size; expected = 2; actual = 4",
                "file is damaged",
                "xref stream, offset 71: unknown xref stream entry type 97",
                "Attempting to reconstruct cross-reference table",
                "xref stream, offset 9: Cross-reference stream data has the wrong size; expected = 2; actual = 4",
                "reported number of objects (1) is not one plus the highest object number (1)",
            ]
        );
    }

    #[test]
    fn xref_stream_previous_offset_validation_ignores_non_integer_prev() {
        let trailer = ObjectHandle::dictionary(vec![(
            b"/Prev".to_vec(),
            ObjectHandle::string(b"bad".to_vec()),
        )]);
        let registration = XrefRegistration::default();
        let (offset, diagnostics, error) = resolve_previous_xref_offset(
            b"",
            XrefLoadOptions::default(),
            &registration,
            XrefReadContextSpec::ActiveSection,
            &trailer,
            None,
            None,
        )
        .expect("xref-stream /Prev validation should be non-fatal");

        assert_eq!(offset, None);
        assert!(diagnostics.entries().is_empty());
        assert!(
            error.is_none(),
            "xref-stream trailers have no classic trailer error context"
        );
    }

    #[test]
    fn previous_xref_merge_propagates_an_uninitialized_trailer_error() {
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.4".to_owned(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer: ObjectHandle::uninitialized(),
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            raw_entries: BTreeMap::new(),
            first_xref_item_offset: 0,
            classic_trailer_offset: None,
            pending_reconstruction_trigger: None,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: Some(empty_bootstrap_cache()),
            header_offset: 0,
            already_reconstructed: false,
        };
        let mut registration = XrefRegistration::default();

        let error = merge_previous_xref_sections(
            b"",
            "1.4",
            &mut loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
        )
        .expect_err("an uninitialized trailer must fail at the dictionary boundary");
        assert!(matches!(error, Error::Internal(message) if message.contains("uninitialized")));
    }

    #[test]
    fn previous_xref_merge_propagates_an_indirect_prev_resolution_error() {
        let bytes = classic_xref_with_indirect_previous_in_older_section();
        let current_xref = bytes
            .windows(b"startxref\n".len())
            .position(|window| window == b"startxref\n")
            .expect("current xref has a startxref marker");
        let startxref = bytes[current_xref + b"startxref\n".len()..]
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| value.parse::<usize>().ok())
            .expect("startxref contains the current xref offset");
        let mut registration = XrefRegistration::default();
        let mut loaded = parse_xref_from_start(
            &bytes,
            startxref,
            startxref as u64,
            "1.4",
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
            true,
        )
        .expect("current xref section should parse before walking /Prev");

        let error = merge_previous_xref_sections(
            &bytes,
            "1.4",
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
        )
        .expect_err("an indirect /Prev with a stale xref row must propagate its trigger");
        assert!(matches!(error, Error::Parse { message, .. } if message == "expected 2 0 obj"));
    }

    #[test]
    fn previous_xref_merge_orders_successful_classic_diagnostics_before_live_warnings() {
        let bytes = hybrid_xref_with_classic_and_live_warning();
        let previous_xref = bytes
            .windows(b"xref\n0 6".len())
            .position(|window| window == b"xref\n0 6")
            .expect("the hybrid fixture has a classic xref section");
        let mut loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/Prev".to_vec(),
            ObjectHandle::integer(i64::try_from(previous_xref).unwrap()),
        )]));
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), true, 101);
        let mut registration = XrefRegistration::default();

        merge_previous_xref_sections(
            &bytes,
            "1.5",
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            Some(resolver.as_ref()),
        )
        .expect("the valid hybrid /Prev section should merge");

        let messages: Vec<_> = resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message_string())
            .collect();
        let trailer_warning = messages
            .iter()
            .position(|message| message == "stream keyword found in trailer")
            .expect("the classic trailer warning is retained");
        let live_warning = messages
            .iter()
            .position(|message| message == "expected endobj")
            .expect("the hybrid xref stream warning is retained");
        assert!(
            trailer_warning < live_warning,
            "classic trailer diagnostics must precede hybrid live diagnostics: {messages:?}"
        );
    }

    #[test]
    fn previous_xref_merge_orders_hybrid_read_before_builder_diagnostics() {
        let bytes = hybrid_xref_with_classic_live_and_builder_warning();
        let previous_xref = bytes
            .windows(b"xref\n0 6".len())
            .position(|window| window == b"xref\n0 6")
            .expect("the hybrid fixture has a classic xref section");
        let mut loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/Prev".to_vec(),
            ObjectHandle::integer(i64::try_from(previous_xref).unwrap()),
        )]));
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), true, 102);
        let mut registration = XrefRegistration::default();

        merge_previous_xref_sections(
            &bytes,
            "1.5",
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            Some(resolver.as_ref()),
        )
        .expect("the warning-bearing hybrid /Prev section should merge");

        let messages: Vec<_> = resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message_string())
            .collect();
        let trailer_warning = messages
            .iter()
            .position(|message| message == "stream keyword found in trailer")
            .expect("the classic trailer warning is retained");
        let live_warning = messages
            .iter()
            .position(|message| message == "expected endobj")
            .expect("the hybrid read warning is retained");
        let builder_warning = messages
            .iter()
            .position(|message| message.contains("Cross-reference stream data has the wrong size"))
            .unwrap_or_else(|| panic!("the hybrid builder warning is retained: {messages:?}"));
        assert!(
            trailer_warning < live_warning && live_warning < builder_warning,
            "qpdf call order must be trailer, read, builder: {messages:?}"
        );
    }

    #[test]
    fn previous_xref_merge_keeps_successful_xref_stream_diagnostics() {
        let previous_xref = 5usize;
        let mut bytes = b"%pad\n".to_vec();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Length 3 >>\nstream\n\0\0\0\nendstream\nendobj\n%tail\n",
        );
        let mut loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/Prev".to_vec(),
            ObjectHandle::integer(i64::try_from(previous_xref).unwrap()),
        )]));
        let mut registration = XrefRegistration::default();

        merge_previous_xref_sections(
            &bytes,
            "1.4",
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
        )
        .expect("the warning-bearing previous xref stream should merge");
        assert!(loaded
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("Cross-reference stream data has the wrong size")));
    }

    #[test]
    fn previous_xref_merge_orders_nonclassic_error_diagnostics_without_a_sink() {
        let previous_xref = 5usize;
        let mut bytes = b"%pad\n".to_vec();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Length 4 >>\nstream\nabcd\nendstream\nendobj\n%tail\n",
        );
        let mut loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/Prev".to_vec(),
            ObjectHandle::integer(i64::try_from(previous_xref).unwrap()),
        )]));
        let mut registration = XrefRegistration::default();

        let error = merge_previous_xref_sections(
            &bytes,
            "1.4",
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
        )
        .expect_err("the malformed previous xref stream must fail");
        assert!(
            matches!(&error, Error::Parse { .. }),
            "unexpected error: {error:?}"
        );
        assert!(loaded
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("Cross-reference stream data has the wrong size")));
    }

    #[test]
    fn reconstruction_contexts_retain_the_precomputed_index_across_xref_routes() {
        let mut entries = BTreeMap::new();
        entries.insert(ObjectRef::new(1, 0), XrefEntry::Uncompressed { offset: 0 });
        let reference_offsets = reconstructed_reference_offsets(&entries);

        let mut previous_loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/Size".to_vec(),
            ObjectHandle::integer(1),
        )]));
        let mut registration = XrefRegistration::default();
        merge_previous_xref_sections(
            b"",
            "1.4",
            &mut previous_loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::Reconstruction {
                line_scan_entries: &entries,
                reference_offsets: &reference_offsets,
            },
            None,
        )
        .expect("a reconstruction context without /Prev should be a no-op");

        let mut hybrid_loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/XRefStm".to_vec(),
            ObjectHandle::integer(1),
        )]));
        let result = merge_xref_stream_from_classic_trailer(
            b"",
            0,
            &mut hybrid_loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::Reconstruction {
                line_scan_entries: &entries,
                reference_offsets: &reference_offsets,
            },
            None,
        );
        assert!(matches!(result, Err(Error::Parse { .. })));

        let mut cached_hybrid_loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/XRefStm".to_vec(),
            ObjectHandle::integer(1),
        )]));
        let shared = empty_bootstrap_cache();
        let result = merge_xref_stream_from_classic_trailer(
            b"",
            0,
            &mut cached_hybrid_loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ReconstructionWithCache {
                line_scan_entries: &entries,
                reference_offsets: &reference_offsets,
                bootstrap_cache: &shared,
            },
            None,
        );
        assert!(matches!(result, Err(Error::Parse { .. })));
    }

    #[test]
    fn strict_classic_xref_rejects_non_integer_previous_offset() {
        let (bytes, offset) = classic_xref_with_malformed_previous();
        let mut reader = std::io::Cursor::new(bytes);
        let error = load_xref_snapshot(&mut reader, false)
            .expect_err("strict xref chain validation must reject malformed /Prev");

        assert!(matches!(
            error,
            Error::Parse {
                offset: actual_offset,
                message,
            } if actual_offset == offset
                && message == "/Prev key in trailer dictionary is not an integer"
        ));
    }

    #[test]
    fn repair_mode_reports_classic_trailer_validation_before_recovery() {
        let (bytes, _) = classic_xref_with_trailer("<< /Root 1 0 R >>");
        let mut reader = std::io::Cursor::new(bytes);
        let loaded = load_xref_snapshot(&mut reader, true)
            .expect("repair mode must recover a trailer missing /Size");
        let messages: Vec<_> = loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message_string())
            .collect();

        assert_eq!(
            messages,
            vec![
                "file is damaged",
                "trailer dictionary lacks /Size key",
                "Attempting to reconstruct cross-reference table",
            ]
        );
    }

    #[test]
    fn ordinary_classic_xref_loading_keeps_the_bootstrap_source_lazy() {
        let (bytes, _) = classic_xref_with_trailer("<< /Size 1 /Root 1 0 R >>");
        let mut reader = std::io::Cursor::new(bytes);
        let state = load_xref_state_with_options(&mut reader, XrefLoadOptions::default())
            .expect("ordinary classic xref should load");
        let bootstrap_cache = state
            .bootstrap_cache
            .expect("owner-less xref state has a bootstrap cache");
        let cache = bootstrap_cache.borrow();
        let document = cache
            .handle_document
            .as_ref()
            .expect("handle parser creates the shared bootstrap owner");

        assert!(
            document.bytes.get().is_none(),
            "unused trailer references must not force the source snapshot"
        );
    }

    /// A hybrid-reference file: a classic `xref` table with an `/XRefStm`
    /// pointing at a supplementary cross-reference stream whose own
    /// `/Filter` is an indirect reference (`4 0 R`) to a name object,
    /// rather than a direct `/Filter` name in the stream's own dictionary.
    /// This is a legal but unusual construction (confirmed accepted by live
    /// qpdf 11.9.0 `--check`, exit 0, no reconstruction) that exercises
    /// canonical `ObjectHandle::get_stream_data` `/Filter` dereference at a
    /// point where this context's deferred source snapshot may not yet be
    /// populated.
    fn hybrid_xref_with_indirect_filter() -> Vec<u8> {
        fn entry(kind: u8, offset: u16, gen: u8) -> [u8; 4] {
            let [hi, lo] = offset.to_be_bytes();
            [kind, hi, lo, gen]
        }

        let mut bytes = b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = [0usize; 6];

        offsets[1] = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offsets[2] = bytes.len();
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        offsets[3] = bytes.len();
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        offsets[4] = bytes.len();
        bytes.extend_from_slice(b"4 0 obj\n/ASCIIHexDecode\nendobj\n");

        offsets[5] = bytes.len();
        let mut raw_entries = Vec::new();
        raw_entries.extend_from_slice(&entry(0, 0, 0));
        for &offset in &offsets[1..=5] {
            raw_entries.extend_from_slice(&entry(1, offset as u16, 0));
        }
        let mut stream_data = raw_entries
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>()
            .into_bytes();
        stream_data.push(b'>');
        bytes.extend_from_slice(b"5 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /XRef /Filter 4 0 R /W [1 2 1] /Size 6 /Root 1 0 R /Length {} >>",
                stream_data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"\nstream\n");
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let xref_stream_offset = offsets[5];

        let classic_xref_offset = bytes.len();
        bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for &offset in &offsets[1..=5] {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R /XRefStm {xref_stream_offset} >>\n")
                .as_bytes(),
        );
        bytes.extend_from_slice(format!("startxref\n{classic_xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    fn hybrid_xref_with_historical_stream_revision() -> Vec<u8> {
        let mut bytes = hybrid_xref_with_indirect_filter();
        let previous_xref = bytes
            .windows(b"xref\n0 6".len())
            .position(|window| window == b"xref\n0 6")
            .expect("the base hybrid fixture has a classic xref section");
        let object_offsets: Vec<_> = (1..=4)
            .map(|object| {
                let marker = format!("{object} 0 obj\n");
                bytes
                    .windows(marker.len())
                    .position(|window| window == marker.as_bytes())
                    .expect("the base hybrid fixture has the page graph object")
            })
            .collect();
        let current_xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for offset in object_offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(b"0000000000 00000 f \n");
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 6 /Root 1 0 R /Prev {previous_xref} >>\nstartxref\n{current_xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    fn canonical_test_resolver(
        bytes: Vec<u8>,
        entries: BTreeMap<ObjectRef, XrefEntry>,
        allow_repair: bool,
        unique_id: u64,
    ) -> Rc<ResolverHandle<std::io::Cursor<Vec<u8>>>> {
        ResolverHandle::new_shared(
            std::io::Cursor::new(bytes),
            0,
            entries,
            allow_repair,
            false,
            Diagnostics::default(),
            crate::reader::resolver::ResolverWarningOptions::new(
                crate::QPDFLogger::create(),
                true,
                Vec::new(),
            ),
            unique_id,
        )
    }

    struct FailingCanonicalOwner {
        transport_error: bool,
        diagnostics: RefCell<Diagnostics>,
    }

    impl CanonicalTrailerOwner for FailingCanonicalOwner {
        fn indirect_handle(&self, _object_ref: ObjectRef) -> ObjectHandle {
            ObjectHandle::uninitialized()
        }

        fn direct_handle(&self, value: ObjectValue) -> ObjectHandle {
            ObjectHandle::from_value(value)
        }

        fn install_xref_entries(&self, _entries: BTreeMap<ObjectRef, XrefEntry>) {}

        fn set_header_offset(&self, _offset: usize) {}

        fn begin_parse(&self) -> Result<()> {
            Ok(())
        }

        fn end_parse(&self) {}

        fn read_object_at_offset(
            &self,
            _offset: u64,
            _expected: ObjectRef,
            _description: Option<Vec<u8>>,
        ) -> Result<(ObjectHandle, Option<u64>)> {
            Err(Error::parse(0, "synthetic canonical read failure"))
        }

        fn read_xref_stream_at_offset(
            &self,
            _offset: u64,
            _description: Option<Vec<u8>>,
        ) -> Result<(ObjectHandle, Option<u64>)> {
            self.diagnostics.borrow_mut().push(damaged_warning(
                b"synthetic.pdf",
                b"",
                "synthetic canonical read failure",
                Some(0),
            ));
            if self.transport_error {
                Err(Error::Io(std::io::Error::other(
                    "synthetic transport failure",
                )))
            } else {
                Err(Error::parse(0, "synthetic canonical read failure"))
            }
        }

        fn push_warning(&self, warning: QpdfExc) -> Result<()> {
            self.diagnostics.borrow_mut().push(warning);
            Ok(())
        }

        fn repair_diagnostics(&self) -> Diagnostics {
            self.diagnostics.borrow().clone()
        }
    }

    #[test]
    fn canonical_owner_warning_sink_records_a_local_diagnostic() {
        let owner = FailingCanonicalOwner {
            transport_error: false,
            diagnostics: RefCell::new(Diagnostics::default()),
        };
        owner
            .push_warning(damaged_warning(
                b"synthetic.pdf",
                b"",
                "synthetic live warning",
                Some(0),
            ))
            .expect("the synthetic owner warning sink accepts the diagnostic");
        assert_eq!(owner.repair_diagnostics().entries().len(), 1);
    }

    #[test]
    fn canonical_xref_context_rejects_malformed_stream_data() {
        let resolver = canonical_test_resolver(Vec::new(), BTreeMap::new(), false, 3);
        let mut context = CanonicalXrefContext::new(resolver.as_ref(), Vec::new());
        assert!(context
            .resolve_dictionary_value(&ObjectHandle::uninitialized(), "Type")
            .is_none());

        let dictionary = |extra: Option<ObjectHandle>, size: i64| {
            let mut entries = vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"XRef".to_vec())),
                (
                    b"/W".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::integer(1),
                        ObjectHandle::integer(0),
                        ObjectHandle::integer(0),
                    ]),
                ),
                (b"/Size".to_vec(), ObjectHandle::integer(size)),
            ];
            if let Some(extra) = extra {
                entries.push((b"/Filter".to_vec(), extra));
            }
            ObjectHandle::dictionary(entries)
        };

        let filtered = ObjectHandle::stream(
            dictionary(Some(ObjectHandle::name(b"ASCIIHexDecode".to_vec())), 1),
            Rc::new(b"not-hex".to_vec()),
        );
        let mut registration = XrefRegistration::default();
        assert!(build_xref_stream(
            &mut context,
            10,
            XrefStreamObjectRead {
                object_ref: ObjectRef::new(2, 0),
                object: filtered,
                stream_data_offset: Some(20),
            },
            &mut registration,
        )
        .is_err());

        let short = ObjectHandle::stream(dictionary(None, 2), Rc::new(vec![0]));
        assert!(build_xref_stream(
            &mut context,
            11,
            XrefStreamObjectRead {
                object_ref: ObjectRef::new(3, 0),
                object: short,
                stream_data_offset: Some(21),
            },
            &mut registration,
        )
        .is_err());
    }

    #[test]
    fn zero_width_xref_stream_uses_qpdf_damaged_warning_shape() {
        let resolver = canonical_test_resolver(Vec::new(), BTreeMap::new(), false, 3);
        let mut context = CanonicalXrefContext::new(resolver.as_ref(), Vec::new());
        let dictionary = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"XRef".to_vec())),
            (
                b"/W".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                ]),
            ),
            (b"/Size".to_vec(), ObjectHandle::integer(1)),
        ]);
        let stream = ObjectHandle::stream(dictionary, Rc::new(Vec::new()));
        let mut registration = XrefRegistration::default();
        let error = build_xref_stream(
            &mut context,
            3,
            XrefStreamObjectRead {
                object_ref: ObjectRef::new(1, 0),
                object: stream,
                stream_data_offset: None,
            },
            &mut registration,
        )
        .expect_err("qpdf rejects a zero-sized xref-stream entry");

        let Error::QpdfExc(warning) = error else {
            // cov:ignore: the preceding qpdf xref assertion makes this defensive arm unreachable
            panic!("qpdf xref-stream damage must remain a structured warning"); // cov:ignore: defensive variant mismatch is unreachable after the qpdf xref assertion
        };
        assert_eq!(warning.get_object(), b"xref stream");
        assert_eq!(warning.get_file_position(), 3);
        assert_eq!(
            warning.message_string(),
            "Cross-reference stream's /W indicates entry size of 0"
        );
    }

    #[test]
    fn canonical_xref_stream_reports_qpdfs_wrong_size_warning_for_a_recovered_payload_eol() {
        // qpdf's own recovered length (`recoverStreamLength`, `QPDF.cc:1482-1497`)
        // spans every byte up to the "endstream" it finds, which includes the
        // EOL that conventionally precedes that keyword. `getStreamData`
        // decodes exactly that span with no separate trim, so a real qpdf run
        // on this fixture warns "wrong size; expected = 1; actual = 2"
        // (verified against qpdf 11.9.0). The canonical-owner path now
        // reports the same warning instead of silently trimming the EOL
        // before the size comparison.
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let xref_pos = bytes.len();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /W [1 0 0] /Size 1 >>\nstream\n\0\nendstream\nendobj\n%tail\n",
        );
        let resolver = canonical_test_resolver(bytes, BTreeMap::new(), true, 4);
        let mut registration = XrefRegistration::default();
        let state = parse_xref_stream_with_canonical_owner(
            xref_pos,
            xref_pos as u64,
            "1.4".to_owned(),
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            resolver.as_ref(),
        )
        .expect("a recovered canonical xref stream should still parse its one free entry");

        assert_eq!(
            resolver.recovered_stream_eol(ObjectRef::new(1, 0)),
            Some(crate::parser::RecoveredStreamEol::Lf)
        );
        assert!(state.loaded.entries.is_empty());
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("wrong size; expected = 1; actual = 2")));
    }

    #[test]
    fn canonical_xref_candidate_keeps_first_trailer_and_maximum_offset() {
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let first_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /W [1 0 0] /Size 1 /Marker /First /Length 1 >>\nstream\n\0\nendstream\nendobj\n",
        );
        let second_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"2 0 obj\n<< /Type /XRef /W [1 0 0] /Size 1 /Marker /Second /Length 1 >>\nstream\n\0\nendstream\nendobj\n%tail\n",
        );
        let entries = BTreeMap::from([
            (
                ObjectRef::new(1, 0),
                XrefEntry::Uncompressed {
                    offset: first_offset,
                },
            ),
            (
                ObjectRef::new(2, 0),
                XrefEntry::Uncompressed {
                    offset: second_offset,
                },
            ),
        ]);
        let resolver = canonical_test_resolver(bytes.clone(), entries.clone(), false, 5);
        let (candidate, diagnostics) = find_xref_stream_trailer_candidate(
            &bytes,
            &entries,
            XrefLoadOptions::default(),
            &Rc::from(vec![first_offset, second_offset]),
            None,
            Some(resolver.as_ref()),
        );
        let candidate = candidate.expect("at least one canonical xref candidate");
        assert!(diagnostics.entries().is_empty(), "{diagnostics:?}");
        assert_eq!(candidate.max_offset, second_offset);
        assert_eq!(
            candidate
                .trailer
                .try_get_key(b"/Marker")
                .expect("marker")
                .try_as_name()
                .expect("name"),
            Some(b"First".to_vec())
        );
    }

    #[test]
    fn canonical_xref_read_maps_owner_failures_like_qpdf() {
        for transport_error in [false, true] {
            let owner = FailingCanonicalOwner {
                transport_error,
                diagnostics: RefCell::new(Diagnostics::default()),
            };
            let _ = owner.indirect_handle(ObjectRef::new(1, 0));
            let _ = owner.direct_handle(ObjectValue::Integer(1));
            owner.install_xref_entries(BTreeMap::new());
            owner.set_header_offset(0);
            owner.begin_parse().expect("synthetic parse guard");
            owner.end_parse();
            assert!(owner
                .read_object_at_offset(0, ObjectRef::new(1, 0), None)
                .is_err());
            let mut registration = XrefRegistration::default();
            let error = parse_xref_stream_with_canonical_owner(
                0,
                0,
                "1.4".to_owned(),
                XrefLoadOptions::default(),
                &mut registration,
                &owner,
            )
            .expect_err("synthetic owner read must fail");
            if transport_error {
                assert!(matches!(error, Error::Io(_)));
            } else {
                assert!(matches!(
                    error,
                    Error::Parse { message, .. } if message == "xref not found"
                ));
            }
            // The warning stays on the owner: for a real document
            // `push_qpdf_warning` both logs it and records it, and `engine.rs`
            // installs the owner sink onto that same document. This delivers
            // one repair warning, matching qpdf (`QPDF.cc:518-522`).
            assert_eq!(owner.repair_diagnostics().entries().len(), 1);
        }
    }

    #[test]
    fn canonical_xref_builder_failure_delivers_payload_warning_to_owner() {
        let mut bytes =
            b"1 0 obj\n<< /Type /XRef /W [1 0 0] /Size 1 /Length 2 >>\nstream\n".to_vec();
        bytes.extend_from_slice(&[9, 0]);
        bytes.extend_from_slice(b"\nendstream\nendobj\n%tail\n");
        let resolver = canonical_test_resolver(bytes, BTreeMap::new(), false, 7);
        let mut registration = XrefRegistration::default();
        let error = parse_xref_stream_with_canonical_owner(
            0,
            0,
            "1.4".to_owned(),
            XrefLoadOptions::default(),
            &mut registration,
            resolver.as_ref(),
        )
        .expect_err("an unknown xref entry type must fail the canonical build");

        assert!(matches!(
            error,
            Error::Parse { message, .. } if message == "unknown xref stream entry type 9"
        ));
        let owner_diagnostics = resolver.repair_diagnostics();
        assert_eq!(owner_diagnostics.entries().len(), 1);
        assert!(owner_diagnostics.entries()[0]
            .message_string()
            .contains("wrong size"));
    }

    #[test]
    fn canonical_xref_read_keeps_a_recovered_null_out_of_the_cache_slot() {
        let object_ref = ObjectRef::new(1, 0);
        let mut bytes = b"%PDF-1.4\n1 0 obj\n".to_vec();
        let offset = 9u64;
        bytes.extend(std::iter::repeat_n(b'[', 501));
        bytes.extend_from_slice(b"\nendobj\n%tail\n");
        let resolver = canonical_test_resolver(
            bytes,
            BTreeMap::from([(object_ref, XrefEntry::Uncompressed { offset })]),
            false,
            6,
        );
        let owner: &dyn CanonicalTrailerOwner = resolver.as_ref();
        let (read, _) = owner
            .read_xref_stream_at_offset(offset, None)
            .expect("a malformed cached xref slot still returns a null handle");

        assert_eq!(read.object_ref(), Some(object_ref));
        assert!(read.is_null());
        assert!(!resolver.get_object_handle(object_ref).is_resolved());
    }

    #[test]
    fn ownerless_hybrid_resolution_forwards_a_reconstruction_trigger() {
        let object_ref = ObjectRef::new(1, 0);
        let bytes = b"%pad\n2 0 obj\n0\nendobj\n";
        let object_offset = 5;
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            QpdfObjGen::new(1, 0),
            XrefEntry::Uncompressed {
                offset: object_offset,
            },
        );
        let shared = empty_bootstrap_cache();
        let indirect = {
            let context = XrefReadContext::new(
                bytes,
                XrefReadContextSpec::ActiveSectionWithCache {
                    bootstrap_cache: &shared,
                },
                &registration,
                XrefLoadOptions {
                    allow_repair: true,
                    ..XrefLoadOptions::default()
                },
            );
            context.document.handle_for_reference(object_ref)
        };
        let mut loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/XRefStm".to_vec(),
            indirect,
        )]));
        loaded.loaded.repair_diagnostics.push(damaged_warning(
            b"existing.pdf",
            b"",
            "existing warning",
            None,
        ));
        let mut sink = Diagnostics::default();

        let error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            Some(&mut sink),
            XrefReadContextSpec::ActiveSectionWithCache {
                bootstrap_cache: &shared,
            },
            None,
        )
        .expect_err("a stale indirect /XRefStm must trigger reconstruction");

        assert!(
            matches!(
                &error,
                Error::Parse { message, .. } if message == "expected 1 0 obj"
            ),
            "unexpected error: {error:?}"
        );
        assert_eq!(sink.entries().len(), 1);
    }

    #[test]
    fn hybrid_xref_stream_with_indirect_filter_loads_without_reconstruction() {
        let bytes = hybrid_xref_with_indirect_filter();
        let mut reader = std::io::Cursor::new(bytes);
        let state = load_xref_state_with_options(&mut reader, XrefLoadOptions::default())
            .expect("hybrid xref with an indirect /Filter must load");

        assert!(
            !state.already_reconstructed,
            "a valid indirect /Filter on the /XRefStm stream must not force \
             cross-reference reconstruction"
        );
    }

    #[test]
    fn ownerless_hybrid_xref_stream_keeps_builder_diagnostics() {
        let bytes = hybrid_xref_with_classic_live_and_builder_warning();
        let xref_stream_pos = bytes
            .windows(b"5 0 obj\n".len())
            .position(|window| window == b"5 0 obj\n")
            .expect("the hybrid fixture has its xref stream object");
        let filter_pos = bytes
            .windows(b"4 0 obj\n".len())
            .position(|window| window == b"4 0 obj\n")
            .expect("the hybrid fixture has its indirect filter object");
        let mut loaded = loaded_state_with_trailer(ObjectHandle::dictionary(vec![(
            b"/XRefStm".to_vec(),
            ObjectHandle::integer(i64::try_from(xref_stream_pos).unwrap()),
        )]));
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            QpdfObjGen::new(4, 0),
            XrefEntry::Uncompressed {
                offset: filter_pos as u64,
            },
        );
        registration.insert_xref_entry(
            QpdfObjGen::new(5, 0),
            XrefEntry::Uncompressed {
                offset: xref_stream_pos as u64,
            },
        );
        merge_xref_stream_from_classic_trailer(
            &bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
        )
        .expect("the ownerless hybrid xref stream should remain recoverable");
        let messages: Vec<_> = loaded
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message_string())
            .collect();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("Cross-reference stream data has the wrong size")),
            "builder diagnostics: {messages:?}"
        );
    }

    #[test]
    fn canonical_owner_keeps_hybrid_xref_stream_and_filter_in_one_cache() {
        let bytes = hybrid_xref_with_indirect_filter();
        let resolver = ResolverHandle::new_shared(
            std::io::Cursor::new(bytes.clone()),
            0,
            BTreeMap::new(),
            false,
            false,
            Diagnostics::default(),
            crate::reader::resolver::ResolverWarningOptions::new(
                crate::QPDFLogger::create(),
                true,
                Vec::new(),
            ),
            1,
        );
        let state =
            load_xref_state_from_bytes(&bytes, XrefLoadOptions::default(), Some(resolver.as_ref()))
                .expect("canonical owner should load the hybrid xref stream");

        let xref_stream = resolver.get_object_handle(ObjectRef::new(5, 0));
        let filter = resolver.get_object_handle(ObjectRef::new(4, 0));
        assert!(
            !xref_stream.is_resolved(),
            "a known xref-stream slot must not be overwritten by the special qpdf read"
        );
        assert!(
            filter.is_resolved(),
            "indirect /Filter metadata must resolve through the canonical resolver"
        );
        xref_stream
            .try_dereference()
            .expect("the later canonical lookup must resolve the xref stream");
        assert!(xref_stream.is_resolved());
        assert!(
            state
                .parsed_xref_streams
                .contains_key(&ObjectRef::new(5, 0)),
            "canonical xref streams retain provenance for final cache registration"
        );
    }

    #[test]
    fn canonical_historical_xref_stream_is_not_a_live_object() {
        let bytes = hybrid_xref_with_historical_stream_revision();
        let pdf = crate::Pdf::open_with_options(
            std::io::Cursor::new(bytes),
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("the incremental historical xref fixture opens");
        let historical = ObjectRef::new(5, 0);

        assert!(
            pdf.canonical_object_refs().contains(&historical),
            "qpdf object-cache enumeration retains the historical xref stream"
        );
        assert!(
            !pdf.canonical_live_object_refs().contains(&historical),
            "a superseded xref stream must not be treated as an effective live object"
        );
    }

    #[test]
    fn canonical_owner_keeps_reconstruction_candidate_in_one_cache() {
        let bytes = hybrid_xref_with_indirect_filter();
        let recovered = recover_xref_entries(&bytes, false, b"").expect("scan candidate entries");
        let mut entries = recovered.entries;
        let mut parsed_xref_streams = BTreeMap::new();
        let mut repair_diagnostics = Diagnostics::default();
        let mut trailer_references = BTreeSet::new();
        let resolver = ResolverHandle::new_shared(
            std::io::Cursor::new(bytes.clone()),
            0,
            BTreeMap::new(),
            true,
            false,
            Diagnostics::default(),
            crate::reader::resolver::ResolverWarningOptions::new(
                crate::QPDFLogger::create(),
                true,
                Vec::new(),
            ),
            2,
        );

        recover_trailer_from_xref_stream_candidate(
            &bytes,
            "1.5",
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut entries,
            &mut parsed_xref_streams,
            &mut repair_diagnostics,
            &mut trailer_references,
            None,
            None,
            Some(resolver.as_ref()),
        )
        .expect("the canonical reconstruction candidate must be recoverable");

        assert!(resolver
            .get_object_handle(ObjectRef::new(5, 0))
            .is_resolved());
        assert!(resolver
            .get_object_handle(ObjectRef::new(4, 0))
            .is_resolved());
        assert!(parsed_xref_streams.contains_key(&ObjectRef::new(5, 0)));
    }

    #[test]
    fn candidate_recovery_passes_one_offset_index_through_reentry_and_size() {
        let bytes = hybrid_xref_with_indirect_filter();
        let recovered = recover_xref_entries(&bytes, false, b"").expect("scan candidate entries");
        let mut entries = recovered.entries;
        let mut parsed_xref_streams = BTreeMap::new();
        let mut repair_diagnostics = Diagnostics::default();
        let mut trailer_references = BTreeSet::new();

        let recovered = recover_trailer_from_xref_stream_candidate(
            &bytes,
            "1.5",
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut entries,
            &mut parsed_xref_streams,
            &mut repair_diagnostics,
            &mut trailer_references,
            None,
            None,
            None,
        )
        .expect("the scanned xref-stream candidate must be recoverable");

        assert!(recovered.1 > 0);
        assert!(!entries.is_empty());
    }

    #[test]
    fn candidate_reentry_seed_drops_stale_type_one_rows_but_keeps_line_scan_rows() {
        let entries = BTreeMap::from([(
            ObjectRef::new(5, 0),
            XrefEntry::Uncompressed { offset: 100 },
        )]);
        let preexisting_raw = BTreeMap::from([
            (
                QpdfObjGen::new(5, 0),
                XrefEntry::Uncompressed { offset: 200 },
            ),
            (
                QpdfObjGen::new(6, 0),
                XrefEntry::Compressed {
                    stream: 9,
                    index: 0,
                },
            ),
        ]);

        let registration = seed_candidate_reentry_registration(&entries, Some(&preexisting_raw))
            .expect("qpdf-sized object references fit the raw identity");

        assert_eq!(
            registration.raw_entries.get(&QpdfObjGen::new(5, 0)),
            Some(&XrefEntry::Uncompressed { offset: 100 })
        );
        assert_eq!(
            registration.raw_entries.get(&QpdfObjGen::new(6, 0)),
            Some(&XrefEntry::Compressed {
                stream: 9,
                index: 0,
            })
        );
    }

    #[test]
    fn recovered_state_merge_retains_accumulated_non_type_one_raw_rows() {
        let mut recovered = loaded_state_with_trailer(ObjectHandle::dictionary(Vec::new()));
        let mut accumulated = loaded_state_with_trailer(ObjectHandle::dictionary(Vec::new()));
        accumulated.raw_entries.insert(
            QpdfObjGen::new(6, 0),
            XrefEntry::Compressed {
                stream: 9,
                index: 0,
            },
        );

        recovered = merge_recovered_qpdf_state(recovered, accumulated, &BTreeSet::new());

        assert_eq!(
            recovered.raw_entries.get(&QpdfObjGen::new(6, 0)),
            Some(&XrefEntry::Compressed {
                stream: 9,
                index: 0,
            })
        );
    }

    #[test]
    fn reconstructed_size_revalidation_uses_the_recovery_offset_index() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let object_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{object_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 0 R /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );

        let mut reader = std::io::Cursor::new(bytes);
        let mut initial_registration = XrefRegistration::default();
        let mut initial_diagnostics = Diagnostics::default();
        let mut initial_first_xref_item_offset = None;
        let initial = parse_xref_from_start(
            reader.get_ref(),
            xref,
            xref as u64,
            "1.4",
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut initial_registration,
            Some(&mut initial_diagnostics),
            XrefReadContextSpec::ActiveSection,
            Some(&mut initial_first_xref_item_offset),
            true,
        )
        .expect("the initial classic section must defer the stale /Size mismatch");
        assert!(initial.pending_reconstruction_trigger.is_some());

        let recovered = load_xref_state_with_options(
            &mut reader,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        )
        .expect("repair mode must complete the recovered size revalidation");
        assert!(recovered.already_reconstructed);
        assert!(recovered
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message_string().contains("expected 2 0 obj")));
    }

    #[test]
    fn bootstrap_trigger_and_diagnostic_append_stay_on_handle_state() {
        let object_ref = ObjectRef::new(1, 0);
        let mut entries = BTreeMap::new();
        entries.insert(object_ref, XrefEntry::Uncompressed { offset: 1 });
        let state = Rc::new(RefCell::new(BootstrapHandleState {
            reconstruction_trigger: Some((1, "header mismatch".to_owned())),
            ..BootstrapHandleState::default()
        }));
        let document = BootstrapHandleDocument::new_with_state(
            Some(b"x"),
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            state,
        );
        let handle = document.handle_for_reference(object_ref);
        let error = <BootstrapHandleDocument as DocumentResolver>::resolve_indirect(
            &document, object_ref, &handle,
        )
        .expect_err("a reconstruction trigger propagates the parse error");
        assert!(matches!(error, Error::Parse { .. }));

        let registration = XrefRegistration::default();
        let mut context = XrefReadContext::new(
            b"",
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        context
            .document
            .push_diagnostic(damaged_warning(b"", b"", "late diagnostic", None));
        let mut diagnostics = Diagnostics::default();
        context.append_diagnostics_to(&mut diagnostics);
        assert_eq!(diagnostics.entries().len(), 1);
    }

    #[test]
    fn repair_diagnostics_preserve_each_qpdf_trigger_error_shape() {
        let errors = vec![
            Error::QpdfExc(damaged_warning(b"input.pdf", b"", "qpdf trigger", Some(4))),
            Error::parse(8, "xref not found"),
            Error::parse(9, "can't find startxref"),
            Error::parse(10, "loop detected following xref tables"),
            Error::parse(11, "trailer dictionary is invalid"),
            Error::parse(12, "unknown xref stream entry type 9"),
            Error::parse(13, "other parse error"),
            Error::Io(std::io::Error::other("read failed")),
            Error::SystemBytes(b"system bytes".to_vec()),
            Error::System("system".to_owned()),
            Error::Internal("internal".to_owned()),
            Error::Unsupported("unsupported".to_owned()),
        ];

        for error in errors {
            let mut diagnostics = Diagnostics::default();
            push_repair_diagnostics(&mut diagnostics, &error, 99, b"input.pdf");
            assert_eq!(diagnostics.len(), 3);
            assert_eq!(
                diagnostics.entries()[0].get_message_detail(),
                b"file is damaged"
            );
            assert_eq!(
                diagnostics.entries()[2].get_message_detail(),
                b"Attempting to reconstruct cross-reference table"
            );
        }

        let mut diagnostics = Diagnostics::default();
        push_repair_diagnostics(
            &mut diagnostics,
            &Error::System(
                "overflow/underflow converting 9900000000000000000 to 64-bit integer".to_owned(),
            ),
            99,
            b"input.pdf",
        );
        assert_eq!(
            diagnostics.entries()[1].get_message_detail(),
            b"error reading xref: overflow/underflow converting 9900000000000000000 to 64-bit integer"
        );
        assert_eq!(diagnostics.entries()[1].get_file_position(), 0);
    }

    #[test]
    fn trailer_parser_diagnostics_retain_qpdf_object_description() {
        let diagnostics = trailer_diagnostics(
            750,
            vec![ParserDiagnostic {
                relative_offset: 3,
                message: "treating unexpected brace token as null".to_owned(),
            }],
            b"bad13.pdf",
            None,
        );

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].get_object(), b"trailer");
        assert_eq!(
            diagnostics[0].what_bytes(),
            b"bad13.pdf (trailer, offset 753): treating unexpected brace token as null"
        );
    }

    #[test]
    fn trailer_hex_diagnostics_preserve_raw_invalid_bytes() {
        let diagnostics = trailer_diagnostics(
            750,
            vec![
                ParserDiagnostic {
                    relative_offset: 0,
                    message: "invalid character (�) in hexstring".to_owned(),
                },
                ParserDiagnostic {
                    relative_offset: 4,
                    message: "invalid character (�) in hexstring".to_owned(),
                },
                ParserDiagnostic {
                    relative_offset: 7,
                    message: "invalid character (�) in hexstring".to_owned(),
                },
                ParserDiagnostic {
                    relative_offset: 8,
                    message: "invalid character (�) in hexstring".to_owned(),
                },
            ],
            b"bad13.pdf",
            Some(b"<a\xa8><a>\xa8<a"),
        );

        assert_eq!(
            diagnostics[0].get_message_detail(),
            b"invalid character (\xa8) in hexstring"
        );
        assert_eq!(
            diagnostics[1].get_message_detail(),
            "invalid character (�) in hexstring".as_bytes()
        );
        assert_eq!(
            diagnostics[2].get_message_detail(),
            b"invalid character (\xa8) in hexstring"
        );
        assert_eq!(
            diagnostics[3].get_message_detail(),
            "invalid character (�) in hexstring".as_bytes()
        );
    }

    #[test]
    fn resolve_length_maps_a_reconstruction_trigger_error_to_missing() {
        let object_ref = ObjectRef::new(1, 0);
        let mut entries = BTreeMap::new();
        entries.insert(object_ref, XrefEntry::Uncompressed { offset: 1 });
        let state = Rc::new(RefCell::new(BootstrapHandleState {
            reconstruction_trigger: Some((1, "header mismatch".to_owned())),
            ..BootstrapHandleState::default()
        }));
        let document = BootstrapHandleDocument::new_with_state(
            Some(b"x"),
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            state,
        );

        assert_eq!(
            document.resolve_length(object_ref),
            ResolvedStreamLength::Missing
        );
    }

    #[test]
    fn classic_xref_validation_covers_non_dictionary_and_invalid_hybrid_trailers() {
        let (bytes, xref) = classic_xref_with_trailer("42");
        let mut registration = XrefRegistration::default();
        let error = parse_xref_from_start(
            &bytes,
            xref,
            xref as u64,
            "1.4",
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
            true,
        )
        .expect_err("classic trailer must be a dictionary");
        assert!(matches!(
            error,
            Error::QpdfExc(exception)
                if exception.get_message_detail() == b"expected trailer dictionary"
        ));

        let (bytes, xref) = classic_xref_with_trailer("<< /Size 1 /XRefStm (bad) >>");
        let mut registration = XrefRegistration::default();
        let error = parse_xref_from_start(
            &bytes,
            xref,
            xref as u64,
            "1.4",
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
            None,
            true,
        )
        .expect_err("a non-integer hybrid offset is invalid");
        assert!(error.to_string().contains("invalid /XRefStm"));
    }

    #[test]
    fn trailer_reference_collection_and_candidate_reader_keep_canonical_handles() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Child".to_vec(),
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1),
            )]),
            Rc::new(b"data".to_vec()),
        );
        assert!(collect_trailer_references(&stream).contains(&ObjectRef::new(7, 0)));

        let registration = XrefRegistration::default();
        let bytes = b"1 0 obj\n42\nendobj\n";
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        assert!(
            read_xref_candidate(&mut context, bytes, 0, bytes.len(), 0, ObjectRef::new(2, 0),)
                .is_none()
        );

        let bytes = b"1 0 obj\n42 extra\nendobj\n";
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        let completed =
            read_xref_candidate(&mut context, bytes, 0, bytes.len(), 0, ObjectRef::new(1, 0))
                .expect("a well-framed object is read before it is committed");
        assert!(context.diagnostics.entries().is_empty());
        assert!(commit_xref_candidate(&mut context, completed, 0, ObjectRef::new(1, 0)).is_some());
        assert!(!context.diagnostics.entries().is_empty());

        let bytes = b"1 0 obj\n<< /Info (unterminated";
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );
        let completed =
            read_xref_candidate(&mut context, bytes, 0, bytes.len(), 0, ObjectRef::new(1, 0))
                .expect("a truncated object is recovered as a null read");
        assert!(is_recovered_null_candidate(&completed));
        // qpdf caches the null it substitutes for a failed read
        // (`QPDF.cc:1738-1748`); the recovered null stays cached so a later
        // reference neither re-reads the object nor repeats its diagnostics.
        let cached = commit_xref_candidate(&mut context, completed, 0, ObjectRef::new(1, 0))
            .expect("a recovered null is still committed to the cache");
        assert!(cached.is_null());
        assert!(context.cache.get(&ObjectRef::new(1, 0)).is_some());
        assert!(!context.diagnostics.entries().is_empty());

        let malformed = vec![b'['; crate::parser::MAX_PARSE_DEPTH + 1];
        let (trailer, diagnostics) = parse_trailer_candidate(&malformed, 0, b"", None);
        assert!(trailer.is_none());
        assert!(!diagnostics.is_empty());
    }

    #[test]
    fn bootstrap_cache_disconnects_reference_cycles_before_drop() {
        let first_ref = ObjectRef::new(1, 0);
        let second_ref = ObjectRef::new(2, 0);
        let first = ObjectHandle::new_indirect_unresolved(first_ref, -1);
        let second = ObjectHandle::new_indirect_unresolved(second_ref, -1);
        first.set_resolved(ObjectValue::Dictionary(BTreeMap::from([(
            b"/next".to_vec(),
            second.clone(),
        )])));
        second.set_resolved(ObjectValue::Dictionary(BTreeMap::from([(
            b"/next".to_vec(),
            first.clone(),
        )])));

        let cache = BootstrapCache {
            handle_state: Rc::new(RefCell::new(BootstrapHandleState {
                handles: BTreeMap::from([(first_ref, first.clone()), (second_ref, second.clone())]),
                ..BootstrapHandleState::default()
            })),
            handle_document: None,
            handle_document_owners: Vec::new(),
        };

        drop(cache);

        assert!(!first.is_indirect());
        assert!(!second.is_indirect());
    }

    #[test]
    fn bootstrap_cache_cleans_owned_states_and_detaches_stream_values() {
        let state = Rc::new(RefCell::new(BootstrapHandleState::default()));
        let entries = BTreeMap::new();
        let document = BootstrapHandleDocument::new_with_state(
            Some(b""),
            XrefEntryLookup::Registration(&entries),
            XrefLoadOptions::default(),
            Rc::clone(&state),
        );
        let cache = BootstrapCache {
            handle_state: state.clone(),
            handle_document: None,
            handle_document_owners: vec![document],
        };
        drop(state);
        drop(cache);

        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(
                b"/Length".to_vec(),
                ObjectHandle::integer(0),
            )]),
            stream_data: Some(Rc::new(Vec::new())),
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });
        let detached = detach_bootstrap_handle(&stream).expect("stream detaches");
        assert!(detached.get_filter_on_write().expect("stream flag"));
        assert_eq!(
            detached
                .as_stream_dict()
                .expect("detached stream dictionary")
                .get_key(b"/Length")
                .as_integer(),
            Some(0)
        );
    }

    #[test]
    fn xref_registration_ignores_negative_raw_object_numbers() {
        let mut registration = XrefRegistration::default();
        let key = QpdfObjGen::new(-1, 0);

        registration.insert_xref_entry(key, XrefEntry::Uncompressed { offset: 1 });
        registration.insert_free_xref_entry(key);

        assert!(registration.raw_entries.is_empty());
        assert!(registration.entries.is_empty());
        assert!(registration.deleted_objects.is_empty());
    }
}
