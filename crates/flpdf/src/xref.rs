//! qpdf correspondence: QPDF.cc xref loading and repair.
use crate::diagnostics::Diagnostic;
use crate::object::collect_qpdf_object_references;
use crate::parser::{parse_indirect_object, parse_indirect_object_with_diagnostics, Parser};
use crate::reader::file_object::{
    finish_file_object, parse_file_object_syntax, FileObjectDiagnostic, RecoveryPolicy,
    ResolvedStreamLength,
};
use crate::tokenizer::{Token, TokenType, Tokenizer};
use crate::{filters, Diagnostics, Dictionary, Error, Object, ObjectRef, Result, XrefEntry};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone)]
pub struct LoadedXref {
    pub version: String,
    pub startxref: u64,
    pub entries: BTreeMap<ObjectRef, XrefEntry>,
    pub trailer: Dictionary,
    pub last_xref_form: XrefForm,
    pub repair_diagnostics: Diagnostics,
}

#[derive(Debug, Clone)]
pub(crate) struct LoadedXrefState {
    pub(crate) loaded: LoadedXref,
    pub(crate) trailer_references: BTreeSet<ObjectRef>,
    pub(crate) parsed_xref_streams: BTreeMap<ObjectRef, Object>,
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
        object_ref: ObjectRef,
        entry: XrefEntry,
    },
    Free {
        object_ref: ObjectRef,
    },
}

#[derive(Debug, Default)]
struct XrefRegistration {
    entries: BTreeMap<ObjectRef, XrefEntry>,
    deleted_objects: BTreeSet<u32>,
}

impl XrefRegistration {
    /// qpdf `insertXrefEntry`: a deleted object number suppresses every later
    /// live registration, while an exact object-generation collision is
    /// first-wins because sections are read newest to oldest.
    fn insert_xref_entry(&mut self, object_ref: ObjectRef, entry: XrefEntry) {
        if self.deleted_objects.contains(&object_ref.number) {
            return;
        }
        self.entries.entry(object_ref).or_insert(entry);
    }

    /// qpdf `insertFreeXrefEntry`: free rows are represented only by the
    /// object-number tombstone, and a matching exact live generation wins.
    fn insert_free_xref_entry(&mut self, object_ref: ObjectRef) {
        if !self.entries.contains_key(&object_ref) {
            self.deleted_objects.insert(object_ref.number);
        }
    }

    fn snapshot(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        self.entries.clone()
    }
}

/// The `QPDF::Members` settings the cross-reference loader consults, carried
/// together the way qpdf keeps them on `m` rather than as parallel arguments.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct XrefLoadOptions {
    /// qpdf `m->attempt_recovery`: run the reconstruction pass when the strict
    /// parse fails.
    pub(crate) allow_repair: bool,
    /// qpdf `m->ignore_xref_streams` (`QPDF::setIgnoreXRefStreams`): never read
    /// a cross-reference stream.
    pub(crate) ignore_xref_streams: bool,
}

/// Load the cross-reference table and trailer dictionary from `reader`, with
/// the qpdf-style recovery pass disabled (strict parse).
///
/// # Errors
///
/// Calls [`load_xref_and_trailer_with_repair`] with repair disabled, so it
/// propagates the same errors that function raises when `allow_repair` is
/// `false`:
///
/// - [`Error::Io`] when reading the input fails.
/// - [`Error::Parse`] when the PDF header, `startxref`, or a cross-reference
///   section is malformed (including a `startxref`/`/Prev` offset that does not
///   fit `usize` and a circular `/Prev` chain).
/// - [`Error::Missing`] when a required cross-reference stream entry (such as
///   `/Size` or `/W`) is absent.
/// - [`Error::Unsupported`] when a cross-reference stream uses an unsupported
///   object or entry type.
pub fn load_xref_and_trailer<R: Read + Seek>(reader: &mut R) -> Result<LoadedXref> {
    load_xref_and_trailer_with_repair(reader, false)
}

/// Load the cross-reference table and trailer dictionary from `reader`, running
/// the qpdf-style recovery pass when `allow_repair` is `true`.
///
/// # Errors
///
/// - [`Error::Io`] when seeking or reading the input fails.
/// - [`Error::Parse`] when `allow_repair` is `false` and the PDF header is
///   missing or its version is not UTF-8, or when `startxref`, a
///   cross-reference table or stream, or a `/Prev` chain is malformed
///   (including offsets that do not fit `usize` and a circular `/Prev` chain).
/// - [`Error::OpenFailure`] when `allow_repair` is `true`, repair diagnostics
///   were accumulated, and the linear scan still cannot recover a trailer.
///   [`Error::open_failure`] exposes both the terminal source error and the
///   preceding diagnostics.
/// - [`Error::Missing`] when a required cross-reference stream entry (such as
///   `/Size` or `/W`) is absent and `allow_repair` is `false`.
/// - [`Error::Unsupported`] when a cross-reference stream uses an unsupported
///   object or entry type and `allow_repair` is `false`.
pub fn load_xref_and_trailer_with_repair<R: Read + Seek>(
    reader: &mut R,
    allow_repair: bool,
) -> Result<LoadedXref> {
    load_xref_state_with_options(
        reader,
        XrefLoadOptions {
            allow_repair,
            ..XrefLoadOptions::default()
        },
    )
    .map(|state| state.loaded)
}

pub(crate) fn load_xref_state_with_options<R: Read + Seek>(
    reader: &mut R,
    options: XrefLoadOptions,
) -> Result<LoadedXrefState> {
    let allow_repair = options.allow_repair;
    let mut source_bytes = Vec::new();
    reader.seek(SeekFrom::Start(0))?;
    reader.read_to_end(&mut source_bytes)?;

    let mut initial_diagnostics = Diagnostics::default();
    let (version, header_offset) = if allow_repair {
        match find_qpdf_header(&source_bytes) {
            Some((offset, version)) => (version, offset),
            None => {
                initial_diagnostics.push(Diagnostic::warning("can't find PDF header", None));
                ("1.2".to_string(), 0)
            }
        }
    } else {
        (parse_header(&source_bytes)?, 0)
    };
    let bytes = &source_bytes[header_offset..];
    let mut parse_errors = Vec::new();
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
        Err(_) if allow_repair => {
            parse_errors.push(Error::parse(0, "startxref does not fit usize"));
            0
        }
        Err(_) => return Err(Error::parse(0, "startxref does not fit usize")),
    };

    let mut registration = XrefRegistration::default();
    let mut loaded = match parse_xref_from_start(
        bytes,
        xref_pos,
        startxref,
        &version,
        options,
        &mut registration,
        None,
    ) {
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
            let mut recovered = recover_xref_from_linear_scan(
                bytes,
                version,
                startxref,
                trigger,
                None,
                options,
                initial_diagnostics,
            )?;
            recovered.header_offset = header_offset;
            return Ok(recovered);
        }
        Err(error) => return Err(error),
    };
    prepend_repair_diagnostics(&mut loaded.loaded.repair_diagnostics, initial_diagnostics);

    if let Err(error) =
        merge_previous_xref_sections(bytes, &version, &mut loaded, options, &mut registration)
    {
        if allow_repair {
            let trigger = parse_errors.into_iter().next().unwrap_or(error);
            let recovered = recover_xref_from_linear_scan(
                bytes,
                version,
                startxref,
                trigger,
                Some(&loaded.loaded.trailer),
                options,
                Diagnostics::default(),
            )?;
            let mut recovered = merge_recovered_qpdf_state(recovered, loaded);
            recovered.header_offset = header_offset;
            return Ok(recovered);
        }
        return Err(error);
    }

    loaded.loaded.entries = registration.snapshot();
    append_xref_size_warning(&mut loaded.loaded, &registration.deleted_objects);
    registration.deleted_objects.clear();

    if let Some(error) = parse_errors.into_iter().next() {
        push_repair_diagnostics(&mut loaded.loaded.repair_diagnostics, &error, startxref);
    }

    loaded.header_offset = header_offset;
    Ok(loaded)
}

fn parse_xref_from_start(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: &str,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
) -> Result<LoadedXrefState> {
    if bytes
        .get(xref_pos..)
        .is_some_and(|tail| tail.starts_with(b"xref"))
    {
        let mut cursor = ByteCursor::new(bytes, xref_pos + 4);
        let (entries, trailer) = parse_xref_table(&mut cursor, bytes)?;
        let mut deferred_free = Vec::new();
        for entry in entries {
            match entry {
                ParsedXrefEntry::Live { object_ref, entry } => {
                    registration.insert_xref_entry(object_ref, entry);
                }
                ParsedXrefEntry::Free { object_ref } => deferred_free.push(object_ref),
            }
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
            trailer_references,
            parsed_xref_streams: BTreeMap::new(),
            header_offset: 0,
            already_reconstructed: false,
        };
        merge_xref_stream_from_classic_trailer(
            bytes,
            xref_pos,
            &mut loaded,
            options,
            registration,
        )?;
        for object_ref in deferred_free {
            registration.insert_free_xref_entry(object_ref);
        }
        loaded.loaded.entries = registration.snapshot();
        return Ok(loaded);
    }

    parse_xref_stream(
        bytes,
        xref_pos,
        startxref,
        version.to_string(),
        options,
        registration,
        error_diagnostics_sink,
    )
}

/// Read the optional hybrid-reference stream named by a classic trailer's
/// `/XRefStm`. This is the `QPDF::read_xrefTable` branch at QPDF.cc:915-927:
/// it reads the stream before the table's deferred free entries and deliberately
/// discards the stream trailer's `/Prev` continuation.
fn merge_xref_stream_from_classic_trailer(
    bytes: &[u8],
    classic_xref_pos: usize,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
) -> Result<()> {
    let Some(xref_stream_offset) = loaded.loaded.trailer.get("XRefStm") else {
        return Ok(());
    };

    // qpdf's ignore gate precedes both the integer check and read_xrefStream.
    // Do not rely on parse_xref_stream's internal gate: at this call site qpdf
    // succeeds without inspecting an ignored, malformed `/XRefStm` value.
    if options.ignore_xref_streams {
        return Ok(());
    }

    let Some(xref_stream_offset) = xref_stream_offset.as_integer() else {
        return Err(Error::parse(classic_xref_pos, "invalid /XRefStm"));
    };
    let xref_stream_pos = usize::try_from(xref_stream_offset).map_err(|_| {
        // qpdf passes the signed integer to InputSource::seek; a negative
        // value therefore fails as an invalid seek rather than as malformed
        // `/XRefStm` syntax.
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("xref stream offset {xref_stream_offset} is before the file start"),
        ))
    })?;

    // The hybrid stream contributes entries and raw-object discovery state, but
    // its own trailer is not the current trailer and its `/Prev` is ignored.
    let hybrid = parse_xref_stream(
        bytes,
        xref_stream_pos,
        xref_stream_pos as u64,
        loaded.loaded.version.clone(),
        options,
        registration,
        None,
    )?;
    for diagnostic in hybrid.loaded.repair_diagnostics.entries() {
        loaded.loaded.repair_diagnostics.push(diagnostic.clone());
    }
    loaded
        .trailer_references
        .extend(hybrid.trailer_references.iter().copied());
    loaded
        .parsed_xref_streams
        .extend(hybrid.parsed_xref_streams);

    loaded.loaded.entries = registration.snapshot();

    Ok(())
}

fn merge_previous_xref_sections(
    bytes: &[u8],
    version: &str,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
) -> Result<()> {
    let mut visited = HashSet::new();
    let mut previous_offset = parse_previous_xref_offset(&loaded.loaded.trailer);

    while let Some(offset) = previous_offset {
        let previous_pos = usize::try_from(offset)
            .map_err(|_| Error::parse(0, "xref /Prev does not fit usize"))?;

        if !visited.insert(offset) {
            return Err(Error::parse(0, "loop detected following xref tables"));
        }

        let previous = parse_xref_from_start(
            bytes,
            previous_pos,
            offset,
            version,
            options,
            registration,
            None,
        )?;
        for diagnostic in previous.loaded.repair_diagnostics.entries() {
            loaded.loaded.repair_diagnostics.push(diagnostic.clone());
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

        previous_offset = parse_previous_xref_offset(&previous.loaded.trailer);
    }

    loaded.loaded.entries = registration.snapshot();

    Ok(())
}

fn parse_previous_xref_offset(trailer: &Dictionary) -> Option<u64> {
    trailer
        .get("Prev")
        .and_then(|offset| parse_non_negative_u64(offset, "/Prev").ok())
        .filter(|&offset| offset != 0)
}

fn collect_trailer_references(trailer: &Dictionary) -> BTreeSet<ObjectRef> {
    let mut references = BTreeSet::new();
    collect_qpdf_object_references(&Object::Dictionary(trailer.clone()), &mut references);
    references
}

/// Emit qpdf's post-chain `/Size` warning while the construction-scoped
/// deleted-object set is still available.
fn append_xref_size_warning(loaded: &mut LoadedXref, deleted_objects: &BTreeSet<u32>) {
    let Some(Object::Integer(size)) = loaded.trailer.get("Size") else {
        return;
    };
    let max_live = loaded
        .entries
        .keys()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0);
    let max_deleted = deleted_objects.iter().copied().max().unwrap_or(0);
    let max_object = max_live.max(max_deleted);

    if *size < 1 || *size - 1 != i64::from(max_object) {
        loaded.repair_diagnostics.push(Diagnostic::warning(
            format!(
                "reported number of objects ({size}) is not one plus the highest object number ({max_object})"
            ),
            None,
        ));
    }
}

fn recover_xref_from_linear_scan(
    bytes: &[u8],
    version: String,
    startxref: u64,
    trigger_error: Error,
    fallback_trailer: Option<&Dictionary>,
    options: XrefLoadOptions,
    mut repair_diagnostics: Diagnostics,
) -> Result<LoadedXrefState> {
    push_repair_diagnostics(&mut repair_diagnostics, &trigger_error, startxref);

    let mut entries = recover_xref_entries(bytes)
        .map_err(|error| Error::with_open_diagnostics(error, repair_diagnostics.clone()))?;
    let mut parsed_xref_streams = BTreeMap::new();
    let mut extra_trailer_references = BTreeSet::new();
    let mut deleted_object_numbers = BTreeSet::new();

    // qpdf's `reconstruct_xref` (`QPDF.cc:564-616`) gates BOTH its `trailer`
    // keyword scan (`!m->trailer.isInitialized() && t1.isWord("trailer")`)
    // and its `/Type /XRef` candidate search (`if
    // (!m->trailer.isInitialized())`) on the trailer not already being
    // known. `fallback_trailer` -- the trailer from a successfully parsed
    // newest revision whose `/Prev` chain later broke -- models exactly
    // that already-initialized state: qpdf never looks at a stray candidate
    // elsewhere in the file when the correct trailer is already in hand, so
    // neither `recover_trailer` nor the candidate search runs at all in
    // that case. `startxref` (the position that produced `fallback_trailer`)
    // is already valid then too, so it needs no adjustment; it is only
    // rewritten to the candidate's own verified re-entry point when the
    // candidate path is what actually recovered the trailer. `last_xref_form`
    // is left as a placeholder (`Table`) in the `fallback_trailer` case: the
    // caller (`load_xref_state_with_options`) always overwrites it via
    // `merge_recovered_qpdf_state` with the already-successfully-parsed
    // revision's own real form once this returns.
    let (trailer, recovered_startxref, recovered_form) = if let Some(trailer) = fallback_trailer {
        (trailer.clone(), startxref, XrefForm::Table)
    } else {
        match recover_trailer(bytes) {
            Ok(trailer) => (trailer, startxref, XrefForm::Table),
            Err(_) => match recover_trailer_from_xref_stream_candidate(
                bytes,
                &version,
                options,
                &mut entries,
                &mut parsed_xref_streams,
                &mut repair_diagnostics,
                &mut extra_trailer_references,
                &mut deleted_object_numbers,
            ) {
                Ok((trailer, max_offset, form)) => (trailer, max_offset, form),
                Err(candidate_error) => {
                    return Err(Error::with_open_diagnostics(
                        candidate_error,
                        repair_diagnostics,
                    ));
                }
            },
        }
    };

    // The ObjStm gap-filler runs last so that entries recovered from a real
    // xref-stream re-entry above (authoritative) are not blocked by flpdf's own
    // invented compressed-entry guesses (see `recover_objstm_compressed_entries`).
    recover_objstm_compressed_entries(bytes, &mut entries, &deleted_object_numbers);

    let mut trailer_references = collect_trailer_references(&trailer);
    trailer_references.extend(extra_trailer_references);

    Ok(LoadedXrefState {
        loaded: LoadedXref {
            version,
            startxref: recovered_startxref,
            entries,
            trailer,
            last_xref_form: recovered_form,
            repair_diagnostics,
        },
        trailer_references,
        parsed_xref_streams,
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

/// Load the cross-reference table and trailer dictionary from `reader`, with the
/// qpdf-style recovery pass always enabled (best-effort).
///
/// # Errors
///
/// Calls [`load_xref_and_trailer_with_repair`] with repair enabled, so
/// malformed cross-reference data is recovered rather than reported. A missing
/// header or invalid header version records qpdf's warning and uses version
/// 1.2. It still fails with:
///
/// - [`Error::Io`] when seeking or reading the input fails.
/// - [`Error::OpenFailure`] when repair diagnostics were accumulated but the
///   linear scan cannot recover a trailer.
///   [`Error::open_failure`] exposes both the terminal source error and the
///   preceding diagnostics.
pub fn load_xref_and_trailer_best_effort<R: Read + Seek>(reader: &mut R) -> Result<LoadedXref> {
    load_xref_and_trailer_with_repair(reader, true)
}

/// Recover uncompressed object offsets by replaying qpdf's `reconstruct_xref`
/// (`libqpdf/QPDF.cc`, qpdf 11.9.0): scan the file line by line, and on each line
/// whose first token sequence is `int int obj`, record the object at the offset of
/// its *number* token. Only the first token of a line is inspected, object bodies
/// are never parsed, and the last occurrence of an object in the file wins (qpdf's
/// `insertReconstructedXrefEntry` overwrites). Inspecting at most three short
/// tokens per line — never re-parsing a body to end-of-file — makes the scan
/// linear in the file size, unlike a per-candidate full-object parse which an
/// unterminated literal string can drive to quadratic cost.
///
/// qpdf records only uncompressed (type-1) entries during reconstruction and
/// declines to look inside object streams (`reconstruct_xref` trailing comment in
/// QPDF.cc). The caller runs flpdf's own `/Type /ObjStm` gap-filler
/// ([`recover_objstm_compressed_entries`]) and the qpdf-native xref-stream
/// candidate re-entry ([`recover_trailer_from_xref_stream_candidate`]) against
/// this function's result afterward, in that order — see
/// [`recover_xref_from_linear_scan`].
pub(crate) fn recover_xref_entries(bytes: &[u8]) -> Result<BTreeMap<ObjectRef, XrefEntry>> {
    let mut entries = BTreeMap::new();
    let mut line_start = 0usize;
    while line_start < bytes.len() {
        let next_line_start = next_line_start(bytes, line_start);
        if let Some((object_ref, offset)) =
            scan_object_header_at_line(bytes, line_start, next_line_start)
        {
            entries.insert(object_ref, XrefEntry::Uncompressed { offset });
        }
        line_start = next_line_start;
    }

    Ok(entries)
}

/// How many further offset-*positions* (not bytes) a truncated candidate's
/// window may extend into on a fallback retry, independent of every other
/// entry's own retries. Bounding by position rather than sharing a global
/// retry budget across the whole scan means no fixed number of unrelated,
/// individually-truncated entries earlier in the (ascending object-number)
/// scan can ever deny a later, genuine candidate its own retry -- qpdf's
/// per-object recovery has no shared budget at all. Total work stays
/// O(file size): at most this many *additional* candidates are examined per
/// retry, and only entries within this many positions of the end of the
/// offset-sorted list can ever have their retry reach all the way to EOF.
const XREF_CANDIDATE_FALLBACK_SPAN: usize = 64;

/// qpdf's second trailer-recovery fallback (`reconstruct_xref`, `QPDF.cc:577-608`,
/// qpdf 11.9.0): entered only when the line scan found no `trailer` keyword.
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
/// recovering damaged file". `deleted_object_numbers` collects every object
/// number the candidate's own revision chain marks free (via its own
/// `XrefRegistration`, which -- like `entries` -- no longer stores `Free`
/// rows directly): the caller folds these into the ObjStm gap-filler's
/// occupied-number set so a real free entry still blocks resurrection even
/// though it has no map presence of its own.
#[allow(clippy::too_many_arguments)]
fn recover_trailer_from_xref_stream_candidate(
    bytes: &[u8],
    version: &str,
    options: XrefLoadOptions,
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    parsed_xref_streams: &mut BTreeMap<ObjectRef, Object>,
    repair_diagnostics: &mut Diagnostics,
    trailer_references: &mut BTreeSet<ObjectRef>,
    deleted_object_numbers: &mut BTreeSet<u32>,
) -> Result<(Dictionary, u64, XrefForm)> {
    let (candidate, discovery_diagnostics) = find_xref_stream_trailer_candidate(bytes, entries);
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
    // terminal "error decoding candidate xref stream..." message).
    // `error_diagnostics_sink` recovers that same warning here: written only
    // when `parse_xref_stream` itself fails after already computing it.
    //
    // The candidate's own re-entry gets a fresh `XrefRegistration`, scoped to
    // just this call and its `/Prev` chain -- qpdf's `insertXrefEntry`/
    // `insertFreeXrefEntry` priority is local to `read_xref`'s own walk of
    // that one revision chain, not shared with the line scan's entries.
    let mut reentry_registration = XrefRegistration::default();
    let mut reentry = match parse_xref_from_start(
        bytes,
        max_offset as usize,
        max_offset,
        version,
        options,
        &mut reentry_registration,
        Some(&mut *repair_diagnostics),
    ) {
        Ok(reentry) => reentry,
        Err(_) => {
            return Err(Error::parse(
                0,
                "error decoding candidate xref stream while recovering damaged file",
            ));
        }
    };
    if merge_previous_xref_sections(
        bytes,
        version,
        &mut reentry,
        options,
        &mut reentry_registration,
    )
    .is_err()
    {
        // The candidate's own initial parse succeeded and may have already
        // recorded repair diagnostics (e.g. stream-length recovery); qpdf's
        // warnings accumulate as they are emitted and are never rolled back
        // by a later exception in the same `read_xref` call
        // (`QPDF.cc:496-497`'s `warn()` appends immediately), so propagate
        // them before reporting the terminal error.
        for diagnostic in reentry.loaded.repair_diagnostics.entries() {
            repair_diagnostics.push(diagnostic.clone());
        }
        return Err(Error::parse(
            0,
            "error decoding candidate xref stream while recovering damaged file",
        ));
    }

    // `reentry.loaded.entries` is already the live-only snapshot of
    // `reentry_registration` (free rows never get a map entry, matching
    // `XrefRegistration::insert_free_xref_entry`). Insert its live entries,
    // but block by object *number* rather than the exact `ObjectRef` --
    // `occupied_numbers` mirrors `entries`' own object numbers so this
    // priority check is O(1) instead of rescanning every existing key per
    // recovered entry, and matches qpdf's `insertXrefEntry`/
    // `insertFreeXrefEntry` (`QPDF.cc:1149-1206`) both keying priority off
    // the number alone.
    let mut occupied_numbers: HashSet<u32> =
        entries.keys().map(|object_ref| object_ref.number).collect();
    for (object_ref, xref_entry) in reentry.loaded.entries {
        if occupied_numbers.insert(object_ref.number) {
            entries.insert(object_ref, xref_entry);
        }
    }
    // The candidate/its `/Prev` chain's own free rows have no map presence
    // to block the ObjStm gap-filler with (`reentry_registration.entries`
    // never holds them either); surface the object numbers directly so the
    // caller can seed the gap-filler's occupied-number set with them,
    // mirroring qpdf's `m->deleted_objects` (`std::set<int>`, `QPDF.hh:1466`)
    // discarding a type-0 row's own generation field
    // (`QPDF.cc:1120-1124`, "Ignore fields[2]").
    deleted_object_numbers.extend(reentry_registration.deleted_objects);
    parsed_xref_streams.extend(reentry.parsed_xref_streams);
    trailer_references.extend(reentry.trailer_references);
    // The candidate re-entry (and any `/Prev` chain it follows) can itself
    // emit repair warnings (e.g. stream-length recovery); propagate them the
    // same way `merge_previous_xref_sections` already does for its own
    // `/Prev` reentries, instead of silently discarding them.
    for diagnostic in reentry.loaded.repair_diagnostics.entries() {
        repair_diagnostics.push(diagnostic.clone());
    }

    Ok((candidate.trailer, max_offset, reentry.loaded.last_xref_form))
}

/// The `/Type /XRef` candidate this file's line-scanned entries point at:
/// its dictionary (which may or may not be the winning trailer -- see
/// [`find_xref_stream_trailer_candidate`]'s doc) and its true maximum
/// offset (the re-entry point).
struct XrefStreamCandidate {
    trailer: Dictionary,
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
///
/// A separate offset-sorted index bounds each candidate's parse window to
/// the offset of its byte-adjacent neighbor, retrying with a wider window
/// when that truncates a real object (a header-like line recorded inside an
/// object's own payload can become a bogus next offset). That retry extends
/// by [`XREF_CANDIDATE_FALLBACK_SPAN`] further offset-*positions* rather
/// than sharing one global attempt-count budget across the whole scan
/// (mirroring [`recover_objstm_compressed_entries`]'s windowing otherwise):
/// a shared budget lets enough earlier, unrelated truncated entries deny a
/// later, genuine candidate its own retry, which qpdf's per-object recovery
/// has no equivalent of.
fn find_xref_stream_trailer_candidate(
    bytes: &[u8],
    entries: &BTreeMap<ObjectRef, XrefEntry>,
) -> (Option<XrefStreamCandidate>, Diagnostics) {
    let mut offsets: Vec<u64> = entries
        .values()
        .filter_map(|entry| match entry {
            XrefEntry::Uncompressed { offset } => Some(*offset),
            XrefEntry::Compressed { .. } | XrefEntry::Free { .. } => None,
        })
        .collect();
    offsets.sort_unstable();

    let mut max_offset = 0u64;
    let mut trailer: Option<Dictionary> = None;
    let mut discovery_diagnostics = Diagnostics::default();
    for entry in entries.values() {
        let XrefEntry::Uncompressed { offset } = *entry else {
            continue;
        };
        let start = offset as usize;
        let next_offset_index = offsets.partition_point(|&candidate| candidate <= offset);
        let window_end = offsets
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
        let parsed = match parse_indirect_object_with_diagnostics(
            &bytes[start..window_end],
            RecoveryPolicy::Bounded,
        ) {
            Ok(parsed) => Some(parsed),
            Err(_) if window_end < bytes.len() => {
                let wide_index = next_offset_index
                    .saturating_add(XREF_CANDIDATE_FALLBACK_SPAN)
                    .min(offsets.len());
                let wide_end = offsets
                    .get(wide_index)
                    .map_or(bytes.len(), |&next| next as usize);
                parse_indirect_object_with_diagnostics(
                    &bytes[start..wide_end],
                    RecoveryPolicy::Bounded,
                )
                .ok()
            }
            Err(_) => None,
        };
        let Some((object_ref, object, diagnostics)) = parsed else {
            continue;
        };
        for diagnostic in diagnostics {
            discovery_diagnostics.push(xref_file_object_diagnostic(object_ref, offset, diagnostic));
        }
        // qpdf's `getObjectByObjGen` (`QPDF.cc:585`) resolves the object
        // before `isStreamOfType` (`QPDF.cc:587`) is even checked, so a
        // non-stream object's own read warnings (e.g. "expected endobj",
        // `QPDF.cc:1352-1355`) are collected above regardless of whether it
        // turns out to be a stream at all.
        let Object::Stream(stream) = object else {
            continue;
        };
        if !is_xref_stream_dict(&stream.dict) {
            continue;
        }
        if offset > max_offset {
            max_offset = offset;
            if trailer.is_none() {
                trailer = Some(stream.dict.clone());
            }
        }
    }

    let candidate = trailer.map(|dict| XrefStreamCandidate {
        trailer: dict,
        max_offset,
    });
    (candidate, discovery_diagnostics)
}

fn is_xref_stream_dict(dict: &Dictionary) -> bool {
    matches!(dict.get("Type"), Some(Object::Name(name)) if name.as_slice() == b"XRef")
}

/// Upper bound on read-to-end fallbacks during ObjStm recovery (see
/// [`recover_objstm_compressed_entries`]). Each fallback may parse to end of
/// file, so the count is capped to keep the total work O(file size) while still
/// recovering a handful of object streams whose payloads happen to contain a
/// header-like line.
const MAX_OBJSTM_RECOVERY_FALLBACKS: u32 = 64;

/// Recover the compressed objects packed in any recovered `/Type /ObjStm`,
/// emitting `XrefEntry::Compressed` entries that point back at the stream.
///
/// Each recovered object is parsed within the window that ends at the next
/// recovered object's offset (or end-of-file for the last). The windows are
/// disjoint, so the common case is bounded by the file size — a malformed object
/// cannot drive the parse to end-of-file once per candidate. When a window does
/// not hold a complete object — a header-like line (`int int obj`) recorded
/// inside an object stream's payload became the next offset and truncated it —
/// it retries against the rest of the file so the stream's own `/Length`
/// delimits it. Those retries are capped by [`MAX_OBJSTM_RECOVERY_FALLBACKS`] so
/// a flood of stream-like candidates cannot reintroduce quadratic cost.
///
/// `recover_xref_entries` itself only performs qpdf's own line scan (see its
/// doc); this pass is flpdf's own addition and does not run automatically, so
/// every caller of `recover_xref_entries` that wants its objects resolvable
/// must call this afterward (`recover_xref_from_linear_scan` runs it after
/// candidate re-entry so authoritative real entries are not blocked by these
/// invented guesses; `ResolverCore`'s own resolve-time reconstruction retry
/// in `reader/resolver.rs`, which has no candidate re-entry of its own, runs
/// it immediately after).
pub(crate) fn recover_objstm_compressed_entries(
    bytes: &[u8],
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    extra_occupied_numbers: &BTreeSet<u32>,
) {
    // The line scan only ever inserts `XrefEntry::Uncompressed`, so every entry here is
    // an uncompressed object whose offset bounds a window.
    let mut offsets: Vec<u64> = Vec::new();
    for entry in entries.values() {
        if let XrefEntry::Uncompressed { offset } = entry {
            offsets.push(*offset);
        }
    }
    offsets.sort_unstable();

    // Mirrors `entries`' own object numbers so the number-based priority
    // check in `recover_compressed_offsets_from_objstm` stays O(1) instead
    // of rescanning every existing key per packed object -- a damaged but
    // otherwise large PDF can pack tens of thousands of objects here.
    // `extra_occupied_numbers` folds in object numbers a candidate xref
    // stream's own revision chain marked free (`XrefRegistration` no longer
    // gives those a map entry to be picked up by `entries.keys()` above; see
    // `recover_trailer_from_xref_stream_candidate`'s `deleted_object_numbers`
    // out-param).
    let mut occupied_numbers: HashSet<u32> = entries
        .keys()
        .map(|object_ref| object_ref.number)
        .chain(extra_occupied_numbers.iter().copied())
        .collect();

    let mut fallbacks = MAX_OBJSTM_RECOVERY_FALLBACKS;
    for (index, &offset) in offsets.iter().enumerate() {
        let start = offset as usize;
        let window_end = offsets
            .get(index + 1)
            .map_or(bytes.len(), |next| *next as usize);
        if try_recover_objstm_in(entries, &mut occupied_numbers, &bytes[start..window_end]) {
            continue;
        }
        // The bounded window stopped short of a complete object. Retry against
        // the rest of the file so a real ObjStm truncated by a header-like line
        // in its payload is still recovered, capped so it stays linear.
        if window_end < bytes.len() && fallbacks > 0 {
            fallbacks -= 1;
            try_recover_objstm_in(entries, &mut occupied_numbers, &bytes[start..]);
        }
    }
}

/// Parse the indirect object in `slice`; if it is a `/Type /ObjStm`, insert its
/// packed objects' compressed entries. Returns `false` only when `slice` did not
/// contain a complete object (a parse error) — the signal that a bounded window
/// may have truncated a real stream and a wider retry is worthwhile.
fn try_recover_objstm_in(
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    occupied_numbers: &mut HashSet<u32>,
    slice: &[u8],
) -> bool {
    match parse_indirect_object(slice, RecoveryPolicy::RequireTerminator) {
        Ok((object_ref, Object::Stream(stream))) => {
            if let Some(Object::Name(type_name)) = stream.dict.get("Type") {
                if type_name.as_slice() == b"ObjStm" {
                    recover_compressed_offsets_from_objstm(
                        entries,
                        occupied_numbers,
                        object_ref,
                        &stream,
                    );
                }
            }
            true
        }
        Ok(_) => true,
        Err(_) => false,
    }
}

fn recover_compressed_offsets_from_objstm(
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    occupied_numbers: &mut HashSet<u32>,
    stream_ref: ObjectRef,
    stream: &crate::Stream,
) {
    let Ok(decoded_data) = crate::filters::decode_stream_data(&stream.dict, &stream.data) else {
        return;
    };

    let object_count =
        match parse_non_negative_u64(stream.dict.get("N").unwrap_or(&Object::Integer(0)), "/N") {
            Ok(count) => match usize::try_from(count) {
                Ok(count) => count,
                Err(_) => return,
            },
            Err(_) => return,
        };

    let mut tokenizer = Tokenizer::new(&decoded_data);
    for index in 0..object_count {
        let number = match tokenizer.next_integer() {
            Ok(number) => match parse_non_negative_i64(number, "ObjStm object number") {
                Ok(number) => number,
                Err(_) => return,
            },
            Err(_) => return,
        };
        let object_ref = match u32::try_from(number) {
            Ok(object_ref) => ObjectRef::new(object_ref, 0),
            Err(_) => return,
        };

        match tokenizer.next_integer() {
            Ok(offset) => {
                if parse_non_negative_i64(offset, "ObjStm object offset").is_err() {
                    return;
                }
                // Block by object number, not the exact `(number, 0)` key:
                // a `Free` tombstone recorded at a nonzero generation (see
                // `recover_trailer_from_xref_stream_candidate`) must still
                // stop this invented entry from resurrecting the object.
                if occupied_numbers.insert(object_ref.number) {
                    entries.insert(
                        object_ref,
                        XrefEntry::Compressed {
                            stream: stream_ref.number,
                            index: u32::try_from(index).unwrap_or(u32::MAX),
                        },
                    );
                }
            }
            Err(_) => return,
        }
    }
}

fn parse_non_negative_i64(value: i64, name: &str) -> Result<u64> {
    if value < 0 {
        return Err(Error::parse(0, format!("{name} is negative")));
    }
    Ok(value as u64)
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
fn push_repair_diagnostics(diagnostics: &mut Diagnostics, trigger_error: &Error, startxref: u64) {
    diagnostics.push(Diagnostic::warning("file is damaged", None));
    let (message, offset) = match trigger_error {
        Error::Parse { offset: 0, message } if message == "xref not found" => {
            ("can't find startxref".to_string(), None)
        }
        Error::Parse { message, .. } if message == "can't find startxref" => {
            (message.clone(), None)
        }
        Error::Parse { message, .. } if message == "loop detected following xref tables" => {
            (message.clone(), None)
        }
        Error::Parse { offset, message } => (message.clone(), Some(*offset as u64)),
        other => (other.to_string(), Some(startxref)),
    };
    diagnostics.push(Diagnostic::warning(message, offset));
    diagnostics.push(Diagnostic::warning(
        "Attempting to reconstruct cross-reference table",
        None,
    ));
}

fn recover_trailer(bytes: &[u8]) -> Result<Dictionary> {
    let marker = b"trailer";
    let Some(pos) = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        return Err(Error::parse(0, "trailer dictionary not found"));
    };

    let cursor = ByteCursor::new(bytes, pos + marker.len());
    let mut tokenizer = Tokenizer::new(&bytes[cursor.pos..]);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    match parser.object()? {
        Object::Dictionary(trailer) => Ok(trailer),
        _ => Err(Error::parse(
            cursor.pos + parser.position(),
            "trailer dictionary is not a dictionary",
        )),
    }
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

fn parse_scan_integer(token: &Token) -> Option<i64> {
    if !token.is_integer() {
        return None;
    }
    std::str::from_utf8(&token.value).ok()?.parse().ok()
}

/// If the line beginning at `line_start` opens with an `int int obj` token
/// sequence, return the recovered object and the offset of its number token.
///
/// Mirrors qpdf's `reconstruct_xref` per-line logic: the first token must begin
/// on this line (otherwise the line records nothing — qpdf's
/// `token_start >= next_line_start` guard, here enforced by bounding the first
/// token read to `next_line_start`), the second and third tokens may spill onto
/// following lines, and the object/generation must satisfy qpdf's
/// `insertReconstructedXrefEntry` guards (`obj > 0`, `0 <= gen < 65535`).
fn scan_object_header_at_line(
    bytes: &[u8],
    line_start: usize,
    next_line_start: usize,
) -> Option<(ObjectRef, u64)> {
    // Bounding the first token to this line is what keeps a whitespace- or
    // comment-only line from re-scanning the remaining file on every iteration.
    let number_token = read_scan_token(bytes, line_start, next_line_start)?;
    let obj = parse_scan_integer(&number_token)?;

    let gen_token = read_scan_token(bytes, number_token.end, bytes.len())?;
    let gen = parse_scan_integer(&gen_token)?;

    let obj_token = read_scan_token(bytes, gen_token.end, bytes.len())?;
    if !obj_token.is_word_value(b"obj") {
        return None;
    }

    // qpdf's `insertReconstructedXrefEntry` guards (`obj > 0`, `0 <= gen < 65535`).
    if obj <= 0 || !(0..65535).contains(&gen) {
        return None;
    }
    let number = u32::try_from(obj).ok()?;
    let generation = u16::try_from(gen).ok()?;
    Some((
        ObjectRef::new(number, generation),
        number_token.start as u64,
    ))
}

fn parse_xref_table(
    cursor: &mut ByteCursor<'_>,
    bytes: &[u8],
) -> Result<(Vec<ParsedXrefEntry>, Dictionary)> {
    let mut entries = Vec::new();
    loop {
        let first_token = cursor.read_token()?;
        if first_token.is_word_value(b"trailer") {
            break;
        }

        let first = parse_xref_subsection_u32(&first_token)?;
        let count = cursor.read_u32()?;
        for index in 0..count {
            cursor.skip_ws();
            let offset = cursor.read_fixed_u64(10)?;
            cursor.skip_ws();
            let generation = cursor.read_fixed_u16(5)?;
            cursor.skip_ws();
            let in_use = cursor.read_byte()?;
            cursor.skip_line();
            match in_use {
                b'f' => {
                    let _next = offset;
                    entries.push(ParsedXrefEntry::Free {
                        object_ref: ObjectRef::new(first + index, generation),
                    });
                }
                b'n' => {
                    entries.push(ParsedXrefEntry::Live {
                        object_ref: ObjectRef::new(first + index, generation),
                        entry: XrefEntry::Uncompressed { offset },
                    });
                }
                _ => return Err(Error::parse(0, "xref table entry status is not f or n")),
            }
        }
    }

    let mut tokenizer = Tokenizer::new(&bytes[cursor.pos..]);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    let trailer = match parser.object()? {
        Object::Dictionary(dict) => dict,
        _ => {
            return Err(Error::parse(
                cursor.pos + parser.position(),
                "trailer is not a dictionary",
            ));
        }
    };

    Ok((entries, trailer))
}

fn parse_xref_stream(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: String,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
) -> Result<LoadedXrefState> {
    // qpdf's `read_xrefStream` wraps its whole body in
    // `if (!m->ignore_xref_streams)` and otherwise falls straight through to
    // `throw damagedPDF("", xref_offset, "xref not found")` — the offset is
    // never read, so this precedes the end-of-file check below. The same error
    // is what a non-stream object at the offset produces.
    if options.ignore_xref_streams {
        return Err(Error::parse(xref_pos, "xref not found"));
    }
    let allow_repair = options.allow_repair;
    let tail = bytes
        .get(xref_pos..)
        .filter(|slice| !slice.is_empty())
        .ok_or_else(|| Error::parse(xref_pos, "xref stream offset is beyond end of file"))?;
    let pending = parse_file_object_syntax(tail).map_err(|err| err.rebase_offset(xref_pos))?;
    let object_ref = pending.object_ref;
    let unresolved_length = pending
        .indirect_length_ref()
        .map(|_| ResolvedStreamLength::Missing);
    let policy = if allow_repair {
        RecoveryPolicy::Bounded
    } else {
        RecoveryPolicy::RequireEndstream
    };
    let mut completed = finish_file_object(tail, pending, unresolved_length, policy)
        .map_err(|err| err.rebase_offset(xref_pos))?;
    // Xref streams are not encrypted, but filter decoding still requires the
    // logical payload rather than qpdf's raw recovery EOL.
    let _recovered_eol = completed.remove_included_recovery_eol_for_decryption();
    let object = completed.object;
    let mut repair_diagnostics = Diagnostics::default();
    for diagnostic in completed.diagnostics {
        repair_diagnostics.push(xref_file_object_diagnostic(
            object_ref,
            xref_pos as u64,
            diagnostic,
        ));
    }
    // qpdf's own read of this object (`readObjectAtOffset`, `QPDF.cc:956`)
    // happens before `processXRefStream` validates `/Type`, `/W`, `/Index`,
    // `/Size`, or the entry data (`QPDF.cc:960-1128`); any repair warning
    // already recorded above (e.g. stream-length recovery) is `warn()`-style
    // member state, not rolled back by a later validation failure in the
    // same call (empirically confirmed against qpdf 11.9.0: a candidate
    // needing repair whose `/W` then fails validation still shows the
    // repair warning before the terminal error). The remaining, fallible
    // steps run inside this closure so a failure past this point can still
    // hand `repair_diagnostics` to `error_diagnostics_sink` before this
    // function's own `Err` propagates -- mirroring that same qpdf ordering
    // without changing what any `error_diagnostics_sink: None` caller
    // observes (the sink is write-only, and only on this closure's `Err`).
    let repair_diagnostics_for_result = repair_diagnostics.clone();
    let build = move || -> Result<LoadedXrefState> {
        let stream = match &object {
            Object::Stream(stream) => stream,
            _ => return Err(Error::parse(xref_pos, "xref not found")),
        };
        // QPDF::read_xrefStream accepts an xref stream only when
        // `isStreamOfType("/XRef")` succeeds. The shared parser owns this
        // check for both direct startxref streams and classic-trailer
        // `/XRefStm` targets.
        if !matches!(
            stream.dict.get("Type"),
            Some(Object::Name(type_name)) if type_name.as_slice() == b"XRef"
        ) {
            return Err(Error::parse(xref_pos, "xref not found"));
        }

        let trailer = stream.dict.clone();
        let size = parse_non_negative_u64(
            trailer
                .get("Size")
                .ok_or(Error::Missing("XRef stream /Size"))?,
            "/Size",
        )?;
        let size = u32::try_from(size).map_err(|_| Error::parse(0, "/Size does not fit u32"))?;

        let widths = parse_xref_widths(&trailer)?;
        let index = parse_xref_index(&trailer, size)?;
        let ranges = build_xref_ranges(index)?;
        let stream_data = filters::decode_stream_data(&stream.dict, &stream.data)?;
        let mut cursor = ByteCursor::new(&stream_data, 0);
        let entries = parse_xref_entries(&mut cursor, size, &ranges, widths)?;
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
        let trailer_references = collect_trailer_references(&trailer);
        let parsed_xref_streams = BTreeMap::from([(object_ref, object)]);

        Ok(LoadedXrefState {
            loaded: LoadedXref {
                version,
                startxref,
                entries: registration.snapshot(),
                trailer,
                last_xref_form: XrefForm::Stream,
                repair_diagnostics: repair_diagnostics_for_result,
            },
            trailer_references,
            parsed_xref_streams,
            header_offset: 0,
            already_reconstructed: false,
        })
    };

    match build() {
        Ok(state) => Ok(state),
        Err(error) => {
            if let Some(sink) = error_diagnostics_sink {
                for diagnostic in repair_diagnostics.entries() {
                    sink.push(diagnostic.clone());
                }
            }
            Err(error)
        }
    }
}

fn xref_file_object_diagnostic(
    object_ref: ObjectRef,
    offset: u64,
    diagnostic: FileObjectDiagnostic,
) -> Diagnostic {
    Diagnostic::warning(
        format!(
            "(object {} {}, offset {}): {}",
            object_ref.number,
            object_ref.generation,
            offset.saturating_add(diagnostic.relative_offset as u64),
            diagnostic.kind.message()
        ),
        Some(offset.saturating_add(diagnostic.relative_offset as u64)),
    )
}

type XrefWidths = (usize, usize, usize);

fn parse_xref_widths(trailer: &Dictionary) -> Result<XrefWidths> {
    let Object::Array(values) = trailer.get("W").ok_or(Error::Missing("XRef stream /W"))? else {
        return Err(Error::parse(0, "/W must be array"));
    };

    if values.len() != 3 {
        return Err(Error::parse(0, "/W must contain three integers"));
    }

    let w0 = parse_usize(parse_non_negative_u64(&values[0], "/W[0]")?, "/W[0]")?;
    let w1 = parse_usize(parse_non_negative_u64(&values[1], "/W[1]")?, "/W[1]")?;
    let w2 = parse_usize(parse_non_negative_u64(&values[2], "/W[2]")?, "/W[2]")?;

    Ok((w0, w1, w2))
}

fn parse_xref_index(trailer: &Dictionary, size: u32) -> Result<Vec<u32>> {
    match trailer.get("Index") {
        None => Ok(vec![0, size]),
        Some(Object::Array(values)) => {
            if values.len() % 2 != 0 {
                return Err(Error::parse(
                    0,
                    "/Index must contain an even number of integers",
                ));
            }

            let mut index = Vec::with_capacity(values.len());
            for value in values {
                let integer = parse_non_negative_u64(value, "/Index")?;
                index.push(
                    integer
                        .try_into()
                        .map_err(|_| Error::parse(0, "xref /Index value must fit u32"))?,
                );
            }
            Ok(index)
        }
        _ => Err(Error::parse(0, "/Index must be array")),
    }
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
    size: u32,
    ranges: &[(u32, u32)],
    widths: XrefWidths,
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
            if start + index >= usize::try_from(size).unwrap_or(usize::MAX) {
                return Err(Error::parse(0, "xref range exceeds /Size"));
            }

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
            match object_type {
                0 => {
                    let _next = field1;
                    let _generation = field2;
                    entries.push(ParsedXrefEntry::Free {
                        object_ref: ObjectRef::new(object_number, 0),
                    });
                }
                1 => {
                    let generation = u16::try_from(field2)
                        .map_err(|_| Error::parse(0, "generation does not fit u16"))?;
                    entries.push(ParsedXrefEntry::Live {
                        object_ref: ObjectRef::new(object_number, generation),
                        entry: XrefEntry::Uncompressed { offset: field1 },
                    });
                }
                2 => {
                    let stream = u32::try_from(field1).map_err(|_| {
                        Error::parse(0, "xref stream object number does not fit u32")
                    })?;
                    let index = u32::try_from(field2)
                        .map_err(|_| Error::parse(0, "xref stream index does not fit u32"))?;
                    entries.push(ParsedXrefEntry::Live {
                        object_ref: ObjectRef::new(object_number, 0),
                        entry: XrefEntry::Compressed { stream, index },
                    });
                }
                _ => {
                    return Err(Error::Unsupported(format!(
                        "unsupported xref entry type {object_type}"
                    )))
                }
            }
        }
    }

    Ok(entries)
}

fn parse_non_negative_u64(value: &Object, name: &str) -> Result<u64> {
    let Object::Integer(integer) = value else {
        return Err(Error::parse(0, format!("{name} is not integer")));
    };
    if *integer < 0 {
        return Err(Error::parse(0, format!("{name} is negative")));
    }
    Ok(*integer as u64)
}

fn parse_usize(value: u64, name: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::parse(0, format!("{name} does not fit usize")))
}

fn parse_header(bytes: &[u8]) -> Result<String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err(Error::parse(0, "missing PDF header"));
    }

    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')
        .unwrap_or(bytes.len());
    let header = std::str::from_utf8(&bytes[5..end])
        .map_err(|_| Error::parse(5, "PDF version is not utf-8"))?;
    Ok(header.to_string())
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
    let Some(pos) = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        return Err(Error::parse(bytes.len(), "can't find startxref"));
    };

    let mut cursor = ByteCursor::new(bytes, pos + marker.len());
    cursor.read_u64()
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
        while matches!(
            self.bytes.get(self.pos),
            Some(b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
        ) {
            self.pos += 1;
        }
    }

    fn skip_line(&mut self) {
        while !matches!(self.bytes.get(self.pos), None | Some(b'\n' | b'\r')) {
            self.pos += 1;
        }
        while matches!(self.bytes.get(self.pos), Some(b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn read_byte(&mut self) -> Result<u8> {
        let Some(byte) = self.bytes.get(self.pos).copied() else {
            return Err(Error::parse(self.pos, "unexpected end of input"));
        };
        self.pos += 1;
        Ok(byte)
    }

    fn read_u32(&mut self) -> Result<u32> {
        let token = self.read_token()?;
        parse_xref_subsection_u32(&token)
    }

    fn read_u64(&mut self) -> Result<u64> {
        let token = self.read_token()?;
        if !token.is_integer() {
            return Err(Error::parse(token.start, "expected unsigned integer"));
        }
        let text = std::str::from_utf8(&token.value)
            .map_err(|_| Error::parse(token.start, "number is not utf-8"))?;
        text.parse::<u64>()
            .map_err(|_| Error::parse(token.start, "invalid unsigned integer"))
    }

    fn read_token(&mut self) -> Result<Token> {
        let mut tokenizer = Tokenizer::new(self.bytes);
        tokenizer.allow_eof();
        tokenizer.set_position(self.pos)?;
        let token = tokenizer.read_token(false, 0)?;
        self.pos = tokenizer.position();
        Ok(token)
    }

    fn read_fixed_u64(&mut self, width: usize) -> Result<u64> {
        self.read_fixed(width)?
            .parse::<u64>()
            .map_err(|_| Error::parse(self.pos, "invalid fixed-width u64"))
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

    fn read_fixed_u16(&mut self, width: usize) -> Result<u16> {
        self.read_fixed(width)?
            .parse::<u16>()
            .map_err(|_| Error::parse(self.pos, "invalid fixed-width u16"))
    }

    fn read_fixed(&mut self, width: usize) -> Result<&str> {
        if self.pos + width > self.bytes.len() {
            return Err(Error::parse(
                self.pos,
                "unexpected end of fixed-width field",
            ));
        }
        let text = std::str::from_utf8(&self.bytes[self.pos..self.pos + width])
            .map_err(|_| Error::parse(self.pos, "field is not utf-8"))?;
        self.pos += width;
        Ok(text)
    }
}

fn parse_xref_subsection_u32(token: &Token) -> Result<u32> {
    if !token.is_integer() || !token.value.iter().all(u8::is_ascii_digit) {
        return Err(Error::parse(token.start, "expected unsigned integer"));
    }
    let value = std::str::from_utf8(&token.value)
        .map_err(|_| Error::parse(token.start, "number is not utf-8"))?
        .parse::<u64>()
        .map_err(|_| Error::parse(token.start, "invalid unsigned integer"))?;
    u32::try_from(value).map_err(|_| Error::parse(token.start, "number does not fit u32"))
}

#[cfg(test)]
mod tests {
    use super::{
        append_xref_size_warning, find_xref_stream_trailer_candidate,
        load_xref_and_trailer_with_repair, load_xref_state_with_options,
        prepend_repair_diagnostics, recover_trailer_from_xref_stream_candidate, LoadedXref,
        XrefForm, XrefLoadOptions, XrefRegistration,
    };
    use crate::{Diagnostic, Diagnostics, Dictionary, Object, ObjectRef, XrefEntry};
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Cursor;

    #[test]
    fn xref_registration_preserves_exact_generations_and_live_precedence() {
        let mut registration = XrefRegistration::default();
        let object_zero = ObjectRef::new(7, 0);
        let object_two = ObjectRef::new(7, 2);

        registration.insert_xref_entry(object_zero, XrefEntry::Uncompressed { offset: 10 });
        registration.insert_free_xref_entry(object_zero);
        registration.insert_xref_entry(object_two, XrefEntry::Uncompressed { offset: 20 });

        assert!(registration.entries.contains_key(&object_zero));
        assert!(registration.entries.contains_key(&object_two));
        assert!(registration.deleted_objects.is_empty());
    }

    #[test]
    fn xref_registration_free_object_suppresses_later_generations() {
        let mut registration = XrefRegistration::default();
        let object_zero = ObjectRef::new(8, 0);
        let object_two = ObjectRef::new(8, 2);

        registration.insert_free_xref_entry(object_zero);
        registration.insert_xref_entry(object_two, XrefEntry::Uncompressed { offset: 30 });

        assert!(!registration.entries.contains_key(&object_zero));
        assert!(!registration.entries.contains_key(&object_two));
        assert_eq!(
            registration
                .deleted_objects
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [8]
        );
    }

    #[test]
    fn xref_size_warning_is_skipped_without_a_declared_size() {
        let mut loaded = LoadedXref {
            version: "1.7".to_string(),
            startxref: 0,
            entries: BTreeMap::new(),
            trailer: Dictionary::new(),
            last_xref_form: XrefForm::Table,
            repair_diagnostics: Diagnostics::default(),
        };

        append_xref_size_warning(&mut loaded, &BTreeSet::from([5]));

        assert!(loaded.repair_diagnostics.entries().is_empty());
    }

    #[test]
    fn initial_repair_diagnostics_precede_recovered_diagnostics() {
        let mut recovered = Diagnostics::default();
        recovered.push(Diagnostic::warning("recovered", Some(12)));
        let mut initial = Diagnostics::default();
        initial.push(Diagnostic::warning("initial", None));

        prepend_repair_diagnostics(&mut recovered, initial);

        assert_eq!(
            recovered
                .entries()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            ["initial", "recovered"]
        );
    }

    #[test]
    fn failed_repair_retains_qpdf_warning_sequence() {
        let mut input = Cursor::new(b"%PDF-1.7\nstartxref\n0\n%%EOF\n");
        let error =
            load_xref_and_trailer_with_repair(&mut input, true).expect_err("repair must fail");
        let (source, diagnostics) = error
            .open_failure()
            .expect("repair failure carries diagnostics");
        let entries = diagnostics.entries();

        assert_eq!(
            entries
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "file is damaged",
                "can't find startxref",
                "Attempting to reconstruct cross-reference table",
            ]
        );
        assert_eq!(entries[1].offset, None);
        assert!(source
            .to_string()
            .contains("unable to find trailer dictionary while recovering damaged file"));
    }

    #[test]
    fn empty_entry_repair_retains_qpdf_warning_sequence() {
        let mut input =
            Cursor::new(b"%PDF-1.7\ntrailer\n<< /Size 1 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n");
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("qpdf accepts a recovered trailer with no xref entries");

        assert!(loaded.entries.is_empty());
        assert_eq!(
            loaded.trailer.get_ref("Root"),
            Some(crate::ObjectRef::new(1, 0))
        );
        assert_eq!(
            loaded
                .repair_diagnostics
                .entries()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "file is damaged",
                "can't find startxref",
                "Attempting to reconstruct cross-reference table",
            ]
        );
    }

    /// Build `"<number> 0 obj\n<< /Type /XRef /W [1 1 1] <extra_dict> /Length N >>\n
    /// stream\n<stream_data>\nendstream\nendobj\n"` -- a minimal `/Type /XRef`
    /// indirect object usable both as a reconstructed type-1 candidate and,
    /// when re-entered, as a real cross-reference stream.
    fn xref_stream_object_bytes(number: u32, extra_dict: &str, stream_data: &[u8]) -> Vec<u8> {
        let mut object = Vec::new();
        object.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        object.extend_from_slice(
            format!(
                "<< /Type /XRef /W [1 1 1] {extra_dict} /Length {} >>\n",
                stream_data.len()
            )
            .as_bytes(),
        );
        object.extend_from_slice(b"stream\n");
        object.extend_from_slice(stream_data);
        object.extend_from_slice(b"\nendstream\nendobj\n");
        object
    }

    #[test]
    fn xref_stream_candidate_never_overrides_an_already_recovered_trailer() {
        // qpdf's `reconstruct_xref` (`QPDF.cc:564-616`) gates both its
        // `trailer` keyword scan and its `/Type /XRef` candidate search on
        // `!m->trailer.isInitialized()`. When the newest revision's own xref
        // parses fine (establishing a trailer) but its `/Prev` chain later
        // breaks, `m->trailer` is already initialized by the time
        // `reconstruct_xref` would run, so qpdf never looks at candidate
        // streams at all -- the already-known trailer (and its `/Root`) is
        // kept untouched.
        //
        // Object 9 (low file offset, high object number) is the real newest
        // revision: valid, `/Marker 111`, `/Prev` pointing nowhere. Object 2
        // (high file offset -- written last -- low object number) is an
        // unrelated, unreferenced stray `/Type /XRef` object with its own
        // clean (no `/Prev`) chain and a different `/Marker 222`.
        // Ascending-object-number order visits object 2 first, and it also
        // has the highest offset, so an unguarded candidate search would let
        // it win both the trailer and the re-entry point -- proving the
        // already-known trailer (111) must be preferred, not looked past.
        let mut bytes = b"%PDF-1.5\n".to_vec();

        let stream_data = [0u8, 0, 0];
        let object9_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(
            9,
            "/Size 10 /Index [9 1] /Marker 111 /Prev 999999",
            &stream_data,
        ));
        bytes.extend(xref_stream_object_bytes(
            2,
            "/Size 10 /Index [2 1] /Marker 222",
            &stream_data,
        ));
        bytes.extend_from_slice(format!("startxref\n{object9_offset}\n%%EOF\n").as_bytes());
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true).expect(
            "the already-parsed newest trailer recovers even though its own /Prev is broken",
        );

        assert_eq!(
            loaded.trailer.get("Marker"),
            Some(&crate::Object::Integer(111)),
            "the already-successfully-parsed trailer must win over any stray /Type /XRef candidate"
        );
        // The winning trailer came from object 9's own real /Type /XRef
        // stream, not a line-scan reconstruction, so `last_xref_form` must
        // reflect that -- `merge_recovered_qpdf_state` must carry over the
        // already-successfully-parsed revision's real form, not leave
        // `recover_xref_from_linear_scan`'s `Table` placeholder in place.
        assert_eq!(loaded.last_xref_form, crate::XrefForm::Stream);
    }

    #[test]
    fn xref_stream_candidate_discovery_recovers_a_mismatched_but_usable_length() {
        // qpdf's candidate discovery reads objects through the exact same
        // repair-capable path as everything else: `getObjectByObjGen` ->
        // `readStream`, which unconditionally retries via
        // `recoverStreamLength` whenever `m->attempt_recovery` is set
        // (`QPDF.cc:1368-1393`) -- and `attempt_recovery` defaults to `true`
        // and is never toggled off partway through `reconstruct_xref`
        // (`QPDF.hh:1461`). There is no qpdf-side notion of "candidate
        // discovery is stricter than the later re-entry."
        //
        // `/Length 3` is a directly-usable positive integer (so it looks
        // authoritative) but the real entry data is 6 bytes; byte 3 is not
        // `endstream`. Only a repair-capable stream read still finds this
        // candidate.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let object_offset = bytes.len() as u64;
        let stream_data = [0u8, 0, 0, 1, object_offset as u8, 0];
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(b"<< /Type /XRef /W [1 1 1] /Size 2 /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true).expect(
            "qpdf recovers the candidate through the same repair-capable stream read \
             it always uses",
        );

        assert_eq!(
            loaded.trailer.get("Type"),
            Some(&crate::Object::Name(b"XRef".to_vec()))
        );
    }

    #[test]
    fn xref_stream_candidate_discovery_diagnostics_survive_a_later_reentry_failure() {
        // Discovery only checks `/Type /XRef` (`is_xref_stream_dict`); it
        // never validates `/W`, so a candidate with a mismatched-but-usable
        // `/Length` (repaired during discovery's own repair-capable read,
        // `QPDF.cc:1350-1393`) can still be accepted as a candidate even
        // though its `/W` is malformed and the *re-entry*'s `parse_xref_stream`
        // (which does validate `/W`) then fails. qpdf already warned about
        // the stream-length recovery at discovery time -- that warning must
        // not vanish just because the later, unrelated re-entry failure is
        // what's ultimately reported.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let object_offset = bytes.len() as u64;
        let stream_data = [0u8, 0, 0, 1, object_offset as u8, 0];
        bytes.extend_from_slice(b"1 0 obj\n");
        // `/W [1 1]` (only two elements) is invalid for `parse_xref_widths`,
        // but discovery never inspects it.
        bytes.extend_from_slice(b"<< /Type /XRef /W [1 1] /Size 2 /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let error = load_xref_and_trailer_with_repair(&mut input, true)
            .expect_err("re-entry fails on the malformed /W array");
        let (source, diagnostics) = error
            .open_failure()
            .expect("repair failure carries diagnostics");

        assert!(source
            .to_string()
            .contains("error decoding candidate xref stream while recovering damaged file"));
        let recovered_length_warnings = diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("recovered stream length"))
            .count();
        // Empirically verified against qpdf 11.9.0 (`--check` on this exact
        // shape): the candidate is read twice -- once by discovery
        // (`getObjectByObjGen`) and once, independently, by re-entry's own
        // `readObjectAtOffset` (`QPDF.cc:956`, before `processXRefStream`
        // validates `/W` and throws) -- so its stream-length repair warns
        // twice, not once. Losing the re-entry warning (as flpdf previously
        // did) under-reports what a real qpdf run would show.
        assert_eq!(
            recovered_length_warnings, 2,
            "the candidate's stream-length repair must warn once at discovery and once at \
             re-entry, matching real qpdf's own double warning for this shape"
        );
    }

    #[test]
    fn xref_stream_candidate_discovery_diagnostics_from_a_non_winning_candidate_survive() {
        // qpdf's candidate search resolves *every* type-1 entry
        // (`getObjectByObjGen(iter.first)` runs before the
        // `isStreamOfType("/XRef")` check, `QPDF.cc:585-589`), so a repair
        // warning fires for any stream object that needs one, independent
        // of whether it ends up being the `max_offset` winner. Object 2
        // (lower object number, lower offset, needs stream-length repair)
        // never becomes the winner -- object 9 (higher offset, clean) does,
        // and its own re-entry succeeds cleanly -- so object 2's warning has
        // no other path to the caller and must come from discovery itself.
        let mut bytes = b"%PDF-1.5\n".to_vec();

        let candidate2_offset = bytes.len() as u64;
        let mismatched_stream_data = [0u8, 0, 0, 1, candidate2_offset as u8, 0];
        bytes.extend_from_slice(b"2 0 obj\n");
        bytes.extend_from_slice(b"<< /Type /XRef /W [1 1 1] /Size 10 /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&mismatched_stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let candidate9_offset = bytes.len() as u64;
        let clean_stream_data = [1u8, candidate9_offset as u8, 0];
        bytes.extend(xref_stream_object_bytes(
            9,
            "/Size 10 /Index [9 1]",
            &clean_stream_data,
        ));
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("object 9's clean re-entry recovers the document");

        assert!(
            loaded
                .repair_diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("recovered stream length")),
            "the non-winning candidate's own discovery-time repair warning must still surface"
        );
    }

    #[test]
    fn xref_stream_candidate_reentry_diagnostics_are_preserved() {
        // qpdf's `read_xref(max_offset)` re-entry (`QPDF.cc:601`) runs
        // through the same `warn()`-emitting machinery as any other xref
        // read; those warnings are not qpdf-internal bookkeeping, they are
        // part of the same observable warning sequence
        // `push_repair_diagnostics` and `merge_previous_xref_sections`
        // (`QPDF.cc` `/Prev` handling) already propagate for every other
        // xref source. Dropping `reentry.loaded.repair_diagnostics` here
        // would hide, from library and CLI consumers, exactly the kind of
        // stream-length-recovery warning this same mismatched-but-usable
        // `/Length` triggers.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let object_offset = bytes.len() as u64;
        let stream_data = [0u8, 0, 0, 1, object_offset as u8, 0];
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(b"<< /Type /XRef /W [1 1 1] /Size 2 /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("qpdf recovers this candidate through repair-capable stream reading");

        assert!(
            loaded
                .repair_diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("recovered stream length")),
            "the candidate re-entry's own stream-length-recovery warning must surface"
        );
    }

    #[test]
    fn xref_stream_only_repair_recovers_trailer_from_candidate() {
        // qpdf 11.9.0 `reconstruct_xref` (`QPDF.cc:577-608`): no `trailer`
        // keyword exists anywhere in this file, so the only way to recover is
        // to find a reconstructed `/Type /XRef` stream and re-enter
        // `read_xref` at its offset.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let object_offset = bytes.len() as u64;
        // /Size 2: object 0 free, object 1 (this very stream) uncompressed at
        // its own offset. The offset must fit the 1-byte /W field.
        let stream_data = [0u8, 0, 0, 1, object_offset as u8, 0];
        bytes.extend(xref_stream_object_bytes(1, "/Size 2", &stream_data));
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("qpdf recovers the trailer from the reconstructed /XRef stream candidate");

        assert_eq!(
            loaded.trailer.get("Type"),
            Some(&crate::Object::Name(b"XRef".to_vec()))
        );
        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(1, 0)),
            Some(&crate::XrefEntry::Uncompressed {
                offset: object_offset
            })
        );
        // flpdf-specific (no qpdf counterpart -- qpdf's own writer has no
        // incremental-update mode): `startxref` must become the real,
        // verified position of the recovered candidate, not the original
        // corrupt `startxref` value (999999). A subsequent incremental write
        // uses this field as `/Prev`; leaving it at 999999 would produce a
        // `/Prev` that points nowhere and cannot be reopened.
        assert_eq!(loaded.startxref, object_offset);
        // flpdf-specific: `last_xref_form` drives incremental-write shape
        // (`writer.rs:1018,1072,1082`) -- the verified re-entered section is
        // a real `/Type /XRef` stream, so a subsequent incremental write
        // must also emit a stream, not silently downgrade to a classic
        // table and disable object-stream packing.
        assert_eq!(loaded.last_xref_form, crate::XrefForm::Stream);
    }

    #[test]
    fn xref_stream_only_repair_reports_candidate_decode_failure_when_streams_ignored() {
        // qpdf 11.9.0 `read_xrefStream` (`QPDF.cc:951`): with
        // `ignore_xref_streams` set, the re-entry at `max_offset` always
        // fails, so `reconstruct_xref` throws "error decoding candidate xref
        // stream while recovering damaged file" even though a candidate
        // exists (candidate *discovery* does not consult the option).
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let object_offset = bytes.len() as u64;
        let stream_data = [0u8, 0, 0, 1, object_offset as u8, 0];
        bytes.extend(xref_stream_object_bytes(1, "/Size 2", &stream_data));
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let error = load_xref_state_with_options(
            &mut input,
            XrefLoadOptions {
                allow_repair: true,
                ignore_xref_streams: true,
            },
        )
        .expect_err("qpdf cannot decode the candidate xref stream when streams are ignored");
        let (source, _diagnostics) = error
            .open_failure()
            .expect("repair failure carries diagnostics");

        assert!(source
            .to_string()
            .contains("error decoding candidate xref stream while recovering damaged file"));
    }

    #[test]
    fn xref_stream_candidate_discovery_diagnostics_survive_when_no_candidate_exists_at_all() {
        // qpdf's candidate search resolves every type-1 entry unconditionally
        // (`QPDF.cc:585-589`), regardless of whether *any* of them turns out
        // to be a `/Type /XRef` stream. A non-XRef stream that needs
        // stream-length repair still warns even when the whole search
        // ultimately finds no candidate at all (`trailer.map` returning
        // `None` must not silently drop diagnostics collected along the
        // way).
        let mut bytes = b"%PDF-1.5\n".to_vec();
        // Not /Type /XRef -- an ordinary stream object that still needs its
        // own stream-length repair (mismatched but usable /Length).
        let stream_data = *b"abcdef";
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(b"<< /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let error = load_xref_and_trailer_with_repair(&mut input, true)
            .expect_err("no /Type /XRef candidate exists anywhere in this file");
        let (source, diagnostics) = error
            .open_failure()
            .expect("repair failure carries diagnostics");

        assert!(source
            .to_string()
            .contains("unable to find trailer dictionary while recovering damaged file"));
        assert!(
            diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("recovered stream length")),
            "a non-candidate stream's own repair warning must survive even when no \
             /Type /XRef candidate is ever found"
        );
    }

    #[test]
    fn xref_stream_candidate_discovery_diagnostics_stay_in_ascending_object_number_order() {
        // Empirically verified against qpdf 11.9.0 (`--check` on this exact
        // shape): with object 1 (lower number, visited first, but written
        // *last* so it has the higher offset and wins `max_offset`) and
        // object 2 (higher number, visited second, written first so it has
        // the lower offset and never wins) both needing stream-length
        // repair, qpdf's real warning sequence is object 1's own discovery
        // warning, THEN object 2's, THEN object 1's *separate* re-entry
        // warning (re-entry re-reads the winner independently of discovery,
        // `read_xrefStream` -> `readObjectAtOffset`, `QPDF.cc:956`, which
        // does not consult the object cache the way discovery's
        // `getObjectByObjGen` does) -- discovery warnings interleave in
        // scan order regardless of winner status, and the winner's own
        // re-entry warning always comes last, after the whole scan.
        let mut bytes = b"%PDF-1.5\n".to_vec();

        // Object 2 (non-winner): written first, so it has the lower offset.
        // /Size 2, default /Index [0 2]: object 0 free, object 2 itself
        // uncompressed at its own offset. Six real stream bytes; /Length 3
        // (directly usable but mismatched) forces bounded recovery.
        let off2 = bytes.len() as u32;
        let stream2 = [0u8, 0, 0, 1, off2 as u8, 0];
        bytes.extend_from_slice(b"2 0 obj\n");
        bytes.extend_from_slice(b"<< /Type /XRef /W [1 1 1] /Size 2 /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&stream2);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        // Object 1 (winner): written second, so it has the higher offset,
        // but its lower object number puts it first in ascending scan order.
        let off1 = bytes.len() as u32;
        let stream1 = [0u8, 0, 0, 1, off1 as u8, 0];
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(b"<< /Type /XRef /W [1 1 1] /Size 2 /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&stream1);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("object 1 wins and its re-entry recovers the document");

        let messages: Vec<&str> = loaded
            .repair_diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("recovered stream length"))
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert_eq!(
            messages.len(),
            3,
            "object 1 warns once at discovery and once at re-entry, object 2 warns once \
             at discovery; got {messages:?}"
        );
        assert!(
            messages[0].contains("object 1"),
            "object 1's own discovery warning must come first (lower object number, \
             visited first in ascending scan order); got {messages:?}"
        );
        assert!(
            messages[1].contains("object 2"),
            "object 2's discovery warning must come next, in scan order; got {messages:?}"
        );
        assert!(
            messages[2].contains("object 1"),
            "the winner's own re-entry warning must come last, after the whole scan; \
             got {messages:?}"
        );
    }

    #[test]
    fn xref_stream_candidate_diagnostics_are_preserved_when_its_own_prev_chain_fails() {
        // A companion to `xref_stream_candidate_reentry_diagnostics_are_preserved`,
        // for the *failure* path: the candidate's own initial parse succeeds
        // (and, since `/Length 3` is a directly-usable-but-mismatched
        // integer, emits a "recovered stream length" repair diagnostic) but
        // its own `/Prev` chain then fails to decode. qpdf's `warn()` calls
        // append to `m->warnings` as they happen and are never rolled back
        // by a later exception in the same `read_xref` call, so those
        // already-emitted warnings must still surface even though the
        // overall candidate recovery ultimately fails.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let object_offset = bytes.len() as u64;
        let stream_data = [0u8, 0, 0, 1, object_offset as u8, 0];
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(
            b"<< /Type /XRef /W [1 1 1] /Size 2 /Prev 999998 /Length 3 >>\nstream\n",
        );
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let error = load_xref_and_trailer_with_repair(&mut input, true)
            .expect_err("the candidate's own /Prev chain fails to decode");
        let (source, diagnostics) = error
            .open_failure()
            .expect("repair failure carries diagnostics");

        assert!(source
            .to_string()
            .contains("error decoding candidate xref stream while recovering damaged file"));
        assert!(
            diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("recovered stream length")),
            "the candidate's own pre-failure repair warning must survive"
        );
    }

    #[test]
    fn xref_stream_candidate_trailer_prefers_lowest_objgen_over_highest_offset() {
        // qpdf 11.9.0 `reconstruct_xref` (`QPDF.cc:592-597`): `setTrailer`
        // only ever takes effect once, so the placeholder trailer set inside
        // the max-offset-tracking loop is the *first* candidate encountered
        // while iterating `m->xref_table` -- a `std::map<QPDFObjGen, _>`, so
        // that's ascending *object number* order, not ascending *offset*
        // order, and the two are deliberately made to disagree here:
        //   object 9  (written 1st -> lowest offset,  highest object number)
        //   object 2  (written 2nd -> middle offset,  lowest object number)
        //   object 15 (written 3rd -> highest offset, highest object number)
        // Ascending object-number order visits 2, then 9, then 15, so object
        // 2's dictionary wins the trailer even though it is neither the
        // first-written nor the lowest-offset candidate. `max_offset` (and
        // therefore the re-entry point) is still the true maximum, object
        // 15 -- object 5 is only visible through *its* real cross-reference
        // stream data (it has no `"N G obj"` header of its own), so its
        // presence in the recovered entries proves the re-entry used object
        // 15 while the trailer came from object 2.
        let mut bytes = b"%PDF-1.5\n".to_vec();

        let stream_data = [0u8, 0, 0];
        bytes.extend(xref_stream_object_bytes(
            9,
            "/Size 1 /Marker 999",
            &stream_data,
        ));
        bytes.extend(xref_stream_object_bytes(
            2,
            "/Size 1 /Marker 222",
            &stream_data,
        ));

        let last_stream_data = [2u8, 9, 0];
        let object15_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(
            15,
            "/Size 6 /Index [5 1] /Marker 555",
            &last_stream_data,
        ));

        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("qpdf recovers the trailer and re-enters the highest-offset candidate");

        assert_eq!(
            loaded.trailer.get("Marker"),
            Some(&crate::Object::Integer(222))
        );
        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(5, 0)),
            Some(&crate::XrefEntry::Compressed {
                stream: 9,
                index: 0
            })
        );
        // `startxref` becomes the re-entry point (object 15's real offset,
        // the true max), not the trailer-winning candidate's own offset
        // (object 2) and not the original corrupt value (999999) -- a
        // subsequent incremental write's `/Prev` must land somewhere real.
        assert_eq!(loaded.startxref, object15_offset);
    }

    #[test]
    fn xref_stream_candidate_recovers_despite_in_stream_header_truncating_window() {
        // Mirrors `best_effort_recovers_objstm_truncated_by_in_stream_header`'s
        // technique: the candidate's own stream payload contains a line that
        // looks like an indirect-object header ("9 0 obj"), so the line scan
        // records a bogus entry at that in-stream offset, which becomes the
        // candidate's window end and truncates the bounded parse before the
        // real `endstream`/`endobj`. `RecoveryPolicy::Bounded` (used for
        // candidate discovery, see `xref_stream_candidate_discovery_recovers_a_mismatched_but_usable_length`)
        // never hard-fails on a missing terminator: with no `endstream`
        // inside the truncated window, it falls through to treating the
        // window's remaining bytes as the stream's raw data, so the windowed
        // parse still succeeds directly here (garbage stream payload, but the
        // dict itself, including `/Type /XRef`, survives intact) -- the
        // unbounded fallback below is not what recovers this particular
        // fixture. `/W [0 1 1]` (the type field defaults to 1 and consumes no
        // byte) lets the raw entry bytes double as this ASCII text without
        // any entry decoding as an invalid type, unlike `/W [1 1 1]` where
        // the leading '7' byte (0x37) would itself be read as an
        // out-of-range entry type.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let stream_data = b"7 0\n9 0 obj\n";
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /XRef /W [0 1 1] /Size 6 /Length {} >>\n",
                stream_data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"stream\n");
        bytes.extend_from_slice(stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("the truncated candidate is still recovered as a /Type /XRef stream");

        assert_eq!(
            loaded.trailer.get("Type"),
            Some(&crate::Object::Name(b"XRef".to_vec()))
        );
    }

    #[test]
    fn find_xref_stream_trailer_candidate_falls_back_when_window_truncates_the_object_header() {
        // Unlike the stream-payload truncation above, a window too small to
        // hold even the "N G obj" header itself is a genuine parse error
        // under any `RecoveryPolicy` (including `Bounded`) -- there is no
        // stream-boundary leniency to fall back on before a `stream` keyword
        // is ever reached. Driving `find_xref_stream_trailer_candidate`
        // directly (as `..._skips_compressed_and_free_entries` does) makes
        // this easy to force: a same-number-space decoy entry 5 bytes into
        // the real candidate's own span becomes its window end.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let candidate_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(3, "/Size 1", &[0u8, 0, 0]));

        let mut entries = BTreeMap::new();
        entries.insert(
            ObjectRef::new(3, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset,
            },
        );
        entries.insert(
            ObjectRef::new(9, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset + 5,
            },
        );

        let (candidate, _diagnostics) = find_xref_stream_trailer_candidate(&bytes, &entries);
        let candidate = candidate
            .expect("the unbounded retry still finds the candidate past its truncated window");
        assert_eq!(candidate.max_offset, candidate_offset);
        assert_eq!(
            candidate.trailer.get("Type"),
            Some(&Object::Name(b"XRef".to_vec()))
        );
    }

    #[test]
    fn find_xref_stream_trailer_candidate_survives_many_unrelated_truncated_entries() {
        // The window-truncation fallback must not be a single budget shared
        // across the whole scan: 64 unrelated entries (object numbers 1-64,
        // visited first in ascending order) that each need a fallback retry
        // must not exhaust a real /Type /XRef candidate's own retry (object
        // 200, visited last, its own window truncated the same way as
        // `..._falls_back_when_window_truncates_the_object_header`). qpdf's
        // per-object recovery has no such shared budget at all.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let mut entries = BTreeMap::new();

        // 64 decoys: single non-whitespace bytes (so a tokenizer can't skip
        // past one into whatever follows), each entry's window bounded by
        // the next decoy's offset (1 byte), so every one fails to parse
        // even a bare "N G obj" header and needs (and gets denied, under
        // the old shared-counter code) a wider retry.
        for number in 1..=64u32 {
            let offset = bytes.len() as u64;
            bytes.push(b'X');
            entries.insert(
                ObjectRef::new(number, 0),
                XrefEntry::Uncompressed { offset },
            );
        }

        let candidate_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(200, "/Size 1", &[0u8, 0, 0]));
        entries.insert(
            ObjectRef::new(200, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset,
            },
        );
        // Truncates candidate 200's own window, exactly like the existing
        // single-decoy truncation test, forcing it to need its own fallback.
        entries.insert(
            ObjectRef::new(201, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset + 5,
            },
        );

        let (candidate, _diagnostics) = find_xref_stream_trailer_candidate(&bytes, &entries);
        let candidate = candidate.expect(
            "64 unrelated entries needing their own fallback must not deny \
             candidate 200's fallback, visited last in ascending object-number order",
        );
        assert_eq!(candidate.max_offset, candidate_offset);
    }

    #[test]
    fn xref_stream_candidate_free_entry_blocks_objstm_gap_filler_resurrection() {
        // `recover_objstm_compressed_entries` has no qpdf counterpart at all
        // (`QPDF.cc:611-614` explicitly declines to scan ObjStm contents
        // during reconstruction: "probably not worth the trouble"); it runs
        // unconditionally, after the candidate re-entry, over every
        // `Uncompressed` entry currently in `entries`. If the candidate
        // re-entry's own `Free` rows were dropped instead of recorded as
        // tombstones, that gap-filler would find object 8's real offset,
        // decode it as an `/Type /ObjStm`, and wrongly resurrect object 7 --
        // even though the candidate's own revision chain explicitly frees it.
        //
        // Revision 1 (xref stream 3): object 7 packed compressed in ObjStm 8.
        // Revision 2 (xref stream 4, `/Prev` -> revision 1, the recovery
        // candidate): object 7 marked free. Verified against real qpdf
        // 11.9.0 `--show-xref` on this exact shape: object 7 is absent from
        // the reconstructed table (objects 1, 3, 4, 8 recovered).
        fn entry(entry_type: u8, f1: u32, f2: u8) -> [u8; 4] {
            let f1_bytes = f1.to_be_bytes();
            [entry_type, f1_bytes[1], f1_bytes[2], f2]
        }

        let mut bytes = b"%PDF-1.7\n".to_vec();

        let off1 = bytes.len() as u32;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let off8 = bytes.len() as u32;
        let objstm_header = b"7 0\n";
        let objstm_body = b"<< /Foo /Bar >>\n";
        let mut objstm_payload = objstm_header.to_vec();
        objstm_payload.extend_from_slice(objstm_body);
        bytes.extend_from_slice(b"8 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /ObjStm /N 1 /First {} /Length {} >>\n",
                objstm_header.len(),
                objstm_payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"stream\n");
        bytes.extend_from_slice(&objstm_payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let off3 = bytes.len() as u32;
        let mut rev1_entries = Vec::new();
        rev1_entries.extend(entry(0, 0, 0)); // 0 free
        rev1_entries.extend(entry(1, off1, 0)); // 1
        rev1_entries.extend(entry(1, off3, 0)); // 3 = this stream
        rev1_entries.extend(entry(2, 8, 0)); // 7 compressed in objstm 8, index 0
        rev1_entries.extend(entry(1, off8, 0)); // 8 = the objstm
        bytes.extend_from_slice(b"3 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /XRef /W [1 2 1] /Size 9 /Index [0 1 1 1 3 1 7 1 8 1] /Length {} >>\n",
                rev1_entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"stream\n");
        bytes.extend_from_slice(&rev1_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let off4 = bytes.len() as u32;
        let mut rev2_entries = Vec::new();
        rev2_entries.extend(entry(1, off4, 0)); // 4 = this stream
        rev2_entries.extend(entry(0, 0, 0)); // 7 now free
        bytes.extend_from_slice(b"4 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /XRef /W [1 2 1] /Size 9 /Prev {off3} /Index [4 1 7 1] /Length {} >>\n",
                rev2_entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"stream\n");
        bytes.extend_from_slice(&rev2_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("qpdf recovers the trailer from the reconstructed /XRef stream candidate");

        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(7, 0)),
            None,
            "a real free entry from the candidate's own revision chain must block the \
             ObjStm gap-filler, matching qpdf leaving object 7 unresolvable -- `entries` only \
             ever holds live rows (`XrefRegistration` never gives a free row a map entry), so \
             a blocked object is simply absent, not a stored `Free` tombstone"
        );
        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(8, 0)),
            Some(&crate::XrefEntry::Uncompressed {
                offset: off8 as u64
            }),
        );
    }

    #[test]
    fn xref_stream_candidate_free_entry_blocks_by_object_number_not_exact_generation() {
        // qpdf's `processXRefStream` (`QPDF.cc:1120-1124`) hardcodes
        // `QPDFObjGen(obj, 0)` for a type-0 row and explicitly discards
        // field 2 ("Ignore fields[2], which we don't care about in this
        // case"); `insertFreeXrefEntry` (`QPDF.cc:1187-1190`) then records
        // only the object *number* in `m->deleted_objects`
        // (`std::set<int>`, `QPDF.hh:1466`) -- generation plays no part in
        // qpdf's free/deleted bookkeeping. A real xref stream commonly
        // writes a nonzero field 2 for a freed object (the generation to
        // use *if the number is reused*), so this fixture -- identical to
        // `xref_stream_candidate_free_entry_blocks_objstm_gap_filler_resurrection`
        // except object 7's free row now carries generation 1 -- must
        // still block the ObjStm gap-filler, which always probes generation
        // 0. Blocking only the exact `(7, 1)` key (as a naive tombstone
        // merge would) leaves `(7, 0)` open to resurrection.
        fn entry(entry_type: u8, f1: u32, f2: u8) -> [u8; 4] {
            let f1_bytes = f1.to_be_bytes();
            [entry_type, f1_bytes[1], f1_bytes[2], f2]
        }

        let mut bytes = b"%PDF-1.7\n".to_vec();

        let off1 = bytes.len() as u32;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let off8 = bytes.len() as u32;
        let objstm_header = b"7 0\n";
        let objstm_body = b"<< /Foo /Bar >>\n";
        let mut objstm_payload = objstm_header.to_vec();
        objstm_payload.extend_from_slice(objstm_body);
        bytes.extend_from_slice(b"8 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /ObjStm /N 1 /First {} /Length {} >>\n",
                objstm_header.len(),
                objstm_payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"stream\n");
        bytes.extend_from_slice(&objstm_payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let off3 = bytes.len() as u32;
        let mut rev1_entries = Vec::new();
        rev1_entries.extend(entry(0, 0, 0)); // 0 free
        rev1_entries.extend(entry(1, off1, 0)); // 1
        rev1_entries.extend(entry(1, off3, 0)); // 3 = this stream
        rev1_entries.extend(entry(2, 8, 0)); // 7 compressed in objstm 8, index 0
        rev1_entries.extend(entry(1, off8, 0)); // 8 = the objstm
        bytes.extend_from_slice(b"3 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /XRef /W [1 2 1] /Size 9 /Index [0 1 1 1 3 1 7 1 8 1] /Length {} >>\n",
                rev1_entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"stream\n");
        bytes.extend_from_slice(&rev1_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let off4 = bytes.len() as u32;
        let mut rev2_entries = Vec::new();
        rev2_entries.extend(entry(1, off4, 0)); // 4 = this stream
        rev2_entries.extend(entry(0, 0, 1)); // 7 now free, next-use generation 1
        bytes.extend_from_slice(b"4 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /XRef /W [1 2 1] /Size 9 /Prev {off3} /Index [4 1 7 1] /Length {} >>\n",
                rev2_entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"stream\n");
        bytes.extend_from_slice(&rev2_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("qpdf recovers the trailer from the reconstructed /XRef stream candidate");

        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(7, 0)),
            None,
            "object number 7 must stay unresolvable at generation 0, matching qpdf's \
             number-only deleted_objects bookkeeping, regardless of which generation the \
             free row's own tombstone landed at"
        );
        assert!(
            !matches!(
                loaded.entries.get(&crate::ObjectRef::new(7, 1)),
                Some(crate::XrefEntry::Compressed { .. })
            ),
            "object 7 must not be resurrected as compressed at any generation"
        );
    }

    #[test]
    fn recover_trailer_from_xref_stream_candidate_carries_prev_chain_trailer_references() {
        // A successful candidate re-entry can walk `/Prev` through older
        // trailers via `merge_previous_xref_sections`, which accumulates
        // `trailer_references` from each one (`QPDF.cc`'s `/Prev` handling
        // has no separate concept -- dangling-reference discovery walks the
        // same trailer chain). Object 99 here is referenced *only* by the
        // older revision's own `/Info` key, unreachable any other way, so
        // its presence in the returned set proves the re-entry's own
        // `trailer_references` were carried out, not just the winning
        // trailer's direct keys.
        let mut bytes = b"%PDF-1.5\n".to_vec();

        let older_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(
            2,
            "/Size 100 /Index [2 1] /Info 99 0 R",
            &[1u8, older_offset as u8, 0],
        ));

        let candidate_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(
            9,
            &format!("/Size 100 /Index [9 1] /Prev {older_offset}"),
            &[1u8, candidate_offset as u8, 0],
        ));

        let mut entries = BTreeMap::new();
        entries.insert(
            ObjectRef::new(9, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset,
            },
        );
        let mut parsed_xref_streams = BTreeMap::new();
        let mut repair_diagnostics = Diagnostics::default();
        let mut trailer_references = BTreeSet::new();
        let mut deleted_object_numbers = BTreeSet::new();
        let options = XrefLoadOptions {
            allow_repair: true,
            ..XrefLoadOptions::default()
        };

        let (_trailer, max_offset, form) = recover_trailer_from_xref_stream_candidate(
            &bytes,
            "1.5",
            options,
            &mut entries,
            &mut parsed_xref_streams,
            &mut repair_diagnostics,
            &mut trailer_references,
            &mut deleted_object_numbers,
        )
        .expect("candidate re-enters and follows its own /Prev chain");

        assert_eq!(max_offset, candidate_offset);
        assert_eq!(form, XrefForm::Stream);
        assert!(
            trailer_references.contains(&ObjectRef::new(99, 0)),
            "the /Prev chain's own trailer references must be carried out; got {trailer_references:?}"
        );
    }

    #[test]
    fn find_xref_stream_trailer_candidate_skips_compressed_and_free_entries() {
        // `entries` passed to this function only ever holds `Uncompressed`
        // entries in production (the line scan's own output), so this drives
        // it directly to prove the `Compressed`/`Free` branches -- mirroring
        // qpdf's `entry.getType() != 1 { continue; }` -- do not crash or get
        // mistaken for a candidate if that ever changes.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let candidate_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(3, "/Size 1", &[0u8, 0, 0]));

        let mut entries = BTreeMap::new();
        entries.insert(ObjectRef::new(1, 0), XrefEntry::Free { next: 0 });
        entries.insert(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 9,
                index: 0,
            },
        );
        entries.insert(
            ObjectRef::new(3, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset,
            },
        );

        let (candidate, _diagnostics) = find_xref_stream_trailer_candidate(&bytes, &entries);
        let candidate =
            candidate.expect("the lone Uncompressed entry is still found as a candidate");
        assert_eq!(candidate.max_offset, candidate_offset);
        assert_eq!(
            candidate.trailer.get("Type"),
            Some(&Object::Name(b"XRef".to_vec()))
        );
    }

    #[test]
    fn find_xref_stream_trailer_candidate_preserves_diagnostics_from_non_stream_discovery_reads() {
        // qpdf's `getObjectByObjGen(iter.first)` (`QPDF.cc:585`) resolves
        // *every* type-1 entry unconditionally, before the
        // `isStreamOfType("/XRef")` check (`QPDF.cc:587`) ever runs -- and
        // `readObject` warns "expected endobj" (`QPDF.cc:1352-1355`) for any
        // object kind, not just streams. A reconstructed entry that parses
        // to something other than a stream (a plain array here) but is
        // missing its own `endobj` must still surface that warning; it must
        // not be silently dropped just because it isn't the `Object::Stream`
        // this function is ultimately searching for.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let non_stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n[6 0 R]\nnot-endobj\n");
        let candidate_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(2, "/Size 1", &[0u8, 0, 0]));

        let mut entries = BTreeMap::new();
        entries.insert(
            ObjectRef::new(1, 0),
            XrefEntry::Uncompressed {
                offset: non_stream_offset,
            },
        );
        entries.insert(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset,
            },
        );

        let (candidate, discovery_diagnostics) =
            find_xref_stream_trailer_candidate(&bytes, &entries);
        candidate.expect("object 2 is still found as the /Type /XRef candidate");
        assert!(
            discovery_diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("expected endobj")),
            "object 1's own missing-endobj warning must survive even though it isn't a stream"
        );
    }
}
