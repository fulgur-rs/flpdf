//! Load, validate, and recover PDF cross-reference data. Xref streams are
//! parsed as live `ObjectHandle` graphs before their encoded payloads enter the
//! handle-native filter pipeline.
//!
//! qpdf correspondence: QPDF.cc xref loading and repair.
//!
//! Every xref route runs through one
//! `CanonicalTrailerOwner` -- the document's own resolver -- exactly as
//! qpdf's `QPDF::processInputSource` hands the document its input source
//! before any parsing starts (`libqpdf/QPDF.cc:245-275`) and then reads PDF
//! objects only as they are needed (`include/qpdf/QPDF.hh:67-97,1453-1457`).
//! That owner supplies xref streams, hybrid sections, `/Prev` chains and
//! reconstruction candidates alike; there is no second, owner-less parser.
//!
//! `load_xref_state_from_source` is that route: it keeps only the
//! header/tail and current xref-section windows, fetches `/Prev` sections
//! from the same live source when they fall outside the window, and performs
//! reconstruction as a chunked live-source scan.
//!
//! qpdf's `xref_offset == 0` check (`libqpdf/QPDF.cc:450-452`) throws
//! `damagedPDF("can't find startxref")` immediately and never calls
//! `read_xref` at all, whether the zero came from a missing/malformed
//! `startxref` or a syntactically valid `startxref` that explicitly names
//! offset 0. `load_xref_state_from_window` preserves that boundary and
//! enters the line-scan recovery directly, so an object at logical offset 0
//! cannot enter the canonical cache as a speculative xref read before
//! `reconstruct_xref` chooses the effective occurrence
//! (`libqpdf/QPDF.cc:450-469,516-531`).
//!
//! Warnings produced while loading the cross-reference table are buffered in
//! `LoadedXref::repair_diagnostics` and delivered in `warn()` call order,
//! matching qpdf's single push_back-only `m->warnings`
//! (`libqpdf/QPDF.cc:487-494`). The buffer exists because Rust's function
//! boundaries make this module produce those warnings in separate pieces --
//! one per candidate parse or recovery attempt -- where qpdf appends to one
//! member as it goes. It is a container substitute that does not change the
//! order warnings are reported in, or the bytes written.
use crate::object_handle::ObjectValue;
use crate::parser::{
    parse_qpdf_file_object_handle_with_diagnostics, HandleResolver, ParserDiagnostic,
};
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::qutil::{qpdf_string_to_int_checked, QpdfIntParse};
use crate::reader::resolver::ResolverHandle;
use crate::tokenizer::{Token, TokenType, Tokenizer};
use crate::writer::DecodeLevel;
use crate::{
    Diagnostics, Error, ObjectHandle, ObjectRef, QpdfErrorCode, QpdfExc, Result, XrefEntry,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read, Seek};
use std::rc::Rc;

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
/// offset-read and warning boundaries needed to keep that ownership intact.
pub(crate) trait CanonicalTrailerOwner {
    fn indirect_handle(&self, object_ref: ObjectRef) -> ObjectHandle;
    fn direct_handle(&self, value: ObjectValue) -> ObjectHandle;
    fn install_xref_entries(&self, entries: BTreeMap<ObjectRef, XrefEntry>);
    fn discard_cached_generations(&self, object_gens: &[QpdfObjGen]);
    fn set_header_offset(&self, offset: usize);
    /// Flips the owner's reconstruction flag on, mirroring qpdf's
    /// `m->reconstructed_xref = true` inside `reconstruct_xref`
    /// (`QPDF.cc:518-524`), which runs both at open time (`:464`) and during
    /// object resolution (`:1617`) against the same `QPDF` instance. Xref
    /// loading calls this the moment its own reconstruction succeeds, so the
    /// owner's guard is armed by the loader itself rather than batched into a
    /// returned struct and applied afterward.
    ///
    /// The sequence point is not yet qpdf's. qpdf assigns the flag on entry,
    /// before it warns and before the scan runs, so a recovery that re-enters
    /// mid-scan is rejected and a scan that fails partway still leaves the
    /// flag set; this call happens once the scan has succeeded. Moving it to
    /// the entry point fails nine tests today, because this crate's
    /// candidate-xref re-entry does re-enter recovery while the scan is
    /// running.
    fn set_reconstructed_xref(&self);
    /// qpdf's `m->attempt_recovery` (`QPDF.hh:1461`), consulted at parse
    /// entry (`QPDF.cc:463`) and during the resolve-time retry
    /// (`QPDF.cc:1614-1637`) against the same `QPDF` instance. Xref loading
    /// reads this from the owner instead of carrying its own copy through
    /// `XrefLoadOptions`.
    fn attempt_recovery(&self) -> bool;
    /// qpdf's live `m->file` source boundary (`QPDF.hh:67-97,1453-1457`).
    /// Xref loading uses these operations instead of a complete input
    /// snapshot.
    fn source_seek(&self, offset: u64) -> Result<()>;
    fn source_tell(&self) -> Result<u64>;
    fn source_length(&self) -> Result<u64>;
    fn source_read(&self, buffer: &mut [u8]) -> Result<usize>;
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
    fn source_seek(&self, offset: u64) -> Result<()> {
        self.seek(offset)
    }

    fn source_tell(&self) -> Result<u64> {
        self.tell()
    }

    fn source_length(&self) -> Result<u64> {
        ResolverHandle::source_length(self)
    }

    fn source_read(&self, buffer: &mut [u8]) -> Result<usize> {
        self.read(buffer)
    }

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

    fn discard_cached_generations(&self, object_gens: &[QpdfObjGen]) {
        ResolverHandle::discard_cached_generations(self, object_gens);
    }

    fn set_header_offset(&self, offset: usize) {
        ResolverHandle::set_header_offset(self, offset);
    }

    fn set_reconstructed_xref(&self) {
        ResolverHandle::set_reconstructed_xref(self, true);
    }

    fn attempt_recovery(&self) -> bool {
        ResolverHandle::attempt_recovery(self)
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
/// qpdf warning sink. The document has one live owner and must not hand a
/// second logger channel to `engine.rs`.
fn deliver_canonical_diagnostics(
    owner: &dyn CanonicalTrailerOwner,
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    let batch = diagnostics.drain();
    for (delivered, warning) in batch.entries().iter().enumerate() {
        if let Err(error) = owner.push_warning(warning.clone()) {
            // qpdf appends to `m->warnings` one `warn()` call at a time, so a
            // sink that gives out partway through a batch leaves the warnings
            // it has not reached yet still pending. Draining the batch before
            // delivering any of it would discard that tail instead, which is a
            // difference in behavior rather than in container.
            for undelivered in &batch.entries()[delivered + 1..] {
                diagnostics.push(undelivered.clone());
            }
            return Err(error);
        }
    }
    Ok(())
}

fn with_xref_open_diagnostics(error: Error, owner: &dyn CanonicalTrailerOwner) -> Error {
    Error::with_open_diagnostics(error, owner.repair_diagnostics())
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
    /// qpdf's `m->uncompressed_after_compressed`: true once any xref stream
    /// section read for this document has a type-1 (or type-0) entry after
    /// its first type-2 entry (`QPDF.cc:1070,1110-1116`). Sticky across every
    /// section in the `/Prev` chain and the hybrid `/XRefStm`, and consumed
    /// later by `checkLinearizationInternal` (`QPDF_linearization.cc:478-481`).
    pub(crate) uncompressed_after_compressed: bool,
    /// Byte position qpdf's `readTrailer` records for a classic trailer
    /// keyword. `None` identifies an xref-stream dictionary, whose `/Prev`
    /// diagnostics have a different qpdf description and remain on that
    /// existing path.
    pub(crate) classic_trailer_offset: Option<usize>,
    pub(crate) trailer_references: BTreeSet<ObjectRef>,
    pub(crate) parsed_xref_streams: BTreeMap<ObjectRef, ObjectHandle>,
    pub(crate) header_offset: usize,
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
            .map(|key| i64::from(key.get_obj()))
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
) -> Vec<QpdfObjGen> {
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
    for object_gen in &lower_generations {
        raw_entries.remove(object_gen);
        if let Some(object_ref) = object_gen.to_object_ref() {
            entries.remove(&object_ref);
            parsed_xref_streams.remove(&object_ref);
        }
    }
    lower_generations
}

/// Drop trailer references whose exact raw generation was removed by qpdf's
/// post-chain highest-generation cleanup. Other dangling trailer references
/// remain visible in the cache, matching `QPDFParser`'s ordinary behavior.
fn discard_trailer_references(
    trailer_references: &mut BTreeSet<ObjectRef>,
    discarded_generations: &[QpdfObjGen],
) {
    trailer_references.retain(|object_ref| {
        QpdfObjGen::try_from_object_ref(*object_ref).map_or(true, |object_gen| {
            !discarded_generations.contains(&object_gen)
        })
    });
}

/// The `QPDF::Members` settings the cross-reference loader consults, carried
/// together the way qpdf keeps them on `m` rather than as parallel arguments.
///
/// `m->attempt_recovery` is not carried here: xref loading reads it from the
/// `CanonicalTrailerOwner` (`attempt_recovery()`), matching qpdf's single
/// `m->attempt_recovery` consulted from both `parse()` and the resolve-time
/// retry against the same `QPDF` instance, rather than a copy threaded
/// through this options struct.
#[derive(Debug, Clone, Default)]
pub(crate) struct XrefLoadOptions {
    /// qpdf `m->ignore_xref_streams` (`QPDF::setIgnoreXRefStreams`): never read
    /// a cross-reference stream.
    pub(crate) ignore_xref_streams: bool,
    /// qpdf's input source name, retained in every warning's QPDFExc.
    pub(crate) description: Vec<u8>,
}

fn damaged_warning(
    filename: &[u8],
    object: impl AsRef<[u8]>,
    message: impl AsRef<[u8]>,
    offset: Option<u64>,
) -> QpdfExc {
    QpdfExc::new(
        QpdfErrorCode::DamagedPdf,
        filename,
        object,
        offset
            .map(|value| i64::try_from(value).unwrap_or(i64::MAX))
            .unwrap_or(0),
        message.as_ref(),
    )
}

struct CanonicalTrailerParser<'document> {
    owner: &'document dyn CanonicalTrailerOwner,
    description_template: Rc<Vec<u8>>,
}

impl<'document> CanonicalTrailerParser<'document> {
    fn new(owner: &'document dyn CanonicalTrailerOwner, filename: &[u8]) -> Self {
        Self {
            owner,
            description_template: trailer_description_template(filename),
        }
    }
}

impl HandleResolver for CanonicalTrailerParser<'_> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.owner.indirect_handle(object_ref)
    }

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        self.owner.direct_handle(value)
    }

    fn description_template(&self) -> Option<Rc<Vec<u8>>> {
        Some(Rc::clone(&self.description_template))
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
    /// (`QPDFObjectHandle.cc:1289-1292`). The canonical-owner context uses
    /// the resolver-backed handle's own `get_stream_data` pipe rather than a
    /// second materialized decoder.
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

fn read_live_source_range(
    owner: &dyn CanonicalTrailerOwner,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>> {
    owner.source_seek(offset)?;
    let mut bytes = Vec::with_capacity(length);
    let mut chunk = vec![0u8; 64 * 1024];
    while bytes.len() < length {
        let requested = (length - bytes.len()).min(chunk.len());
        let read = owner.source_read(&mut chunk[..requested])?;
        if read == 0 {
            return Err(Error::parse(
                usize::try_from(
                    owner
                        .source_tell()
                        .unwrap_or(offset.saturating_add(bytes.len() as u64)),
                )
                .unwrap_or(usize::MAX),
                "unexpected end of input source",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

const LIVE_XREF_PROBE_SIZE: usize = 4 * 1024;
const LIVE_XREF_GROWTH_SIZE: usize = 64 * 1024;

fn classic_xref_start(bytes: &[u8]) -> Option<usize> {
    let position = bytes.iter().position(|byte| !is_pdf_space(*byte))?;
    bytes
        .get(position..)
        .is_some_and(|tail| tail.starts_with(b"xref"))
        .then_some(position)
}

fn trailer_dictionary_end(bytes: &[u8], trailer_keyword_end: usize) -> Option<usize> {
    let mut tokenizer = Tokenizer::new(bytes);
    tokenizer.allow_eof();
    tokenizer.set_position(trailer_keyword_end).ok()?;
    let mut dictionary_depth = 0usize;
    let mut array_depth = 0usize;
    loop {
        let token = tokenizer.read_token(false, 0).ok()?;
        match token.token_type {
            TokenType::DictOpen => dictionary_depth = dictionary_depth.saturating_add(1),
            TokenType::DictClose if dictionary_depth == 1 && array_depth == 0 => {
                return Some(tokenizer.position())
            }
            TokenType::DictClose if dictionary_depth > 1 => dictionary_depth -= 1,
            TokenType::ArrayOpen => array_depth = array_depth.saturating_add(1),
            TokenType::ArrayClose if array_depth > 0 => array_depth -= 1,
            TokenType::Eof => return None,
            _ => {}
        }
    }
}

fn classic_trailer_dictionary_end(bytes: &[u8], xref_start: usize) -> Option<usize> {
    let mut line_start = xref_start;
    while line_start < bytes.len() {
        let line_end = bytes[line_start..]
            .iter()
            .position(|byte| matches!(byte, b'\n' | b'\r'))
            .map_or(bytes.len(), |offset| line_start + offset);
        let first = bytes[line_start..line_end]
            .iter()
            .position(|byte| !is_pdf_space(*byte))
            .map(|offset| line_start + offset);
        if let Some(first) = first {
            let tail = &bytes[first..line_end];
            if tail.starts_with(b"trailer")
                && tail
                    .get(b"trailer".len())
                    .is_none_or(|byte| is_pdf_delimiter(*byte))
            {
                return trailer_dictionary_end(bytes, first + b"trailer".len());
            }
        } // cov:ignore: LLVM maps the non-trailer line arm's closing edge to the branch condition
        line_start = if line_end == bytes.len() {
            bytes.len()
        } else {
            line_end + 1
        };
        while bytes
            .get(line_start.saturating_sub(1))
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
            && bytes
                .get(line_start)
                .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            line_start += 1;
        }
    }
    None
}

/// Read only the current xref section from a canonical live source. Xref
/// streams need a small probe because their object body is read by the
/// resolver owner; classic tables grow until their trailer dictionary closes,
/// so trailing file bytes never become part of the xref-loading buffer.
fn read_live_xref_window(owner: &dyn CanonicalTrailerOwner, offset: u64) -> Result<Vec<u8>> {
    let source_length = owner.source_length()?;
    let available = source_length.saturating_sub(offset);
    if available == 0 {
        return Ok(Vec::new());
    }
    let available = usize::try_from(available)
        .map_err(|_| Error::parse(0, "xref source is too large for this target"))?;
    let mut target = available.min(LIVE_XREF_PROBE_SIZE);
    let mut bytes = Vec::with_capacity(target);
    loop {
        if bytes.len() < target {
            let more = read_live_source_range(
                owner,
                offset.saturating_add(bytes.len() as u64),
                target - bytes.len(),
            )?; // cov:ignore: LLVM does not attribute the tested growth-window read edge to this fallible expression
            bytes.extend_from_slice(&more);
        } // cov:ignore: LLVM maps the tested growth-window branch to the read condition
        let Some(xref_start) = classic_xref_start(&bytes) else {
            return Ok(bytes);
        };
        if let Some(end) = classic_trailer_dictionary_end(&bytes, xref_start) {
            let after_trailer = &bytes[end..];
            if let Some(line_end) = after_trailer
                .iter()
                .position(|byte| matches!(byte, b'\n' | b'\r'))
            {
                bytes.truncate(end + line_end + 1);
                return Ok(bytes);
            }
        }
        if target == available {
            return Ok(bytes);
        }
        target = target.saturating_add(LIVE_XREF_GROWTH_SIZE).min(available);
    }
}

/// Construct a `CanonicalTrailerOwner` for a byte buffer and load xref state
/// through it, mirroring `Pdf::open`'s own owner construction
/// (`crates/flpdf/src/engine.rs::Pdf::open_with_repair_mode_as`).
///
/// This is the byte-slice case of qpdf's `QPDF::processMemoryFile`
/// (`libqpdf/QPDF.cc:259-268`), which wraps the bytes in a `BufferInputSource`
/// and hands it to `QPDF::processInputSource`
/// (`libqpdf/QPDF.cc:271-275`: `m->file = source; parse(password);`) -- so
/// the document owns its input source before any parsing, exactly as
/// `QPDF::Members::Members` (`libqpdf/QPDF.cc:198`) leaves it. The owner is
/// what arms `QPDF::ParseGuard` (`include/qpdf/QPDF.hh:797-812`) through the
/// parser context `QPDF::readTrailer` passes as `this`
/// (`libqpdf/QPDF.cc:1317`; `libqpdf/QPDFParser.cc:34`).
///
/// `attempt_recovery` is set once on the owner at construction, the same
/// single source `Pdf::open_with_repair_mode_as` uses for `options.repair`
/// (`crates/flpdf/src/engine.rs:196-217`) -- qpdf has one
/// `m->attempt_recovery` bit, and xref loading reads it back from the owner
/// (`attempt_recovery()`, B26) rather than a copy carried through
/// `XrefLoadOptions`.
///
/// The owner is returned alongside the result, not inside `Ok`, because
/// handles in `LoadedXrefState` borrow their identity from it, and because a
/// failed load still leaves warnings on the owner for a caller to inspect,
/// the way `Pdf::open`'s own error arms read
/// `resolver.repair_diagnostics()` (`crates/flpdf/src/engine.rs:224-248`).
#[cfg(test)]
pub(crate) fn load_xref_state_through_canonical_owner<R: Read + Seek + 'static>(
    reader: R,
    allow_repair: bool,
    options: XrefLoadOptions,
    logger: crate::QPDFLogger,
    suppress_warnings: bool,
    pdf_unique_id: u64,
) -> (Rc<ResolverHandle<R>>, Result<LoadedXrefState>) {
    let warning_options = crate::reader::resolver::ResolverWarningOptions::new(
        logger,
        suppress_warnings,
        options.description.clone(),
    );
    let owner = ResolverHandle::new_shared(
        reader,
        0,
        BTreeMap::new(),
        allow_repair,
        false,
        Diagnostics::default(),
        warning_options,
        pdf_unique_id,
    );
    let loaded = load_xref_state_from_source(owner.as_ref(), options);
    (owner, loaded)
}

/// `Pdf::open` xref loading through qpdf's live input-source boundary. It
/// keeps bounded prefix/tail/xref windows instead of a complete snapshot.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_xref_state_from_source(
    owner: &dyn CanonicalTrailerOwner,
    options: XrefLoadOptions,
) -> Result<LoadedXrefState> {
    let physical_length = owner.source_length()?;
    let prefix_length = usize::try_from(physical_length.min(1024)).unwrap_or(1024);
    let prefix = match read_live_source_range(owner, 0, prefix_length) {
        Err(Error::QpdfExc(exception)) if exception.get_error_code() == QpdfErrorCode::System => {
            // qpdf's processFile C wrapper catches the initial FileInputSource
            // runtime error rather than a QPDFExc (`qpdf-c.cc:70-79`), so this
            // initial read retains its empty-location SystemBytes shape.
            let mut message = options.description.clone();
            message.extend_from_slice(b": read 1024 bytes");
            return Err(Error::SystemBytes(message));
        }
        other => other?,
    };
    let mut initial_diagnostics = Diagnostics::default();
    let (version, header_offset) = match find_qpdf_header(&prefix) {
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
    owner.set_header_offset(header_offset);
    deliver_canonical_diagnostics(owner, &mut initial_diagnostics)?;

    let logical_length = physical_length.saturating_sub(header_offset as u64);
    let tail_length = usize::try_from(logical_length.min(1054)).unwrap_or(1054);
    let tail_start = logical_length.saturating_sub(tail_length as u64);
    let tail = read_live_source_range(owner, tail_start, tail_length)?;
    let startxref = match parse_startxref(&tail) {
        Ok(offset) => offset,
        Err(error) if owner.attempt_recovery() => {
            // The canonical recovery scanner reads the same live source in
            // chunks. Keep no complete input snapshot merely because qpdf's
            // `read_xref` handoff failed at `startxref`.
            return load_xref_state_from_window(
                &[],
                0,
                version,
                header_offset,
                0,
                options,
                Diagnostics::default(),
                vec![error],
                owner,
            );
        }
        Err(error) => {
            deliver_canonical_diagnostics(owner, &mut initial_diagnostics)?;
            return Err(error);
        }
    };
    if startxref == 0 {
        // qpdf's `xref_offset == 0` guard skips `read_xref` entirely
        // (`QPDF.cc:450-452`). Do not put the direct reconstruction handoff
        // behind the speculative warning buffer: live candidate reads must
        // follow the already-delivered reconstruction trio.
        return load_xref_state_from_window(
            &[],
            0,
            version,
            header_offset,
            0,
            options,
            Diagnostics::default(),
            Vec::new(),
            owner,
        );
    }
    let xref_window = if startxref >= logical_length {
        Vec::new()
    } else {
        read_live_xref_window(owner, startxref)?
    };
    let first_non_space = xref_window.iter().position(|byte| !is_pdf_space(*byte));
    let starts_classic_xref = first_non_space
        .and_then(|pos| xref_window.get(pos..))
        .is_some_and(|tail| tail.starts_with(b"xref"));
    let looks_like_xref_stream = first_non_space
        .and_then(|pos| xref_window.get(pos..pos.saturating_add(1024)))
        .is_some_and(|window| {
            window
                .windows(b"/Type /XRef".len())
                .any(|bytes| bytes == b"/Type /XRef")
        });
    if owner.attempt_recovery() && !starts_classic_xref && !looks_like_xref_stream {
        // qpdf's outer parse does not make a speculative live object read when
        // the startxref bytes are visibly neither a classic table nor an xref
        // stream. Enter the one recovery path directly so its diagnostics are
        // emitted once, not once by the speculative attempt and once again by
        // the recovery retry.
        return load_xref_state_from_window(
            &xref_window,
            startxref,
            version,
            header_offset,
            startxref,
            options,
            initial_diagnostics,
            Vec::new(),
            owner,
        );
    }
    load_xref_state_from_window(
        &xref_window,
        startxref,
        version,
        header_offset,
        startxref,
        options,
        Diagnostics::default(),
        Vec::new(),
        owner,
    )
}

#[allow(clippy::too_many_arguments)]
fn load_xref_state_from_window(
    bytes: &[u8],
    source_base: u64,
    version: String,
    header_offset: usize,
    startxref: u64,
    options: XrefLoadOptions,
    mut initial_diagnostics: Diagnostics,
    mut parse_errors: Vec<Error>,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
) -> Result<LoadedXrefState> {
    let allow_repair = canonical_trailer_owner.attempt_recovery();
    let xref_pos = match startxref
        .checked_sub(source_base)
        .and_then(|offset| usize::try_from(offset).ok())
    {
        Some(xref_pos) => xref_pos,
        None if allow_repair => {
            parse_errors.push(Error::parse(0, "startxref does not fit source window"));
            0
        }
        None => return Err(Error::parse(0, "startxref does not fit source window")),
    };
    // qpdf's `m->deleted_objects` exists for the whole `QPDF` read, so every
    // reconstruction handoff below consults the same registration filter --
    // including the `startxref == 0` one, where no section has been read yet
    // and the filter is therefore still empty.
    let mut registration = XrefRegistration::default();
    // qpdf's `xref_offset == 0` check (`QPDF.cc:450-452`) throws
    // damagedPDF("can't find startxref") immediately and never calls
    // read_xref at all -- whether xref_offset is 0 because startxref itself
    // could not be parsed, or because a syntactically valid `startxref`
    // explicitly names offset 0. `parse_startxref`'s Ok(0) case (an explicit
    // zero) and its Err fallback (a missing/malformed startxref) both leave
    // `startxref == 0` here, and both hand straight to reconstruction in
    // repair mode, so no route reads a speculative xref at logical offset
    // zero.
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
            &registration.deleted_objects,
            options.clone(),
            initial_diagnostics,
            None,
            canonical_trailer_owner,
        )?;
        recovered.header_offset = header_offset;
        return Ok(recovered);
    }

    let mut observed_first_xref_item_offset = None;
    // No caller-local diagnostic sink: the owner is the single warning sink
    // for this read, matching qpdf's one `m->warnings`
    // (`libqpdf/QPDF.cc:487-494`). The initial section's own warnings are
    // already delivered through `deliver_canonical_diagnostics` inside the
    // parse, so there is nothing left to forward before reconstruction.
    let mut loaded = match parse_xref_from_start_with_owner(
        bytes,
        xref_pos,
        source_base,
        startxref,
        &version,
        options.clone(),
        &mut registration,
        None,
        Some(&mut observed_first_xref_item_offset),
        true,
        canonical_trailer_owner,
    ) {
        Ok(loaded) => loaded,
        Err(error) if allow_repair => {
            // Report the first recorded failure; this parse error is only the
            // trigger when the startxref stage itself succeeded.
            let trigger = parse_errors.into_iter().next().unwrap_or(error);
            deliver_canonical_diagnostics(canonical_trailer_owner, &mut initial_diagnostics)?; // cov:ignore: this branch only propagates a canonical warning-sink failure from a failed xref parse; the sink boundary is covered by Pdf open failure tests
            let mut recovered = recover_xref_from_linear_scan(
                bytes,
                version,
                startxref,
                trigger,
                None,
                Some(&registration.entries),
                Some(&registration.raw_entries),
                &registration.deleted_objects,
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
    // `initial_diagnostics` is always empty by this point: the header-check
    // warning it can carry (pushed above) is delivered through
    // `deliver_canonical_diagnostics` before the first `load_xref_state_from_window`
    // call in every production caller, and a delivery failure there returns
    // early via `?` without ever reaching this branch. Verified empirically
    // (2026-09-20, flpdf-po4te): a hard assertion here never fired across the
    // full workspace test suite, all 589 qpdf qtest fixtures, and all 177
    // tests/fixtures/compat/*.pdf fixtures under both `--check` and
    // `--qdf --object-streams=generate`.
    debug_assert!(initial_diagnostics.entries().is_empty());
    deliver_canonical_diagnostics(
        canonical_trailer_owner,
        &mut loaded.loaded.repair_diagnostics,
    )?; // cov:ignore: this is the defensive logger-failure edge after a successful initial xref parse; the same live sink is covered at the Pdf open boundary

    let mut previous_parse_diagnostics = Diagnostics::default();
    if let Err(error) = merge_previous_xref_sections_with_observer(
        bytes,
        source_base,
        &version,
        &mut loaded,
        options.clone(),
        &mut registration,
        Some(&mut previous_parse_diagnostics),
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
                &deleted_objects,
                options.clone(),
                previous_parse_diagnostics,
                observed_first_xref_item_offset,
                canonical_trailer_owner,
            )?;
            let mut recovered = merge_recovered_qpdf_state(recovered, loaded);
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
    // Keep `/Size` in the xref-loading responsibility boundary: the route
    // resolves it through the document's own resolver, which already owns the
    // active xref table.
    let (resolved_size, size_reconstruction_trigger) = {
        canonical_trailer_owner.install_xref_entries(registration.snapshot());
        let mut context =
            CanonicalXrefContext::new(canonical_trailer_owner, options.description.clone());
        let value = context.resolve_dictionary_value(&loaded.loaded.trailer, "Size");
        let trigger = context.take_reconstruction_trigger();
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
        let deleted_objects = std::mem::take(&mut registration.deleted_objects);
        let recovered = recover_xref_from_linear_scan(
            bytes,
            version.clone(),
            startxref,
            error,
            Some(&loaded.loaded.trailer),
            None, // cov:ignore: an existing fallback trailer suppresses candidate re-entry, so no prior candidate state is consumed here
            None, // cov:ignore: no prior raw registration is consumed here
            &deleted_objects,
            options.clone(),
            diagnostics,
            None,
            canonical_trailer_owner,
        )?; // cov:ignore: recover_xref_entries has no fallible branch; retain defensive propagation
        let mut recovered = merge_recovered_qpdf_state(recovered, loaded);
        recovered.header_offset = header_offset;

        // qpdf continues the original read_xref call after
        // readObjectAtOffset(true, ...) reconstructs the table: its
        // m->trailer.getKey("/Size").getIntValueAsInt() at QPDF.cc:689
        // therefore resolves the value against the newly reconstructed xref
        // before the :697-704 size consistency warning. Re-run that one
        // post-reconstruction lookup through the same owner that read the
        // recovered xref table -- the same resolver that read it.
        let (recovered_size, recovered_size_diagnostics) = {
            canonical_trailer_owner.install_xref_entries(recovered.loaded.entries.clone());
            let mut context =
                CanonicalXrefContext::new(canonical_trailer_owner, options.description.clone());
            let value = context.resolve_dictionary_value(&recovered.loaded.trailer, "Size");
            let mut diagnostics = Diagnostics::default();
            context.append_diagnostics_to(&mut diagnostics);
            (value, diagnostics)
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

    let discarded_generations = discard_lower_generations(
        &mut loaded.raw_entries,
        &mut loaded.loaded.entries,
        &mut loaded.parsed_xref_streams,
    );
    canonical_trailer_owner.discard_cached_generations(&discarded_generations);
    discard_trailer_references(&mut loaded.trailer_references, &discarded_generations);
    loaded.header_offset = header_offset;
    Ok(loaded)
}

#[allow(clippy::too_many_arguments)]
fn parse_xref_from_start_with_owner(
    bytes: &[u8],
    xref_pos: usize,
    source_base: u64,
    startxref: u64,
    version: &str,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    first_xref_item_offset_sink: Option<&mut Option<u64>>,
    validate_current_classic_trailer: bool,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
) -> Result<LoadedXrefState> {
    parse_xref_from_start_with_owner_and_build_diagnostics(
        bytes,
        xref_pos,
        source_base,
        startxref,
        version,
        options,
        registration,
        error_diagnostics_sink,
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
    source_base: u64,
    startxref: u64,
    version: &str,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    first_xref_item_offset_sink: Option<&mut Option<u64>>,
    validate_current_classic_trailer: bool,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
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
        let base = usize::try_from(source_base).unwrap_or(usize::MAX);
        let mut cursor = ByteCursor::with_base(
            bytes,
            base,
            base.saturating_add(xref_pos).saturating_add(skip),
        );
        let table = parse_xref_table(
            &mut cursor,
            bytes,
            first_xref_item_offset_sink,
            &options.description,
        );
        let (entries, trailer_start, mut table_diagnostics, first_xref_item_offset) = match table {
            Ok(table) => table,
            Err(error) => {
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
        // The initial classic subsection is already known when qpdf calls
        // readTrailer, so parser-created indirect children belong to the
        // document's one obj_cache.
        canonical_trailer_owner.install_xref_entries(registration.snapshot());
        let (trailer, trailer_parser_diagnostics) = {
            let mut trailer_parser =
                CanonicalTrailerParser::new(canonical_trailer_owner, &options.description);
            read_trailer(
                bytes,
                trailer_start,
                base,
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
            for diagnostic in &trailer_diags {
                canonical_trailer_owner.push_warning(diagnostic.clone())?;
            }
            return Err(Error::QpdfExc(QpdfExc::new(
                QpdfErrorCode::DamagedPdf,
                &options.description,
                b"",
                i64::try_from(trailer_start).unwrap_or(i64::MAX),
                b"expected trailer dictionary",
            )));
        }
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
            trailer_references,
            parsed_xref_streams: BTreeMap::new(),
            header_offset: 0,
            // A classic xref table has no type-2 entries at all.
            uncompressed_after_compressed: false,
        };
        for diagnostic in trailer_diags {
            loaded.loaded.repair_diagnostics.push(diagnostic);
        }
        deliver_canonical_diagnostics(
            canonical_trailer_owner,
            &mut loaded.loaded.repair_diagnostics,
        )?; // cov:ignore: this only propagates an injected logger failure after classic trailer parsing; the live sink is covered at the Pdf open boundary
        if validate_current_classic_trailer {
            {
                let mut context =
                    CanonicalXrefContext::new(canonical_trailer_owner, options.description.clone());
                let validation =
                    validate_classic_trailer(&mut context, &loaded.loaded.trailer, trailer_start);
                context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
                validation
            }?;
            deliver_canonical_diagnostics(
                canonical_trailer_owner,
                &mut loaded.loaded.repair_diagnostics,
            )?; // cov:ignore: this is the defensive logger-failure edge after classic trailer validation; the sink boundary is covered by Pdf open failure tests
        }
        merge_xref_stream_from_classic_trailer_with_build_diagnostics(
            xref_pos,
            &mut loaded,
            options.clone(),
            registration,
            error_diagnostics_sink,
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

    let absolute_xref_pos = source_base
        .saturating_add(xref_pos as u64)
        .try_into()
        .unwrap_or(usize::MAX);
    match parse_xref_stream(
        absolute_xref_pos,
        startxref,
        version.to_string(),
        options.clone(),
        registration,
        canonical_trailer_owner,
    ) {
        Ok(state) => Ok(state),
        Err(failure) => Err(failure.error),
    }
}

/// Validate the first classic trailer exactly where qpdf's
/// `QPDF::read_xrefTable` does (`QPDF.cc:902-912`). The trailer offset is the
/// position immediately after the `trailer` keyword, which is the location
/// `QPDF::readTrailer` restores on its `InputSource` before constructing the
/// `QPDFExc` (`QPDF.cc:1313-1327`).
fn validate_classic_trailer(
    context: &mut dyn XrefObjectContext,
    trailer: &ObjectHandle,
    trailer_offset: usize,
) -> Result<()> {
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
    if !is_integer? {
        return Err(Error::parse(
            trailer_offset,
            "/Size key in trailer dictionary is not an integer",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn merge_xref_stream_from_classic_trailer_with_build_diagnostics(
    classic_xref_pos: usize,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    mut error_diagnostics_sink: Option<&mut Diagnostics>,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
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

    let xref_stream_value = {
        let mut context =
            CanonicalXrefContext::new(canonical_trailer_owner, options.description.clone());
        let value = context.resolve_dictionary_value(&loaded.loaded.trailer, "XRefStm");
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
    let hybrid = match parse_xref_stream(
        xref_stream_pos,
        xref_stream_pos as u64,
        loaded.loaded.version.clone(),
        options.clone(),
        registration,
        canonical_trailer_owner,
    ) {
        Ok(hybrid) => hybrid,
        Err(failure) => {
            if let Some(sink) = error_diagnostics_sink.as_mut() {
                for diagnostic in loaded.loaded.repair_diagnostics.entries() {
                    sink.push(diagnostic.clone());
                }
            }
            return Err(failure.error);
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
    loaded.uncompressed_after_compressed |= hybrid.uncompressed_after_compressed;
    loaded
        .trailer_references
        .extend(hybrid.trailer_references.iter().copied());
    loaded
        .parsed_xref_streams
        .extend(hybrid.parsed_xref_streams);

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
    source_base: u64,
    version: &str,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
) -> Result<()> {
    merge_previous_xref_sections_with_observer(
        bytes,
        source_base,
        version,
        loaded,
        options.clone(),
        registration,
        error_diagnostics_sink,
        None,
        canonical_trailer_owner,
    )
}

#[allow(clippy::too_many_arguments)]
fn merge_previous_xref_sections_with_observer(
    bytes: &[u8],
    source_base: u64,
    version: &str,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    mut error_diagnostics_sink: Option<&mut Diagnostics>,
    mut first_xref_item_offset_sink: Option<&mut Option<u64>>,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
) -> Result<()> {
    let mut visited = HashSet::new();
    if loaded.loaded.startxref != 0 {
        visited.insert(loaded.loaded.startxref);
    }
    let previous_offset_result = resolve_previous_xref_offset(
        options.clone(),
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
        // A canonical xref window starts at the newest section. For an older
        // `/Prev`, fetch only that section from the same live source rather
        // than falling back to a complete input snapshot.
        let source_window_end = source_base.saturating_add(bytes.len() as u64);
        let live_previous_window = if offset < source_base || offset >= source_window_end {
            Some(read_live_xref_window(canonical_trailer_owner, offset)?)
        } else {
            None
        };
        let previous_bytes = live_previous_window.as_deref().unwrap_or(bytes);
        let previous_source_base = live_previous_window
            .as_ref()
            .map_or(source_base, |_| offset);
        let previous_pos = if live_previous_window.is_some() {
            0
        } else {
            usize::try_from(offset.saturating_sub(previous_source_base))
                .map_err(|_| Error::parse(0, "xref /Prev does not fit usize"))?
        };

        if !visited.insert(offset) {
            return Err(Error::parse(0, "loop detected following xref tables"));
        }

        let mut previous_error_diagnostics = Diagnostics::default();
        let mut previous_build_diagnostics = Diagnostics::default();
        let previous_result = parse_xref_from_start_with_owner_and_build_diagnostics(
            previous_bytes,
            previous_pos,
            previous_source_base,
            offset,
            version,
            options.clone(),
            registration,
            Some(&mut previous_error_diagnostics),
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
                let classic_section = previous_bytes
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
        loaded.uncompressed_after_compressed |= previous.uncompressed_after_compressed;
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
            options.clone(),
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
    options: XrefLoadOptions,
    trailer: &ObjectHandle,
    trailer_offset: Option<usize>,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
) -> Result<(Option<u64>, Diagnostics, Option<Error>)> {
    let mut context =
        CanonicalXrefContext::new(canonical_trailer_owner, options.description.clone());
    resolve_previous_xref_offset_with_context(&mut context, trailer, trailer_offset)
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
    deleted_objects: &BTreeSet<u32>,
    options: XrefLoadOptions,
    mut repair_diagnostics: Diagnostics,
    observed_first_xref_item_offset: Option<u64>,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
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

    let recovered = recover_xref_entries_from_source(
        canonical_trailer_owner,
        fallback_trailer.is_none(),
        &options.description,
        deleted_objects,
    )
    .map_err(|error| {
        // cov:ignore-start: defensive open-failure wrapper after a line-scan parser error; the live sink boundary is covered by Pdf open failure tests
        with_xref_open_diagnostics(error, canonical_trailer_owner)
    })?;
    // cov:ignore-end
    // qpdf's third `insertReconstructedXrefEntry` condition
    // (`QPDF.cc:1204-1209`): a scanned row whose object number a free row
    // already registered is never written to `m->xref_table`, and
    // `reconstruct_xref` clears that filter only after the scan completes
    // (`QPDF.cc:575`). `recover_xref_entries_from_source` now applies this
    // suppression per row, inside the scan itself (via
    // `insert_reconstructed_xref_entry`), which is qpdf's own call site for
    // it; the rows in `recovered.entries` already reflect it.
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

    // qpdf's `reconstruct_xref` (`QPDF.cc:564-616`) gates BOTH its `trailer`
    // keyword scan (`!m->trailer.isInitialized() && t1.isWord("trailer")`)
    // and its `/Type /XRef` candidate search (`if
    // (!m->trailer.isInitialized())`) on the trailer not already being
    // known. `fallback_trailer` -- the trailer from a successfully parsed
    // newest revision whose `/Prev` chain later broke -- models exactly
    // that already-initialized state: qpdf never looks at a stray candidate
    // elsewhere in the file when the correct trailer is already in hand, so
    // neither the scanner's trailer capture nor the candidate search runs at
    // all in that case. `startxref` (the position that produced
    // `fallback_trailer`)
    // is already valid then too, so it needs no adjustment; it is only
    // rewritten to the candidate's own verified re-entry point when the
    // candidate path is what actually recovered the trailer. `last_xref_form`
    // is left as a placeholder (`Table`) in the `fallback_trailer` case: the
    // caller (`load_xref_state_from_window`) always overwrites it via
    // `merge_recovered_qpdf_state` with the already-successfully-parsed
    // revision's own real form once this returns.
    let mut candidate_xref_reentered = false;
    let (
        trailer,
        recovered_startxref,
        recovered_form,
        recovered_first_xref_item_offset,
        recovered_uncompressed_after_compressed,
    ) = if let Some(trailer) = fallback_trailer {
        (trailer.clone(), startxref, XrefForm::Table, 0, false)
    } else {
        match recovered.trailer {
            Some(trailer) => (trailer, startxref, XrefForm::Table, 0, false),
            None => match recover_trailer_from_xref_stream_candidate(
                bytes,
                &version,
                options.clone(),
                &mut entries,
                &mut parsed_xref_streams,
                &mut repair_diagnostics,
                &mut extra_trailer_references,
                preexisting_raw_entries,
                canonical_trailer_owner,
            ) {
                Ok((
                    trailer,
                    max_offset,
                    form,
                    _deleted_objects,
                    first_xref_item_offset,
                    uncompressed_after_compressed,
                )) => {
                    // Candidate re-entry has already consumed its local
                    // tombstones while filtering `entries`; never retain
                    // them past this recovery operation.
                    candidate_xref_reentered = true;
                    (
                        trailer,
                        max_offset,
                        form,
                        first_xref_item_offset,
                        uncompressed_after_compressed,
                    )
                }
                Err(candidate_error) => {
                    return Err(with_xref_open_diagnostics(
                        candidate_error,
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
    canonical_trailer_owner.install_xref_entries(entries.clone());
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
        let discarded_generations =
            discard_lower_generations(&mut raw_entries, &mut entries, &mut parsed_xref_streams);
        canonical_trailer_owner.discard_cached_generations(&discarded_generations);
        discard_trailer_references(&mut trailer_references, &discarded_generations);
    }
    canonical_trailer_owner.set_reconstructed_xref();
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
        trailer_references,
        parsed_xref_streams,
        header_offset: 0,
        uncompressed_after_compressed: recovered_uncompressed_after_compressed,
    })
}

fn merge_recovered_qpdf_state(
    mut recovered: LoadedXrefState,
    mut accumulated: LoadedXrefState,
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
    recovered.uncompressed_after_compressed |= accumulated.uncompressed_after_compressed;
    recovered.classic_trailer_offset = accumulated
        .classic_trailer_offset
        .or(recovered.classic_trailer_offset);
    // The reconstruction scan's own `insertReconstructedXrefEntry` filter was
    // already applied inside `recover_xref_from_linear_scan`, which is where
    // qpdf applies it (`QPDF.cc:1197-1210`). Rows that were already in
    // `m->xref_table` when reconstruction started survive regardless of that
    // filter -- qpdf never retroactively erases them -- so this merge only
    // carries them forward.
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

/// qpdf's `QPDF::insertReconstructedXrefEntry` overwrite step
/// (`libqpdf/QPDF.cc:1197-1210`), applied to a line-scan match whose
/// `obj > 0 && 0 <= gen < 65535` guard (`QPDF.cc:1199-1202`) has already been
/// enforced by [`scan_object_header_after_first_token`]'s own explicit range
/// check before it constructs the `ObjectRef` this function receives. The
/// remaining behavior -- the last occurrence in the file wins, unless the
/// object's plain number
/// is registered in `deleted_objects` -- is shared verbatim by every caller
/// of this scan, whether it runs at document-open time (a real,
/// possibly-nonempty `deleted_objects` inherited from the xref parse that
/// just failed) or at resolve time ([`recover_xref_entries`] always passes
/// an empty set here, because qpdf's own `m->deleted_objects` is guaranteed
/// clear by the time a resolved document can retry `readObjectAtOffset`;
/// see that function's doc comment).
fn insert_reconstructed_xref_entry(
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    object_ref: ObjectRef,
    offset: u64,
    deleted_objects: &BTreeSet<u32>,
) {
    if !deleted_objects.contains(&object_ref.number) {
        entries.insert(object_ref, XrefEntry::Uncompressed { offset });
    }
}

/// Replay only the object-offset half of qpdf's `reconstruct_xref` line scan
/// over a byte buffer, for the resolver's own xref rescan.
///
/// The trailer half of `reconstruct_xref` (`QPDF.cc:564-575`) needs the
/// document's parser context to build handles, so it lives on the canonical
/// live-source scanner [`recover_xref_entries_from_source`] instead. qpdf
/// reaches both halves from the same function; this buffer entry point is the
/// one `ResolverHandle` uses when it re-scans a source whose trailer it
/// already holds.
///
/// qpdf's `m->deleted_objects` is populated only while a normal xref
/// table/stream chain is being registered (`insertFreeXrefEntry`,
/// `QPDF.cc:1186-1192`) and is cleared once that registration finishes
/// (`QPDF.cc:686-708`). The resolve-time retry this function serves
/// (`QPDF::readObjectAtOffset`'s catch at `QPDF.cc:1614-1637`) runs strictly
/// after the document's own open-time xref registration has already
/// completed and cleared it, and performs no xref registration of its own
/// before reaching this scan, so `m->deleted_objects` is always empty here
/// -- this passes that empty set explicitly via
/// [`insert_reconstructed_xref_entry`] rather than omitting the check.
pub(crate) fn recover_xref_entries(bytes: &[u8]) -> Result<BTreeMap<ObjectRef, XrefEntry>> {
    let no_deleted_objects = BTreeSet::new();
    let mut entries = BTreeMap::new();
    let mut line_start = 0usize;
    while line_start < bytes.len() {
        let next_line_start = next_line_start(bytes, line_start);
        if let Some(first_token) = read_scan_token(bytes, line_start, next_line_start) {
            if let Some((object_ref, offset)) =
                scan_object_header_after_first_token(bytes, &first_token)?
            {
                insert_reconstructed_xref_entry(
                    &mut entries,
                    object_ref,
                    offset,
                    &no_deleted_objects,
                );
            }
        }
        line_start = next_line_start;
    }
    Ok(entries)
}

/// Applies qpdf's `insertReconstructedXrefEntry` suppression
/// (`libqpdf/QPDF.cc:1204-1209`) via [`insert_reconstructed_xref_entry`]
/// during the scan itself, using the caller's `deleted_objects` snapshot --
/// qpdf's `m->deleted_objects` exists for the whole `QPDF` read (see
/// `load_xref_state_from_window`'s doc comment), so `reconstruct_xref`'s
/// scan (`QPDF.cc:549-574`) consults whatever that set already holds from
/// an earlier, incompletely registered xref table/stream, without touching
/// it itself.
fn recover_xref_entries_from_source(
    owner: &dyn CanonicalTrailerOwner,
    capture_trailer: bool,
    filename: &[u8],
    deleted_objects: &BTreeSet<u32>,
) -> Result<RecoveredXref> {
    let source_length = owner.source_length()?;
    owner.source_seek(0)?;
    let mut entries = BTreeMap::new();
    let mut trailer = None;
    let mut trailer_diagnostics = Vec::new();
    let mut line = Vec::new();
    let mut line_start = 0u64;
    let mut position = 0u64;
    let mut chunk = [0u8; 8192];

    let mut process_line = |line: &[u8], line_start: u64, next_line_start: u64| {
        let Some(first_token) = read_scan_token(line, 0, line.len()) else {
            return Ok::<(), Error>(());
        };
        if capture_trailer && trailer.is_none() && first_token.is_word_value(b"trailer") {
            let trailer_start = line_start.saturating_add(first_token.end as u64);
            let remaining = source_length.saturating_sub(trailer_start);
            let window_length = usize::try_from(remaining.min(64 * 1024)).unwrap_or(64 * 1024);
            let window = read_live_source_range(owner, trailer_start, window_length)?;
            let result = {
                let mut resolver = CanonicalTrailerParser::new(owner, filename);
                read_trailer(
                    &window,
                    trailer_start as usize,
                    trailer_start as usize,
                    filename,
                    &mut resolver,
                )
            };
            if let Ok((candidate, diagnostics)) = result {
                // qpdf's reconstruct_xref emits parser warnings even when
                // readTrailer returns a non-dictionary candidate
                // (`QPDF.cc:565-568`). The candidate is discarded, but the
                // warning side effects remain on the document.
                trailer_diagnostics.extend(diagnostics);
                if candidate.try_is_dictionary().unwrap_or(false) {
                    trailer = Some(candidate);
                }
            } // cov:ignore: LLVM maps the successful trailer-candidate edge to the inner dictionary branch
            owner.source_seek(next_line_start)?;
        } else if let Some((object_ref, offset)) =
            scan_object_header_after_first_token(line, &first_token)?
        {
            insert_reconstructed_xref_entry(
                &mut entries,
                object_ref,
                line_start.saturating_add(offset),
                deleted_objects,
            );
        }
        Ok(())
    };

    loop {
        owner.source_seek(position)?;
        let read = owner.source_read(&mut chunk)?;
        if read == 0 {
            break;
        }
        for &byte in &chunk[..read] {
            position = position.saturating_add(1);
            if matches!(byte, b'\n' | b'\r') {
                process_line(&line, line_start, position)?;
                line.clear();
                line_start = position;
            } else {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        process_line(&line, line_start, position)?;
    }

    Ok(RecoveredXref {
        entries,
        trailer,
        trailer_diagnostics,
    })
}

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
type RecoveredXrefStream = (ObjectHandle, u64, XrefForm, BTreeSet<u32>, u64, bool);

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
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
) -> Result<RecoveredXrefStream> {
    let (candidate, discovery_diagnostics) =
        find_xref_stream_trailer_candidate(entries, options.clone(), canonical_trailer_owner);
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
    let reentry_result = parse_xref_from_start_with_owner(
        bytes,
        max_offset as usize,
        0,
        max_offset,
        version,
        options.clone(),
        &mut reentry_registration,
        Some(&mut reentry_error_diagnostics),
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
        0,
        version,
        &mut reentry,
        options.clone(),
        &mut reentry_registration,
        Some(&mut previous_failure_diagnostics),
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
    let uncompressed_after_compressed = reentry.uncompressed_after_compressed;
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
    // re-entering the candidate.
    let resolved_size = {
        canonical_trailer_owner.install_xref_entries(entries.clone());
        let mut context =
            CanonicalXrefContext::new(canonical_trailer_owner, options.description.clone());
        let value = context.resolve_dictionary_value(&candidate.trailer, "Size");
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
        uncompressed_after_compressed,
    ))
}

/// The `/Type /XRef` candidate this file's line-scanned entries point at:
/// its dictionary (which may or may not be the winning trailer -- see
/// [`find_xref_stream_trailer_candidate`]'s doc) and its true maximum
/// offset (the re-entry point).
struct XrefStreamCandidate {
    trailer: ObjectHandle,
    max_offset: u64,
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
fn find_xref_stream_trailer_candidate(
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
    });
    (candidate, diagnostics)
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
fn trailer_description_template(filename: &[u8]) -> Rc<Vec<u8>> {
    let mut description = filename.to_vec();
    description.extend_from_slice(b", trailer at offset $PO");
    Rc::new(description)
}

fn read_trailer(
    input: &[u8],
    start: usize,
    base: usize,
    filename: &[u8],
    resolver: &mut dyn HandleResolver,
) -> Result<(ObjectHandle, Vec<QpdfExc>)> {
    let local_start = start.checked_sub(base).unwrap_or(input.len());
    let slice = input
        .get(local_start..)
        .ok_or_else(|| Error::parse(start, "trailer is not a dictionary"))?;
    let parsed = parse_qpdf_file_object_handle_with_diagnostics(
        slice,
        i64::try_from(start).unwrap_or(i64::MAX),
        None,
        resolver,
    )
    .map_err(|error| error.rebase_offset(start))?;
    let trailer = parsed.value;
    // QPDFParser is constructed with the literal description "trailer". The
    // value keeps that source boundary for later accessor warnings, which
    // qpdf renders as `input, trailer at offset N`. Attach the equivalent live
    // template before writer/encryption consumers traverse the trailer.
    trailer.set_shared_description(
        trailer_description_template(filename),
        i64::try_from(start).unwrap_or(i64::MAX),
    );

    let mut diagnostics = trailer_diagnostics(start, parsed.diagnostics, filename, Some(slice));
    if let Some(empty_offset) = parsed.empty_offset {
        diagnostics.push(trailer_warning(
            filename,
            "empty object treated as null",
            Some(start.saturating_add(empty_offset) as u64),
        ));
    } else if trailer.try_is_dictionary()? {
        let mut tokenizer = Tokenizer::new(slice);
        tokenizer
            .set_position(parsed.next_offset)
            .map_err(|error| error.rebase_offset(start))?;
        // `QPDF::readToken(m->file)` (`libqpdf/QPDF.cc:1322`), via the
        // shared `Tokenizer::read_qpdf_token` entrypoint.
        let token = tokenizer
            .read_qpdf_token(0)
            .map_err(|error| error.rebase_offset(start))?;
        if token.is_word_value(b"stream") {
            diagnostics.push(trailer_warning(
                filename,
                "stream keyword found in trailer",
                Some(start.saturating_add(tokenizer.position()) as u64),
            ));
        }
    }
    Ok((trailer, diagnostics))
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
            let message = if diagnostic.message.starts_with(b"invalid character (") {
                source
                    .and_then(|source| invalid_hex_byte(source, diagnostic.relative_offset))
                    .map(|byte| {
                        let mut message = b"invalid character (".to_vec();
                        message.push(byte);
                        message.extend_from_slice(b") in hexstring");
                        message
                    })
                    .unwrap_or(diagnostic.message)
            } else {
                diagnostic.message
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
    tokenizer.set_position(from).ok()?;
    // `QPDF::readToken(m->file, MAX_LEN)` (`libqpdf/QPDF.cc:553,558,559`,
    // `MAX_LEN = 100` at `:548`), via the shared `Tokenizer::read_qpdf_token`
    // entrypoint.
    let token = tokenizer
        .read_qpdf_token(XREF_RECONSTRUCTION_MAX_TOKEN_LEN)
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
    // qpdf's `while (!done)` loop always opens with a subsection header read;
    // the `trailer` keyword is only ever recognised by the lookahead that
    // closes a subsection (`libqpdf/QPDF.cc:851-890`). A table with no
    // subsection at all therefore fails as `xref syntax invalid` rather than
    // being accepted as empty.
    let mut done = false;
    while !done {
        let header_start = cursor.pos;
        let header = cursor.read_bytes(50);
        let (first, count, header_bytes) =
            parse_xref_first_line_with_bytes(&header).ok_or_else(|| {
                Error::QpdfExc(QpdfExc::new(
                    QpdfErrorCode::DamagedPdf,
                    filename,
                    b"xref table",
                    i64::try_from(header_start).unwrap_or(i64::MAX),
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

        // `QPDF::readToken` is fixed at `allow_bad = true`, so a `tt_bad`
        // token here is a value the lookahead simply fails to match rather
        // than an error: qpdf rewinds to the saved position and re-reads the
        // bytes as the next subsection header (`libqpdf/QPDF.cc:886-891`).
        let lookahead_start = cursor.pos;
        if cursor.read_token()?.is_word_value(b"trailer") {
            done = true;
        } else {
            cursor.pos = lookahead_start;
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

/// An xref-stream read's terminal error, boxed for a large `Err` arm.
///
/// A prior revision paired this with a `diagnostics: Diagnostics` field for
/// a caller that buffers a speculative section to order diagnostics itself,
/// but no code path ever constructed it non-empty (verified empirically,
/// 2026-09-20, flpdf-po4te): every consumer of a failed `parse_xref_stream`
/// call only ever iterated over an empty buffer. Removed rather than left as
/// unreachable plumbing.
#[derive(Debug)]
struct XrefStreamFailure {
    error: Error,
}

impl XrefStreamFailure {
    fn new(error: Error) -> Box<Self> {
        Box::new(Self { error })
    }
}

fn parse_xref_stream(
    xref_pos: usize,
    startxref: u64,
    version: String,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    canonical_trailer_owner: &dyn CanonicalTrailerOwner,
) -> std::result::Result<LoadedXrefState, Box<XrefStreamFailure>> {
    // qpdf's `read_xrefStream` wraps its whole body in
    // `if (!m->ignore_xref_streams)` and otherwise falls straight through to
    // `throw damagedPDF("", xref_offset, "xref not found")` — the offset is
    // never read, so this precedes the owner's own read. The same error is
    // what a non-stream object at the offset produces.
    if options.ignore_xref_streams {
        return Err(XrefStreamFailure::new(Error::parse(
            xref_pos,
            "xref not found",
        )));
    }
    parse_xref_stream_with_canonical_owner(
        xref_pos,
        startxref,
        version,
        options,
        registration,
        canonical_trailer_owner,
    )
    .map_err(XrefStreamFailure::new)
}

type XrefWidths = (usize, usize, usize);

type XrefStreamBuild = (
    ObjectHandle,
    Vec<ParsedXrefEntry>,
    BTreeSet<ObjectRef>,
    bool,
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
    // `ensure_source_for_resolution` is a no-op for the canonical-owner
    // context: its resolver reads indirect `/Filter`/`/DecodeParms` values
    // live through `get_stream_data`.
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
    let (entries, uncompressed_after_compressed) = parse_xref_entries(
        &mut cursor,
        &ranges,
        widths,
        stream_data_offset,
        registration,
    )?;
    let trailer_references = collect_trailer_references(&trailer);

    Ok((
        trailer,
        entries,
        trailer_references,
        has_first_xref_item,
        uncompressed_after_compressed,
    ))
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
        match owner.read_xref_stream_at_offset(startxref, Some(b"xref stream".to_vec())) {
            Ok(read) => read,
            Err(error) => {
                let mut diagnostics = Diagnostics::default();
                context.append_diagnostics_to(&mut diagnostics);
                deliver_canonical_diagnostics(owner, &mut diagnostics)?;
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
    deliver_canonical_diagnostics(owner, &mut diagnostics)?;
    let (trailer, entries, trailer_references, has_first_xref_item, uncompressed_after_compressed) =
        match build_result {
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
        trailer_references,
        // Keep the stream handle as qpdf obj_cache provenance. The final Pdf
        // constructor skips effective xref rows, but marks historical/free
        // rows as non-live while retaining them in the complete canonical cache view.
        parsed_xref_streams: BTreeMap::from([(object_ref, handle_object)]),
        header_offset: 0,
        uncompressed_after_compressed,
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
) -> Result<(Vec<ParsedXrefEntry>, bool)> {
    let (w0, w1, w2) = widths;
    let entry_width = w0 + w1 + w2;
    if entry_width == 0 {
        return Err(Error::parse(0, "invalid cross-reference stream widths"));
    }

    let mut entries = Vec::new();
    // qpdf's `saw_first_compressed_object` (`QPDF.cc:1070`): local to one
    // xref stream section, feeding the sticky `m->uncompressed_after_compressed`
    // the caller ORs into the document-wide flag.
    let mut saw_first_compressed_object = false;
    let mut uncompressed_after_compressed = false;
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

            // qpdf's numbering check (`QPDF.cc:1110-1116`): once a type-2
            // (compressed) entry has been seen in this section, any later
            // entry whose type is not 2 trips the sticky document-wide flag.
            if saw_first_compressed_object {
                if object_type != 2 {
                    uncompressed_after_compressed = true;
                }
            } else if object_type == 2 {
                saw_first_compressed_object = true;
            }

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

    Ok((entries, uncompressed_after_compressed))
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
    base: usize,
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(bytes: &'a [u8], pos: usize) -> Self {
        Self::with_base(bytes, 0, pos)
    }

    fn with_base(bytes: &'a [u8], base: usize, pos: usize) -> Self {
        Self { bytes, base, pos }
    }

    fn local_pos(&self) -> Option<usize> {
        self.pos.checked_sub(self.base)
    }

    /// `QPDF::readToken(input, max_len = 0)` (`libqpdf/QPDF.cc:1535-1539`),
    /// via the shared [`Tokenizer::read_qpdf_token`] entrypoint. Bad tokens
    /// are therefore returned to the caller as ordinary values, never
    /// raised. Only the resulting token's positions are rebased by `self`;
    /// the read itself is qpdf's `QPDF::readToken`, unmodified.
    fn read_token(&mut self) -> Result<Token> {
        let mut tokenizer = Tokenizer::new(self.bytes);
        tokenizer.set_position(self.local_pos().unwrap_or(self.bytes.len()))?;
        let mut token = tokenizer.read_qpdf_token(0)?;
        self.pos = self.base.saturating_add(tokenizer.position());
        token.start = token.start.saturating_add(self.base);
        token.end = token.end.saturating_add(self.base);
        token.error_offset = token.error_offset.saturating_add(self.base);
        Ok(token)
    }

    fn read_be_u64(&mut self, width: usize) -> Result<u64> {
        let Some(local) = self.local_pos() else {
            return Err(Error::parse(self.pos, "unexpected end of stream field"));
        };
        if local + width > self.bytes.len() {
            return Err(Error::parse(self.pos, "unexpected end of stream field"));
        }

        let mut value = 0u64;
        for index in local..local + width {
            value = (value << 8) | u64::from(self.bytes[index]);
        }
        self.pos = self.pos.saturating_add(width);
        Ok(value)
    }

    fn read_line(&mut self, max_len: usize) -> Vec<u8> {
        let local_start = self.local_pos().unwrap_or(self.bytes.len());
        let mut end = local_start;
        while end < self.bytes.len() && !matches!(self.bytes[end], b'\n' | b'\r') {
            end += 1;
        }
        let line_end = (local_start + max_len).min(end);
        let line = self.bytes[local_start..line_end].to_vec();
        self.pos = self.base.saturating_add(end);
        while self
            .bytes
            .get(self.local_pos().unwrap_or(self.bytes.len()))
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            self.pos += 1;
        }
        line
    }

    fn read_bytes(&mut self, max_len: usize) -> Vec<u8> {
        let start = self.local_pos().unwrap_or(self.bytes.len());
        let end = start.saturating_add(max_len).min(self.bytes.len());
        self.pos = self.base.saturating_add(end);
        self.bytes[start..end].to_vec()
    }
}

#[cfg(test)]
mod final_handle_tests {
    use super::*;
    use std::cell::RefCell;

    /// Load xref state the way `Pdf::open` does: through a `ResolverHandle`
    /// that owns the input source before parsing starts, mirroring qpdf's
    /// `QPDF::processInputSource` (`libqpdf/QPDF.cc:271-275`).
    ///
    /// The owner is returned alongside the result because every handle in
    /// `LoadedXref` borrows its identity from that owner, and because the
    /// route's warnings live on the owner's own sink rather than in
    /// `LoadedXref::repair_diagnostics` -- the same split `Pdf::open` reads
    /// (`crates/flpdf/src/engine.rs:224-248`).
    fn load_xref_snapshot<R: Read + Seek + 'static>(
        reader: R,
        allow_repair: bool,
        unique_id: u64,
    ) -> (Rc<ResolverHandle<R>>, Result<LoadedXref>) {
        let (owner, state) = load_xref_state_through_canonical_owner(
            reader,
            allow_repair,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            unique_id,
        );
        (owner, state.map(|state| state.loaded))
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
        let (owner, result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new(bytes),
            false,
            XrefLoadOptions {
                description: b"stream-trailer.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            crate::QPDFLogger::create(),
            true,
            8,
        );
        let _state = result.expect("a valid classic trailer with an extra stream token loads");
        // With a live owner, `deliver_canonical_diagnostics` drains each
        // buffered warning into the owner's own sink as it is found
        // (`xref.rs::deliver_canonical_diagnostics`); the returned state's
        // `repair_diagnostics` field stays empty. `Pdf::open` only ever
        // reads the owner's (`crates/flpdf/src/engine.rs:224-248`).
        let owner_diagnostics = owner.repair_diagnostics();
        let warnings: Vec<_> = owner_diagnostics
            .entries()
            .iter()
            .filter(|warning| warning.get_message_detail() == b"stream keyword found in trailer")
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].get_object(), b"trailer");
        assert_eq!(warnings[0].get_file_position(), warning_offset as i64);
    }

    #[test]
    fn canonical_nonzero_startxref_recovery_rebuilds_a_direct_trailer() {
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n999\n%%EOF\n".to_vec();
        let resolver = canonical_test_resolver(bytes, BTreeMap::new(), true, 12);
        let state = load_xref_state_from_source(resolver.as_ref(), XrefLoadOptions::default())
            .expect("canonical recovery should rebuild a nonzero malformed startxref");

        assert!(resolver.reconstructed_xref());
        assert_eq!(state.loaded.trailer.object_ref(), None);
    }

    #[test]
    fn canonical_owner_skips_the_offset_zero_retry_when_startxref_is_missing() {
        // No `startxref` at all, so `parse_startxref` fails and `startxref`
        // becomes 0. Object 1 sits at logical offset 0 and its body has a
        // stray token before `endobj`, which is exactly the shape that
        // makes a real read of it warn. qpdf's `xref_offset == 0` check
        // (`QPDF.cc:450-452`) never attempts this read at all; a canonical
        // owner must match that; the nonzero-startxref counterpart is
        // covered by `nonzero_xref_stream_decode_warning_is_kept_before_recovery`.
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nextra\nendobj\n2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\ntrailer\n<< /Size 3 /Root 1 0 R >>\n%%EOF\n".to_vec();
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), true, 10);
        let _state = load_xref_state_from_source(
            resolver.as_ref(),
            XrefLoadOptions {
                description: b"canonical-offset-zero.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
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
    fn classic_trailer_parse_errors_propagate_through_the_canonical_route() {
        let (bytes, _) = classic_xref_with_trailer("<< /Size 999999999999999999999999 >>");
        let resolver = canonical_test_resolver(bytes, BTreeMap::new(), false, 9);
        let error = load_xref_state_from_source(resolver.as_ref(), XrefLoadOptions::default())
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
        let (owner, result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new({
                let mut bytes = bytes.clone();
                bytes.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
                bytes
            }),
            false,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            40,
        );
        result.expect("one space still leaves the table readable");
        let owner_diagnostics = owner.repair_diagnostics();
        let messages: Vec<_> = owner_diagnostics
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
    fn a_non_dictionary_trailer_delivers_its_warnings_before_the_terminal_error() {
        // qpdf delivers the parser warnings and then throws
        // (`QPDF.cc:565-568,894-905`). Without leading whitespace the table
        // parses far enough to reach the trailer, which is where that split
        // matters.
        let mut bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n 0000000009  00000  n \n");
        bytes.extend_from_slice(b"trailer\n42\n");

        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 3);
        let mut registration = XrefRegistration::default();
        let error = parse_xref_from_start_with_owner(
            &bytes,
            xref,
            0,
            xref as u64,
            "1.7",
            XrefLoadOptions::default(),
            &mut registration,
            None,
            None,
            false,
            resolver.as_ref(),
        )
        .expect_err("a non-dictionary trailer is terminal");
        assert!(matches!(
            error,
            Error::QpdfExc(exception)
                if exception.get_message_detail() == b"expected trailer dictionary"
        ));
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

        let (_owner, result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new(bytes),
            true,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            41,
        );
        let state = result.expect("reconstruction recovers the well-formed objects");
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

            let (owner, result) = load_xref_state_through_canonical_owner(
                std::io::Cursor::new(bytes),
                false,
                XrefLoadOptions::default(),
                crate::QPDFLogger::create(),
                true,
                42,
            );
            result.unwrap_or_else(|error| panic!("separator {separator:?} must parse: {error:?}"));
            let diagnostics = owner.repair_diagnostics().entries().to_vec();
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

        let (owner, result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new(bytes),
            false,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            43,
        );
        result.expect("a vertical tab is a space to qpdf");
        let diagnostics = owner.repair_diagnostics().entries().to_vec();
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
        let (entries, uncompressed_after_compressed) =
            parse_xref_entries(&mut cursor, &[(35, 1)], (1, 0, 1), None, &mut registration)
                .expect("qpdf accepts an /Index row beyond the reported /Size");

        assert!(!uncompressed_after_compressed);
        assert!(matches!(
            entries.as_slice(),
            [ParsedXrefEntry::Live {
                object_ref,
                entry: XrefEntry::Uncompressed { offset: 0 },
            }] if *object_ref == QpdfObjGen::new(35, 0)
        ));
    }

    #[test]
    fn xref_entries_track_uncompressed_after_compressed_like_qpdf() {
        // qpdf's numbering check (`QPDF.cc:1070,1110-1116`): only a type-1/
        // type-0 entry seen *after* the section's first type-2 entry trips
        // the flag. Widths (1, 0, 1) match the sibling test above: one type
        // byte, no field1, one field2 byte.

        // type 2, then type 1: trips.
        let mut cursor = ByteCursor::new(&[2, 0, 1, 0], 0);
        let mut registration = XrefRegistration::default();
        let (_, flagged) =
            parse_xref_entries(&mut cursor, &[(0, 2)], (1, 0, 1), None, &mut registration)
                .expect("two-entry section parses");
        assert!(
            flagged,
            "an uncompressed entry after a compressed one must trip the flag"
        );

        // type 1, then type 2: qpdf's own layout convention, never trips.
        let mut cursor = ByteCursor::new(&[1, 0, 2, 0], 0);
        let mut registration = XrefRegistration::default();
        let (_, flagged) =
            parse_xref_entries(&mut cursor, &[(10, 2)], (1, 0, 1), None, &mut registration)
                .expect("two-entry section parses");
        assert!(
            !flagged,
            "an uncompressed entry strictly before the first compressed one must not trip the flag"
        );

        // type 2, then type 2: no uncompressed entry at all, never trips.
        let mut cursor = ByteCursor::new(&[2, 0, 2, 0], 0);
        let mut registration = XrefRegistration::default();
        let (_, flagged) =
            parse_xref_entries(&mut cursor, &[(20, 2)], (1, 0, 1), None, &mut registration)
                .expect("two-entry section parses");
        assert!(!flagged, "two compressed entries must not trip the flag");
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

        let (_owner, result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new(bytes),
            false,
            XrefLoadOptions {
                description: b"bad5.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            crate::QPDFLogger::create(),
            true,
            9,
        );
        let error =
            result.expect_err("qpdf rejects the malformed classic xref entry in strict mode");

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
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 60);
        let mut registration = XrefRegistration::default();
        let mut diagnostics = Diagnostics::default();
        let error = parse_xref_from_start_with_owner(
            &bytes,
            xref,
            0,
            xref as u64,
            "1.4",
            XrefLoadOptions {
                description: b"bad-spacing.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            &mut registration,
            Some(&mut diagnostics),
            None,
            false,
            resolver.as_ref(),
        )
        .expect_err("a non-dictionary trailer must retain parser diagnostics");

        assert!(matches!(
            error,
            Error::QpdfExc(exception)
                if exception.get_message_detail() == b"xref syntax invalid"
        ));
        assert!(
            diagnostics.entries().is_empty(),
            "the canonical owner is the sole warning sink: {diagnostics:?}"
        );
        let owner_diagnostics = resolver.repair_diagnostics();
        let messages: Vec<_> = owner_diagnostics
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
        let (lenient_owner, lenient_result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new({
                let mut bytes = lenient.clone();
                bytes.extend_from_slice(format!("startxref\n{lenient_xref}\n%%EOF\n").as_bytes());
                bytes
            }),
            false,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            44,
        );
        lenient_result.expect("qpdf accepts the lenient row and keeps reading");
        let lenient_diagnostics = lenient_owner.repair_diagnostics();
        let lenient_messages: Vec<_> = lenient_diagnostics
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
            0,
            xref as u64,
            "1.4",
            XrefLoadOptions {
                description: b"bad-spacing.pdf".to_vec(),
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            None,
            false,
            resolver.as_ref(),
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
        let (owner, result) = load_xref_snapshot(std::io::Cursor::new(bytes.to_vec()), true, 31);
        result.expect("the later dictionary candidate is recoverable");
        let owner_diagnostics = owner.repair_diagnostics();
        let warning = owner_diagnostics
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
        let (owner, result) = load_xref_snapshot(std::io::Cursor::new(bytes), true, 32);
        result.expect("repair mode keeps the parsed trailer after reporting the loop");
        assert_eq!(
            owner
                .repair_diagnostics()
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
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            header_offset: 0,
            uncompressed_after_compressed: false,
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
        let (_owner, result) = load_xref_snapshot(std::io::Cursor::new(bytes), false, 33);
        let error = result.expect_err("strict classic trailer validation must reject the fixture");

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
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 61);
        let mut registration = XrefRegistration::default();
        let mut diagnostics = Diagnostics::default();
        let error = parse_xref_from_start_with_owner(
            &bytes,
            xref,
            0,
            xref as u64,
            "1.4",
            XrefLoadOptions::default(),
            &mut registration,
            Some(&mut diagnostics),
            None,
            true,
            resolver.as_ref(),
        )
        .expect_err("missing /Size must remain a strict validation error");

        assert!(error
            .to_string()
            .contains("trailer dictionary lacks /Size key"));
        assert!(
            diagnostics.entries().is_empty(),
            "the canonical owner is the sole warning sink: {diagnostics:?}"
        );
        assert!(
            resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .any(|diagnostic| diagnostic
                    .message_string()
                    .contains("dictionary ended prematurely")),
            "strict validation must deliver parser diagnostics collected before the error"
        );

        let mut registration = XrefRegistration::default();
        let error = parse_xref_from_start_with_owner(
            &bytes,
            xref,
            0,
            xref as u64,
            "1.4",
            XrefLoadOptions::default(),
            &mut registration,
            None,
            None,
            true,
            resolver.as_ref(),
        )
        .expect_err("the same validation error must not require a diagnostic sink");
        assert!(error
            .to_string()
            .contains("trailer dictionary lacks /Size key"));
    }

    #[test]
    fn malformed_xref_stream_framing_error_is_carried_without_parser_sink() {
        let resolver = canonical_test_resolver(
            b"not an indirect object".to_vec(),
            BTreeMap::new(),
            false,
            66,
        );
        let mut registration = XrefRegistration::default();
        let failure = parse_xref_stream(
            0,
            0,
            "1.5".to_owned(),
            XrefLoadOptions::default(),
            &mut registration,
            resolver.as_ref(),
        )
        .expect_err("a malformed xref-stream object header must fail framing");

        assert!(matches!(failure.error, Error::Parse { .. }));
    }

    #[test]
    fn candidate_xref_stream_wrong_size_warning_survives_later_decode_failure() {
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [1 0 1] /Size 1 /Length 4 >>\nstream\nabcd\nendstream\nendobj\n%%EOF\n";
        let (_owner, result) = load_xref_snapshot(std::io::Cursor::new(bytes.to_vec()), true, 34);
        let error = result.expect_err("the malformed candidate must fail after warning");
        let (source, diagnostics) = error
            .open_failure()
            .expect("permissive candidate failure carries repair diagnostics");
        // cov:ignore-start: the preceding qpdf candidate assertion makes this defensive arm unreachable
        let Error::QpdfExc(source_warning) = source else {
            panic!("candidate recovery must preserve qpdf's structured terminal error");
        };
        // cov:ignore-end
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
        let (owner, result) = load_xref_snapshot(std::io::Cursor::new(bytes.to_vec()), true, 35);
        result.expect("the reconstruction re-entry must skip the already-registered entry");
        let owner_diagnostics = owner.repair_diagnostics();
        let messages: Vec<_> = owner_diagnostics
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
        let resolver = canonical_test_resolver(Vec::new(), BTreeMap::new(), false, 67);
        let (offset, diagnostics, error) = resolve_previous_xref_offset(
            XrefLoadOptions::default(),
            &trailer,
            None,
            resolver.as_ref(),
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
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            header_offset: 0,
            uncompressed_after_compressed: false,
        };
        let resolver = canonical_test_resolver(Vec::new(), BTreeMap::new(), false, 68);
        let mut registration = XrefRegistration::default();

        let error = merge_previous_xref_sections(
            b"",
            0,
            "1.4",
            &mut loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            resolver.as_ref(),
        )
        .expect_err("an uninitialized trailer must fail at the dictionary boundary");
        assert!(matches!(error, Error::Internal(message) if message.contains("uninitialized")));
    }

    #[test]
    fn an_indirect_prev_with_a_stale_xref_row_recovers_through_the_resolver() {
        // The older section's `/Prev 2 0 R` points at an xref row that names
        // object 1's header, so resolving it is the read failure qpdf turns
        // into a reconstruction. Measured with qpdf 11.9.0 on this fixture:
        // `file is damaged`, `(object 2 0, offset 9): expected 2 0 obj`,
        // `Attempting to reconstruct cross-reference table`, `object 2 0 not
        // found in file after regenerating cross reference table`, and then a
        // fifth warning -- `reported number of objects (3) is not one plus the
        // highest object number (1)` -- from qpdf's post-reconstruction
        // `/Size` consistency check (`QPDF.cc:689-704`).
        //
        // The live resolver performs the reconstruction during `/Prev`
        // resolution, so this route never re-enters the loader's own
        // `recover_xref_from_linear_scan`, and that fifth warning is not
        // reached. That gap predates the owner-less cutover: `Pdf::open` has
        // always supplied the canonical owner here, and
        // `CanonicalXrefContext::take_reconstruction_trigger` has always
        // returned `None`. Pin the four warnings this route does deliver so
        // the gap cannot widen unnoticed.
        let bytes = classic_xref_with_indirect_previous_in_older_section();
        let (owner, result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new(bytes),
            true,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            62,
        );
        result.expect("repair mode must survive the stale indirect /Prev");
        let messages: Vec<_> = owner
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message_string())
            .collect();
        assert_eq!(
            messages,
            vec![
                "file is damaged".to_owned(),
                "expected 2 0 obj".to_owned(),
                "Attempting to reconstruct cross-reference table".to_owned(),
                "object 2 0 not found in file after regenerating cross reference table".to_owned(),
            ],
            "{messages:?}"
        );
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
            0,
            "1.5",
            &mut loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            resolver.as_ref(),
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
            0,
            "1.5",
            &mut loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            resolver.as_ref(),
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
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), true, 70);
        let mut registration = XrefRegistration::default();

        merge_previous_xref_sections(
            &bytes,
            0,
            "1.4",
            &mut loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            resolver.as_ref(),
        )
        .expect("the warning-bearing previous xref stream should merge");
        assert!(resolver
            .repair_diagnostics()
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
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), true, 71);
        let mut registration = XrefRegistration::default();

        let error = merge_previous_xref_sections(
            &bytes,
            0,
            "1.4",
            &mut loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            resolver.as_ref(),
        )
        .expect_err("the malformed previous xref stream must fail");
        assert!(
            matches!(&error, Error::Parse { .. }),
            "unexpected error: {error:?}"
        );
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message_string()
                .contains("Cross-reference stream data has the wrong size")));
    }

    #[test]
    fn strict_classic_xref_rejects_non_integer_previous_offset() {
        let (bytes, offset) = classic_xref_with_malformed_previous();
        let (_owner, result) = load_xref_snapshot(std::io::Cursor::new(bytes), false, 36);
        let error = result.expect_err("strict xref chain validation must reject malformed /Prev");

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
        let (owner, result) = load_xref_snapshot(std::io::Cursor::new(bytes), true, 37);
        result.expect("repair mode must recover a trailer missing /Size");
        let owner_diagnostics = owner.repair_diagnostics();
        let messages: Vec<_> = owner_diagnostics
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
        /// `None` accepts every warning. `Some(n)` accepts `n` and then fails,
        /// so a sink that gives out partway through a batch can be observed.
        accept_warnings: Option<usize>,
    }

    impl CanonicalTrailerOwner for FailingCanonicalOwner {
        fn indirect_handle(&self, _object_ref: ObjectRef) -> ObjectHandle {
            ObjectHandle::uninitialized()
        }

        fn direct_handle(&self, value: ObjectValue) -> ObjectHandle {
            ObjectHandle::from_value(value)
        }

        fn install_xref_entries(&self, _entries: BTreeMap<ObjectRef, XrefEntry>) {}

        fn discard_cached_generations(&self, _object_gens: &[QpdfObjGen]) {} // cov:ignore: failure-injection owner has no canonical cache to purge

        fn set_header_offset(&self, _offset: usize) {}

        fn set_reconstructed_xref(&self) {} // cov:ignore: failure-injection owner never reaches a successful reconstruction

        fn attempt_recovery(&self) -> bool {
            false
        }

        fn source_seek(&self, _offset: u64) -> Result<()> {
            Ok(())
        }

        fn source_tell(&self) -> Result<u64> {
            Ok(0)
        }

        fn source_length(&self) -> Result<u64> {
            Ok(0)
        }

        fn source_read(&self, _buffer: &mut [u8]) -> Result<usize> {
            Ok(0)
        }

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
            if self
                .accept_warnings
                .is_some_and(|limit| self.diagnostics.borrow().len() >= limit)
            {
                return Err(Error::parse(0, "synthetic warning sink failure"));
            }
            self.diagnostics.borrow_mut().push(warning);
            Ok(())
        }

        fn repair_diagnostics(&self) -> Diagnostics {
            self.diagnostics.borrow().clone()
        }
    }

    /// qpdf appends to `m->warnings` one `warn()` call at a time, so a sink
    /// that gives out partway through a batch leaves the warnings it has not
    /// reached yet still pending. Draining the batch before delivering any of
    /// it would discard that tail instead.
    #[test]
    fn a_failing_warning_sink_leaves_the_undelivered_tail_pending() {
        let owner = FailingCanonicalOwner {
            transport_error: false,
            diagnostics: RefCell::new(Diagnostics::default()),
            accept_warnings: Some(1),
        };
        let mut diagnostics = Diagnostics::default();
        for message in [
            b"first".as_slice(),
            b"second".as_slice(),
            b"third".as_slice(),
        ] {
            diagnostics.push(damaged_warning(b"synthetic.pdf", b"", message, None));
        }

        let error = deliver_canonical_diagnostics(&owner, &mut diagnostics)
            .expect_err("the sink gives out on the second warning");
        assert!(error.to_string().contains("synthetic warning sink failure"));
        assert_eq!(owner.diagnostics.borrow().len(), 1);

        let pending: Vec<String> = diagnostics
            .entries()
            .iter()
            .map(|entry| entry.message_string())
            .collect();
        assert_eq!(pending.len(), 1, "only the warning after the failing one");
        assert!(
            pending[0].contains("third"),
            "the undelivered tail must survive, got {pending:?}"
        );
    }

    #[test]
    fn canonical_owner_warning_sink_records_a_local_diagnostic() {
        let owner = FailingCanonicalOwner {
            transport_error: false,
            diagnostics: RefCell::new(Diagnostics::default()),
            accept_warnings: None,
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

        // cov:ignore-start: the preceding qpdf xref assertion makes this defensive arm unreachable
        let Error::QpdfExc(warning) = error else {
            panic!("qpdf xref-stream damage must remain a structured warning");
        };
        // cov:ignore-end
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
            XrefLoadOptions::default(),
            &mut registration,
            resolver.as_ref(),
        )
        .expect("a recovered canonical xref stream should still parse its one free entry");

        let stream = resolver.get_object_handle(ObjectRef::new(1, 0));
        assert!(stream.as_stream_dict().is_some());
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
            &entries,
            XrefLoadOptions::default(),
            resolver.as_ref(),
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
                accept_warnings: None,
            };
            let _ = owner.indirect_handle(ObjectRef::new(1, 0));
            let _ = owner.direct_handle(ObjectValue::Integer(1));
            owner.install_xref_entries(BTreeMap::new());
            owner.set_header_offset(0);
            assert!(!owner.attempt_recovery());
            owner.source_seek(0).expect("synthetic source seek");
            assert_eq!(owner.source_tell().expect("synthetic source tell"), 0);
            assert_eq!(owner.source_length().expect("synthetic source length"), 0);
            let mut source_buffer = [0u8; 1];
            assert_eq!(
                owner
                    .source_read(&mut source_buffer)
                    .expect("synthetic source read"),
                0
            );
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
    fn live_xref_window_stops_after_the_classic_trailer_and_lookahead() {
        let trailing = b"xref\n\ntrailer << /Nested << /Size 1 >> >> stream\nignored tail\n";
        let resolver = canonical_test_resolver(trailing.to_vec(), BTreeMap::new(), true, 20);
        let window = read_live_xref_window(resolver.as_ref(), 0).expect("classic xref window");
        assert!(window.ends_with(b"stream\n"));
        assert!(!window.ends_with(b"ignored tail\n"));

        let nested_array = b"xref\ntrailer << /Items [ << /Size 1 >> ] >> stream\nignored tail\n";
        let resolver = canonical_test_resolver(nested_array.to_vec(), BTreeMap::new(), true, 20);
        let window = read_live_xref_window(resolver.as_ref(), 0)
            .expect("array contents must not end the trailer dictionary");
        assert!(window.ends_with(b"stream\n"));
        assert!(!window.ends_with(b"ignored tail\n"));

        let malformed_array = b"xref\ntrailer\n<< /Items [ >>\n>>\nstartxref\n0\n";
        let resolver = canonical_test_resolver(malformed_array.to_vec(), BTreeMap::new(), true, 20);
        let window = read_live_xref_window(resolver.as_ref(), 0)
            .expect("an unclosed array keeps the full bounded probe");
        assert_eq!(window, malformed_array);

        let no_following = b"xref\ntrailer\n<< /Size 1 >>";
        let resolver = canonical_test_resolver(no_following.to_vec(), BTreeMap::new(), true, 21);
        let window = read_live_xref_window(resolver.as_ref(), 0)
            .expect("EOF-terminated classic xref window");
        assert_eq!(window, no_following);

        let mut large = b"xref\n".to_vec();
        large.extend(std::iter::repeat_n(b' ', LIVE_XREF_PROBE_SIZE + 1));
        large.extend_from_slice(b"\ntrailer\n<< /Size 1 >>\n");
        let resolver = canonical_test_resolver(large, BTreeMap::new(), true, 25);
        let window = read_live_xref_window(resolver.as_ref(), 0)
            .expect("classic xref window should grow past the probe");
        assert!(window.ends_with(b"<< /Size 1 >>\n"));

        let resolver = canonical_test_resolver(b"short".to_vec(), BTreeMap::new(), true, 26);
        assert!(read_live_xref_window(resolver.as_ref(), 99)
            .expect("an xref offset beyond EOF is an empty window")
            .is_empty());

        assert_eq!(classic_xref_start(b"  xref\n"), Some(2));
        assert!(classic_xref_start(b"% comment\n").is_none());
        assert!(classic_trailer_dictionary_end(b"xref\nnot-a-trailer\n", 0).is_none());
        assert!(trailer_dictionary_end(b"<< ", 0).is_none());
    }

    #[test]
    fn live_source_range_reports_an_unexpected_eof_and_uses_tell() {
        let resolver = canonical_test_resolver(Vec::new(), BTreeMap::new(), false, 22);
        let error = read_live_source_range(resolver.as_ref(), 0, 1)
            .expect_err("a nonempty read from an empty source must fail");
        assert!(matches!(
            error,
            Error::Parse { message, .. } if message == "unexpected end of input source"
        ));
    }

    #[test]
    fn live_recovery_source_parses_a_trailer_candidate() {
        let bytes = b"trailer\n<< /Size 1 >>\n".to_vec();
        let resolver = canonical_test_resolver(bytes, BTreeMap::new(), true, 23);
        let recovered = recover_xref_entries_from_source(
            resolver.as_ref(),
            true,
            b"input.pdf",
            &BTreeSet::new(),
        )
        .expect("live reconstruction scanner should parse the trailer");
        assert!(recovered.trailer.is_some());
    }

    /// Direct unit coverage of qpdf's `insertReconstructedXrefEntry`
    /// overwrite-or-suppress step (`libqpdf/QPDF.cc:1197-1210`), now shared
    /// by both [`recover_xref_entries`] (resolve time) and
    /// [`recover_xref_entries_from_source`] (open time) through
    /// [`insert_reconstructed_xref_entry`].
    #[test]
    fn insert_reconstructed_xref_entry_overwrites_unless_deleted() {
        let mut entries = BTreeMap::new();
        let object_ref = ObjectRef::new(4, 0);
        let deleted = BTreeSet::from([4u32]);

        // qpdf's `m->deleted_objects.count(obj)` guard: a tombstoned object
        // number is never (re)written, matching flpdf-3yn9.48.157's fixture.
        insert_reconstructed_xref_entry(&mut entries, object_ref, 100, &deleted);
        assert!(entries.is_empty());

        // Not deleted: the row is written.
        insert_reconstructed_xref_entry(&mut entries, object_ref, 100, &BTreeSet::new());
        assert_eq!(
            entries.get(&object_ref),
            Some(&XrefEntry::Uncompressed { offset: 100 })
        );

        // A later occurrence of the same object/generation in the file
        // overwrites the earlier one -- qpdf's plain `xref_table[...] = ...`
        // assignment, not `try_emplace`.
        insert_reconstructed_xref_entry(&mut entries, object_ref, 200, &BTreeSet::new());
        assert_eq!(
            entries.get(&object_ref),
            Some(&XrefEntry::Uncompressed { offset: 200 })
        );
    }

    /// [`recover_xref_entries_from_source`]'s own `deleted_objects` filter,
    /// exercised directly against the scan (rather than through the full
    /// `Pdf::open` recovery pipeline that
    /// `reconstruction_after_a_committed_free_row_suppresses_it_like_qpdf`,
    /// in `tests/canonical_xref_owner_route_tests.rs`, already covers
    /// end-to-end).
    #[test]
    fn live_recovery_source_suppresses_a_tombstoned_object_number() {
        let bytes = b"1 0 obj\n<< >>\nendobj\n2 0 obj\n<< >>\nendobj\n".to_vec();
        let resolver = canonical_test_resolver(bytes, BTreeMap::new(), true, 79);
        let deleted_objects = BTreeSet::from([1u32]);
        let recovered = recover_xref_entries_from_source(
            resolver.as_ref(),
            false,
            b"input.pdf",
            &deleted_objects,
        )
        .expect("live reconstruction scanner should scan both object headers");
        assert!(
            !recovered.entries.contains_key(&ObjectRef::new(1, 0)),
            "a tombstoned object number must not be resurrected by the scan"
        );
        assert!(recovered.entries.contains_key(&ObjectRef::new(2, 0)));
    }

    #[test]
    fn canonical_source_window_retries_a_failed_repair_window() {
        let bytes = b"%PDF-1.4\n1 0 obj\n<< /Type /XRef /W [0 0 0] /Size 1 /Length 1 >>\nstream\n\x00\nendstream\nendobj\nstartxref\n9\n%%EOF\n".to_vec();
        let resolver = canonical_test_resolver(bytes, BTreeMap::new(), true, 27);
        let error = load_xref_state_from_source(resolver.as_ref(), XrefLoadOptions::default())
            .expect_err("a failed xref-stream repair must reach the live retry");
        assert!(error.to_string().contains("recovering damaged file"));
    }

    #[test]
    fn xref_window_and_byte_cursor_keep_their_defensive_error_boundaries() {
        let repair_owner = canonical_test_resolver(Vec::new(), BTreeMap::new(), true, 47);
        let recovered = load_xref_state_from_window(
            &[],
            10,
            "1.4".to_owned(),
            0,
            5,
            XrefLoadOptions::default(),
            Diagnostics::default(),
            Vec::new(),
            repair_owner.as_ref(),
        );
        assert!(recovered.is_err());

        let strict_owner = canonical_test_resolver(Vec::new(), BTreeMap::new(), false, 48);
        let strict = load_xref_state_from_window(
            &[],
            10,
            "1.4".to_owned(),
            0,
            5,
            XrefLoadOptions::default(),
            Diagnostics::default(),
            Vec::new(),
            strict_owner.as_ref(),
        );
        assert!(strict.is_err());

        let mut cursor = ByteCursor::with_base(&[], 10, 0);
        assert!(matches!(
            cursor.read_be_u64(1),
            Err(Error::Parse { message, .. }) if message == "unexpected end of stream field"
        ));

        let (_owner, result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new(b"%PDF-1.4\n%%EOF\n".to_vec()),
            false,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            49,
        );
        let error = result.expect_err("strict loading must reject a missing startxref");
        assert!(matches!(
            error,
            Error::Parse { message, .. } if message == "can't find startxref"
        ));
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
    fn hybrid_xref_stream_with_indirect_filter_loads_without_reconstruction() {
        let bytes = hybrid_xref_with_indirect_filter();
        let (owner, result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new(bytes),
            false,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            45,
        );
        let _state = result.expect("hybrid xref with an indirect /Filter must load");

        assert!(
            !owner.reconstructed_xref(),
            "a valid indirect /Filter on the /XRefStm stream must not force \
             cross-reference reconstruction"
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
        let state = load_xref_state_from_source(resolver.as_ref(), XrefLoadOptions::default())
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
        let mut entries = recover_xref_entries(&bytes).expect("scan candidate entries");
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
            XrefLoadOptions::default(),
            &mut entries,
            &mut parsed_xref_streams,
            &mut repair_diagnostics,
            &mut trailer_references,
            None,
            resolver.as_ref(),
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
        let mut entries = recover_xref_entries(&bytes).expect("scan candidate entries");
        let mut parsed_xref_streams = BTreeMap::new();
        let mut repair_diagnostics = Diagnostics::default();
        let mut trailer_references = BTreeSet::new();
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), true, 69);

        let recovered = recover_trailer_from_xref_stream_candidate(
            &bytes,
            "1.5",
            XrefLoadOptions::default(),
            &mut entries,
            &mut parsed_xref_streams,
            &mut repair_diagnostics,
            &mut trailer_references,
            None,
            resolver.as_ref(),
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

        recovered = merge_recovered_qpdf_state(recovered, accumulated);

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

        let (recovered_owner, recovered_result) = load_xref_state_through_canonical_owner(
            std::io::Cursor::new(bytes),
            true,
            XrefLoadOptions::default(),
            crate::QPDFLogger::create(),
            true,
            46,
        );
        let _recovered =
            recovered_result.expect("repair mode must complete the recovered size revalidation");
        assert!(recovered_owner.reconstructed_xref());
        assert!(recovered_owner
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message_string().contains("expected 2 0 obj")));
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
                message: b"treating unexpected brace token as null".to_vec(),
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
                    message: "invalid character (�) in hexstring".as_bytes().to_vec(),
                },
                ParserDiagnostic {
                    relative_offset: 4,
                    message: "invalid character (�) in hexstring".as_bytes().to_vec(),
                },
                ParserDiagnostic {
                    relative_offset: 7,
                    message: "invalid character (�) in hexstring".as_bytes().to_vec(),
                },
                ParserDiagnostic {
                    relative_offset: 8,
                    message: "invalid character (�) in hexstring".as_bytes().to_vec(),
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
    fn classic_xref_validation_covers_non_dictionary_and_invalid_hybrid_trailers() {
        let (bytes, xref) = classic_xref_with_trailer("42");
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 64);
        let mut registration = XrefRegistration::default();
        let error = parse_xref_from_start_with_owner(
            &bytes,
            xref,
            0,
            xref as u64,
            "1.4",
            XrefLoadOptions::default(),
            &mut registration,
            None,
            None,
            true,
            resolver.as_ref(),
        )
        .expect_err("classic trailer must be a dictionary");
        assert!(matches!(
            error,
            Error::QpdfExc(exception)
                if exception.get_message_detail() == b"expected trailer dictionary"
        ));

        let (bytes, xref) = classic_xref_with_trailer("<< /Size 1 /XRefStm (bad) >>");
        let resolver = canonical_test_resolver(bytes.clone(), BTreeMap::new(), false, 65);
        let mut registration = XrefRegistration::default();
        let error = parse_xref_from_start_with_owner(
            &bytes,
            xref,
            0,
            xref as u64,
            "1.4",
            XrefLoadOptions::default(),
            &mut registration,
            None,
            None,
            true,
            resolver.as_ref(),
        )
        .expect_err("a non-integer hybrid offset is invalid");
        assert!(error.to_string().contains("invalid /XRefStm"));
    }

    #[test]
    fn trailer_reference_collection_keeps_indirect_stream_children() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Child".to_vec(),
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1),
            )]),
            Rc::new(b"data".to_vec()),
        );
        assert!(collect_trailer_references(&stream).contains(&ObjectRef::new(7, 0)));
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
