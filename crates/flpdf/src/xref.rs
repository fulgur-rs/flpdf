//! qpdf correspondence: QPDF.cc xref loading and repair.
//!
//! The bootstrap handle route follows qpdf 11.9.0's
//! `QPDF::read_xref`/`read_xrefStream`/`processXRefStream` ordering
//! (`libqpdf/QPDF.cc:626-710,846-1148`): the xref stream is parsed as a live
//! `ObjectHandle` graph, `/Type`/`/W`/`/Index`/`/Size` are inspected, and only
//! then is the encoded payload passed to the handle-native filter pipeline.
//! The short-lived `BootstrapHandleDocument` supplies the pre-`Pdf` owner
//! that qpdf's `QPDFParser` already has at this point; it is not the canonical
//! post-open resolver owned by `flpdf-25kg.3.5`.
use crate::diagnostics::Diagnostic;
use crate::object_handle::{DocumentResolver, ObjectValue};
use crate::parser::{
    parse_qpdf_direct_object_handle_with_diagnostics,
    parse_qpdf_file_object_handle_with_diagnostics, HandleResolver, ParserDiagnostic,
};
use crate::reader::file_object::{
    finish_file_object_handle, parse_file_object_handle_syntax, parse_file_object_header,
    FileObjectDiagnostic, HandleFileObjectRead, RecoveryPolicy, ResolvedStreamLength,
};
use crate::tokenizer::{Token, TokenType, Tokenizer};
use crate::{filters, Diagnostics, Error, ObjectHandle, ObjectRef, Result, XrefEntry};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::rc::{Rc, Weak};

#[derive(Debug, Clone)]
pub struct LoadedXref {
    pub version: String,
    pub startxref: u64,
    pub entries: BTreeMap<ObjectRef, XrefEntry>,
    pub trailer: ObjectHandle,
    pub last_xref_form: XrefForm,
    pub repair_diagnostics: Diagnostics,
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
                handle.disconnect();
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
    /// qpdf's `m->first_xref_item_offset`, populated while reading the xref
    /// section and consumed later by `checkLinearizationInternal`.
    pub(crate) first_xref_item_offset: u64,
    pub(crate) trailer_references: BTreeSet<ObjectRef>,
    pub(crate) parsed_xref_streams: BTreeMap<ObjectRef, ObjectHandle>,
    /// Objects resolved while reading xref streams stay available to the
    /// post-chain trailer validation, matching qpdf's shared object cache.
    pub(crate) bootstrap_cache: SharedBootstrapCache,
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
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    parsed_xref_streams: &mut BTreeMap<ObjectRef, ObjectHandle>,
) {
    let mut previous: Option<ObjectRef> = None;
    let mut lower_generations = Vec::new();
    for &object_ref in entries.keys() {
        if let Some(previous_ref) = previous {
            if previous_ref.number == object_ref.number {
                lower_generations.push(previous_ref);
            }
        }
        previous = Some(object_ref);
    }
    for object_ref in lower_generations {
        entries.remove(&object_ref);
        parsed_xref_streams.remove(&object_ref);
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

/// The owner that qpdf uses while an xref section is still being read.
///
/// This is deliberately separate from the post-bootstrap `ResolverCore`.
/// qpdf's `read_xrefStream` can dereference `/Type`, `/Length`, and other
/// dictionary values before the canonical document resolver exists. The
/// three construction sites mirror the three qpdf call-order contexts:
/// reconstruction's complete line-scan table, the current classic/hybrid
/// section, and a section reached through `/Prev`.
#[derive(Debug, Clone, Copy)]
enum XrefReadContextSpec<'a> {
    ActiveSection,
    ActiveSectionWithCache {
        bootstrap_cache: &'a SharedBootstrapCache,
    },
    Reconstruction {
        line_scan_entries: &'a BTreeMap<ObjectRef, XrefEntry>,
    },
    ReconstructionWithCache {
        line_scan_entries: &'a BTreeMap<ObjectRef, XrefEntry>,
        bootstrap_cache: &'a SharedBootstrapCache,
    },
}

/// The qpdf description passed to `readObjectAtOffset` for a bootstrap object.
/// Ordinary resolution uses an empty description, while
/// `QPDF::read_xrefStream` passes `"xref stream"` so file-object warnings carry
/// the same prefix (`QPDF.cc:949-963,1298-1313`).
#[derive(Debug, Clone, Copy)]
enum XrefObjectDescription {
    Ordinary,
    XrefStream,
}

impl XrefObjectDescription {
    fn warning_prefix(self) -> &'static str {
        match self {
            Self::Ordinary => "",
            Self::XrefStream => "xref stream: ",
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

/// The short-lived document context used while qpdf is reading an xref
/// stream. It exists before the post-open `ResolverCore`, but it still gives
/// every parsed direct child the same weak `DocumentResolver` and gives every
/// `N G R` the same canonical handle slot. The owner and its mutable state are
/// shared through [`BootstrapCache`] for one xref-loading operation, matching
/// qpdf's document-level cache rather than one context's local parse lifetime.
struct BootstrapHandleDocument {
    bytes: Rc<[u8]>,
    entry_lookup: RefCell<BTreeMap<ObjectRef, XrefEntry>>,
    options: XrefLoadOptions,
    state: Rc<RefCell<BootstrapHandleState>>,
    resolver: RefCell<Option<Weak<dyn DocumentResolver>>>,
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
        bytes: &[u8],
        entry_lookup: XrefEntryLookup<'_>,
        options: XrefLoadOptions,
        state: Rc<RefCell<BootstrapHandleState>>,
    ) -> Rc<Self> {
        let document = Rc::new(Self {
            bytes: Rc::from(bytes),
            entry_lookup: RefCell::new(entry_lookup.owned_entries()),
            options,
            state,
            resolver: RefCell::new(None),
        });
        let resolver: Rc<dyn DocumentResolver> = document.clone();
        *document.resolver.borrow_mut() = Some(Rc::downgrade(&resolver));
        document
    }

    fn refresh_entry_lookup(&self, entry_lookup: XrefEntryLookup<'_>) {
        *self.entry_lookup.borrow_mut() = entry_lookup.owned_entries();
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
        self.state
            .borrow_mut()
            .diagnostics
            .push(Diagnostic::warning(message, offset));
    }

    fn push_diagnostic(&self, diagnostic: Diagnostic) {
        self.state.borrow_mut().diagnostics.push(diagnostic);
    }

    fn take_reconstruction_trigger(&self) -> Option<Error> {
        self.state
            .borrow_mut()
            .reconstruction_trigger
            .take()
            .map(|(offset, message)| Error::parse(offset as usize, message))
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
    ) -> Result<HandleFileObjectRead> {
        let mut parser = BootstrapHandleParser {
            document: self,
            description,
        };
        let pending = parse_file_object_handle_syntax(input, &mut parser)?;
        let resolved_length = pending
            .indirect_length_ref()
            .map(|object_ref| self.resolve_length(object_ref));
        let _ = (absolute_offset, description);
        finish_file_object_handle(input, pending, resolved_length, policy)
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
            // `resolve_indirect` above catches every resolution failure and
            // falls back to a warned Null rather than propagating Err, so
            // try_as_integer never actually returns Err for a handle from
            // this resolver; kept only for symmetry with the general
            // ObjectHandle contract, matching the prior `.ok()` fallback.
            Err(_) => ResolvedStreamLength::Missing, // cov:ignore: this resolver's resolve_indirect never propagates Err
        }
    }

    fn read_uncompressed_object(
        &self,
        object_ref: ObjectRef,
        offset: u64,
    ) -> Result<(ObjectValue, i64)> {
        let start = usize::try_from(offset)
            .ok()
            .ok_or_else(|| Error::parse(0, "object offset does not fit usize"))?;
        let input = self
            .bytes
            .get(start..)
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
            .read_file_object(input, offset, policy, XrefObjectDescription::Ordinary)
            .map_err(|error| error.rebase_offset(start))?;
        for diagnostic in &completed.diagnostics {
            self.push_diagnostic(xref_file_object_diagnostic(
                XrefObjectDescription::Ordinary,
                completed.object_ref,
                offset,
                diagnostic.clone(),
            ));
        }
        let parsed_offset = completed.object.get_parsed_offset();
        let _ = completed.remove_included_recovery_eol_for_decryption();
        // cov:ignore-start: the handle parser guarantees an exclusively owned direct top-level value
        let value = completed.object.into_direct_value().ok_or_else(|| {
            Error::Internal(format!(
                "bootstrap object {} {} did not produce a direct value",
                object_ref.number, object_ref.generation
            ))
        })?;
        // cov:ignore-end
        Ok((value.0, parsed_offset))
    }

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
        let stream_dict = stream_handle.as_stream_dict().ok_or_else(|| {
            Error::parse(
                0,
                format!("supposed object stream {stream_number} is not a stream"),
            )
        })?;
        if stream_dict.try_get_key(b"/Type")?.try_as_name()?.as_deref() != Some(b"ObjStm") {
            self.push_warning(
                format!("supposed object stream {stream_number} has wrong type"),
                None,
            );
        }
        let object_count = self.handle_integer(&stream_dict, b"/N", "object stream /N")?;
        let first = self.handle_integer(&stream_dict, b"/First", "object stream /First")?;
        let stream_data = stream_handle
            .as_stream_data()
            .ok_or_else(|| Error::parse(0, "supposed object stream has no data"))?;
        let decoded = filters::decode_stream_data_from_handle(
            &stream_dict,
            &stream_data,
            filters::DecodeLimits::default(),
        )?;

        let mut tokenizer = Tokenizer::new(&decoded);
        let mut members = BTreeMap::new();
        for _ in 0..object_count {
            let object_number = u32::try_from(tokenizer.next_integer()?)
                .map_err(|_| Error::parse(0, "object stream object number is invalid"))?;
            let object_offset = usize::try_from(tokenizer.next_integer()?)
                .map_err(|_| Error::parse(0, "object stream object offset is invalid"))?;
            members.insert(object_number, object_offset);
        }

        for (object_number, object_offset) in members {
            let object_ref = ObjectRef::new(object_number, 0);
            if !matches!(
                self.entry_lookup.borrow().get(&object_ref).copied(),
                Some(XrefEntry::Compressed { stream, .. }) if stream == stream_number
            ) {
                continue;
            }
            let member_start = first
                .checked_add(object_offset)
                .ok_or_else(|| Error::parse(0, "object stream member offset overflow"))?;
            if member_start > decoded.len() {
                return Err(Error::parse(
                    member_start,
                    "object stream member offset is out of range",
                ));
            }
            let mut parser = BootstrapHandleParser {
                document: self,
                description: XrefObjectDescription::Ordinary,
            };
            let parsed = match parse_qpdf_file_object_handle_with_diagnostics(
                &decoded[member_start..],
                i64::try_from(member_start).unwrap_or(i64::MAX),
                Some(i64::try_from(member_start).unwrap_or(i64::MAX)),
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
                    return Err(match error.rebase_offset(member_start) {
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
            for diagnostic in &parsed.diagnostics {
                let offset = member_start.saturating_add(diagnostic.relative_offset);
                self.push_warning(
                    format!(
                        "object stream {stream_number} (object {} 0, offset {offset}): {}",
                        object_ref.number, diagnostic.message
                    ),
                    Some(offset as u64),
                );
            }
            if let Some(empty_offset) = parsed.empty_offset {
                let offset = member_start.saturating_add(empty_offset);
                self.push_warning(
                    format!(
                        "object stream {stream_number} (object {} 0, offset {offset}): empty object treated as null",
                        object_ref.number
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
            let parsed_offset = parsed.parsed_offset;
            // cov:ignore-start: the handle parser guarantees an exclusively owned direct member value
            let value = parsed.value.into_direct_value().ok_or_else(|| {
                Error::Internal(format!(
                    "object stream member {} {} did not produce a direct value",
                    object_ref.number, object_ref.generation
                ))
            })?;
            // cov:ignore-end
            member_handle.set_resolved(value.0);
            member_handle.set_parsed_offset_if_unset(parsed_offset);
        }
        Ok(())
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

impl HandleResolver for BootstrapHandleParser<'_> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.document.handle_for_reference(object_ref)
    }

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        ObjectHandle::from_parsed_value_with_resolver(value, self.document.resolver_weak())
    }

    fn description_template(&self) -> Option<String> {
        Some(match self.description {
            XrefObjectDescription::Ordinary => "object $OG".to_owned(),
            XrefObjectDescription::XrefStream => "xref stream: object $OG".to_owned(),
        })
    }
}

impl DocumentResolver for BootstrapHandleDocument {
    fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
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

        let result = match self.entry_lookup.borrow().get(&object_ref).copied() {
            Some(XrefEntry::Uncompressed { offset }) if offset != 0 => {
                self.read_uncompressed_object(object_ref, offset)
            }
            Some(XrefEntry::Uncompressed { .. }) => {
                self.push_warning("object has offset 0", Some(0));
                Ok((ObjectValue::Null, -1))
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
                        Ok((ObjectValue::Null, -1))
                    }
                    Err(error) => Err(error),
                }
            }
            Some(XrefEntry::Free { .. }) | None => Ok((ObjectValue::Null, -1)),
        };
        self.state.borrow_mut().resolving.remove(&object_ref);

        match result {
            Ok((value, parsed_offset)) => {
                handle.set_resolved(value);
                handle.set_parsed_offset_if_unset(parsed_offset);
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
                let offset = match &error {
                    Error::Parse { offset, .. } => Some(*offset as u64),
                    _ => None, // cov:ignore: byte-backed bootstrap object reads surface parse errors
                };
                self.push_warning(error.to_string(), offset);
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
struct XrefReadContext {
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

impl XrefReadContext {
    fn new(
        bytes: &[u8],
        spec: XrefReadContextSpec<'_>,
        registration: &XrefRegistration,
        options: XrefLoadOptions,
    ) -> Self {
        let (entry_lookup, shared) = match spec {
            XrefReadContextSpec::ActiveSection => (
                XrefEntryLookup::Registration(&registration.entries),
                empty_bootstrap_cache(),
            ),
            XrefReadContextSpec::ActiveSectionWithCache { bootstrap_cache } => (
                XrefEntryLookup::Registration(&registration.entries),
                Rc::clone(bootstrap_cache),
            ),
            XrefReadContextSpec::Reconstruction { line_scan_entries } => (
                XrefEntryLookup::Reconstruction {
                    line_scan_entries,
                    registration_entries: &registration.entries,
                },
                empty_bootstrap_cache(),
            ),
            XrefReadContextSpec::ReconstructionWithCache {
                line_scan_entries,
                bootstrap_cache,
            } => (
                XrefEntryLookup::Reconstruction {
                    line_scan_entries,
                    registration_entries: &registration.entries,
                },
                Rc::clone(bootstrap_cache),
            ),
        };
        let document = {
            let mut cache = shared.borrow_mut();
            if let Some(document) = cache.handle_document.as_ref() {
                document.refresh_entry_lookup(entry_lookup);
                Rc::clone(document)
            } else {
                let document = BootstrapHandleDocument::new_with_state(
                    bytes,
                    entry_lookup,
                    options,
                    Rc::clone(&cache.handle_state),
                );
                cache.handle_document = Some(Rc::clone(&document));
                document
            }
        };
        let handle_diagnostics_len = shared
            .borrow()
            .handle_state
            .borrow()
            .diagnostics
            .entries()
            .len();
        Self {
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
        let result = self
            .document
            .read_file_object(input, absolute_offset, policy, description);
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
        let value = dictionary.try_get_key(&name).ok()?;
        let _ = value.try_dereference();
        self.sync_handle_diagnostics();
        Some(value)
    }

    fn sync_handle_diagnostics(&mut self) {
        let state = self.document.state.borrow();
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
        self.document.take_reconstruction_trigger()
    }
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
    .and_then(|mut state| {
        // The bootstrap resolver is intentionally temporary: callers receive
        // the xref/trailer snapshot, not a live Pdf. Preserve the trailer's
        // observable indirect references as detached handles before the
        // temporary cache's cycle-breaking Drop runs.
        let bootstrap_cache = state.bootstrap_cache;
        state.loaded.trailer = detach_bootstrap_handle(&state.loaded.trailer)?;
        drop(bootstrap_cache);
        Ok(state.loaded)
    })
}

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
        // cov:ignore-start: converting the u64 startxref offset can overflow
        // only on a 32-bit target; the supported CI target is 64-bit.
        Err(_) if allow_repair => {
            parse_errors.push(Error::parse(0, "startxref does not fit usize"));
            0
        }
        // cov:ignore-end
        Err(_) => return Err(Error::parse(0, "startxref does not fit usize")), // cov:ignore: the same u64-to-usize overflow is unrepresentable on the supported target
    };

    let mut registration = XrefRegistration::default();
    let mut initial_parse_diagnostics = Diagnostics::default();
    let mut observed_first_xref_item_offset = None;
    let mut loaded = match parse_xref_from_start(
        bytes,
        xref_pos,
        startxref,
        &version,
        options,
        &mut registration,
        Some(&mut initial_parse_diagnostics),
        XrefReadContextSpec::ActiveSection,
        Some(&mut observed_first_xref_item_offset),
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
            for diagnostic in initial_parse_diagnostics.entries() {
                initial_diagnostics.push(diagnostic.clone());
            }
            let mut recovered = recover_xref_from_linear_scan(
                bytes,
                version,
                startxref,
                trigger,
                None,
                options,
                initial_diagnostics,
                observed_first_xref_item_offset,
            )?;
            discard_lower_generations(
                &mut recovered.loaded.entries,
                &mut recovered.parsed_xref_streams,
            );
            recovered.header_offset = header_offset;
            return Ok(recovered);
        }
        Err(error) => return Err(error),
    };
    prepend_repair_diagnostics(&mut loaded.loaded.repair_diagnostics, initial_diagnostics);

    let mut previous_parse_diagnostics = Diagnostics::default();
    if let Err(error) = merge_previous_xref_sections_with_observer(
        bytes,
        &version,
        &mut loaded,
        options,
        &mut registration,
        Some(&mut previous_parse_diagnostics),
        XrefReadContextSpec::ActiveSection,
        Some(&mut observed_first_xref_item_offset),
    ) {
        if allow_repair {
            let deleted_objects = std::mem::take(&mut registration.deleted_objects);
            let trigger = parse_errors.into_iter().next().unwrap_or(error);
            let recovered = recover_xref_from_linear_scan(
                bytes,
                version,
                startxref,
                trigger,
                Some(&loaded.loaded.trailer),
                options,
                previous_parse_diagnostics,
                observed_first_xref_item_offset,
            )?;
            let mut recovered = merge_recovered_qpdf_state(recovered, loaded, &deleted_objects);
            discard_lower_generations(
                &mut recovered.loaded.entries,
                &mut recovered.parsed_xref_streams,
            );
            recovered.header_offset = header_offset;
            return Ok(recovered);
        }
        return Err(error);
    }

    loaded.loaded.entries = registration.snapshot();
    // qpdf's post-chain `m->trailer.getKey("/Size").getIntValueAsInt()`
    // dereferences indirect `/Size` values through the completed active xref
    // table before applying the consistency warning (`QPDF.cc:689-704`).
    // Keep the resolver in the xref-loading responsibility boundary: the
    // canonical document resolver is constructed only after this stage.
    let mut size_context = XrefReadContext::new(
        bytes,
        XrefReadContextSpec::ActiveSectionWithCache {
            bootstrap_cache: &loaded.bootstrap_cache,
        },
        &registration,
        options,
    );
    let resolved_size = size_context.resolve_dictionary_value(&loaded.loaded.trailer, "Size");
    let size_reconstruction_trigger = size_context.take_reconstruction_trigger();
    if size_reconstruction_trigger.is_none() {
        size_context.cache.commit();
    }
    size_context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);

    // qpdf's ordinary post-chain `/Size` lookup calls `resolve`, whose
    // `readObjectAtOffset(true, ...)` can reconstruct the xref table when the
    // active entry points at a different object header (`QPDF.cc:1605-1623`).
    // This is the same top-level `reconstruct_xref` responsibility as an
    // initial xref failure, not a size-validation warning: consume the
    // deferred trigger before comparing `/Size`, and run the line-scan
    // recovery with the already-established trailer (`QPDF.cc:516-575`).
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
            options,
            diagnostics,
            None,
        )?; // cov:ignore: recover_xref_entries has no fallible branch; retain defensive propagation
        let deleted_objects = std::mem::take(&mut registration.deleted_objects);
        let mut recovered = merge_recovered_qpdf_state(recovered, loaded, &deleted_objects);
        recovered.header_offset = header_offset;

        // qpdf continues the original read_xref call after
        // readObjectAtOffset(true, ...) reconstructs the table: its
        // m->trailer.getKey("/Size").getIntValueAsInt() at QPDF.cc:689
        // therefore resolves the value against the newly reconstructed xref
        // before the :697-704 size consistency warning. Re-run that one
        // post-reconstruction lookup through the reconstruction context; the
        // recovery state is already the canonical line-scan xref table.
        let (recovered_size, recovered_size_diagnostics) = {
            let reconstruction_registration = XrefRegistration::default();
            let mut recovered_size_context = XrefReadContext::new(
                bytes,
                XrefReadContextSpec::ReconstructionWithCache {
                    line_scan_entries: &recovered.loaded.entries,
                    bootstrap_cache: &recovered.bootstrap_cache,
                },
                &reconstruction_registration,
                options,
            );
            let recovered_size =
                recovered_size_context.resolve_dictionary_value(&recovered.loaded.trailer, "Size");
            recovered_size_context.cache.commit();
            (recovered_size, recovered_size_context.diagnostics.clone())
        };
        for diagnostic in recovered_size_diagnostics.entries() {
            recovered.loaded.repair_diagnostics.push(diagnostic.clone());
        }
        append_xref_size_warning_for(
            recovered_size.as_ref(),
            &recovered.loaded.entries,
            &BTreeSet::new(),
            &mut recovered.loaded.repair_diagnostics,
        );
        discard_lower_generations(
            &mut recovered.loaded.entries,
            &mut recovered.parsed_xref_streams,
        );
        return Ok(recovered);
    }

    append_xref_size_warning_for(
        resolved_size.as_ref(),
        &loaded.loaded.entries,
        &registration.deleted_objects,
        &mut loaded.loaded.repair_diagnostics,
    );
    // This is the ordinary `read_xref` lifetime: qpdf keeps
    // `m->deleted_objects` through `/Size` validation, then clears it
    // (`QPDF.cc:686-708`). `reconstruct_xref` has a distinct line-scan
    // lifetime and clears before candidate re-read (`:516-575`, `:576-607`).
    // The set implements only registration suppression (`:1187-1210`), never
    // resolver or mutation history, and must not cross the xref-loader boundary.
    registration.deleted_objects.clear();

    if let Some(error) = parse_errors.into_iter().next() {
        push_repair_diagnostics(&mut loaded.loaded.repair_diagnostics, &error, startxref);
    }

    discard_lower_generations(&mut loaded.loaded.entries, &mut loaded.parsed_xref_streams);
    loaded.header_offset = header_offset;
    Ok(loaded)
}

#[allow(clippy::too_many_arguments)]
fn parse_xref_from_start(
    bytes: &[u8],
    xref_pos: usize,
    startxref: u64,
    version: &str,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    mut error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
    first_xref_item_offset_sink: Option<&mut Option<u64>>,
) -> Result<LoadedXrefState> {
    if bytes
        .get(xref_pos..)
        .is_some_and(|tail| tail.starts_with(b"xref"))
    {
        let mut cursor = ByteCursor::new(bytes, xref_pos + 4);
        let (entries, trailer_start, table_diagnostics, first_xref_item_offset) = parse_xref_table(
            &mut cursor,
            bytes,
            error_diagnostics_sink.as_deref_mut(),
            first_xref_item_offset_sink,
        )?;
        let mut deferred_free = Vec::new();
        for entry in entries {
            match entry {
                ParsedXrefEntry::Live { object_ref, entry } => {
                    registration.insert_xref_entry(object_ref, entry);
                }
                ParsedXrefEntry::Free { object_ref } => deferred_free.push(object_ref),
            }
        }
        let mut trailer_context = XrefReadContext::new(bytes, context_spec, registration, options);
        let trailer_slice = bytes
            .get(trailer_start..)
            .ok_or_else(|| Error::parse(trailer_start, "trailer is not a dictionary"))?;
        let mut trailer_parser = BootstrapHandleParser {
            document: &trailer_context.document,
            description: XrefObjectDescription::Ordinary,
        };
        let (trailer_value, _, trailer_parser_diagnostics) =
            parse_qpdf_direct_object_handle_with_diagnostics(
                trailer_slice,
                i64::try_from(trailer_start).unwrap_or(i64::MAX),
                None,
                &mut trailer_parser,
            )
            .map_err(|error| error.rebase_offset(trailer_start))?;
        let trailer = ObjectHandle::from_parsed_value_with_resolver(
            trailer_value,
            trailer_context.document.resolver_weak(),
        );
        if !trailer.try_is_dictionary()? {
            return Err(Error::parse(trailer_start, "trailer is not a dictionary"));
        }
        let mut trailer_diags = table_diagnostics;
        trailer_diags.extend(trailer_diagnostics(
            trailer_start,
            trailer_parser_diagnostics,
        ));
        let mut bootstrap_diagnostics = Diagnostics::default();
        trailer_context.append_diagnostics_to(&mut bootstrap_diagnostics);
        // cov:ignore-start: the trailer parser does not dereference its
        // indirect children, so this pre-Pdf bootstrap diagnostic sink is
        // empty for every reachable trailer shape.
        if let Some(sink) = error_diagnostics_sink.as_deref_mut() {
            for diagnostic in bootstrap_diagnostics.entries() {
                sink.push(diagnostic.clone());
            }
        }
        // cov:ignore-end
        let bootstrap_cache = trailer_context.cache.shared();
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
            first_xref_item_offset,
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
        merge_xref_stream_from_classic_trailer(
            bytes,
            xref_pos,
            &mut loaded,
            options,
            registration,
            error_diagnostics_sink,
            context_spec,
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
        context_spec,
    )
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
    destination: &SharedBootstrapCache,
    source: &SharedBootstrapCache,
) {
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
fn merge_xref_stream_from_classic_trailer(
    bytes: &[u8],
    classic_xref_pos: usize,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    mut error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
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

    let hybrid_bootstrap_cache = match context_spec {
        XrefReadContextSpec::ActiveSection | XrefReadContextSpec::Reconstruction { .. } => {
            &loaded.bootstrap_cache
        }
        XrefReadContextSpec::ActiveSectionWithCache { bootstrap_cache }
        | XrefReadContextSpec::ReconstructionWithCache {
            bootstrap_cache, ..
        } => bootstrap_cache,
    };
    let hybrid_context_spec = match context_spec {
        XrefReadContextSpec::ActiveSection | XrefReadContextSpec::ActiveSectionWithCache { .. } => {
            XrefReadContextSpec::ActiveSectionWithCache {
                bootstrap_cache: hybrid_bootstrap_cache,
            }
        }
        XrefReadContextSpec::Reconstruction { line_scan_entries }
        | XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries, ..
        } => XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries,
            bootstrap_cache: hybrid_bootstrap_cache,
        },
    };
    let mut context = XrefReadContext::new(bytes, hybrid_context_spec, registration, options);
    let xref_stream_value = context.resolve_dictionary_value(&loaded.loaded.trailer, "XRefStm");
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
    let Some(xref_stream_offset) = // cov:ignore: LLVM maps the covered hybrid-offset let-else binding to its continuation edge
        xref_stream_value.and_then(|value| value.try_as_integer().ok().flatten())
    else {
        context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
        if let Some(sink) = error_diagnostics_sink.as_mut() {
            for diagnostic in loaded.loaded.repair_diagnostics.entries() {
                sink.push(diagnostic.clone());
            }
        }
        return Err(Error::parse(classic_xref_pos, "invalid /XRefStm"));
    };
    context.append_diagnostics_to(&mut loaded.loaded.repair_diagnostics);
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
        options,
        registration,
        Some(&mut hybrid_error_diagnostics),
        hybrid_context_spec,
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
    for diagnostic in hybrid.loaded.repair_diagnostics.entries() {
        loaded.loaded.repair_diagnostics.push(diagnostic.clone());
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
    merge_bootstrap_cache_prefer_source(&loaded.bootstrap_cache, &hybrid.bootstrap_cache);

    loaded.loaded.entries = registration.snapshot();

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
fn merge_previous_xref_sections(
    bytes: &[u8],
    version: &str,
    loaded: &mut LoadedXrefState,
    options: XrefLoadOptions,
    registration: &mut XrefRegistration,
    error_diagnostics_sink: Option<&mut Diagnostics>,
    context_spec: XrefReadContextSpec<'_>,
) -> Result<()> {
    merge_previous_xref_sections_with_observer(
        bytes,
        version,
        loaded,
        options,
        registration,
        error_diagnostics_sink,
        context_spec,
        None,
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
) -> Result<()> {
    let mut visited = HashSet::new();
    let chain_bootstrap_cache = match context_spec {
        XrefReadContextSpec::ActiveSection | XrefReadContextSpec::Reconstruction { .. } => {
            Rc::clone(&loaded.bootstrap_cache)
        }
        XrefReadContextSpec::ActiveSectionWithCache { bootstrap_cache }
        | XrefReadContextSpec::ReconstructionWithCache {
            bootstrap_cache, ..
        } => Rc::clone(bootstrap_cache),
    };
    let section_context_spec = match context_spec {
        XrefReadContextSpec::ActiveSection | XrefReadContextSpec::ActiveSectionWithCache { .. } => {
            XrefReadContextSpec::ActiveSectionWithCache {
                bootstrap_cache: &chain_bootstrap_cache,
            }
        }
        XrefReadContextSpec::Reconstruction { line_scan_entries }
        | XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries, ..
        } => XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries,
            bootstrap_cache: &chain_bootstrap_cache,
        },
    };
    let (mut previous_offset, previous_diagnostics, reconstruction_trigger) =
        resolve_previous_xref_offset(
            bytes,
            options,
            registration,
            section_context_spec,
            &loaded.loaded.trailer,
        );
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

        let previous = parse_xref_from_start(
            bytes,
            previous_pos,
            offset,
            version,
            options,
            registration,
            error_diagnostics_sink.as_deref_mut(),
            section_context_spec,
            first_xref_item_offset_sink.as_deref_mut(),
        )?;
        for diagnostic in previous.loaded.repair_diagnostics.entries() {
            loaded.loaded.repair_diagnostics.push(diagnostic.clone());
        }
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
        let (next_previous_offset, previous_diagnostics, reconstruction_trigger) =
            resolve_previous_xref_offset(
                bytes,
                options,
                registration,
                section_context_spec,
                &previous.loaded.trailer,
            );
        for diagnostic in previous_diagnostics.entries() {
            loaded.loaded.repair_diagnostics.push(diagnostic.clone());
        }
        if let Some(error) = reconstruction_trigger {
            return Err(error);
        }
        previous_offset = next_previous_offset;
    }

    loaded.loaded.entries = registration.snapshot();

    Ok(())
}

fn resolve_previous_xref_offset(
    bytes: &[u8],
    options: XrefLoadOptions,
    registration: &XrefRegistration,
    context_spec: XrefReadContextSpec<'_>,
    trailer: &ObjectHandle,
) -> (Option<u64>, Diagnostics, Option<Error>) {
    let mut context = XrefReadContext::new(bytes, context_spec, registration, options);
    let offset = context
        .resolve_dictionary_value(trailer, "Prev")
        .and_then(|offset| parse_non_negative_u64_handle(&offset, "/Prev").ok())
        .filter(|&offset| offset != 0);
    let reconstruction_trigger = context.take_reconstruction_trigger();
    context.cache.commit();
    let diagnostics = context.diagnostics.clone();
    (offset, diagnostics, reconstruction_trigger)
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

fn append_xref_size_warning_for(
    size: Option<&ObjectHandle>,
    entries: &BTreeMap<ObjectRef, XrefEntry>,
    deleted_objects: &BTreeSet<u32>,
    repair_diagnostics: &mut Diagnostics,
) {
    let Some(size) = size.and_then(|value| value.try_as_integer().ok().flatten()) else {
        return;
    };
    let max_live = entries
        .keys()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0);
    let max_deleted = deleted_objects.iter().copied().max().unwrap_or(0);
    let max_object = max_live.max(max_deleted);

    if size < 1 || size - 1 != i64::from(max_object) {
        repair_diagnostics.push(Diagnostic::warning(
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
    options: XrefLoadOptions,
    mut repair_diagnostics: Diagnostics,
    observed_first_xref_item_offset: Option<u64>,
) -> Result<LoadedXrefState> {
    // qpdf mutates `m->first_xref_item_offset` while reading object 0's row,
    // before a later row can throw, and `reconstruct_xref` preserves that
    // member across the exception (`QPDF.cc:846-869, 626-708`). Rust's
    // `Result::Err` cannot carry the successful prefix, so the xref reader
    // supplies this explicit side channel instead of recovering through a
    // sentinel value.
    push_repair_diagnostics(&mut repair_diagnostics, &trigger_error, startxref);

    let recovered = recover_xref_entries(bytes, fallback_trailer.is_none())
        .map_err(|error| Error::with_open_diagnostics(error, repair_diagnostics.clone()))?;
    let mut entries = recovered.entries;
    for diagnostic in recovered.trailer_diagnostics {
        repair_diagnostics.push(diagnostic);
    }
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
    let (trailer, recovered_startxref, recovered_form, recovered_first_xref_item_offset) =
        if let Some(trailer) = fallback_trailer {
            (trailer.clone(), startxref, XrefForm::Table, 0)
        } else {
            match recovered.trailer {
                Some(trailer) => (trailer, startxref, XrefForm::Table, 0),
                None => match recover_trailer_from_xref_stream_candidate(
                    bytes,
                    &version,
                    options,
                    &mut entries,
                    &mut parsed_xref_streams,
                    &mut repair_diagnostics,
                    &mut extra_trailer_references,
                ) {
                    Ok((trailer, max_offset, form, _deleted_objects, first_xref_item_offset)) => {
                        // Candidate re-entry has already consumed its local
                        // tombstones while filtering `entries`; never retain
                        // them past this recovery operation.
                        (trailer, max_offset, form, first_xref_item_offset)
                    }
                    Err(candidate_error) => {
                        return Err(Error::with_open_diagnostics(
                            candidate_error,
                            repair_diagnostics,
                        ));
                    }
                },
            }
        };
    let recovered_first_xref_item_offset =
        observed_first_xref_item_offset.unwrap_or(recovered_first_xref_item_offset);

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
        first_xref_item_offset: recovered_first_xref_item_offset,
        trailer_references,
        parsed_xref_streams,
        bootstrap_cache: empty_bootstrap_cache(),
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
    merge_bootstrap_cache_prefer_source(&recovered.bootstrap_cache, &accumulated.bootstrap_cache);
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
    pub(crate) trailer_diagnostics: Vec<Diagnostic>,
}

pub(crate) fn recover_xref_entries(bytes: &[u8], capture_trailer: bool) -> Result<RecoveredXref> {
    let mut entries = BTreeMap::new();
    let mut trailer = None;
    let mut trailer_diagnostics = Vec::new();
    let mut line_start = 0usize;
    while line_start < bytes.len() {
        let next_line_start = next_line_start(bytes, line_start);
        if let Some(first_token) = read_scan_token(bytes, line_start, next_line_start) {
            if capture_trailer && trailer.is_none() && first_token.is_word_value(b"trailer") {
                let (candidate, diagnostics) = parse_trailer_candidate(bytes, first_token.end);
                trailer = candidate;
                trailer_diagnostics.extend(diagnostics);
            } else if let Some((object_ref, offset)) =
                scan_object_header_after_first_token(bytes, &first_token)
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
#[allow(clippy::too_many_arguments)]
fn recover_trailer_from_xref_stream_candidate(
    bytes: &[u8],
    version: &str,
    options: XrefLoadOptions,
    entries: &mut BTreeMap<ObjectRef, XrefEntry>,
    parsed_xref_streams: &mut BTreeMap<ObjectRef, ObjectHandle>,
    repair_diagnostics: &mut Diagnostics,
    trailer_references: &mut BTreeSet<ObjectRef>,
) -> Result<(ObjectHandle, u64, XrefForm, BTreeSet<u32>, u64)> {
    let (candidate, discovery_diagnostics) =
        find_xref_stream_trailer_candidate(bytes, entries, options);
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
        XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries: entries,
            bootstrap_cache: &candidate.bootstrap_cache,
        },
        None,
    ) {
        Ok(reentry) => reentry,
        Err(_) => {
            return Err(Error::parse(
                0,
                "error decoding candidate xref stream while recovering damaged file",
            ));
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
    // Buffer diagnostics from a failing `/Prev` section. The merge helper
    // otherwise writes them directly to the outer accumulator before it
    // returns, which would place them ahead of the candidate diagnostics.
    let mut previous_failure_diagnostics = Diagnostics::default();
    if merge_previous_xref_sections(
        bytes,
        version,
        &mut reentry,
        options,
        &mut reentry_registration,
        Some(&mut previous_failure_diagnostics),
        XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries: entries,
            bootstrap_cache: &candidate.bootstrap_cache,
        },
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
        return Err(Error::parse(
            0,
            "error decoding candidate xref stream while recovering damaged file",
        ));
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
    // qpdf's post-chain `m->trailer.getKey("/Size").getIntValueAsInt()`
    // dereferences an indirect `/Size` through the reconstructed table
    // (`QPDF.cc:697`). Resolve it with the same bootstrap context used while
    // re-entering the candidate instead of inspecting the raw reference.
    let mut size_context = XrefReadContext::new(
        bytes,
        XrefReadContextSpec::ReconstructionWithCache {
            line_scan_entries: entries,
            bootstrap_cache: &candidate.bootstrap_cache,
        },
        &reentry_registration,
        options,
    );
    let resolved_size = size_context.resolve_dictionary_value(&candidate.trailer, "Size");
    size_context.cache.commit();
    size_context.append_diagnostics_to(repair_diagnostics);
    append_xref_size_warning_for(
        resolved_size.as_ref(),
        entries,
        &deleted_objects,
        repair_diagnostics,
    );

    Ok((
        candidate.trailer,
        max_offset,
        reentry.loaded.last_xref_form,
        deleted_objects,
        first_xref_item_offset,
    ))
}

/// The `/Type /XRef` candidate this file's line-scanned entries point at:
/// its dictionary (which may or may not be the winning trailer -- see
/// [`find_xref_stream_trailer_candidate`]'s doc) and its true maximum
/// offset (the re-entry point).
struct XrefStreamCandidate {
    trailer: ObjectHandle,
    max_offset: u64,
    /// qpdf's reconstruction pass resolves and caches every type-1 object
    /// while discovering candidates. Reuse that cache for the later
    /// post-chain `/Size` lookup so a repair warning is not emitted again.
    bootstrap_cache: SharedBootstrapCache,
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
/// than sharing one global attempt-count budget across the whole scan:
/// a shared budget lets enough earlier, unrelated truncated entries deny a
/// later, genuine candidate its own retry, which qpdf's per-object recovery
/// has no equivalent of.
fn find_xref_stream_trailer_candidate(
    bytes: &[u8],
    entries: &BTreeMap<ObjectRef, XrefEntry>,
    options: XrefLoadOptions,
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
    let mut trailer: Option<ObjectHandle> = None;
    let mut discovery_diagnostics = Diagnostics::default();
    let empty_registration = XrefRegistration::default();
    let mut context = XrefReadContext::new(
        bytes,
        XrefReadContextSpec::Reconstruction {
            line_scan_entries: entries,
        },
        &empty_registration,
        options,
    );
    let mut emitted_diagnostics = 0usize;
    for (&object_ref, entry) in entries {
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
        let parsed = if let Some(cached) = context.cache.get(&object_ref) {
            Some(cached)
        } else {
            read_xref_candidate(&mut context, bytes, start, window_end, offset, object_ref).or_else(
                // cov:ignore-start: bounded candidate recovery is a defensive
                // retry for false next-object offsets; valid qpdf candidates
                // are fully parsed by the first bounded read.
                || {
                    if window_end >= bytes.len() {
                        return None;
                    }
                    let wide_index = next_offset_index
                        .saturating_add(XREF_CANDIDATE_FALLBACK_SPAN)
                        .min(offsets.len());
                    let wide_end = offsets
                        .get(wide_index)
                        .map_or(bytes.len(), |&next| next as usize);
                    read_xref_candidate(&mut context, bytes, start, wide_end, offset, object_ref)
                },
                // cov:ignore-end
            )
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
        bootstrap_cache: context.cache.shared(),
    });
    append_new_context_diagnostics(
        &context,
        &mut discovery_diagnostics,
        &mut emitted_diagnostics,
    );
    (candidate, discovery_diagnostics)
}

fn read_xref_candidate(
    context: &mut XrefReadContext,
    bytes: &[u8],
    start: usize,
    end: usize,
    offset: u64,
    object_ref: ObjectRef,
) -> Option<ObjectHandle> {
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
    for diagnostic in completed.diagnostics {
        context.diagnostics.push(xref_file_object_diagnostic(
            XrefObjectDescription::Ordinary,
            object_ref,
            offset,
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

fn is_xref_stream_dict(context: &mut XrefReadContext, dict: &ObjectHandle) -> bool {
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
        // qpdf's outer `parse` catches non-QPDF exceptions raised by
        // `read_xref` and turns them into a damaged-PDF exception with the
        // fixed `error reading xref: ` prefix and offset zero
        // (`QPDF.cc:450-464`). Keep the inner I/O message rather than the
        // public Error display, which would add flpdf's `I/O error: ` label.
        Error::Io(error) => (format!("error reading xref: {error}"), None),
        other => (other.to_string(), Some(startxref)),
    };
    diagnostics.push(Diagnostic::warning(message, offset));
    diagnostics.push(Diagnostic::warning(
        "Attempting to reconstruct cross-reference table",
        None,
    ));
}

fn parse_trailer_candidate(bytes: &[u8], start: usize) -> (Option<ObjectHandle>, Vec<Diagnostic>) {
    let Some(slice) = bytes.get(start..) else {
        return (None, Vec::new());
    };
    let mut resolver = XrefDetachedHandles;
    // qpdf's reconstruct_xref calls readTrailer() unconditionally and only
    // rejects a non-dictionary result afterward ("Oh well.  It was worth a
    // try.", `QPDF.cc:566-568`); any warning the parser already raised while
    // building that rejected candidate still reaches `m->warnings`. Extract
    // diagnostics regardless of whether the parse ultimately produced a
    // dictionary, a different object, or an error.
    let result = parse_qpdf_direct_object_handle_with_diagnostics(
        slice,
        i64::try_from(start).unwrap_or(i64::MAX),
        None,
        &mut resolver,
    );
    let (trailer, parser_diagnostics) = match result {
        Ok((value, _, diagnostics)) => {
            let handle = ObjectHandle::from_value(value);
            (
                handle
                    .try_is_dictionary()
                    .ok()
                    .filter(|is_dict| *is_dict)
                    .map(|_| handle),
                diagnostics,
            )
        }
        Err(_) => (None, Vec::new()), // cov:ignore: the context-aware parser recovers malformed trailer tokens as null with diagnostics
    };
    let diagnostics = trailer_diagnostics(start, parser_diagnostics);
    (trailer, diagnostics)
}

/// Format qpdf's `readTrailer()` parser diagnostics with its
/// `object_description = "trailer"` attribution (`QPDF::readTrailer`,
/// `QPDF.cc:1313-1317`; `QPDFExc::createWhat`, `QPDFExc.cc:18-49`):
/// `(trailer, offset N): <message>`, where `N` is the absolute file offset
/// qpdf's `frame->offset` (`QPDFParser.hh:38-44`) would report -- `start`
/// (the trailer parser's own byte-slice origin) plus the parser's
/// slice-relative offset.
fn trailer_diagnostics(start: usize, diagnostics: Vec<ParserDiagnostic>) -> Vec<Diagnostic> {
    diagnostics
        .into_iter()
        .map(|diagnostic| {
            let offset = (start as u64).saturating_add(diagnostic.relative_offset as u64);
            Diagnostic::warning(
                format!("(trailer, offset {offset}): {}", diagnostic.message),
                Some(offset),
            )
        })
        .collect()
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
) -> Option<(ObjectRef, u64)> {
    let obj = parse_scan_integer(number_token)?;

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
    _error_diagnostics_sink: Option<&mut Diagnostics>,
    mut first_xref_item_offset_sink: Option<&mut Option<u64>>,
) -> Result<(Vec<ParsedXrefEntry>, usize, Vec<Diagnostic>, u64)> {
    let mut entries = Vec::new();
    let mut first_xref_item_offset = 0;
    loop {
        let first_token = cursor.read_token()?;
        if first_token.is_word_value(b"trailer") {
            break;
        }

        let first = parse_xref_subsection_u32(&first_token)?;
        let count = cursor.read_u32()?;
        if first == 0 && count > 0 {
            cursor.skip_ws();
            first_xref_item_offset = cursor.pos as u64;
            if let Some(sink) = first_xref_item_offset_sink.as_deref_mut() {
                *sink = Some(first_xref_item_offset);
            }
        }
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

    let trailer_start = cursor.pos;
    let _ = bytes;
    Ok((entries, trailer_start, Vec::new(), first_xref_item_offset))
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
            // cov:ignore-start: raw and handle framing read the same bytes; this is a defensive divergence arm
            Err(error) => {
                context.append_diagnostics_to(&mut repair_diagnostics);
                if let Some(sink) = error_diagnostics_sink {
                    for diagnostic in repair_diagnostics.entries() {
                        sink.push(diagnostic.clone());
                    }
                }
                return Err(error.rebase_offset(xref_pos));
            } // cov:ignore-end
        };
        // Xref streams are not encrypted, but filter decoding still requires
        // the logical payload rather than qpdf's raw recovery EOL.
        let _recovered_handle_eol = handle_completed.remove_included_recovery_eol_for_decryption();
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
                XrefObjectDescription::XrefStream,
                object_ref,
                xref_pos as u64,
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
        // repair warning before the terminal error). The remaining, fallible
        // steps run inside this closure so a failure past this point can still
        // hand `repair_diagnostics` to `error_diagnostics_sink` before this
        // function's own `Err` propagates -- mirroring that same qpdf ordering
        // without changing what any `error_diagnostics_sink: None` caller
        // observes (the sink is write-only, and only on this closure's `Err`).
        let build = || -> Result<XrefStreamBuild> {
            let handle_stream_dict = handle_object
                .as_stream_dict()
                .ok_or_else(|| Error::parse(xref_pos, "xref not found"))?;
            // QPDF::read_xrefStream accepts an xref stream only when
            // `isStreamOfType("/XRef")` succeeds. The shared parser owns this
            // check for both direct startxref streams and classic-trailer
            // `/XRefStm` targets.
            if !is_xref_stream_handle(&handle_object)? {
                return Err(Error::parse(xref_pos, "xref not found"));
            }

            let trailer = handle_stream_dict.clone();
            let size_value = handle_stream_dict
                .as_dictionary()
                .and_then(|entries| entries.get(b"/Size".as_slice()).cloned())
                .ok_or(Error::Missing("XRef stream /Size"))?;
            let size = parse_non_negative_u64_handle(&size_value, "/Size")?;
            let size =
                u32::try_from(size).map_err(|_| Error::parse(0, "/Size does not fit u32"))?;

            let widths = parse_xref_widths_handle(&handle_stream_dict)?;
            let index = parse_xref_index_handle(&handle_stream_dict, size)?;
            let ranges = build_xref_ranges(index)?;
            let has_first_xref_item = ranges.iter().any(|&(start, count)| start == 0 && count > 0);
            let handle_stream_data = handle_object
                .as_stream_data()
                .ok_or_else(|| Error::parse(xref_pos, "xref stream has no data"))?;
            let stream_data = filters::decode_stream_data_from_handle(
                &handle_stream_dict,
                &handle_stream_data,
                filters::DecodeLimits::default(),
            )?;
            let entry_size = widths
                .0
                .checked_add(widths.1)
                .and_then(|size| size.checked_add(widths.2))
                .ok_or_else(|| Error::parse(xref_pos, "xref stream entry size overflow"))?;
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
            let size_warning = if stream_data.len() < expected_size {
                return Err(Error::parse(
                    xref_pos,
                    format!(
                        "Cross-reference stream data has the wrong size; expected = {expected_size}; actual = {}",
                        stream_data.len()
                    ),
                ));
            } else if stream_data.len() > expected_size {
                Some(Diagnostic::warning(
                    format!(
                        "(xref stream, offset {xref_pos}): Cross-reference stream data has the wrong size; expected = {expected_size}; actual = {}",
                        stream_data.len()
                    ),
                    None,
                ))
            } else {
                None
            };
            let mut cursor = ByteCursor::new(&stream_data, 0);
            let entries = parse_xref_entries(&mut cursor, size, &ranges, widths)?;
            let trailer_references = collect_trailer_references(&trailer);

            Ok((
                trailer,
                entries,
                trailer_references,
                has_first_xref_item,
                size_warning,
            ))
        };
        let build_result = build().map(
            |(trailer, entries, trailer_references, has_first_xref_item, size_warning)| {
                (
                    object_ref,
                    handle_object.clone(),
                    trailer,
                    entries,
                    trailer_references,
                    has_first_xref_item,
                    size_warning,
                )
            },
        );
        let reconstruction_trigger = context.take_reconstruction_trigger();
        context.append_diagnostics_to(&mut repair_diagnostics);
        context.cache.commit();
        let bootstrap_cache = context.cache.shared();
        (build_result, reconstruction_trigger, bootstrap_cache)
    };

    let (
        object_ref,
        handle_object,
        trailer,
        entries,
        trailer_references,
        has_first_xref_item,
        size_warning,
    ) = match build_result {
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

    if let Some(size_warning) = size_warning {
        repair_diagnostics.push(size_warning);
    }

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
        first_xref_item_offset: if has_first_xref_item {
            xref_pos as u64
        } else {
            0
        },
        trailer_references,
        parsed_xref_streams,
        bootstrap_cache,
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
    diagnostic: FileObjectDiagnostic,
) -> Diagnostic {
    Diagnostic::warning(
        format!(
            "({}object {} {}, offset {}): {}",
            description.warning_prefix(),
            object_ref.number,
            object_ref.generation,
            offset.saturating_add(diagnostic.relative_offset as u64),
            diagnostic.kind.message()
        ),
        Some(offset.saturating_add(diagnostic.relative_offset as u64)),
    )
}

type XrefWidths = (usize, usize, usize);

type XrefStreamBuild = (
    ObjectHandle,
    Vec<ParsedXrefEntry>,
    BTreeSet<ObjectRef>,
    bool,
    Option<Diagnostic>,
);

// qpdf 11.9.0's QPDF::processXRefStream rejects each /W value above
// sizeof(qpdf_offset_t) before summing the entry size (libqpdf/QPDF.cc:986-1003).
// qpdf_offset_t is a long long (include/qpdf/Types.h:31), so use the
// corresponding fixed-width Rust type rather than a platform-sized usize.
const MAX_XREF_FIELD_WIDTH: usize = std::mem::size_of::<i64>();

fn is_xref_stream_handle(stream: &ObjectHandle) -> Result<bool> {
    let Some(stream_dict) = stream.as_stream_dict() else {
        return Ok(false);
    };
    Ok(stream_dict.try_get_key(b"/Type")?.try_as_name()?.as_deref() == Some(b"XRef"))
}

fn parse_xref_widths_handle(stream_dict: &ObjectHandle) -> Result<XrefWidths> {
    let value = stream_dict
        .as_dictionary()
        .and_then(|entries| entries.get(b"/W".as_slice()).cloned())
        .ok_or(Error::Missing("XRef stream /W"))?;
    let values = value
        .try_as_array()?
        .ok_or_else(|| Error::parse(0, "/W must be array"))?;
    if values.len() != 3 {
        return Err(Error::parse(0, "/W must contain three integers"));
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

fn parse_xref_index_handle(stream_dict: &ObjectHandle, size: u32) -> Result<Vec<u32>> {
    let value = stream_dict.try_get_key(b"/Index")?;
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
mod final_handle_tests {
    use super::*;

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
    fn bootstrap_trigger_and_diagnostic_append_stay_on_handle_state() {
        let object_ref = ObjectRef::new(1, 0);
        let mut entries = BTreeMap::new();
        entries.insert(object_ref, XrefEntry::Uncompressed { offset: 1 });
        let state = Rc::new(RefCell::new(BootstrapHandleState {
            reconstruction_trigger: Some((1, "header mismatch".to_owned())),
            ..BootstrapHandleState::default()
        }));
        let document = BootstrapHandleDocument::new_with_state(
            b"x",
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
            .push_diagnostic(Diagnostic::warning("late diagnostic", None));
        let mut diagnostics = Diagnostics::default();
        context.append_diagnostics_to(&mut diagnostics);
        assert_eq!(diagnostics.entries().len(), 1);
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
        )
        .expect_err("classic trailer must be a dictionary");
        assert!(error.to_string().contains("trailer is not a dictionary"));

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
        assert!(
            read_xref_candidate(&mut context, bytes, 0, bytes.len(), 0, ObjectRef::new(1, 0),)
                .is_some()
        );
        assert!(!context.diagnostics.entries().is_empty());

        let malformed = vec![b'['; crate::parser::MAX_PARSE_DEPTH + 1];
        let (trailer, diagnostics) = parse_trailer_candidate(&malformed, 0);
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
            b"",
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
}
