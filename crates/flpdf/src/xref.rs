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
use crate::object::collect_qpdf_object_references;
use crate::object_handle::{canonical_dictionary_key_from_legacy, DocumentResolver, ObjectValue};
use crate::parser::{
    parse_qpdf_file_object, parse_qpdf_file_object_handle_with_diagnostics, HandleResolver, Parser,
    ParserDiagnostic,
};
use crate::reader::file_object::{
    finish_file_object, finish_file_object_handle, parse_file_object_handle_syntax,
    parse_file_object_header, parse_file_object_syntax, FileObjectDiagnostic, FileObjectRead,
    HandleFileObjectRead, PendingFileObject, RecoveryPolicy, ResolvedStreamLength,
};
use crate::tokenizer::{Token, TokenType, Tokenizer};
use crate::{
    filters, Diagnostics, Dictionary, Error, Object, ObjectHandle, ObjectRef, Result, XrefEntry,
};
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::rc::{Rc, Weak};

#[derive(Debug, Clone)]
pub struct LoadedXref {
    pub version: String,
    pub startxref: u64,
    pub entries: BTreeMap<ObjectRef, XrefEntry>,
    pub trailer: Dictionary,
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
    /// Legacy raw materializations retained for bootstrap consumers that still
    /// require `Object` values. The handle state below is the canonical
    /// cross-context resolution/cache boundary.
    objects: BTreeMap<ObjectRef, Object>,
    /// qpdf's one-operation cache and resolution state. Keeping this state
    /// apart from the owner document avoids making each context a new resolver
    /// identity while retaining the document strongly below.
    handle_state: Rc<RefCell<BootstrapHandleState>>,
    handle_document: Option<Rc<BootstrapHandleDocument>>,
    handle_document_owners: Vec<Rc<BootstrapHandleDocument>>,
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
    pub(crate) parsed_xref_streams: BTreeMap<ObjectRef, Object>,
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
    parsed_xref_streams: &mut BTreeMap<ObjectRef, Object>,
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
    #[cfg(test)]
    fn new(bytes: &[u8], entry_lookup: XrefEntryLookup<'_>, options: XrefLoadOptions) -> Rc<Self> {
        Self::new_with_state(
            bytes,
            entry_lookup,
            options,
            Rc::new(RefCell::new(BootstrapHandleState::default())),
        )
    }

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

    fn seed_object(&self, object_ref: ObjectRef, object: &Object) -> Result<()> {
        let handle = self.handle_for_reference(object_ref);
        if handle.is_resolved() {
            return Ok(());
        }
        let value = self.object_value_from_raw(object)?;
        handle.set_resolved(value);
        Ok(())
    }

    fn object_handle_from_raw(&self, object: &Object) -> Result<ObjectHandle> {
        if let Object::Reference(object_ref) = object {
            return Ok(self.handle_for_reference(*object_ref));
        }
        let value = self.object_value_from_raw(object)?;
        Ok(ObjectHandle::from_parsed_value_with_resolver(
            value,
            self.resolver_weak(),
        ))
    }

    fn object_value_from_raw(&self, object: &Object) -> Result<ObjectValue> {
        Ok(match object {
            Object::Null => ObjectValue::Null,
            Object::Boolean(value) => ObjectValue::Boolean(*value),
            Object::Integer(value) => ObjectValue::Integer(*value),
            Object::Real(value) => ObjectValue::Real(*value),
            Object::RealLiteral { value, literal } => ObjectValue::RealLiteral {
                value: *value,
                literal: literal.clone(),
            },
            Object::Name(value) => ObjectValue::Name(value.clone()),
            Object::String(value) => ObjectValue::String(value.clone()),
            Object::Operator(value) => ObjectValue::Operator(value.clone()),
            Object::InlineImage(value) => ObjectValue::InlineImage(value.clone()),
            Object::Array(values) => ObjectValue::Array(
                values
                    .iter()
                    .map(|value| self.object_handle_from_raw(value))
                    .collect::<Result<Vec<_>>>()?,
            ),
            Object::Dictionary(dictionary) => ObjectValue::Dictionary(
                dictionary
                    .iter()
                    .map(|(key, value)| {
                        Ok((
                            canonical_dictionary_key_from_legacy(key),
                            self.object_handle_from_raw(value)?,
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?,
            ),
            Object::Stream(stream) => ObjectValue::Stream {
                stream_dict: ObjectHandle::from_parsed_value_with_resolver(
                    self.object_value_from_raw(&Object::Dictionary(stream.dict.clone()))?,
                    self.resolver_weak(),
                ),
                stream_data: Some(Rc::new(stream.data.clone())),
                stream_provider: None,
                stream_length: stream.data.len(),
            },
            Object::Reference(object_ref) => ObjectValue::Reference(*object_ref),
        })
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
        let resolved_length =
            pending
                .indirect_length_ref()
                .map(|object_ref| match self.resolve_length(object_ref) {
                    Some(value) => ResolvedStreamLength::Integer(value),
                    None => ResolvedStreamLength::Missing,
                });
        let _ = (absolute_offset, description);
        finish_file_object_handle(input, pending, resolved_length, policy)
    }

    fn resolve_length(&self, object_ref: ObjectRef) -> Option<i64> {
        let handle = self.handle_for_reference(object_ref);
        handle.try_as_integer().ok().flatten()
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
            let parsed = parse_qpdf_file_object_handle_with_diagnostics(
                &decoded[member_start..],
                i64::try_from(member_start).unwrap_or(i64::MAX),
                Some(i64::try_from(member_start).unwrap_or(i64::MAX)),
                &mut parser,
            )?;
            for diagnostic in parsed.diagnostics {
                let offset = member_start.saturating_add(diagnostic.relative_offset);
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

/// A per-context overlay over qpdf's shared bootstrap object cache.
#[derive(Debug)]
struct XrefObjectCache {
    shared: SharedBootstrapCache,
    overlay: BTreeMap<ObjectRef, Object>,
}

impl XrefObjectCache {
    fn new(shared: SharedBootstrapCache) -> Self {
        Self {
            shared,
            overlay: BTreeMap::new(),
        }
    }

    fn get(&self, object_ref: &ObjectRef) -> Option<Object> {
        let handle = self
            .shared
            .borrow()
            .handle_state
            .borrow()
            .handles
            .get(object_ref)
            .cloned();
        if let Some(object) = handle
            .filter(|handle| handle.is_resolved())
            .and_then(|handle| handle.materialize().ok())
        {
            return Some(object);
        }
        if let Some(value) = self.overlay.get(object_ref) {
            return Some(value.clone());
        }
        let shared = self.shared.borrow();
        if let Some(value) = shared.objects.get(object_ref) {
            return Some(value.clone());
        }
        None
    }

    fn insert(&mut self, object_ref: ObjectRef, object: Object) {
        self.overlay.insert(object_ref, object);
    }

    fn mark_object_stream_resolved(&mut self, stream_number: u32) -> bool {
        self.shared
            .borrow_mut()
            .handle_state
            .borrow_mut()
            .resolved_object_streams
            .insert(stream_number)
    }

    fn commit(&mut self) {
        if self.overlay.is_empty() {
            return;
        }
        self.shared
            .borrow_mut()
            .objects
            .extend(std::mem::take(&mut self.overlay));
    }

    fn visible_objects(&self) -> BTreeMap<ObjectRef, Object> {
        let mut objects = self.shared.borrow().objects.clone();
        objects.extend(self.overlay.clone());
        objects
    }

    fn shared(&self) -> SharedBootstrapCache {
        Rc::clone(&self.shared)
    }

    fn handle_document(
        &self,
        bytes: &[u8],
        entry_lookup: XrefEntryLookup<'_>,
        options: XrefLoadOptions,
    ) -> Rc<BootstrapHandleDocument> {
        let mut shared = self.shared.borrow_mut();
        if let Some(document) = shared.handle_document.as_ref() {
            document.refresh_entry_lookup(entry_lookup);
            return Rc::clone(document);
        }
        let document = BootstrapHandleDocument::new_with_state(
            bytes,
            entry_lookup,
            options,
            Rc::clone(&shared.handle_state),
        );
        shared.handle_document = Some(Rc::clone(&document));
        document
    }
}

/// A qpdf-shaped, read-time resolver for xref bootstrap objects.
///
/// The context borrows the currently visible xref entries through an explicit
/// lookup view and owns only its raw cache overlay/recursion guard; handle
/// identity, handle recursion, and handle diagnostics live in the shared
/// bootstrap state. It never delegates to `Pdf::resolve` or the later
/// canonical resolver, and it never uses an absent optional entry table to
/// distinguish bootstrap phases.
struct XrefReadContext<'bytes, 'entries> {
    bytes: &'bytes [u8],
    entry_lookup: XrefEntryLookup<'entries>,
    options: XrefLoadOptions,
    cache: XrefObjectCache,
    resolving: BTreeSet<ObjectRef>,
    diagnostics: Diagnostics,
    handle_diagnostics_len: usize,
    reconstruction_trigger: Option<(u64, String)>,
    /// Built lazily: the first handle-native use for a shared cache copies the
    /// whole input into a fresh `Rc<[u8]>` (needed so `ObjectHandle`'s
    /// `'static` resolver can outlive this context), and later contexts reuse
    /// that owner and state. Most contexts -- e.g.
    /// every `/Prev` hop whose value is the ordinary direct integer, per ISO
    /// 32000-2 7.5.8.4 -- never touch handle-native resolution at all, so
    /// deferring construction until [`Self::handle_document`] is actually
    /// called (from real handle-native work, not from the unconditional
    /// diagnostics/trigger sync below) avoids that copy entirely instead of
    /// merely amortizing it.
    handle_document: OnceCell<Rc<BootstrapHandleDocument>>,
}

impl<'bytes, 'entries> XrefReadContext<'bytes, 'entries> {
    fn new(
        bytes: &'bytes [u8],
        spec: XrefReadContextSpec<'entries>,
        registration: &'entries XrefRegistration,
        options: XrefLoadOptions,
    ) -> Self {
        let (entry_lookup, bootstrap_cache) = match spec {
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
        let handle_diagnostics_len = bootstrap_cache
            .borrow()
            .handle_state
            .borrow()
            .diagnostics
            .entries()
            .len();
        Self {
            bytes,
            entry_lookup,
            options,
            cache: XrefObjectCache::new(bootstrap_cache),
            resolving: BTreeSet::new(),
            diagnostics: Diagnostics::default(),
            handle_diagnostics_len,
            reconstruction_trigger: None,
            handle_document: OnceCell::new(),
        }
    }

    /// Returns this context's [`BootstrapHandleDocument`], constructing it on
    /// first use. Call sites that only need to observe whether handle-native
    /// work already happened (diagnostics sync, reconstruction-trigger
    /// draining) must use `self.handle_document.get()` instead so an
    /// unrelated lookup does not force the deferred copy.
    fn handle_document(&self) -> &Rc<BootstrapHandleDocument> {
        let bytes = self.bytes;
        let entry_lookup = self.entry_lookup;
        let options = self.options;
        self.handle_document
            .get_or_init(|| self.cache.handle_document(bytes, entry_lookup, options))
    }

    fn sync_handle_cache_from_raw(&self) -> Result<()> {
        let objects = self.cache.visible_objects();
        let document = self.handle_document();
        for (object_ref, object) in objects {
            document.seed_object(object_ref, &object)?;
        }
        Ok(())
    }

    fn object_policy(&self) -> RecoveryPolicy {
        if self.options.allow_repair {
            RecoveryPolicy::Bounded
        } else {
            RecoveryPolicy::RequireEndstream
        }
    }

    fn read_file_object(
        &mut self,
        input: &[u8],
        absolute_offset: u64,
        policy: RecoveryPolicy,
        description: XrefObjectDescription,
    ) -> Result<FileObjectRead> {
        let pending = parse_file_object_syntax(input)?;
        let resolved_length = self.resolve_stream_length(&pending);
        let completed = finish_file_object(input, pending, resolved_length, policy)?;
        for diagnostic in &completed.diagnostics {
            self.diagnostics.push(xref_file_object_diagnostic(
                description,
                completed.object_ref,
                absolute_offset,
                diagnostic.clone(),
            ));
        }
        Ok(completed)
    }

    #[cfg(test)]
    fn handle_for_reference(&self, object_ref: ObjectRef) -> Option<ObjectHandle> {
        Some(self.handle_document().handle_for_reference(object_ref))
    }

    fn read_file_object_handle(
        &mut self,
        input: &[u8],
        absolute_offset: u64,
        policy: RecoveryPolicy,
        description: XrefObjectDescription,
    ) -> Result<HandleFileObjectRead> {
        self.sync_handle_cache_from_raw()?;
        let result =
            self.handle_document()
                .read_file_object(input, absolute_offset, policy, description);
        self.sync_handle_diagnostics();
        result
    }

    fn sync_handle_diagnostics(&mut self) {
        // Peek only: a context that never performed handle-native work has no
        // diagnostics to sync, and must not force the deferred construction
        // in `Self::handle_document` just to discover that.
        let Some(handle_document) = self.handle_document.get() else {
            return;
        };
        let state = handle_document.state.borrow();
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

    fn read_file_object_for_reference(
        &mut self,
        input: &[u8],
        absolute_offset: u64,
        object_ref: ObjectRef,
        policy: RecoveryPolicy,
    ) -> Result<FileObjectRead> {
        let absolute_base = usize::try_from(absolute_offset).unwrap_or(usize::MAX);
        // qpdf's `readObjectAtOffset` checks the expected object generation
        // (`QPDF.cc:1591-1612`) immediately after reading the indirect-object
        // header, before `readObject` parses the body. It rejects object ID 0
        // (`QPDF.cc:1599-1605`) before that expected-reference comparison. In
        // strict mode qpdf warns and continues parsing a nonzero mismatch, so
        // emit that warning before the body diagnostics. Recovery keeps the
        // established bootstrap reconstruction sequence below.
        if !self.options.allow_repair && policy != RecoveryPolicy::Bounded {
            let actual_object_ref = parse_file_object_header(input)
                .map_err(|error| error.rebase_offset(absolute_base))?;
            if actual_object_ref.number == 0 {
                return Err(Error::parse(absolute_base, "object with ID 0"));
            }
            if actual_object_ref != object_ref {
                let message = format!(
                    "expected {} {} obj",
                    object_ref.number, object_ref.generation
                );
                self.diagnostics.push(Diagnostic::warning(
                    format!(
                        "(object {} {}, offset {}): {}",
                        object_ref.number, object_ref.generation, absolute_offset, message
                    ),
                    Some(absolute_offset),
                ));
            }
        }

        let completed = self
            .read_file_object(
                input,
                absolute_offset,
                policy,
                XrefObjectDescription::Ordinary,
            )
            .map_err(|error| error.rebase_offset(absolute_base))?;
        if completed.object_ref != object_ref
            && (self.options.allow_repair || policy == RecoveryPolicy::Bounded)
        {
            let message = format!(
                "expected {} {} obj",
                object_ref.number, object_ref.generation
            );
            if self.reconstruction_trigger.is_none() {
                self.reconstruction_trigger = Some((absolute_offset, message.clone()));
            }
            return Err(Error::parse(absolute_base, message));
        }
        Ok(completed)
    }

    fn resolve_stream_length(
        &mut self,
        pending: &PendingFileObject,
    ) -> Option<ResolvedStreamLength> {
        let object_ref = pending.indirect_length_ref()?;
        let value = self.resolve_reference(object_ref);
        Some(match value {
            Object::Integer(value) => ResolvedStreamLength::Integer(value),
            Object::Null => ResolvedStreamLength::Missing,
            _ => ResolvedStreamLength::Invalid,
        })
    }

    fn resolve_dictionary_value(&mut self, dictionary: &Dictionary, key: &str) -> Option<Object> {
        dictionary.get(key).map(|value| self.resolve_value(value))
    }

    fn resolve_value(&mut self, value: &Object) -> Object {
        match value {
            Object::Reference(object_ref) => self.resolve_reference(*object_ref),
            _ => value.clone(),
        }
    }

    /// qpdf QPDF::resolveObjectsInStream's once-only object-stream read and
    /// cache population (QPDF.cc:1756-1833). This stays in the bootstrap
    /// context: the later canonical resolver and the legacy Pdf route are
    /// deliberately not reachable from here.
    fn resolve_objects_in_stream(&mut self, stream_number: u32) -> Result<()> {
        if !self.cache.mark_object_stream_resolved(stream_number) {
            return Ok(());
        }

        self.sync_handle_cache_from_raw()?;

        let stream_handle = self
            .handle_document()
            .handle_for_reference(ObjectRef::new(stream_number, 0));
        let dereference_result = stream_handle.try_dereference();
        self.sync_handle_diagnostics();
        dereference_result?;
        let stream_dict = stream_handle.as_stream_dict().ok_or_else(|| {
            Error::parse(
                0,
                format!("supposed object stream {stream_number} is not a stream"),
            )
        })?;
        if stream_dict.try_get_key(b"/Type")?.try_as_name()?.as_deref() != Some(b"ObjStm") {
            self.diagnostics.push(Diagnostic::warning(
                format!("supposed object stream {stream_number} has wrong type"),
                None,
            ));
        }

        let object_count =
            self.handle_document()
                .handle_integer(&stream_dict, b"/N", "object stream /N")?;
        let first = self.handle_document().handle_integer(
            &stream_dict,
            b"/First",
            "object stream /First",
        )?;
        let stream_data = stream_handle
            .as_stream_data()
            .ok_or_else(|| Error::parse(0, "supposed object stream has no data"))?;
        let decoded_stream_data = filters::decode_stream_data_from_handle(
            &stream_dict,
            &stream_data,
            filters::DecodeLimits::default(),
        )?;

        let mut tokenizer = Tokenizer::new(&decoded_stream_data);
        let mut members = BTreeMap::new();
        for _ in 0..object_count {
            let object_number = u32::try_from(tokenizer.next_integer()?)
                .map_err(|_| Error::parse(0, "object stream object number is invalid"))?;
            let object_offset = usize::try_from(tokenizer.next_integer()?)
                .map_err(|_| Error::parse(0, "object stream object offset is invalid"))?;
            // qpdf stores these in a map, so a duplicate object number keeps
            // the last header offset (QPDF.cc:1778-1789).
            members.insert(object_number, object_offset);
        }

        for (object_number, object_offset) in members {
            let object_ref = ObjectRef::new(object_number, 0);
            if !matches!(
                self.entry_lookup.get(&object_ref),
                Some(XrefEntry::Compressed { stream, .. }) if stream == stream_number
            ) {
                // qpdf skips members overridden by a newer effective xref
                // entry (QPDF.cc:1792-1795).
                continue;
            }

            let member_start = first
                .checked_add(object_offset)
                .ok_or_else(|| Error::parse(0, "object stream member offset overflow"))?;
            if member_start > decoded_stream_data.len() {
                return Err(Error::parse(
                    member_start,
                    "object stream member offset is out of range",
                ));
            }

            let parsed = match parse_qpdf_file_object(&decoded_stream_data[member_start..]) {
                Ok((object, diagnostics)) => {
                    for diagnostic in diagnostics {
                        let offset = member_start.saturating_add(diagnostic.relative_offset);
                        self.diagnostics.push(Diagnostic::warning(
                            format!(
                                "object stream {stream_number} (object {} 0, offset {offset}): {}",
                                object_ref.number, diagnostic.message
                            ),
                            Some(offset as u64),
                        ));
                    }
                    object
                }
                Err(error) => {
                    // qpdf lets QPDF::readObjectInStream's parse error abort
                    // resolveObjectsInStream; QPDF::resolve then warns and
                    // nulls only the requested object. The once-only marker
                    // above keeps later members unresolved as well.
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
            self.cache.insert(object_ref, parsed);
        }

        Ok(())
    }

    /// qpdf `QPDF::resolve`'s active-xref lookup, cache, cycle guard, and
    /// resolve-to-null fallback (`QPDF.cc:1700-1753`). Errors from reading a
    /// referenced object are warnings here, matching qpdf's catch-and-null
    /// path; the caller then decides whether the resulting null is valid for
    /// the dictionary key it is reading.
    fn resolve_reference(&mut self, object_ref: ObjectRef) -> Object {
        if let Some(value) = self.cache.get(&object_ref) {
            return value;
        }

        if !self.resolving.insert(object_ref) {
            self.diagnostics.push(Diagnostic::warning(
                format!(
                    "loop detected resolving object {} {}",
                    object_ref.number, object_ref.generation
                ),
                None,
            ));
            self.cache.insert(object_ref, Object::Null);
            return Object::Null;
        }

        let value = match self.entry_lookup.get(&object_ref) {
            Some(XrefEntry::Uncompressed { offset }) => {
                if offset == 0 {
                    self.diagnostics
                        .push(Diagnostic::warning("object has offset 0", Some(0)));
                    return self.finish_resolution(object_ref, Object::Null);
                }
                let result = usize::try_from(offset)
                    .ok()
                    .and_then(|start| self.bytes.get(start..).map(|tail| (start, tail)));
                match result {
                    Some((start, tail)) => {
                        match self.read_file_object_for_reference(
                            tail,
                            offset,
                            object_ref,
                            self.object_policy(),
                        ) {
                            Ok(mut completed) => {
                                let _ = completed.remove_included_recovery_eol_for_decryption();
                                self.resolve_value(&completed.object)
                            }
                            Err(error) => {
                                let deferred_to_reconstruction = self
                                    .reconstruction_trigger
                                    .as_ref()
                                    .is_some_and(|(offset, _)| *offset == start as u64);
                                if !deferred_to_reconstruction {
                                    let diagnostic_offset = match &error {
                                        Error::Parse { offset, .. } => Some(*offset as u64),
                                        _ => Some(start as u64), // cov:ignore: bootstrap reference reads only return parse errors
                                    };
                                    self.diagnostics.push(Diagnostic::warning(
                                        error.to_string(),
                                        diagnostic_offset,
                                    ));
                                }
                                Object::Null
                            }
                        }
                    }
                    None => {
                        self.diagnostics.push(Diagnostic::warning(
                            format!(
                                "object {} {} is beyond the end of the file",
                                object_ref.number, object_ref.generation
                            ),
                            Some(offset),
                        ));
                        Object::Null
                    }
                }
            }
            Some(XrefEntry::Free { .. }) | None => Object::Null,
            Some(XrefEntry::Compressed { stream, .. }) => {
                if let Err(error) = self.resolve_objects_in_stream(stream) {
                    let diagnostic_offset = match &error {
                        Error::Parse { offset, .. } => Some(*offset as u64),
                        _ => None,
                    };
                    self.diagnostics
                        .push(Diagnostic::warning(error.to_string(), diagnostic_offset));
                }
                self.cache.get(&object_ref).unwrap_or(Object::Null)
            }
        };

        // A nested resolution loop updates the same cache slot to null in
        // qpdf (`QPDF.cc:1710-1711`). The outer read must not overwrite that
        // cache entry with the object it was in the middle of parsing; the
        // `isUnresolved` check in `readObjectAtOffset` observes the same state
        // and leaves the null in place.
        if let Some(cached) = self.cache.get(&object_ref) {
            self.resolving.remove(&object_ref);
            return cached;
        }
        self.finish_resolution(object_ref, value)
    }

    fn finish_resolution(&mut self, object_ref: ObjectRef, value: Object) -> Object {
        self.resolving.remove(&object_ref);
        self.cache.insert(object_ref, value.clone());
        value
    }

    fn append_diagnostics_to(&mut self, diagnostics: &mut Diagnostics) {
        self.sync_handle_diagnostics();
        for diagnostic in self.diagnostics.entries() {
            diagnostics.push(diagnostic.clone());
        }
    }

    fn take_reconstruction_trigger(&mut self) -> Option<Error> {
        // The raw framing pass (e.g. an indirect stream `/Length`, read via
        // `read_file_object_for_reference`) always runs, and can set
        // `self.reconstruction_trigger`, before the handle-native metadata
        // pass (e.g. `/Size`, `/W`, `/Index`) that can set
        // `handle_document`'s own trigger -- see `parse_xref_stream`'s raw
        // `read_file_object` call ahead of its `read_file_object_handle` and
        // `build()` calls. Prefer the raw trigger so the reported
        // reconstruction cause and offset are the first one qpdf's own
        // single-pass `readObject` would have hit, matching the
        // first-trigger-wins rule each field already applies on its own.
        // Still drain both unconditionally, matching the prior behavior of
        // always consuming the handle document's trigger on every call. A
        // context that never performed handle-native work has no trigger to
        // drain, so peek with `get()` instead of forcing construction via
        // `Self::handle_document`.
        let self_trigger = self
            .reconstruction_trigger
            .take()
            .map(|(offset, message)| Error::parse(offset as usize, message));
        let handle_trigger = self
            .handle_document
            .get()
            .and_then(|handle_document| handle_document.take_reconstruction_trigger());
        self_trigger.or(handle_trigger)
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
        let (entries, trailer, trailer_diagnostics, first_xref_item_offset) = parse_xref_table(
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
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };
        for diagnostic in trailer_diagnostics {
            loaded.loaded.repair_diagnostics.push(diagnostic);
        }
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
    destination.objects.extend(
        source
            .objects
            .iter()
            .map(|(object_ref, object)| (*object_ref, object.clone())),
    );
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
    if loaded.loaded.trailer.get("XRefStm").is_none() {
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
    let Some(xref_stream_offset) = xref_stream_value.and_then(|value| value.as_integer()) else {
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
    trailer: &Dictionary,
) -> (Option<u64>, Diagnostics, Option<Error>) {
    let mut context = XrefReadContext::new(bytes, context_spec, registration, options);
    let offset = context
        .resolve_dictionary_value(trailer, "Prev")
        .and_then(|offset| parse_non_negative_u64(&offset, "/Prev").ok())
        .filter(|&offset| offset != 0);
    let reconstruction_trigger = context.take_reconstruction_trigger();
    context.cache.commit();
    let diagnostics = context.diagnostics.clone();
    (offset, diagnostics, reconstruction_trigger)
}

fn collect_trailer_references(trailer: &Dictionary) -> BTreeSet<ObjectRef> {
    let mut references = BTreeSet::new();
    collect_qpdf_object_references(&Object::Dictionary(trailer.clone()), &mut references);
    references
}

fn append_xref_size_warning_for(
    size: Option<&Object>,
    entries: &BTreeMap<ObjectRef, XrefEntry>,
    deleted_objects: &BTreeSet<u32>,
    repair_diagnostics: &mut Diagnostics,
) {
    let Some(Object::Integer(size)) = size else {
        return;
    };
    let max_live = entries
        .keys()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0);
    let max_deleted = deleted_objects.iter().copied().max().unwrap_or(0);
    let max_object = max_live.max(max_deleted);

    if *size < 1 || *size - 1 != i64::from(max_object) {
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
    fallback_trailer: Option<&Dictionary>,
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
    pub(crate) trailer: Option<Dictionary>,
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
    parsed_xref_streams: &mut BTreeMap<ObjectRef, Object>,
    repair_diagnostics: &mut Diagnostics,
    trailer_references: &mut BTreeSet<ObjectRef>,
) -> Result<(Dictionary, u64, XrefForm, BTreeSet<u32>, u64)> {
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
    trailer: Dictionary,
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
    let mut trailer: Option<Dictionary> = None;
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
            // qpdf's `getObjectByObjGen` enters the normal resolve path before
            // candidate inspection (`QPDF.cc:585-589`). Keep this candidate
            // marked as active while its dictionary is parsed so a
            // self-referential `/Length` reaches the same cycle guard as any
            // other indirect stream read.
            context.resolving.insert(object_ref);
            let parsed = match context.read_file_object_for_reference(
                &bytes[start..window_end],
                offset,
                object_ref,
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
                    context
                        .read_file_object_for_reference(
                            &bytes[start..wide_end],
                            offset,
                            object_ref,
                            RecoveryPolicy::Bounded,
                        )
                        .ok()
                }
                Err(_) => None,
            };
            let cached_during_read = context.cache.get(&object_ref);
            context.resolving.remove(&object_ref);
            match parsed {
                Some(mut completed) => {
                    let _ = completed.remove_included_recovery_eol_for_decryption();
                    let object = completed.object;
                    match cached_during_read {
                        Some(cached) => Some(cached),
                        None => {
                            context.cache.insert(object_ref, object.clone());
                            Some(object)
                        }
                    }
                }
                None => match cached_during_read {
                    Some(cached) => Some(cached),
                    None => {
                        context.cache.insert(object_ref, Object::Null);
                        None
                    }
                },
            }
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
        let Object::Stream(stream) = object else {
            continue;
        };
        if !is_xref_stream_dict(&mut context, &stream.dict) {
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
                trailer = Some(stream.dict.clone());
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

fn append_new_context_diagnostics(
    context: &XrefReadContext<'_, '_>,
    diagnostics: &mut Diagnostics,
    emitted: &mut usize,
) {
    for diagnostic in context.diagnostics.entries().iter().skip(*emitted) {
        diagnostics.push(diagnostic.clone());
    }
    *emitted = context.diagnostics.entries().len();
}

fn is_xref_stream_dict(context: &mut XrefReadContext<'_, '_>, dict: &Dictionary) -> bool {
    matches!(
        context.resolve_dictionary_value(dict, "Type"),
        Some(Object::Name(name)) if name.as_slice() == b"XRef"
    )
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

fn parse_trailer_candidate(bytes: &[u8], start: usize) -> (Option<Dictionary>, Vec<Diagnostic>) {
    let Some(slice) = bytes.get(start..) else {
        return (None, Vec::new());
    };
    let mut tokenizer = Tokenizer::new(slice);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    // qpdf's reconstruct_xref calls readTrailer() unconditionally and only
    // rejects a non-dictionary result afterward ("Oh well.  It was worth a
    // try.", `QPDF.cc:566-568`); any warning the parser already raised while
    // building that rejected candidate still reaches `m->warnings`. Extract
    // diagnostics regardless of whether the parse ultimately produced a
    // dictionary, a different object, or an error.
    let result = parser.object().ok();
    let diagnostics = trailer_diagnostics(start, parser.take_diagnostics());
    let trailer = match result {
        Some(Object::Dictionary(trailer)) => Some(trailer),
        _ => None,
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
    error_diagnostics_sink: Option<&mut Diagnostics>,
    mut first_xref_item_offset_sink: Option<&mut Option<u64>>,
) -> Result<(Vec<ParsedXrefEntry>, Dictionary, Vec<Diagnostic>, u64)> {
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
    let mut tokenizer = Tokenizer::new(&bytes[cursor.pos..]);
    let mut parser = Parser::with_tokenizer(&mut tokenizer);
    // qpdf's `QPDFParser::warn` dispatches synchronously to `QPDF::warn`
    // (`libqpdf/QPDFParser.cc:488-497`) the moment it fires, independent of
    // whether the surrounding parse ultimately succeeds. A warning already
    // raised while building this trailer (e.g. a duplicated key) therefore
    // survives even when a later token in the same dictionary is fatal, or
    // the parsed result turns out not to be a dictionary. Extract
    // diagnostics before every early return so a failing `parse_xref_table`
    // call still forwards them, mirroring `parse_trailer_candidate`'s
    // handling of the analogous reconstruction-path case above.
    let object_result = parser.object();
    let diagnostics = trailer_diagnostics(trailer_start, parser.take_diagnostics());
    let parsed = match object_result {
        Ok(parsed) => parsed,
        Err(error) => {
            if let Some(sink) = error_diagnostics_sink {
                for diagnostic in diagnostics {
                    sink.push(diagnostic);
                }
            }
            return Err(error);
        }
    };
    let trailer = match parsed {
        Object::Dictionary(dict) => dict,
        _ => {
            if let Some(sink) = error_diagnostics_sink {
                for diagnostic in diagnostics {
                    sink.push(diagnostic);
                }
            }
            return Err(Error::parse(
                cursor.pos + parser.position(),
                "trailer is not a dictionary",
            ));
        }
    };

    Ok((entries, trailer, diagnostics, first_xref_item_offset))
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
        let mut completed = match context.read_file_object(
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
        let _recovered_eol = completed.remove_included_recovery_eol_for_decryption();
        let _recovered_handle_eol = handle_completed.remove_included_recovery_eol_for_decryption();
        let object_ref = completed.object_ref;
        let object = completed.object;
        let handle_object = handle_completed.object;
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
            let stream = match &object {
                Object::Stream(stream) => stream,
                _ => return Err(Error::parse(xref_pos, "xref not found")),
            };
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

            let trailer = stream.dict.clone();
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
                    object,
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
        object,
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
    let parsed_xref_streams = BTreeMap::from([(object_ref, object)]);
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
    Dictionary,
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

#[cfg(test)]
fn parse_xref_index(
    context: &mut XrefReadContext<'_, '_>,
    trailer: &Dictionary,
    size: u32,
) -> Result<Vec<u32>> {
    let Some(value) = context.resolve_dictionary_value(trailer, "Index") else {
        return Ok(vec![0, size]);
    };
    let Object::Array(values) = value else {
        if matches!(value, Object::Null) {
            return Ok(vec![0, size]);
        }
        return Err(Error::parse(0, "/Index must be array"));
    };

    if values.len() % 2 != 0 {
        return Err(Error::parse(
            0,
            "/Index must contain an even number of integers",
        ));
    }

    let mut index = Vec::with_capacity(values.len());
    for value in values {
        let value = context.resolve_value(&value);
        let integer = parse_non_negative_u64(&value, "/Index")?;
        index.push(
            integer
                .try_into()
                .map_err(|_| Error::parse(0, "xref /Index value must fit u32"))?,
        );
    }
    Ok(index)
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
        append_xref_size_warning_for, discard_lower_generations, empty_bootstrap_cache,
        find_xref_stream_trailer_candidate, is_xref_stream_handle,
        load_xref_and_trailer_with_repair, load_xref_state_with_options,
        merge_bootstrap_cache_prefer_source, merge_bootstrap_handle_state_prefer_source,
        merge_previous_xref_sections, merge_xref_stream_from_classic_trailer,
        parse_trailer_candidate, parse_xref_index, parse_xref_stream, parse_xref_table,
        prepend_repair_diagnostics, push_repair_diagnostics,
        recover_trailer_from_xref_stream_candidate, recover_xref_from_linear_scan,
        BootstrapHandleDocument, BootstrapHandleState, ByteCursor, LoadedXref, LoadedXrefState,
        RecoveryPolicy, XrefEntryLookup, XrefForm, XrefLoadOptions, XrefObjectDescription,
        XrefReadContext, XrefReadContextSpec, XrefRegistration,
    };
    use crate::filters;
    use crate::object_handle::DocumentResolver;
    use crate::{
        Diagnostic, Diagnostics, Dictionary, Error, Object, ObjectHandle, ObjectRef, XrefEntry,
    };
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Cursor;
    use std::rc::Rc;

    #[test]
    fn repair_diagnostics_wrap_io_failures_as_qpdf_xref_errors() {
        let mut diagnostics = Diagnostics::default();
        let trigger = Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "seek failed",
        ));

        push_repair_diagnostics(&mut diagnostics, &trigger, 123);

        assert_eq!(
            diagnostics.entries(),
            &[
                Diagnostic::warning("file is damaged", None),
                Diagnostic::warning("error reading xref: seek failed", None),
                Diagnostic::warning("Attempting to reconstruct cross-reference table", None),
            ]
        );
    }

    #[test]
    fn parse_startxref_ignores_markers_before_qpdf_tail_search_window() {
        let mut bytes = b"%PDF-1.5\nstartxref\n42\n%%EOF\n".to_vec();
        bytes.extend(std::iter::repeat_n(b'w', 1055));

        let error = super::parse_startxref(&bytes).expect_err(
            "qpdf only searches for startxref in the final 1054 bytes; an old marker must not win",
        );
        assert!(matches!(
            error,
            Error::Parse { offset: 0, message } if message == "can't find startxref"
        ));
    }

    fn test_objstm_payload(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
        let mut header = Vec::new();
        let mut body = Vec::new();
        for &(object_number, object) in members {
            let offset = body.len();
            header.extend_from_slice(format!("{object_number} {offset} ").as_bytes());
            body.extend_from_slice(object);
            body.push(b'\n');
        }
        let first = header.len();
        let mut payload = header;
        payload.extend_from_slice(&body);
        (payload, first)
    }

    fn test_objstm_bytes(stream_number: u32, members: &[(u32, &[u8])]) -> Vec<u8> {
        test_objstm_bytes_with_type(stream_number, members, "ObjStm")
    }

    fn test_objstm_bytes_with_type(
        stream_number: u32,
        members: &[(u32, &[u8])],
        type_name: &str,
    ) -> Vec<u8> {
        let (payload, first) = test_objstm_payload(members);
        let mut bytes = format!(
            "{stream_number} 0 obj\n<< /Type /{type_name} /N {} /First {first} /Length {} >>\nstream\n",
            members.len(),
            payload.len()
        )
        .into_bytes();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes
    }

    fn with_bootstrap_objstm_context<T>(
        bytes: &[u8],
        stream_offset: u64,
        object_numbers: &[u32],
        test: impl FnOnce(&mut XrefReadContext<'_, '_>) -> T,
    ) -> T {
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        for (index, object_number) in object_numbers.iter().copied().enumerate() {
            registration.insert_xref_entry(
                ObjectRef::new(object_number, 0),
                XrefEntry::Compressed {
                    stream: 8,
                    index: index as u32,
                },
            );
        }
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        test(&mut context)
    }

    fn test_flate_objstm_bytes(stream_number: u32, members: &[(u32, &[u8])]) -> Vec<u8> {
        let (payload, first) = test_objstm_payload(members);
        let encoded = crate::stream_filter::encode_flate(&payload).unwrap();
        let mut bytes = format!(
            "{stream_number} 0 obj\n<< /Type /ObjStm /N {} /First {first} /Filter /FlateDecode /Length {} >>\nstream\n",
            members.len(),
            encoded.len()
        )
        .into_bytes();
        bytes.extend_from_slice(&encoded);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes
    }

    fn test_flate_objstm_bytes_with_decode_parms_ref(
        stream_number: u32,
        members: &[(u32, &[u8])],
        decode_parms_object_number: u32,
    ) -> Vec<u8> {
        let (payload, first) = test_objstm_payload(members);
        let encoded = crate::stream_filter::encode_flate(&payload).unwrap();
        let mut bytes = format!(
            "{stream_number} 0 obj\n<< /Type /ObjStm /N {} /First {first} /Filter /FlateDecode /DecodeParms {decode_parms_object_number} 0 R /Length {} >>\nstream\n",
            members.len(),
            encoded.len()
        )
        .into_bytes();
        bytes.extend_from_slice(&encoded);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes
    }

    #[test]
    fn active_context_uses_borrowed_registration_view() {
        let registration = XrefRegistration::default();
        let context = XrefReadContext::new(
            &[],
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert!(matches!(
            context.entry_lookup,
            XrefEntryLookup::Registration(_)
        ));
    }

    #[test]
    fn bootstrap_document_debug_and_resolution_keep_handle_identity() {
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(ObjectRef::new(1, 0), XrefEntry::Uncompressed { offset: 0 });
        let document = BootstrapHandleDocument::new(
            b"",
            XrefEntryLookup::Registration(&registration.entries),
            XrefLoadOptions::default(),
        );
        let debug = format!("{document:?}");
        assert!(debug.contains("BootstrapHandleDocument"));
        assert!(debug.contains("cache_len: 0"));

        let offset_zero = document.handle_for_reference(ObjectRef::new(1, 0));
        offset_zero.try_dereference().expect("offset-zero handle");
        assert!(offset_zero.is_null());
        offset_zero
            .try_dereference()
            .expect("already-resolved offset-zero handle");
        document
            .resolve_indirect(ObjectRef::new(1, 0), &offset_zero)
            .expect("already-resolved handle through the resolver boundary");
        assert!(document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("offset 0")));

        assert!(!is_xref_stream_handle(&ObjectHandle::integer(1)).expect("non-stream check"));
    }

    #[test]
    fn bootstrap_document_reports_strict_mismatch_and_object_id_errors() {
        let prefix = b"%PDF-1.7\n";
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(b"1 0 obj\n42\nendobj\n");
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: prefix.len() as u64,
            },
        );
        let document = BootstrapHandleDocument::new(
            &bytes,
            XrefEntryLookup::Registration(&registration.entries),
            XrefLoadOptions::default(),
        );
        let mismatched = document.handle_for_reference(ObjectRef::new(2, 0));
        assert_eq!(mismatched.try_as_integer().unwrap(), Some(42));
        assert!(document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected 2 0 obj")));

        let mut reference_bytes = prefix.to_vec();
        reference_bytes.extend_from_slice(b"0 0 obj\n42\nendobj\n");
        let reference_offset = prefix.len() as u64;
        let mut reference_registration = XrefRegistration::default();
        reference_registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: reference_offset,
            },
        );
        let reference_document = BootstrapHandleDocument::new(
            &reference_bytes,
            XrefEntryLookup::Registration(&reference_registration.entries),
            XrefLoadOptions::default(),
        );
        let reference = reference_document.handle_for_reference(ObjectRef::new(2, 0));
        reference
            .try_dereference()
            .expect("object ID zero is handled as a warning");
        assert!(reference_document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("object with ID 0")));
    }

    #[test]
    fn bootstrap_document_rebases_a_body_parse_error_to_the_absolute_file_offset() {
        // The header parse a few lines above already rebases its own error by
        // `start` (`parse_file_object_header(input).map_err(|error|
        // error.rebase_offset(start))`); the body parse must too, or an
        // indirect bootstrap object at a nonzero offset reports a byte
        // position relative to its own tail slice instead of the file.
        //
        // A direct (non-indirect) `/Length` that does not actually reach
        // "endstream" is a hard `Err` from `complete_handle_stream`
        // (`reader/file_object.rs`) under the strict, non-repair policy --
        // unlike `check_endobj`'s softer diagnostic -- so it exercises the
        // rebase on `Self::read_uncompressed_object`'s body-parse call.
        let prefix = b"%PDF-1.7\n";
        let header = b"2 0 obj\n<< /Length 5 >>\n";
        let stream_keyword = b"stream\n";
        let data = b"abcde";
        let mut bytes = prefix.to_vec();
        let object_offset = bytes.len() as u64;
        bytes.extend_from_slice(header);
        bytes.extend_from_slice(stream_keyword);
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(b"\nNOPE\n");
        // `complete_handle_stream` reports the hard "expected endstream"
        // error at `data_start + length`, relative to the object's own tail
        // slice (`input`), before any rebasing.
        let relative_error_offset = (header.len() + stream_keyword.len() + data.len()) as u64;
        let expected_absolute_offset = object_offset + relative_error_offset;

        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: object_offset,
            },
        );
        let document = BootstrapHandleDocument::new(
            &bytes,
            XrefEntryLookup::Registration(&registration.entries),
            XrefLoadOptions::default(),
        );
        let handle = document.handle_for_reference(ObjectRef::new(2, 0));
        handle.try_dereference().expect(
            "resolve_indirect degrades a body parse failure to a warning, matching qpdf's \
             QPDF::resolve catch-and-null fallback, not a hard Err",
        );
        assert!(handle.is_null());

        let diagnostics = document.state.borrow();
        let message = diagnostics
            .diagnostics
            .entries()
            .iter()
            .find(|diagnostic| diagnostic.message.contains("expected endstream"))
            .expect("body parse error must be reported as a warning")
            .message
            .clone();
        assert!(
            message.contains(&format!("byte {expected_absolute_offset}")),
            "offset must be absolute (object offset + relative), got: {message}"
        );
        assert!(
            !message.contains(&format!("byte {relative_error_offset}: ")),
            "offset must not be left relative to the object's own tail slice, got: {message}"
        );
    }

    #[test]
    fn bootstrap_document_repair_and_object_stream_paths_follow_qpdf_cache_rules() {
        let mut repair_bytes = b"%PDF-1.7\n".to_vec();
        let repair_offset = repair_bytes.len() as u64;
        repair_bytes.extend_from_slice(b"1 0 obj\n42\nendobj\n");
        let mut repair_registration = XrefRegistration::default();
        repair_registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: repair_offset,
            },
        );
        let repair_document = BootstrapHandleDocument::new(
            &repair_bytes,
            XrefEntryLookup::Registration(&repair_registration.entries),
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );
        let repaired = repair_document.handle_for_reference(ObjectRef::new(2, 0));
        repaired.try_dereference().expect("repair mismatch warning");
        assert!(repair_document.take_reconstruction_trigger().is_some());

        let members = [
            (2, b"<< /Value /First /Value /Decoded >>".as_slice()),
            (3, b"42".as_slice()),
        ];
        let mut object_stream_bytes = b" \n".to_vec();
        let stream_offset = object_stream_bytes.len() as u64;
        object_stream_bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        let mut object_stream_registration = XrefRegistration::default();
        object_stream_registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        object_stream_registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let object_stream_document = BootstrapHandleDocument::new(
            &object_stream_bytes,
            XrefEntryLookup::Registration(&object_stream_registration.entries),
            XrefLoadOptions::default(),
        );
        let member = object_stream_document.handle_for_reference(ObjectRef::new(2, 0));
        member
            .try_dereference()
            .expect("compressed member resolution");
        assert_eq!(
            member
                .try_get_key(b"/Value")
                .expect("member dictionary")
                .try_as_name()
                .unwrap(),
            Some(b"Decoded".to_vec())
        );
        assert!(object_stream_document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("duplicated key /Value")));
        object_stream_document
            .resolve_objects_in_stream(8)
            .expect("object stream is resolved once");

        let mut wrong_type_bytes = b" \n".to_vec();
        let wrong_type_offset = wrong_type_bytes.len() as u64;
        wrong_type_bytes.extend_from_slice(&test_objstm_bytes_with_type(
            8,
            &[(2, b"42".as_slice())],
            "Other",
        ));
        let mut wrong_type_registration = XrefRegistration::default();
        wrong_type_registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: wrong_type_offset,
            },
        );
        wrong_type_registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let wrong_type_document = BootstrapHandleDocument::new(
            &wrong_type_bytes,
            XrefEntryLookup::Registration(&wrong_type_registration.entries),
            XrefLoadOptions::default(),
        );
        wrong_type_document
            .handle_for_reference(ObjectRef::new(2, 0))
            .try_dereference()
            .expect("wrong object-stream type remains readable");
        assert!(wrong_type_document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("wrong type")));

        let mut missing_member_bytes = b" \n".to_vec();
        missing_member_bytes.extend_from_slice(&test_objstm_bytes(8, &[(2, b"42".as_slice())]));
        let mut missing_member_registration = XrefRegistration::default();
        missing_member_registration
            .insert_xref_entry(ObjectRef::new(8, 0), XrefEntry::Uncompressed { offset: 2 });
        missing_member_registration.insert_xref_entry(
            ObjectRef::new(9, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let missing_member_document = BootstrapHandleDocument::new(
            &missing_member_bytes,
            XrefEntryLookup::Registration(&missing_member_registration.entries),
            XrefLoadOptions::default(),
        );
        let missing_member = missing_member_document.handle_for_reference(ObjectRef::new(9, 0));
        missing_member
            .try_dereference()
            .expect("unlisted compressed member resolves to null");
        assert!(missing_member.is_null());
    }

    #[test]
    fn bootstrap_document_reports_object_stream_decode_and_member_failures() {
        let out_of_range_payload = b"2 999 42\n";
        let out_of_range_first = b"2 999 ".len();
        let mut out_of_range_bytes = b" \n".to_vec();
        let out_of_range_offset = out_of_range_bytes.len() as u64;
        out_of_range_bytes.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /ObjStm /N 1 /First {out_of_range_first} /Length {} >>\nstream\n",
                out_of_range_payload.len()
            )
            .as_bytes(),
        );
        out_of_range_bytes.extend_from_slice(out_of_range_payload);
        out_of_range_bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let mut out_of_range_registration = XrefRegistration::default();
        out_of_range_registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: out_of_range_offset,
            },
        );
        out_of_range_registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let out_of_range_document = BootstrapHandleDocument::new(
            &out_of_range_bytes,
            XrefEntryLookup::Registration(&out_of_range_registration.entries),
            XrefLoadOptions::default(),
        );
        let out_of_range = out_of_range_document
            .resolve_objects_in_stream(8)
            .expect_err("object-stream member offset must be bounded");
        assert!(out_of_range.to_string().contains("out of range"));

        let invalid_filter_payload = b"2 0 42\n";
        let invalid_filter_first = b"2 0 ".len();
        let mut invalid_filter_bytes = b" \n".to_vec();
        let invalid_filter_offset = invalid_filter_bytes.len() as u64;
        invalid_filter_bytes.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /ObjStm /N 1 /First {invalid_filter_first} /Filter /NoSuchFilter /Length {} >>\nstream\n",
                invalid_filter_payload.len()
            )
            .as_bytes(),
        );
        invalid_filter_bytes.extend_from_slice(invalid_filter_payload);
        invalid_filter_bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let mut invalid_filter_registration = XrefRegistration::default();
        invalid_filter_registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: invalid_filter_offset,
            },
        );
        invalid_filter_registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let invalid_filter_document = BootstrapHandleDocument::new(
            &invalid_filter_bytes,
            XrefEntryLookup::Registration(&invalid_filter_registration.entries),
            XrefLoadOptions::default(),
        );
        let invalid_filter = invalid_filter_document
            .resolve_objects_in_stream(8)
            .expect_err("unsupported object-stream filter must be reported");
        assert!(invalid_filter.to_string().contains("NoSuchFilter"));

        let malformed_member_payload = b"2 0 [ 2147483648 0 R ]";
        let malformed_member_first = b"2 0 ".len();
        let mut malformed_member_bytes = b" \n".to_vec();
        let malformed_member_offset = malformed_member_bytes.len() as u64;
        malformed_member_bytes.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /ObjStm /N 1 /First {malformed_member_first} /Length {} >>\nstream\n",
                malformed_member_payload.len()
            )
            .as_bytes(),
        );
        malformed_member_bytes.extend_from_slice(malformed_member_payload);
        malformed_member_bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let mut malformed_member_registration = XrefRegistration::default();
        malformed_member_registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: malformed_member_offset,
            },
        );
        malformed_member_registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let malformed_member_document = BootstrapHandleDocument::new(
            &malformed_member_bytes,
            XrefEntryLookup::Registration(&malformed_member_registration.entries),
            XrefLoadOptions::default(),
        );
        // qpdf's `resolve` wraps the whole `resolveObjectsInStream` call in a
        // try/catch that warns and resolves to null (`QPDF.cc:1715-1747`);
        // `readObjectInStream`'s member loop has no inner catch of its own
        // (`QPDF.cc:1815-1826`), so a malformed member does not abort
        // resolution of the *referencing* handle -- it degrades to a warning
        // and a null value, the same as the missing-member and wrong-type
        // cases above.
        let malformed_member = malformed_member_document.handle_for_reference(ObjectRef::new(2, 0));
        malformed_member
            .try_dereference()
            .expect("malformed member parse error resolves to null with a warning");
        assert!(malformed_member.is_null());
        assert!(malformed_member_document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("integer out of range")));

        // qpdf's `resolve` may temporarily cache a null for a compressed member
        // when resolving the object stream's own `/DecodeParms` re-enters the
        // same object stream (`QPDF.cc:1745-1749`). The outer member loop still
        // parses every member and `updateCache` unconditionally overwrites that
        // provisional value (`QPDF.cc:1821-1828,1843-1857`). The member loop
        // must do the same rather than skipping an already-resolved handle.
        let mut overwritten_member_bytes = b" \n".to_vec();
        let overwritten_member_offset = overwritten_member_bytes.len() as u64;
        overwritten_member_bytes.extend_from_slice(&test_flate_objstm_bytes_with_decode_parms_ref(
            8,
            &[(2, b"<< /Marker 42 >>".as_slice()), (3, b"99".as_slice())],
            2,
        ));
        let mut overwritten_member_registration = XrefRegistration::default();
        overwritten_member_registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: overwritten_member_offset,
            },
        );
        overwritten_member_registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        overwritten_member_registration.insert_xref_entry(
            ObjectRef::new(3, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 1,
            },
        );
        let overwritten_member_document = BootstrapHandleDocument::new(
            &overwritten_member_bytes,
            XrefEntryLookup::Registration(&overwritten_member_registration.entries),
            XrefLoadOptions::default(),
        );
        let overwritten_member =
            overwritten_member_document.handle_for_reference(ObjectRef::new(3, 0));
        overwritten_member
            .try_dereference()
            .expect("member resolution re-enters the object stream");
        assert_eq!(overwritten_member.try_as_integer().unwrap(), Some(99));

        let decode_parms_member =
            overwritten_member_document.handle_for_reference(ObjectRef::new(2, 0));
        assert_eq!(
            decode_parms_member
                .try_get_key(b"/Marker")
                .expect("reentrant DecodeParms member is a dictionary")
                .try_as_integer()
                .unwrap(),
            Some(42)
        );
    }

    #[test]
    fn resolve_indirect_degrades_a_malformed_object_stream_n_to_null_with_a_warning() {
        // qpdf's `resolve` wraps the whole `case 2:
        // resolveObjectsInStream(...)` call in a try/catch
        // (`QPDF.cc:1715-1734`); a non-integer `/N` throws `damagedPDF`
        // inside `resolveObjectsInStream` (`QPDF.cc:1782-1784`), which that
        // catch converts to a warning before falling through to
        // `updateCache(og, QPDF_Null::create(), -1, -1)`
        // (`QPDF.cc:1744-1747`). Resolving a reference into such a stream
        // must degrade to null with a warning, not propagate a hard `Err`
        // that aborts the rest of xref bootstrap loading.
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"8 0 obj\n<< /Type /ObjStm /N /NotAnInteger /First 0 /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let document = BootstrapHandleDocument::new(
            &bytes,
            XrefEntryLookup::Registration(&registration.entries),
            XrefLoadOptions::default(),
        );
        let handle = document.handle_for_reference(ObjectRef::new(2, 0));
        handle
            .try_dereference()
            .expect("a malformed object-stream /N degrades to a warning and a null value, not Err");
        assert!(handle.is_null());
        assert!(document
            .state
            .borrow()
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("/N is not an integer")));
        // The recursion guard must not leak on this path: `resolve_indirect`
        // must remove `object_ref` from `resolving` even when
        // `resolve_objects_in_stream` returns `Err`.
        assert!(!document
            .state
            .borrow()
            .resolving
            .contains(&ObjectRef::new(2, 0)));
    }

    #[test]
    fn active_context_rejects_non_stream_object_stream_entries() {
        let bytes = b"5 0 obj\n42\nendobj\n";
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(ObjectRef::new(5, 0), XrefEntry::Uncompressed { offset: 0 });
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        let error = context
            .resolve_objects_in_stream(5)
            .expect_err("plain object is not an object stream");
        assert!(error.to_string().contains("not a stream"));
    }

    #[test]
    fn bootstrap_file_object_keeps_xref_filter_metadata_in_the_handle_graph() {
        let mut bytes = b"1 0 obj\n<< /Type /XRef /Filter 2 0 R /DecodeParms 3 0 R /Length 3 >>\nstream\nabcendstream\nendobj\n".to_vec();
        let filter_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"2 0 obj\n/FlateDecode\nendobj\n");
        let decode_params_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"3 0 obj\n<< /Predictor 12 >>\nendobj\n");

        let xref_offset = 0;
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(1, 0),
            XrefEntry::Uncompressed {
                offset: xref_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: filter_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(3, 0),
            XrefEntry::Uncompressed {
                offset: decode_params_offset,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        let parsed = context
            .read_file_object_handle(
                &bytes,
                xref_offset,
                RecoveryPolicy::RequireEndstream,
                XrefObjectDescription::XrefStream,
            )
            .expect("handle-native xref object framing");
        let stream = parsed.object;
        let stream_dict = stream.as_stream_dict().expect("xref stream dictionary");
        let filter = stream_dict.try_get_key(b"/Filter").expect("filter handle");
        assert_eq!(filter.object_ref(), Some(ObjectRef::new(2, 0)));
        assert_eq!(
            filter.try_as_name().expect("resolve filter"),
            Some(b"FlateDecode".to_vec())
        );
        let decode_params = stream_dict
            .try_get_key(b"/DecodeParms")
            .expect("decode-params handle");
        assert_eq!(
            decode_params
                .try_get_key(b"/Predictor")
                .expect("resolve decode params")
                .try_as_integer()
                .expect("predictor integer"),
            Some(12)
        );
        assert!(filter.is_same_object_as(
            &context
                .handle_for_reference(ObjectRef::new(2, 0))
                .expect("cached filter handle")
        ));
        assert!(parsed.included_recovery_eol.is_none());
        assert_eq!(parsed.object_ref, ObjectRef::new(1, 0));
    }

    fn parse_handle_xref_stream_fixture(indirect_filters: bool) -> LoadedXrefState {
        let payload = [1, 0, 0];
        let stream_data = if indirect_filters {
            crate::stream_filter::encode_flate(&payload).expect("encode xref payload")
        } else {
            payload.to_vec()
        };
        let filter_entries = if indirect_filters {
            "/Filter 2 0 R /DecodeParms 3 0 R"
        } else {
            ""
        };
        let mut bytes = format!(
            "1 0 obj\n<< /Type /XRef /Size 2 /Index [1 1] /W [1 1 1] {filter_entries} /Length {} >>\nstream\n",
            stream_data.len()
        )
        .into_bytes();
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let filter_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"2 0 obj\n/FlateDecode\nendobj\n");
        let decode_params_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"3 0 obj\n<< /Predictor 1 >>\nendobj\n");

        let mut registration = XrefRegistration::default();
        if indirect_filters {
            registration.insert_xref_entry(
                ObjectRef::new(2, 0),
                XrefEntry::Uncompressed {
                    offset: filter_offset,
                },
            );
            registration.insert_xref_entry(
                ObjectRef::new(3, 0),
                XrefEntry::Uncompressed {
                    offset: decode_params_offset,
                },
            );
        }
        parse_xref_stream(
            &bytes,
            0,
            0,
            "1.7".to_owned(),
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect("handle-native xref stream parse")
    }

    #[test]
    fn xref_stream_decodes_indirect_filter_and_decode_parms_through_handles() {
        let loaded = parse_handle_xref_stream_fixture(true);

        assert_eq!(
            loaded.loaded.entries.get(&ObjectRef::new(1, 0)),
            Some(&XrefEntry::Uncompressed { offset: 0 })
        );
        assert!(!loaded.loaded.repair_diagnostics.has_errors());
    }

    #[test]
    fn xref_stream_decodes_no_filter_payload_through_handles() {
        let loaded = parse_handle_xref_stream_fixture(false);

        assert_eq!(
            loaded.loaded.entries.get(&ObjectRef::new(1, 0)),
            Some(&XrefEntry::Uncompressed { offset: 0 })
        );
        assert!(!loaded.loaded.repair_diagnostics.has_errors());
    }

    fn large_aligned_decode_parms_xref_fixture() -> (Vec<u8>, u64, Vec<u8>, u64, u64) {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let filter_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"2 0 obj\n/FlateDecode\nendobj\n");
        let decode_params_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"3 0 obj\n[<< /Predictor 1 /Ignored (");
        bytes.extend(std::iter::repeat_n(b'x', 1 << 20));
        bytes.extend_from_slice(b") >>]\nendobj\n");
        let root_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"4 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let classic_offset = bytes.len() as u64;
        let mut classic = format!(
            "xref\n0 5\n0000000000 65535 f \n0000000000 00000 f \n{filter_offset:010} 00000 n \n{decode_params_offset:010} 00000 n \n{root_offset:010} 00000 n \ntrailer\n<< /Size 5 /Root 4 0 R /XRefStm 0000000000"
        )
        .into_bytes();
        let trailer_suffix = b" >>\n";
        let startxref_prefix = b"startxref\n";
        let eof_suffix = b"%%EOF\n";
        // The xref-stream offset is the byte after this classic table and its
        // trailer. A fixed-width placeholder keeps the offset calculation
        // independent of the number's decimal width.
        let xref_stream_offset =
            classic_offset + classic.len() as u64 + trailer_suffix.len() as u64;
        let placeholder_start = classic.len() - 10;
        classic[placeholder_start..]
            .copy_from_slice(format!("{xref_stream_offset:010}").as_bytes());
        classic.extend_from_slice(trailer_suffix);
        bytes.extend_from_slice(&classic);

        let payload = {
            let mut payload = Vec::with_capacity(5 * 7);
            payload.extend_from_slice(&[0, 0, 0, 0, 0, 0xff, 0xff]);
            for offset in [
                xref_stream_offset,
                filter_offset,
                decode_params_offset,
                root_offset,
            ] {
                payload.push(1);
                payload.extend_from_slice(&(offset as u32).to_be_bytes());
                payload.extend_from_slice(&0u16.to_be_bytes());
            }
            payload
        };
        let encoded = crate::stream_filter::encode_flate(&payload).expect("encode xref payload");
        let xref_object = format!(
            "1 0 obj\n<< /Type /XRef /Size 5 /W [1 4 2] /Index [0 5] /Filter 2 0 R /DecodeParms 3 0 R /Length {} >>\nstream\n",
            encoded.len()
        );
        bytes.extend_from_slice(xref_object.as_bytes());
        bytes.extend_from_slice(&encoded);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(startxref_prefix);
        bytes.extend_from_slice(format!("{classic_offset}\n").as_bytes());
        bytes.extend_from_slice(eof_suffix);
        (
            bytes,
            xref_stream_offset,
            payload,
            filter_offset,
            decode_params_offset,
        )
    }

    #[test]
    fn qpdf_xref_stream_large_aligned_decode_parms_keeps_shallow_identity() {
        // cov:ignore-start: qpdf availability and pinned version are external test-environment gates
        let qpdf_version = std::process::Command::new("qpdf").arg("--version").output();
        let Ok(qpdf_version) = qpdf_version else {
            eprintln!("skipping qpdf 11.9.0 xref allocation oracle: qpdf unavailable");
            return;
        };
        if !qpdf_version.status.success()
            || String::from_utf8_lossy(&qpdf_version.stdout)
                .lines()
                .next()
                .is_none_or(|line| line.trim() != "qpdf version 11.9.0")
        {
            eprintln!("skipping qpdf 11.9.0 xref allocation oracle: unexpected qpdf version");
            return;
        }
        // cov:ignore-end

        let (bytes, xref_stream_offset, expected_payload, filter_offset, decode_params_offset) =
            large_aligned_decode_parms_xref_fixture();
        let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
        let path = directory.path().join("large-aligned-decode-parms.pdf");
        std::fs::write(&path, &bytes).expect("write large DecodeParms fixture");
        let qpdf = std::process::Command::new("qpdf")
            .args(["--show-object=1", "--filtered-stream-data"])
            .arg(&path)
            .output()
            .expect("run pinned qpdf filtered xref stream probe");
        // cov:ignore-start: qpdf command failure formatting is only reachable when the external oracle fails
        assert!(
            qpdf.status.success(),
            "qpdf filtered xref stream probe failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&qpdf.stdout),
            String::from_utf8_lossy(&qpdf.stderr)
        );
        // cov:ignore-end
        assert!(
            qpdf.stdout
                .windows(expected_payload.len())
                .any(|window| window == expected_payload),
            "qpdf did not decode the expected xref payload"
        );

        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: filter_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(3, 0),
            XrefEntry::Uncompressed {
                offset: decode_params_offset,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        let parsed = context
            .read_file_object_handle(
                &bytes[xref_stream_offset as usize..],
                xref_stream_offset,
                RecoveryPolicy::RequireEndstream,
                XrefObjectDescription::XrefStream,
            )
            .expect("parse large aligned xref stream through handles");
        assert!(parsed.diagnostics.is_empty());
        let stream_dict = parsed
            .object
            .as_stream_dict()
            .expect("xref stream dictionary");
        let decode_params = stream_dict
            .try_get_key(b"/DecodeParms")
            .expect("decode params reference");
        let first = decode_params
            .try_array_item(0)
            .expect("decode params array")
            .expect("aligned decode params item");
        let second = decode_params
            .try_array_item(0)
            .expect("decode params array")
            .expect("aligned decode params item");
        assert!(first.is_same_object_as(&second));
        assert_eq!(
            first
                .try_get_key(b"/Predictor")
                .expect("predictor")
                .try_as_integer()
                .expect("predictor integer"),
            Some(1)
        );
        let stream_data = parsed.object.as_stream_data().expect("xref stream bytes");
        let decoded = filters::decode_stream_data_from_handle(
            &stream_dict,
            &stream_data,
            filters::DecodeLimits::default(),
        )
        .expect("decode aligned xref stream bytes");
        assert_eq!(decoded, expected_payload);

        let loaded =
            load_xref_state_with_options(&mut Cursor::new(bytes), XrefLoadOptions::default())
                .expect("load the hybrid xref fixture through flpdf");
        assert_eq!(
            loaded.loaded.entries.get(&ObjectRef::new(1, 0)),
            Some(&XrefEntry::Uncompressed {
                offset: xref_stream_offset,
            })
        );
        assert!(loaded.loaded.repair_diagnostics.entries().is_empty());
    }

    #[test]
    fn shared_bootstrap_cache_merge_skips_the_same_cache() {
        let cache = empty_bootstrap_cache();
        let object_ref = ObjectRef::new(1, 0);
        cache
            .borrow_mut()
            .objects
            .insert(object_ref, Object::Integer(7));

        merge_bootstrap_cache_prefer_source(&cache, &cache);

        assert_eq!(
            cache.borrow().objects.get(&object_ref),
            Some(&Object::Integer(7))
        );
    }

    #[test]
    fn shared_bootstrap_cache_reuses_handle_identity_and_forwards_diagnostics() {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let object_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"3 0 obj\n4\n");
        let object_ref = ObjectRef::new(3, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            object_ref,
            XrefEntry::Uncompressed {
                offset: object_offset,
            },
        );
        let shared_cache = empty_bootstrap_cache();

        let first_handle = {
            let mut context = XrefReadContext::new(
                &bytes,
                XrefReadContextSpec::ActiveSectionWithCache {
                    bootstrap_cache: &shared_cache,
                },
                &registration,
                XrefLoadOptions::default(),
            );
            let handle = context
                .handle_for_reference(object_ref)
                .expect("test context exposes bootstrap handles");
            handle
                .try_dereference()
                .expect("malformed direct object degrades to a warning");
            context.sync_handle_diagnostics();
            assert_eq!(
                context
                    .diagnostics
                    .entries()
                    .iter()
                    .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
                    .count(),
                1,
                "handle-native object diagnostics must reach the owning context"
            );
            handle
        };

        let second_handle = {
            let mut context = XrefReadContext::new(
                &bytes,
                XrefReadContextSpec::ActiveSectionWithCache {
                    bootstrap_cache: &shared_cache,
                },
                &registration,
                XrefLoadOptions::default(),
            );
            let handle = context
                .handle_for_reference(object_ref)
                .expect("test context exposes bootstrap handles");
            handle
                .try_dereference()
                .expect("the shared resolved handle must remain dereferenceable");
            context.sync_handle_diagnostics();
            assert!(context
                .diagnostics
                .entries()
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("expected endobj")));
            handle
        };

        assert!(first_handle.is_same_object_as(&second_handle));
        assert_eq!(second_handle.try_as_integer().unwrap(), Some(4));
    }

    #[test]
    fn bootstrap_handle_seed_converts_every_raw_scalar_kind() {
        let registration = XrefRegistration::default();
        let document = BootstrapHandleDocument::new(
            b"",
            XrefEntryLookup::Registration(&registration.entries),
            XrefLoadOptions::default(),
        );

        assert!(matches!(
            document
                .object_value_from_raw(&Object::Boolean(true))
                .unwrap(),
            crate::object_handle::ObjectValue::Boolean(true)
        ));
        assert!(matches!(
            document
                .object_value_from_raw(&Object::Real(1.25))
                .unwrap(),
            crate::object_handle::ObjectValue::Real(value) if value == 1.25
        ));
        assert!(matches!(
            document
                .object_value_from_raw(&Object::RealLiteral {
                    value: 0.4,
                    literal: b".400".to_vec(),
                })
                .unwrap(),
            crate::object_handle::ObjectValue::RealLiteral { value, literal }
                if value == 0.4 && literal == b".400"
        ));
        assert!(matches!(
            document
                .object_value_from_raw(&Object::Operator(b"q".to_vec()))
                .unwrap(),
            crate::object_handle::ObjectValue::Operator(value) if value == b"q"
        ));
        assert!(matches!(
            document
                .object_value_from_raw(&Object::InlineImage(b"data".to_vec()))
                .unwrap(),
            crate::object_handle::ObjectValue::InlineImage(value) if value == b"data"
        ));
        assert!(matches!(
            document
                .object_value_from_raw(&Object::Reference(ObjectRef::new(9, 0)))
                .unwrap(),
            crate::object_handle::ObjectValue::Reference(ObjectRef {
                number: 9,
                generation: 0
            })
        ));
    }

    #[test]
    fn shared_bootstrap_handle_state_merge_preserves_source_diagnostics() {
        let destination = Rc::new(RefCell::new(BootstrapHandleState::default()));
        destination
            .borrow_mut()
            .diagnostics
            .push(Diagnostic::warning("destination", None));
        let source = Rc::new(RefCell::new(BootstrapHandleState::default()));
        source
            .borrow_mut()
            .diagnostics
            .push(Diagnostic::warning("source", None));

        merge_bootstrap_handle_state_prefer_source(&destination, &destination);
        merge_bootstrap_handle_state_prefer_source(&destination, &source);

        assert_eq!(
            source
                .borrow()
                .diagnostics
                .entries()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            ["destination", "source"]
        );
    }

    #[test]
    fn shared_bootstrap_cache_merge_retains_both_handle_resolver_owners() {
        let bytes = b"1 0 obj\n4\nendobj\n";
        let object_ref = ObjectRef::new(1, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(object_ref, XrefEntry::Uncompressed { offset: 0 });
        let destination = empty_bootstrap_cache();
        let source = empty_bootstrap_cache();

        {
            let context = XrefReadContext::new(
                bytes,
                XrefReadContextSpec::ActiveSectionWithCache {
                    bootstrap_cache: &destination,
                },
                &registration,
                XrefLoadOptions::default(),
            );
            context.handle_document();
        }
        {
            let context = XrefReadContext::new(
                bytes,
                XrefReadContextSpec::ActiveSectionWithCache {
                    bootstrap_cache: &source,
                },
                &registration,
                XrefLoadOptions::default(),
            );
            context.handle_document();
        }

        let source_document = source.borrow().handle_document.clone().unwrap();
        merge_bootstrap_cache_prefer_source(&destination, &source);

        let merged = destination.borrow();
        assert!(Rc::ptr_eq(
            merged.handle_document.as_ref().unwrap(),
            &source_document
        ));
        assert_eq!(merged.handle_document_owners.len(), 1);
    }

    #[test]
    fn reconstruction_context_preserves_line_scan_entry_precedence() {
        let line_scan_free = ObjectRef::new(2, 0);
        let line_scan_live = ObjectRef::new(3, 0);
        let registration_only = ObjectRef::new(4, 0);
        let line_scan_entries = BTreeMap::from([
            (line_scan_free, XrefEntry::Free { next: 0 }),
            (line_scan_live, XrefEntry::Uncompressed { offset: 30 }),
        ]);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(line_scan_free, XrefEntry::Uncompressed { offset: 20 });
        registration.insert_xref_entry(line_scan_live, XrefEntry::Uncompressed { offset: 40 });
        registration.insert_xref_entry(registration_only, XrefEntry::Uncompressed { offset: 50 });
        let context = XrefReadContext::new(
            &[],
            XrefReadContextSpec::Reconstruction {
                line_scan_entries: &line_scan_entries,
            },
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(context.entry_lookup.get(&line_scan_free), None);
        assert_eq!(
            context.entry_lookup.get(&line_scan_live),
            Some(XrefEntry::Uncompressed { offset: 30 })
        );
        assert_eq!(
            context.entry_lookup.get(&registration_only),
            Some(XrefEntry::Uncompressed { offset: 50 })
        );
    }

    #[test]
    fn reconstruction_previous_sections_reuse_the_selected_bootstrap_cache() {
        let previous_ref = ObjectRef::new(5, 0);
        let mut bytes = b" ".to_vec();
        let previous_object_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"5 0 obj\n101\nendobj\n");
        while bytes.len() < 100 {
            bytes.push(b' ');
        }
        let previous_xref_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\n");

        let line_scan_entries = BTreeMap::from([(
            previous_ref,
            XrefEntry::Uncompressed {
                offset: previous_object_offset,
            },
        )]);
        let make_loaded = || {
            let mut trailer = Dictionary::new();
            trailer.insert("Prev", Object::Reference(previous_ref));
            LoadedXrefState {
                loaded: LoadedXref {
                    version: "1.7".to_string(),
                    startxref: 0,
                    entries: BTreeMap::new(),
                    trailer,
                    last_xref_form: XrefForm::Table,
                    repair_diagnostics: Diagnostics::default(),
                },
                first_xref_item_offset: 0,
                trailer_references: BTreeSet::new(),
                parsed_xref_streams: BTreeMap::new(),
                bootstrap_cache: empty_bootstrap_cache(),
                header_offset: 0,
                already_reconstructed: false,
            }
        };

        let mut reconstructed = make_loaded();
        let mut registration = XrefRegistration::default();
        merge_previous_xref_sections(
            &bytes,
            "1.7",
            &mut reconstructed,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::Reconstruction {
                line_scan_entries: &line_scan_entries,
            },
        )
        .expect_err("the uncached reconstruction context follows the file value");

        let shared_cache = empty_bootstrap_cache();
        shared_cache
            .borrow_mut()
            .objects
            .insert(previous_ref, Object::Integer(previous_xref_offset as i64));
        let mut reconstructed_with_cache = make_loaded();
        let mut cached_registration = XrefRegistration::default();
        merge_previous_xref_sections(
            &bytes,
            "1.7",
            &mut reconstructed_with_cache,
            XrefLoadOptions::default(),
            &mut cached_registration,
            None,
            XrefReadContextSpec::ReconstructionWithCache {
                line_scan_entries: &line_scan_entries,
                bootstrap_cache: &shared_cache,
            },
        )
        .expect("reconstruction context with cache follows an indirect /Prev");

        assert!(reconstructed_with_cache
            .loaded
            .repair_diagnostics
            .entries()
            .is_empty());

        let mut active_with_cache = make_loaded();
        let mut active_registration = XrefRegistration::default();
        active_registration.insert_xref_entry(
            previous_ref,
            XrefEntry::Uncompressed {
                offset: previous_object_offset,
            },
        );
        merge_previous_xref_sections(
            &bytes,
            "1.7",
            &mut active_with_cache,
            XrefLoadOptions::default(),
            &mut active_registration,
            None,
            XrefReadContextSpec::ActiveSectionWithCache {
                bootstrap_cache: &shared_cache,
            },
        )
        .expect("active context with cache follows an indirect /Prev");
        assert!(active_with_cache
            .loaded
            .repair_diagnostics
            .entries()
            .is_empty());
    }

    #[test]
    fn bootstrap_context_resolves_missing_and_free_references_to_null() {
        let missing = ObjectRef::new(9, 0);
        let freed = ObjectRef::new(10, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_free_xref_entry(freed);
        let mut context = XrefReadContext::new(
            &[],
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(context.resolve_reference(missing), Object::Null);
        assert_eq!(context.resolve_reference(freed), Object::Null);
        assert!(context.diagnostics.entries().is_empty());

        let mut dictionary = Dictionary::new();
        dictionary.insert("Index", Object::Reference(missing));
        assert_eq!(
            parse_xref_index(&mut context, &dictionary, 7).unwrap(),
            vec![0, 7]
        );
    }

    #[test]
    fn bootstrap_context_resolves_type2_members_and_caches_all_applicable_members() {
        let members = [
            (2, b"<< /Value /First >>".as_slice()),
            (4, b"[ /Second ]".as_slice()),
        ];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(4, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 1,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        let mut expected_dictionary = Dictionary::new();
        expected_dictionary.insert("Value", Object::Name(b"First".to_vec()));
        let first_value = context.resolve_reference(ObjectRef::new(2, 0));
        assert_eq!(first_value, Object::Dictionary(expected_dictionary));
        assert_eq!(
            context.resolve_reference(ObjectRef::new(4, 0)),
            Object::Array(vec![Object::Name(b"Second".to_vec())])
        );
        assert!(context.diagnostics.entries().is_empty());
    }

    #[test]
    fn bootstrap_context_does_not_replace_an_overridden_objstm_member() {
        let members = [(2, b"10".as_slice()), (3, b"20".as_slice())];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        let standalone_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"2 0 obj\n99\nendobj\n");
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Uncompressed {
                offset: standalone_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(3, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 1,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        let overridden_value = context.resolve_reference(ObjectRef::new(2, 0));
        assert_eq!(overridden_value, Object::Integer(99));
        assert_eq!(
            context.resolve_reference(ObjectRef::new(3, 0)),
            Object::Integer(20)
        );
        assert!(context.diagnostics.entries().is_empty());
    }

    #[test]
    fn bootstrap_context_resolves_indirect_prev_through_objstm() {
        let mut bytes = b" \n".to_vec();
        let object_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n42\nendobj\n");
        let previous_xref_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{object_offset:010} 00000 n \ntrailer\n<< /Size 2 >>\n"
            )
            .as_bytes(),
        );
        let previous_value = previous_xref_offset.to_string();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &[(5, previous_value.as_bytes())]));

        let previous_ref = ObjectRef::new(5, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            previous_ref,
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let mut trailer = Dictionary::new();
        trailer.insert("Prev", Object::Reference(previous_ref));
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };

        merge_previous_xref_sections(
            &bytes,
            "1.7",
            &mut loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect("an indirect /Prev stored in an ObjStm must be followed");
        assert_eq!(
            loaded.loaded.entries.get(&ObjectRef::new(1, 0)),
            Some(&XrefEntry::Uncompressed {
                offset: object_offset,
            })
        );
    }

    #[test]
    fn bootstrap_context_resolves_indirect_xrefstm_through_objstm() {
        let mut bytes = b" \n".to_vec();
        let xref_stream_offset = bytes.len() as u64;
        let xref_stream_data = [0u8; 6];
        bytes.extend_from_slice(
            format!(
                "3 0 obj\n<< /Type /XRef /W [1 1 1] /Size 2 /Length {} >>\nstream\n",
                xref_stream_data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&xref_stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let xref_stream_value = xref_stream_offset.to_string();
        let objstm_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &[(5, xref_stream_value.as_bytes())]));

        let xref_stream_ref = ObjectRef::new(3, 0);
        let hybrid_ref = ObjectRef::new(5, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            xref_stream_ref,
            XrefEntry::Uncompressed {
                offset: xref_stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: objstm_offset,
            },
        );
        registration.insert_xref_entry(
            hybrid_ref,
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let mut trailer = Dictionary::new();
        trailer.insert("XRefStm", Object::Reference(hybrid_ref));
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };

        merge_xref_stream_from_classic_trailer(
            &bytes,
            0,
            &mut loaded,
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect("an indirect /XRefStm stored in an ObjStm must be followed");
        assert!(loaded.parsed_xref_streams.contains_key(&xref_stream_ref));
    }

    #[test]
    fn bootstrap_context_turns_malformed_objstm_metadata_into_null_with_warning() {
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"8 0 obj\n<< /Type /ObjStm /N /bad /First 0 /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(
            context.resolve_reference(ObjectRef::new(2, 0)),
            Object::Null
        );
        assert!(context
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("object stream /N")));
    }

    #[test]
    fn bootstrap_context_decodes_a_filtered_objstm_before_parsing_members() {
        let members = [(2, b"<< /Value /Decoded >>".as_slice())];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_flate_objstm_bytes(8, &members));
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        let mut expected = Dictionary::new();
        expected.insert("Value", Object::Name(b"Decoded".to_vec()));
        assert_eq!(
            context.resolve_reference(ObjectRef::new(2, 0)),
            Object::Dictionary(expected)
        );
        assert!(context.diagnostics.entries().is_empty());
    }

    #[test]
    fn bootstrap_context_rejects_a_non_integer_objstm_first() {
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"8 0 obj\n<< /Type /ObjStm /N 1 /First /bad /Length 0 >>\nstream\n\nendstream\nendobj\n",
        );
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(
            context.resolve_reference(ObjectRef::new(2, 0)),
            Object::Null
        );
        assert!(context
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("object stream /First")));
    }

    #[test]
    fn bootstrap_context_reports_a_malformed_objstm_header_as_null() {
        let payload = b"2 nope << /Value /Bad >>";
        let first = b"2 nope ".len();
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(
            context.resolve_reference(ObjectRef::new(2, 0)),
            Object::Null
        );
        assert!(context
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected integer")));
    }

    #[test]
    fn bootstrap_context_reports_an_objstm_member_parse_failure_as_null() {
        let payload = b"2 0 << /Value";
        let first = b"2 0 ".len();
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(
            context.resolve_reference(ObjectRef::new(2, 0)),
            Object::Null
        );
        assert!(!context.diagnostics.entries().is_empty());
    }

    #[test]
    fn bootstrap_context_stops_caching_later_member_after_parse_failure() {
        let members = [
            (2, b"<< /Value 2147483648 0 R >>".as_slice()),
            (4, b"42".as_slice()),
        ];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2, 4], |context| {
            assert_eq!(
                context.resolve_reference(ObjectRef::new(2, 0)),
                Object::Null
            );
            assert_eq!(
                context.resolve_reference(ObjectRef::new(4, 0)),
                Object::Null
            );
            assert!(!context.diagnostics.entries().is_empty());
        });
    }

    #[test]
    fn bootstrap_context_rebases_member_parse_error_to_decoded_offset() {
        let members = [
            (2, b"42".as_slice()),
            (4, b"<< /Value 2147483648 0 R >>".as_slice()),
        ];
        let (_, first) = test_objstm_payload(&members);
        let member_start = first + b"42\n".len();
        let parse_error_offset = b"<< /Value ".len();
        let expected_offset = member_start + parse_error_offset;
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2, 4], |context| {
            assert_eq!(
                context.resolve_reference(ObjectRef::new(4, 0)),
                Object::Null
            );
            let diagnostic = context
                .diagnostics
                .entries()
                .iter()
                .find(|diagnostic| diagnostic.message.contains("integer out of range"))
                .expect("member parse failure warning");
            assert_eq!(diagnostic.offset, Some(expected_offset as u64));
            assert!(diagnostic.message.contains(&format!(
                "object stream 8 (object 4 0, offset {expected_offset}):"
            )));
        });
    }

    #[test]
    fn bootstrap_context_warns_for_empty_member_and_caches_following_member() {
        let members = [(2, b"endobj".as_slice()), (4, b"42".as_slice())];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2, 4], |context| {
            assert_eq!(
                context.resolve_reference(ObjectRef::new(2, 0)),
                Object::Null
            );
            assert_eq!(
                context.resolve_reference(ObjectRef::new(4, 0)),
                Object::Integer(42)
            );
            assert!(context
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("empty object treated as null")));
        });
    }

    #[test]
    fn bootstrap_context_preserves_wrong_type_warning_while_resolving_members() {
        let members = [(2, b"42".as_slice())];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes_with_type(8, &members, "BadTyp"));
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2], |context| {
            assert_eq!(
                context.resolve_reference(ObjectRef::new(2, 0)),
                Object::Integer(42)
            );
            assert!(context
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("has wrong type")));
        });
    }

    #[test]
    fn bootstrap_context_recovers_non_name_objstm_member_with_live_parser() {
        let members = [(2, b"<< 12 >>".as_slice())];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2], |context| {
            let object = context.resolve_reference(ObjectRef::new(2, 0));
            assert_eq!(
                object
                    .as_dict()
                    .and_then(|dictionary| dictionary.get("QPDFFake1")),
                Some(&Object::Integer(12))
            );
            assert!(context
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic
                    .message
                    .contains("expected dictionary key but found non-name object")));
        });
    }

    #[test]
    fn bootstrap_context_warns_and_caches_null_for_live_parser_error() {
        let members = [(2, b"<< /Value 2147483648 0 R >>".as_slice())];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2], |context| {
            assert_eq!(
                context.resolve_reference(ObjectRef::new(2, 0)),
                Object::Null
            );
            assert!(context
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("integer out of range")));
        });
    }

    #[test]
    fn bootstrap_context_reports_member_offset_out_of_range() {
        let (payload, _) = test_objstm_payload(&[(2, b"42".as_slice())]);
        let first = payload.len() + 100;
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2], |context| {
            assert_eq!(
                context.resolve_reference(ObjectRef::new(2, 0)),
                Object::Null
            );
            assert!(context
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic
                    .message
                    .contains("object stream member offset is out of range")));
        });
    }

    #[test]
    fn bootstrap_context_records_member_parser_diagnostics() {
        let members = [(2, b"/Bad#Name".as_slice()), (4, b"42".as_slice())];
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(&test_objstm_bytes(8, &members));
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2, 4], |context| {
            assert_eq!(
                context.resolve_reference(ObjectRef::new(2, 0)),
                Object::Name(b"Bad\0Name".to_vec())
            );
            assert_eq!(
                context.resolve_reference(ObjectRef::new(4, 0)),
                Object::Integer(42)
            );
            assert!(context
                .diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("stray #")));
        });
    }

    #[test]
    fn bootstrap_context_warns_and_returns_null_for_non_parse_objstm_errors() {
        let payload = b"2 0 42";
        let mut bytes = b" \n".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Filter /NoSuchFilter /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        with_bootstrap_objstm_context(&bytes, stream_offset, &[2], |context| {
            assert_eq!(
                context.resolve_reference(ObjectRef::new(2, 0)),
                Object::Null
            );
            assert!(!context.diagnostics.entries().is_empty());
        });
    }

    #[test]
    fn bootstrap_context_does_not_reenter_a_compressed_objstm_container() {
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(2, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(8, 0),
            XrefEntry::Compressed {
                stream: 8,
                index: 0,
            },
        );
        let mut context = XrefReadContext::new(
            &[],
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(
            context.resolve_reference(ObjectRef::new(2, 0)),
            Object::Null
        );
        assert!(context
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("supposed object stream 8 is not a stream")));
    }

    #[test]
    fn bootstrap_context_reports_reference_read_errors() {
        let object_ref = ObjectRef::new(1, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(object_ref, XrefEntry::Uncompressed { offset: 1 });
        let mut context = XrefReadContext::new(
            b" x",
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(context.resolve_reference(object_ref), Object::Null);
        assert!(context
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.offset == Some(1)));
    }

    #[test]
    fn bootstrap_context_rebases_reference_parse_errors_to_source_offsets() {
        let object_ref = ObjectRef::new(1, 0);
        let object_start = 7usize;
        let object = b"1 0 obj\n<0g>\nendobj\n";
        let bytes = [vec![b' '; object_start], object.to_vec()].concat();
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            object_ref,
            XrefEntry::Uncompressed {
                offset: object_start as u64,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(context.resolve_reference(object_ref), Object::Null);

        let expected_offset = object_start + b"1 0 obj\n".len();
        let diagnostic = context
            .diagnostics
            .entries()
            .first()
            .expect("referenced parse failure warning");
        assert_eq!(diagnostic.offset, Some(expected_offset as u64));
        assert!(
            diagnostic
                .message
                .starts_with(&format!("parse error at byte {expected_offset}:")),
            "diagnostic = {diagnostic:?}"
        );
    }

    #[test]
    fn previous_reference_diagnostics_are_forwarded_before_the_section_read() {
        let bytes = b" 5 0 obj\n999999\n";
        let object_ref = ObjectRef::new(5, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(object_ref, XrefEntry::Uncompressed { offset: 1 });

        let mut trailer = Dictionary::new();
        trailer.insert("Prev", Object::Reference(object_ref));
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };

        let error = merge_previous_xref_sections(
            bytes,
            "1.7",
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("the recovered /Prev value points beyond this fixture");

        assert!(error
            .to_string()
            .contains("xref stream offset is beyond end of file"));
        assert!(loaded
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected endobj")));
    }

    #[test]
    fn previous_section_reference_diagnostics_are_forwarded_after_the_section_read() {
        let mut bytes = b" ".to_vec();
        let object_offset = bytes.len();
        bytes.extend_from_slice(b"6 0 obj\n999999\n");
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 1\n0000000000 65535 f \n6 1\n{object_offset:010} 00000 n \ntrailer\n<< /Size 7 /Prev 6 0 R >>\n"
            )
            .as_bytes(),
        );

        let mut trailer = Dictionary::new();
        trailer.insert("Prev", Object::Integer(xref_offset as i64));
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };
        let mut registration = XrefRegistration::default();

        let error = merge_previous_xref_sections(
            &bytes,
            "1.7",
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("the indirect /Prev value points beyond this fixture");

        assert!(error
            .to_string()
            .contains("xref stream offset is beyond end of file"));
        assert!(loaded
            .loaded
            .repair_diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected endobj")));
    }

    #[test]
    fn previous_section_header_mismatch_requests_reconstruction_after_the_section_read() {
        let mut bytes = b" 3 0 obj\n999\nendobj\n".to_vec();
        let previous_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"xref\n0 3\n0000000000 65535 f \n0000000000 00000 f \n0000000001 00000 n \n",
        );
        bytes.extend_from_slice(b"trailer\n<< /Size 3 /Prev 2 0 R >>\n");

        let mut trailer = Dictionary::new();
        trailer.insert("Prev", Object::Integer(previous_offset as i64));
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };
        let mut registration = XrefRegistration::default();

        let error = merge_previous_xref_sections(
            &bytes,
            "1.7",
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("a previous section's indirect /Prev mismatch must trigger reconstruction");

        assert_eq!(error.to_string(), "parse error at byte 1: expected 2 0 obj");
    }

    #[test]
    fn hybrid_xref_header_mismatch_requests_reconstruction_before_stream_read() {
        let bytes = b" 3 0 obj\n999\n";
        let requested = ObjectRef::new(2, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(requested, XrefEntry::Uncompressed { offset: 1 });
        let mut trailer = Dictionary::new();
        trailer.insert("XRefStm", Object::Reference(requested));
        let mut diagnostics = Diagnostics::default();
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };

        let error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("a hybrid /XRefStm header mismatch must trigger reconstruction");

        assert_eq!(error.to_string(), "parse error at byte 1: expected 2 0 obj");
        assert_eq!(
            diagnostics
                .entries()
                .iter()
                .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
                .count(),
            1
        );
        let no_sink_error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("the no-sink mismatch path must still request reconstruction");
        assert_eq!(
            no_sink_error.to_string(),
            "parse error at byte 1: expected 2 0 obj"
        );
    }

    #[test]
    fn hybrid_xref_normalizes_all_bootstrap_context_cache_variants() {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let xref_stream_offset = bytes.len();
        bytes.extend(xref_stream_object_bytes(
            2,
            "/Size 1 /Index [0 1]",
            &[0, 0, 0],
        ));

        fn loaded_state(xref_stream_offset: usize) -> LoadedXrefState {
            let mut trailer = Dictionary::new();
            trailer.insert("XRefStm", Object::Integer(xref_stream_offset as i64));
            LoadedXrefState {
                loaded: LoadedXref {
                    version: "1.7".to_string(),
                    startxref: 0,
                    entries: BTreeMap::new(),
                    trailer,
                    last_xref_form: XrefForm::Table,
                    repair_diagnostics: Diagnostics::default(),
                },
                first_xref_item_offset: 0,
                trailer_references: BTreeSet::new(),
                parsed_xref_streams: BTreeMap::new(),
                bootstrap_cache: empty_bootstrap_cache(),
                header_offset: 0,
                already_reconstructed: false,
            }
        }

        let options = XrefLoadOptions {
            allow_repair: true,
            ..XrefLoadOptions::default()
        };

        let mut active = loaded_state(xref_stream_offset);
        let mut active_registration = XrefRegistration::default();
        let active_cache = empty_bootstrap_cache();
        merge_xref_stream_from_classic_trailer(
            &bytes,
            0,
            &mut active,
            options,
            &mut active_registration,
            None,
            XrefReadContextSpec::ActiveSectionWithCache {
                bootstrap_cache: &active_cache,
            },
        )
        .expect("active cached context should read the hybrid stream");

        let line_scan_entries = BTreeMap::new();
        let mut reconstruction = loaded_state(xref_stream_offset);
        let mut reconstruction_registration = XrefRegistration::default();
        merge_xref_stream_from_classic_trailer(
            &bytes,
            0,
            &mut reconstruction,
            options,
            &mut reconstruction_registration,
            None,
            XrefReadContextSpec::Reconstruction {
                line_scan_entries: &line_scan_entries,
            },
        )
        .expect("reconstruction context should read the hybrid stream");

        let reconstruction_cache = empty_bootstrap_cache();
        let mut reconstruction_with_cache = loaded_state(xref_stream_offset);
        let mut reconstruction_with_cache_registration = XrefRegistration::default();
        merge_xref_stream_from_classic_trailer(
            &bytes,
            0,
            &mut reconstruction_with_cache,
            options,
            &mut reconstruction_with_cache_registration,
            None,
            XrefReadContextSpec::ReconstructionWithCache {
                line_scan_entries: &line_scan_entries,
                bootstrap_cache: &reconstruction_cache,
            },
        )
        .expect("reconstruction cached context should read the hybrid stream");
    }

    #[test]
    fn hybrid_xref_invalid_offset_forwards_holder_diagnostics_to_sink() {
        let bytes = b" 2 0 obj\n/not-an-offset\n";
        let referenced = ObjectRef::new(2, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(referenced, XrefEntry::Uncompressed { offset: 1 });
        let mut trailer = Dictionary::new();
        trailer.insert("XRefStm", Object::Reference(referenced));
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };
        let mut diagnostics = Diagnostics::default();

        let error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("a non-integer /XRefStm must fail");

        assert_eq!(error.to_string(), "parse error at byte 0: invalid /XRefStm");
        assert!(diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected endobj")));
        let no_sink_error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("the no-sink invalid /XRefStm path must still fail");
        assert_eq!(
            no_sink_error.to_string(),
            "parse error at byte 0: invalid /XRefStm"
        );
    }

    #[test]
    fn hybrid_xref_negative_offset_forwards_holder_diagnostics_to_sink() {
        let bytes = b" 2 0 obj\n-1\n";
        let referenced = ObjectRef::new(2, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(referenced, XrefEntry::Uncompressed { offset: 1 });
        let mut trailer = Dictionary::new();
        trailer.insert("XRefStm", Object::Reference(referenced));
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };
        let mut diagnostics = Diagnostics::default();

        let error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("a negative /XRefStm must fail as an invalid seek");

        assert!(error.to_string().contains("before the file start"));
        assert!(diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected endobj")));
        let no_sink_error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("the no-sink negative /XRefStm path must still fail");
        assert!(no_sink_error.to_string().contains("before the file start"));
    }

    #[test]
    fn hybrid_xref_stream_failure_forwards_stream_diagnostics_to_sink() {
        let bytes = b" 3 0 obj\n<< /NotXRef true >>\n";
        let mut trailer = Dictionary::new();
        trailer.insert("XRefStm", Object::Integer(1));
        let mut loaded = LoadedXrefState {
            loaded: LoadedXref {
                version: "1.7".to_string(),
                startxref: 0,
                entries: BTreeMap::new(),
                trailer,
                last_xref_form: XrefForm::Table,
                repair_diagnostics: Diagnostics::default(),
            },
            first_xref_item_offset: 0,
            trailer_references: BTreeSet::new(),
            parsed_xref_streams: BTreeMap::new(),
            bootstrap_cache: empty_bootstrap_cache(),
            header_offset: 0,
            already_reconstructed: false,
        };
        let mut registration = XrefRegistration::default();
        let mut diagnostics = Diagnostics::default();

        let error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("a non-stream hybrid object must fail xref-stream parsing");

        assert_eq!(error.to_string(), "parse error at byte 1: xref not found");
        assert!(diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected endobj")));
        let no_sink_error = merge_xref_stream_from_classic_trailer(
            bytes,
            0,
            &mut loaded,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("the no-sink hybrid stream failure must still fail");
        assert_eq!(
            no_sink_error.to_string(),
            "parse error at byte 1: xref not found"
        );
    }

    #[test]
    fn hybrid_xref_holder_diagnostics_survive_a_later_stream_failure() {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let bad_stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"3 0 obj\n<< /NotXRef true >>\nendobj\n");
        let holder_offset = bytes.len() as u64;
        bytes.extend_from_slice(format!("2 0 obj\n{bad_stream_offset}\n").as_bytes());
        let xref_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"xref\n0 4\n");
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        bytes.extend_from_slice(b"0000000000 00000 f \n");
        bytes.extend_from_slice(format!("{holder_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{bad_stream_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 4 /XRefStm 2 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );

        let loaded = load_xref_and_trailer_with_repair(&mut Cursor::new(bytes), true)
            .expect("linear recovery must retain the classic trailer after hybrid failure");

        assert_eq!(
            loaded
                .repair_diagnostics
                .entries()
                .iter()
                .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
                .count(),
            1,
            "the indirect /XRefStm holder warning must survive the failed hybrid parse"
        );
    }

    #[test]
    fn xref_stream_read_errors_forward_repair_diagnostics_to_the_sink() {
        let mut bytes =
            b"1 0 obj\n<< /Type /XRef /W [1 1 1] /Size 2 /Length 6 0 R >>\nstream\ntruncated"
                .to_vec();
        let length_offset = bytes.len();
        bytes.extend_from_slice(b"\n6 0 obj\n3\n");
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(6, 0),
            XrefEntry::Uncompressed {
                offset: length_offset as u64 + 1,
            },
        );
        let mut diagnostics = Diagnostics::default();

        let error = parse_xref_stream(
            &bytes,
            0,
            0,
            "1.7".to_string(),
            XrefLoadOptions::default(),
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("a truncated stream cannot satisfy strict xref-stream framing");

        assert!(error.to_string().contains("expected endstream"));
        assert!(diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("expected endobj")));

        let no_sink_error = parse_xref_stream(
            &bytes,
            0,
            0,
            "1.7".to_string(),
            XrefLoadOptions::default(),
            &mut registration,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("the no-sink read failure must retain its parse error");
        assert!(no_sink_error.to_string().contains("expected endstream"));
    }

    #[test]
    fn bootstrap_context_rejects_a_header_for_a_different_object_reference() {
        let bytes = b" 5 0 obj\n42\nendobj\n";
        let requested = ObjectRef::new(1, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(requested, XrefEntry::Uncompressed { offset: 1 });
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );

        assert_eq!(context.resolve_reference(requested), Object::Null);
        assert_eq!(
            context
                .take_reconstruction_trigger()
                .expect("recovery-mode header mismatch must request reconstruction")
                .to_string(),
            "parse error at byte 1: expected 1 0 obj"
        );
    }

    #[test]
    fn bootstrap_header_mismatch_in_strict_mode_warns_and_keeps_parsed_object() {
        let bytes = b" 5 0 obj\n42\nendobj\n";
        let requested = ObjectRef::new(1, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(requested, XrefEntry::Uncompressed { offset: 1 });
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(context.resolve_reference(requested), Object::Integer(42));
        assert_eq!(
            context
                .diagnostics
                .entries()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            ["(object 1 0, offset 1): expected 1 0 obj"]
        );
        assert_eq!(context.diagnostics.entries()[0].offset, Some(1));
    }

    #[test]
    fn bootstrap_object_zero_header_in_strict_mode_warns_and_resolves_null() {
        let bytes = b" 0 0 obj\n42\nendobj\n";
        let requested = ObjectRef::new(1, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(requested, XrefEntry::Uncompressed { offset: 1 });
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(context.resolve_reference(requested), Object::Null);
        assert_eq!(
            context
                .diagnostics
                .entries()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            ["parse error at byte 1: object with ID 0"]
        );
        assert_eq!(context.diagnostics.entries()[0].offset, Some(1));
    }

    #[test]
    fn bootstrap_header_mismatch_enters_xref_reconstruction() {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let old_only_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"5 0 obj\n(old revision)\nendobj\n");

        let previous_xref_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"xref\n0 6\n");
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        bytes.extend_from_slice(b"0000000000 00000 f \n");
        bytes.extend_from_slice(b"0000000000 00000 f \n");
        bytes.extend_from_slice(b"0000000000 00000 f \n");
        bytes.extend_from_slice(b"0000000000 00000 f \n");
        bytes.extend_from_slice(format!("{old_only_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(b"trailer\n<< /Size 6 >>\n");

        let previous_reference_offset = bytes.len() as u64;
        bytes.extend_from_slice(format!("2 0 obj\n{previous_xref_offset}\nendobj\n").as_bytes());
        let wrong_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"3 0 obj\n999\nendobj\n");
        let root_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"4 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let active_xref_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"xref\n0 6\n");
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        bytes.extend_from_slice(b"0000000000 00000 f \n");
        bytes.extend_from_slice(format!("{wrong_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{wrong_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{root_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(b"0000000000 00000 f \n");
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 6 /Root 4 0 R /Prev 2 0 R >>\nstartxref\n{active_xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );

        let loaded = load_xref_and_trailer_with_repair(&mut Cursor::new(bytes), true)
            .expect("recovery must rebuild the table after an indirect /Prev mismatch");

        assert_eq!(
            loaded.entries.get(&ObjectRef::new(2, 0)),
            Some(&XrefEntry::Uncompressed {
                offset: previous_reference_offset
            }),
            "reconstruction must replace the stale offset for the indirect /Prev object"
        );
        assert!(
            !loaded.entries.contains_key(&ObjectRef::new(5, 0)),
            "reconstruction must preserve the active free row's tombstone"
        );
        assert_eq!(
            loaded
                .repair_diagnostics
                .entries()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            [
                "file is damaged",
                "expected 2 0 obj",
                "Attempting to reconstruct cross-reference table",
            ]
        );
    }

    #[test]
    fn bootstrap_context_resolves_generic_indirect_dictionary_values() {
        let size_offset = 2u64;
        let widths_offset = size_offset + b"6 0 obj\n12\nendobj\n".len() as u64;
        let bytes = b"  \n6 0 obj\n12\nendobj\n7 0 obj\n[1 2 3]\nendobj\n";
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(6, 0),
            XrefEntry::Uncompressed {
                offset: size_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(7, 0),
            XrefEntry::Uncompressed {
                offset: widths_offset,
            },
        );
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        let mut dictionary = Dictionary::new();
        dictionary.insert("Size", Object::Reference(ObjectRef::new(6, 0)));
        dictionary.insert("W", Object::Reference(ObjectRef::new(7, 0)));

        assert_eq!(
            context.resolve_dictionary_value(&dictionary, "Size"),
            Some(Object::Integer(12))
        );
        assert_eq!(
            context.resolve_dictionary_value(&dictionary, "W"),
            Some(Object::Array(vec![
                Object::Integer(1),
                Object::Integer(2),
                Object::Integer(3),
            ]))
        );
        assert_eq!(
            context.resolve_reference(ObjectRef::new(6, 0)),
            Object::Integer(12),
            "a second lookup must use the bootstrap cache"
        );
    }

    #[test]
    fn bootstrap_context_reports_offset_zero_and_beyond_eof_references() {
        let offset_zero = ObjectRef::new(8, 0);
        let beyond_eof = ObjectRef::new(9, 0);
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(offset_zero, XrefEntry::Uncompressed { offset: 0 });
        registration.insert_xref_entry(beyond_eof, XrefEntry::Uncompressed { offset: 999 });
        let mut context = XrefReadContext::new(
            &[],
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );

        assert_eq!(context.resolve_reference(offset_zero), Object::Null);
        assert_eq!(context.resolve_reference(beyond_eof), Object::Null);
        let messages = context
            .diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert!(messages
            .iter()
            .any(|message| message == &"object has offset 0"));
        assert!(messages
            .iter()
            .any(|message| message.contains("object 9 0 is beyond the end of the file")));
    }

    #[test]
    fn bootstrap_context_marks_a_non_integer_indirect_stream_length_invalid() {
        let mut bytes = b" ".to_vec();
        let stream_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Length 6 0 R >>\nstream\ncontent\nendstream\nendobj\n",
        );
        let length_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"6 0 obj\n/NotAnInteger\nendobj\n");

        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(
            ObjectRef::new(1, 0),
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        registration.insert_xref_entry(
            ObjectRef::new(6, 0),
            XrefEntry::Uncompressed {
                offset: length_offset,
            },
        );
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );

        let completed = context
            .read_file_object(
                &bytes[stream_offset as usize..],
                stream_offset,
                RecoveryPolicy::Bounded,
                XrefObjectDescription::Ordinary,
            )
            .expect("invalid indirect length should recover by boundary");
        assert!(completed.object.as_stream().is_some());
        assert!(context
            .diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("/Length key in stream dictionary is not an integer")));
    }

    #[test]
    fn bootstrap_context_resolves_a_cyclic_stream_length_to_null() {
        let object = b"1 0 obj\n<< /Length 1 0 R >>\nstream\nabc\nendstream\nendobj\n";
        let object_ref = ObjectRef::new(1, 0);
        let mut registration = XrefRegistration::default();
        // The object is deliberately recorded at a non-zero offset in the
        // table snapshot; the source slice is prefixed to make that offset
        // valid while keeping the fixture readable.
        let bytes = [b' '; 1]
            .into_iter()
            .chain(object.iter().copied())
            .collect::<Vec<_>>();
        registration.insert_xref_entry(object_ref, XrefEntry::Uncompressed { offset: 1 });
        let mut context = XrefReadContext::new(
            &bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );

        assert_eq!(context.resolve_reference(object_ref), Object::Null);
        assert_eq!(
            context.diagnostics.entries()[0].message,
            "loop detected resolving object 1 0"
        );
    }

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
    fn xref_registration_free_object_suppression_is_local_to_registration() {
        let freed = ObjectRef::new(8, 0);
        let later_generation = ObjectRef::new(8, 2);

        let mut first_registration = XrefRegistration::default();
        first_registration.insert_free_xref_entry(freed);
        first_registration
            .insert_xref_entry(later_generation, XrefEntry::Uncompressed { offset: 30 });
        assert!(!first_registration.entries.contains_key(&later_generation));

        let mut fresh_registration = XrefRegistration::default();
        fresh_registration
            .insert_xref_entry(later_generation, XrefEntry::Uncompressed { offset: 30 });
        assert!(fresh_registration.entries.contains_key(&later_generation));
        assert!(fresh_registration.deleted_objects.is_empty());
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

        let size = loaded.trailer.get("Size").cloned();
        append_xref_size_warning_for(
            size.as_ref(),
            &loaded.entries,
            &BTreeSet::from([5]),
            &mut loaded.repair_diagnostics,
        );

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
    fn xref_stream_candidate_discovery_reuses_cached_reference_reads() {
        let mut bytes = b" ".to_vec();
        let candidate_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type 5 0 R /Size 1 /W [1 1 1] /Length 3 >>\nstream\n",
        );
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        let target_offset = bytes.len() as u64;
        // The missing endobj is recoverable, but it must warn. The candidate's
        // indirect /Type resolves this object before discovery reaches its own
        // xref entry, so the later entry must reuse that cached read.
        bytes.extend_from_slice(b"5 0 obj\n/XRef\n");

        let entries = BTreeMap::from([
            (
                ObjectRef::new(1, 0),
                XrefEntry::Uncompressed {
                    offset: candidate_offset,
                },
            ),
            (
                ObjectRef::new(5, 0),
                XrefEntry::Uncompressed {
                    offset: target_offset,
                },
            ),
        ]);
        let (candidate, diagnostics) =
            find_xref_stream_trailer_candidate(&bytes, &entries, XrefLoadOptions::default());

        assert!(candidate.is_some());
        assert_eq!(
            diagnostics
                .entries()
                .iter()
                .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
                .count(),
            1,
            "a cached reference read must not emit the target warning twice"
        );
    }

    #[test]
    fn xref_stream_candidate_discovery_preserves_a_cyclic_null_cache() {
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let candidate_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Length 1 0 R >>\nstream\n",
        );
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let entries = BTreeMap::from([(
            ObjectRef::new(1, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset,
            },
        )]);
        let (candidate, diagnostics) = find_xref_stream_trailer_candidate(
            &bytes,
            &entries,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );

        assert!(
            candidate.is_none(),
            "a self-referential stream length resolves to cached null, so the object is not a candidate"
        );
        assert!(diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message == "loop detected resolving object 1 0"));
    }

    #[test]
    fn xref_stream_candidate_discovery_preserves_a_cyclic_null_after_read_error() {
        let mut bytes = b" ".to_vec();
        let candidate_offset = bytes.len() as u64;
        bytes.extend_from_slice(
            b"2 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Length 1 0 R >>\nstream\n",
        );
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let entries = BTreeMap::from([(
            ObjectRef::new(1, 0),
            XrefEntry::Uncompressed {
                offset: candidate_offset,
            },
        )]);
        let (candidate, diagnostics) = find_xref_stream_trailer_candidate(
            &bytes,
            &entries,
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
        );

        assert!(candidate.is_none());
        assert!(diagnostics
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message == "loop detected resolving object 1 0"));
    }

    #[test]
    fn xref_stream_build_propagates_a_bootstrap_header_mismatch() {
        let mut bytes = b" 1 0 obj\n<< /Type /XRef /W [1 1 1] /Size 1 /Index 2 0 R /Length 3 0 R >>\nstream\n\0\0\0\nendstream\nendobj\n".to_vec();
        let cyclic_length_offset = bytes.len();
        bytes.extend_from_slice(b"3 0 obj\n3 0 R\nendobj\n");
        let mut registration = XrefRegistration::default();
        registration.insert_xref_entry(ObjectRef::new(2, 0), XrefEntry::Uncompressed { offset: 1 });
        registration.insert_xref_entry(
            ObjectRef::new(3, 0),
            XrefEntry::Uncompressed {
                offset: cyclic_length_offset as u64,
            },
        );
        let mut diagnostics = Diagnostics::default();

        let error = parse_xref_stream(
            &bytes,
            1,
            1,
            "1.7".to_string(),
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration,
            Some(&mut diagnostics),
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("a successful xref build with a mismatch must still request reconstruction");

        assert_eq!(error.to_string(), "parse error at byte 1: expected 2 0 obj");
        assert_eq!(
            diagnostics
                .entries()
                .iter()
                .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
                .count(),
            1,
            "a successful build must forward bootstrap diagnostics exactly once"
        );

        let mut registration_without_sink = XrefRegistration::default();
        registration_without_sink
            .insert_xref_entry(ObjectRef::new(2, 0), XrefEntry::Uncompressed { offset: 1 });
        registration_without_sink.insert_xref_entry(
            ObjectRef::new(3, 0),
            XrefEntry::Uncompressed {
                offset: cyclic_length_offset as u64,
            },
        );
        let no_sink_error = parse_xref_stream(
            &bytes,
            1,
            1,
            "1.7".to_string(),
            XrefLoadOptions {
                allow_repair: true,
                ..XrefLoadOptions::default()
            },
            &mut registration_without_sink,
            None,
            XrefReadContextSpec::ActiveSection,
        )
        .expect_err("the no-sink path must still propagate the reconstruction request");
        assert_eq!(
            no_sink_error.to_string(),
            "parse error at byte 1: expected 2 0 obj"
        );
    }

    #[test]
    fn take_reconstruction_trigger_prefers_the_earlier_raw_trigger_over_the_later_handle_trigger() {
        // The raw framing pass (e.g. an indirect stream `/Length`, recorded
        // by `read_file_object_for_reference`) always runs, and can record
        // `context.reconstruction_trigger`, before the handle-native
        // metadata pass (e.g. `/Size`, `/W`, `/Index`, recorded by
        // `BootstrapHandleDocument::read_uncompressed_object`) that can
        // record a second, later mismatch under `handle_document`'s own
        // trigger -- see `parse_xref_stream`'s raw `read_file_object` call
        // ahead of its `read_file_object_handle`/`build()` calls. When both
        // are set in the same context lifetime, `take_reconstruction_trigger`
        // must report the earlier (raw) one, matching qpdf's own read
        // reporting the first structural problem it hits, not the last.
        let bytes = b"";
        let registration = XrefRegistration::default();
        let mut context = XrefReadContext::new(
            bytes,
            XrefReadContextSpec::ActiveSection,
            &registration,
            XrefLoadOptions::default(),
        );
        context.reconstruction_trigger = Some((5, "expected 5 0 obj".to_string()));
        context
            .handle_document()
            .state
            .borrow_mut()
            .reconstruction_trigger = Some((9, "expected 9 0 obj".to_string()));

        let trigger = context
            .take_reconstruction_trigger()
            .expect("a trigger recorded by either pass must be returned");
        assert_eq!(
            trigger.to_string(),
            "parse error at byte 5: expected 5 0 obj",
            "the raw pass's earlier trigger must win over the handle pass's later one"
        );
        // A single call drains both fields, matching the pre-existing
        // always-drain-the-handle-trigger behavior (now made symmetric).
        assert!(context.reconstruction_trigger.is_none());
        assert!(context
            .handle_document()
            .state
            .borrow()
            .reconstruction_trigger
            .is_none());
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
        // corrupt `startxref` value (999999), so reader diagnostics and later
        // xref traversal retain the recovered source position.
        assert_eq!(loaded.startxref, object_offset);
        // flpdf-specific: `last_xref_form` records the verified source form;
        // the re-entered section is a real `/Type /XRef` stream, so later
        // reader consumers see the recovered structural form accurately.
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
    fn xref_stream_candidate_prev_section_repair_warning_survives_its_own_later_failure() {
        // A different failure shape from the companion test above: there,
        // the candidate's own /Prev target never even decodes. Here, the
        // /Prev target (object 2) needs its own stream-length repair (its
        // `/Length 3` is directly-usable-but-mismatched) and *then* fails
        // its own `/W` validation (only 2 elements, needs 3) --
        // `merge_previous_xref_sections`'s own nested `parse_xref_from_start`
        // call must therefore get the same failure-path diagnostics sink the
        // top-level candidate re-entry call already gets. Empirically
        // verified against qpdf 11.9.0 `--check` on this exact shape: each
        // object's "recovered stream length" warning appears twice -- once
        // during discovery (`getObjectByObjGen` resolves every type-1 entry)
        // and once during re-entry (object 1 directly, object 2 through
        // object 1's `/Prev`) -- before the terminal "error decoding
        // candidate xref stream..." message, with object 1's re-entry warning
        // preceding object 2's failing `/Prev` warning.
        let mut bytes = b"%PDF-1.5\n".to_vec();

        // Object 2 (the /Prev target): needs stream-length repair, and its
        // own /W is malformed (2 elements, not 3).
        let off2 = bytes.len() as u64;
        let stream2 = [0u8, 0, 0, 1, off2 as u8, 0];
        bytes.extend_from_slice(b"2 0 obj\n");
        bytes.extend_from_slice(b"<< /Type /XRef /W [1 1] /Size 2 /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&stream2);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        // Object 1 (the winning candidate): needs its own stream-length repair,
        // and its /Prev points at object 2.
        let off1 = bytes.len() as u64;
        let stream1 = [1u8, 0, 0, 1, off1 as u8, 0];
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(
            format!("<< /Type /XRef /W [1 1 1] /Size 2 /Prev {off2} /Index [0 2] /Length 3 >>\nstream\n").as_bytes(),
        );
        bytes.extend_from_slice(&stream1);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let error = load_xref_and_trailer_with_repair(&mut input, true)
            .expect_err("object 2's own /W then fails validation");
        let (source, diagnostics) = error
            .open_failure()
            .expect("repair failure carries diagnostics");

        assert!(source
            .to_string()
            .contains("error decoding candidate xref stream while recovering damaged file"));
        let recovered_length_warnings: Vec<&str> = diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("recovered stream length"))
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert_eq!(
            recovered_length_warnings.len(),
            4,
            "each repaired stream warns once at discovery and once at re-entry"
        );
        assert!(recovered_length_warnings[0].contains("object 1"));
        assert!(recovered_length_warnings[1].contains("object 2"));
        assert!(
            recovered_length_warnings[2].contains("object 1"),
            "the candidate's own re-entry warning must precede the failing /Prev warning; \
             got {recovered_length_warnings:?}"
        );
        assert!(recovered_length_warnings[3].contains("object 2"));
    }

    #[test]
    fn xref_stream_candidate_successful_prev_repair_diagnostics_preserve_order() {
        // The candidate and its valid `/Prev` section both need stream-length
        // recovery. Their re-entry diagnostics must be appended in the same
        // order qpdf reads the sections, without duplicating the candidate's
        // warning when the merge succeeds.
        let mut bytes = b"%PDF-1.5\n".to_vec();

        // Object 2 is a valid older xref-stream section, but its declared
        // length is shorter than its six-byte payload and needs recovery.
        let off2 = bytes.len() as u64;
        let stream2 = [0u8, 0, 0, 1, off2 as u8, 0];
        bytes.extend_from_slice(b"2 0 obj\n");
        bytes.extend_from_slice(b"<< /Type /XRef /W [1 1 1] /Size 2 /Length 3 >>\nstream\n");
        bytes.extend_from_slice(&stream2);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        // Object 1 is the winning candidate and points to object 2 through
        // `/Prev`; it also needs stream-length recovery on both reads.
        let off1 = bytes.len() as u64;
        let stream1 = [1u8, 0, 0, 1, off1 as u8, 0];
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(
            format!("<< /Type /XRef /W [1 1 1] /Size 2 /Prev {off2} /Length 3 >>\nstream\n")
                .as_bytes(),
        );
        bytes.extend_from_slice(&stream1);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("the candidate and its valid /Prev section both recover");
        let recovered_length_warnings: Vec<&str> = loaded
            .repair_diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("recovered stream length"))
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();

        assert_eq!(
            recovered_length_warnings.len(),
            4,
            "each repaired stream warns once at discovery and once at re-entry"
        );
        assert!(recovered_length_warnings[0].contains("object 1"));
        assert!(recovered_length_warnings[1].contains("object 2"));
        assert!(recovered_length_warnings[2].contains("object 1"));
        assert!(recovered_length_warnings[3].contains("object 2"));
    }

    #[test]
    fn xref_stream_candidate_prev_success_then_failure_preserves_all_repair_order() {
        // Exercise the failure path after an earlier `/Prev` section has
        // already merged a repair diagnostic into `reentry.loaded`. That
        // diagnostic must be surfaced before the later failing section's
        // buffered diagnostic.
        let mut bytes = b"%PDF-1.5\n".to_vec();

        // Use a two-byte offset field so the three-object fixture remains
        // valid even after the object dictionaries make the offsets exceed a
        // single byte. Object 3 will fail filter decoding after its length
        // repair, while object 2 is a valid intermediate `/Prev` section.
        let make_stream_data = |offset: u64| {
            let offset = u16::try_from(offset).expect("fixture offset fits in two bytes");
            [1u8, (offset >> 8) as u8, offset as u8, 0]
        };

        // Keep object 3's low offset byte outside PDF whitespace so its
        // deliberately short `/Length` cannot look like an exact boundary.
        bytes.extend_from_slice(b"% xref-chain-padding-for-repair-order-test\n");
        let off3 = bytes.len() as u64;
        let stream3 = make_stream_data(off3);
        bytes.extend_from_slice(b"3 0 obj\n");
        bytes.extend_from_slice(
            b"<< /Type /XRef /W [1 2 1] /Size 2 /Index [1 1] /Filter /Bogus /Length 1 >>\nstream\n",
        );
        bytes.extend_from_slice(&stream3);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let off2 = bytes.len() as u64;
        let stream2 = make_stream_data(off2);
        bytes.extend_from_slice(b"2 0 obj\n");
        bytes.extend_from_slice(
            format!("<< /Type /XRef /W [1 2 1] /Size 2 /Index [1 1] /Prev {off3} /Length 1 >>\nstream\n")
                .as_bytes(),
        );
        bytes.extend_from_slice(&stream2);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let off1 = bytes.len() as u64;
        let stream1 = make_stream_data(off1);
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(
            format!("<< /Type /XRef /W [1 2 1] /Size 2 /Index [1 1] /Prev {off2} /Length 1 >>\nstream\n")
                .as_bytes(),
        );
        bytes.extend_from_slice(&stream1);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let error = load_xref_and_trailer_with_repair(&mut input, true)
            .expect_err("the oldest /Prev section fails its unsupported filter validation");
        let (source, diagnostics) = error
            .open_failure()
            .expect("repair failure carries diagnostics");
        assert!(source
            .to_string()
            .contains("error decoding candidate xref stream while recovering damaged file"));

        let recovered_length_warnings: Vec<&str> = diagnostics
            .entries()
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("recovered stream length"))
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert_eq!(
            recovered_length_warnings.len(),
            6,
            "each section warns at discovery and each re-entered section warns once; got {recovered_length_warnings:?}"
        );
        assert!(recovered_length_warnings[0].contains("object 1"));
        assert!(recovered_length_warnings[1].contains("object 2"));
        assert!(recovered_length_warnings[2].contains("object 3"));
        assert!(recovered_length_warnings[3].contains("object 1"));
        assert!(recovered_length_warnings[4].contains("object 2"));
        assert!(recovered_length_warnings[5].contains("object 3"));
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
        // (object 2) and not the original corrupt value (999999), so the
        // recovered source position remains usable for later xref traversal.
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

        let (candidate, _diagnostics) =
            find_xref_stream_trailer_candidate(&bytes, &entries, XrefLoadOptions::default());
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

        let (candidate, _diagnostics) =
            find_xref_stream_trailer_candidate(&bytes, &entries, XrefLoadOptions::default());
        let candidate = candidate.expect(
            "64 unrelated entries needing their own fallback must not deny \
             candidate 200's fallback, visited last in ascending object-number order",
        );
        assert_eq!(candidate.max_offset, candidate_offset);
    }

    #[test]
    fn xref_stream_candidate_free_entry_remains_absent() {
        // qpdf's candidate re-entry records free rows as number-wide
        // tombstones, not live entries. Object 7 is explicitly free in the
        // newest revision and must therefore remain absent from the recovered
        // live table, even though an older revision contains it as a packed
        // object in ObjStm 8.
        //
        // Revision 1 (xref stream 3): object 7 packed compressed in ObjStm 8.
        // Revision 2 (xref stream 4, `/Prev` -> revision 1, the recovery
        // candidate): object 7 marked free. Verified against real qpdf
        // 11.9.0 `--show-xref` on this exact shape: object 7 is absent from
        // the reconstructed table (objects 1, 3, 4, 8 recovered).
        fn entry(entry_type: u8, f1: u32, f2: u8) -> [u8; 4] {
            let f1_bytes = f1.to_be_bytes();
            [entry_type, f1_bytes[2], f1_bytes[3], f2]
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
            "a real free entry from the candidate's own revision chain leaves object 7 \
             absent from the live table, matching qpdf -- `entries` only ever holds live rows \
             (`XrefRegistration` never gives a free row a map entry)"
        );
        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(8, 0)),
            Some(&crate::XrefEntry::Uncompressed {
                offset: off8 as u64
            }),
        );
    }

    #[test]
    fn xref_stream_candidate_free_entry_is_number_wide() {
        // qpdf's `processXRefStream` (`QPDF.cc:1120-1124`) hardcodes
        // `QPDFObjGen(obj, 0)` for a type-0 row and explicitly discards
        // field 2 ("Ignore fields[2], which we don't care about in this
        // case"); `insertFreeXrefEntry` (`QPDF.cc:1187-1190`) then records
        // only the object *number* in `m->deleted_objects`
        // (`std::set<int>`, `QPDF.hh:1466`) -- generation plays no part in
        // qpdf's free/deleted bookkeeping. A real xref stream commonly
        // writes a nonzero field 2 for a freed object (the generation to
        // use *if the number is reused*), so this fixture -- identical to
        // `xref_stream_candidate_free_entry_remains_absent` except object
        // 7's free row now carries generation 1 -- object number, rather than
        // the row's generation, determines the tombstone. Blocking only the
        // exact `(7, 1)` key (as a naive tombstone merge would) leaves `(7, 0)`
        // live.
        fn entry(entry_type: u8, f1: u32, f2: u8) -> [u8; 4] {
            let f1_bytes = f1.to_be_bytes();
            [entry_type, f1_bytes[2], f1_bytes[3], f2]
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
    fn discard_lower_generations_keeps_only_the_highest_live_row() {
        let mut entries = BTreeMap::from([
            (ObjectRef::new(7, 0), XrefEntry::Uncompressed { offset: 10 }),
            (ObjectRef::new(7, 1), XrefEntry::Uncompressed { offset: 20 }),
            (ObjectRef::new(8, 0), XrefEntry::Uncompressed { offset: 30 }),
        ]);
        // Object 7's discarded generation was already read as an xref stream
        // while walking the /Prev chain (QPDF::removeObject, QPDF.cc:1996-2005,
        // erases both m->xref_table and m->obj_cache; parsed_xref_streams is
        // flpdf's pre-Pdf-construction stand-in for the latter).
        let mut parsed_xref_streams = BTreeMap::from([
            (ObjectRef::new(7, 0), Object::Null),
            (ObjectRef::new(7, 1), Object::Null),
            (ObjectRef::new(8, 0), Object::Null),
        ]);

        discard_lower_generations(&mut entries, &mut parsed_xref_streams);

        assert!(!entries.contains_key(&ObjectRef::new(7, 0)));
        assert_eq!(
            entries.get(&ObjectRef::new(7, 1)),
            Some(&XrefEntry::Uncompressed { offset: 20 })
        );
        assert!(entries.contains_key(&ObjectRef::new(8, 0)));

        assert!(
            !parsed_xref_streams.contains_key(&ObjectRef::new(7, 0)),
            "the discarded generation's already-parsed xref stream must not \
             survive to resurface as a live handle"
        );
        assert!(parsed_xref_streams.contains_key(&ObjectRef::new(7, 1)));
        assert!(parsed_xref_streams.contains_key(&ObjectRef::new(8, 0)));
    }

    #[test]
    fn xref_stream_candidate_live_entry_merge_applies_post_chain_cleanup() {
        // qpdf's `insertXrefEntry` (`QPDF.cc:1149-1181`) keys live-entry
        // priority off the *exact* `QPDFObjGen(obj, f2)` via
        // `m->xref_table.try_emplace(...)` -- only an entry for that same
        // (number, generation) pair blocks a later one. Only
        // `insertFreeXrefEntry`'s own `m->deleted_objects` (`QPDF.cc:1187-1190`)
        // is number-wide. Object number alone is therefore too coarse a key
        // for the live-entry merge below: the line scan can discover an
        // generation 1 body text (`7 1 obj`) while the candidate xref
        // stream supplies a different generation (`7 0`, packed into an
        // ObjStm). The merge
        // keeps both exact generations, then the post-chain cleanup at
        // `QPDF.cc:710-718` removes the lower row from the effective table.
        let mut bytes = b"%PDF-1.7\n".to_vec();

        let off1 = bytes.len() as u32;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let off7g1 = bytes.len() as u32;
        bytes.extend_from_slice(b"7 1 obj\n<< /Foo /Stale >>\nendobj\n");

        let off9 = bytes.len() as u32;
        let objstm_header = b"7 0\n";
        let objstm_body = b"<< /Foo /Current >>\n";
        let mut objstm_payload = objstm_header.to_vec();
        objstm_payload.extend_from_slice(objstm_body);
        bytes.extend_from_slice(b"9 0 obj\n");
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

        fn entry(entry_type: u8, f1: u32, f2: u8) -> [u8; 4] {
            let f1_bytes = f1.to_be_bytes();
            [entry_type, f1_bytes[2], f1_bytes[3], f2]
        }

        let off3 = bytes.len() as u32;
        let mut xref_entries = Vec::new();
        xref_entries.extend(entry(0, 0, 0)); // 0 free
        xref_entries.extend(entry(1, off1, 0)); // 1
        xref_entries.extend(entry(1, off3, 0)); // 3 = this stream
        xref_entries.extend(entry(2, 9, 0)); // 7 gen 0, compressed in objstm 9, index 0
        xref_entries.extend(entry(1, off9, 0)); // 9 = the objstm
        bytes.extend_from_slice(b"3 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /XRef /W [1 2 1] /Size 10 /Index [0 1 1 1 3 1 7 1 9 1] /Length {} >>\n",
                xref_entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(b"stream\n");
        bytes.extend_from_slice(&xref_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("qpdf recovers the trailer from the reconstructed /XRef stream candidate");

        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(7, 1)),
            Some(&crate::XrefEntry::Uncompressed {
                offset: off7g1 as u64
            }),
            "the highest generation must remain in the effective xref view"
        );
        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(7, 0)),
            None,
            "the lower generation must be removed after the chain is loaded"
        );
    }

    #[test]
    fn xref_stream_candidate_preserves_out_of_range_offsets() {
        // A candidate's own re-entry decodes xref-stream *data* -- arbitrary
        // bytes from the file, not offsets rediscovered by re-scanning it --
        // so a corrupt or malicious xref stream can declare an offset that
        // does not exist in the file at all. Object 5's declared offset
        // (1,000,000) is far past this fixture's real length; qpdf preserves
        // the entry and leaves offset validation to the later object read.
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let bogus_offset: u32 = 1_000_000;
        let mut stream1 = Vec::new();
        stream1.push(1u8); // type 1 (uncompressed)
        stream1.extend_from_slice(&bogus_offset.to_be_bytes()); // 4-byte offset
        stream1.push(0u8); // generation
        bytes.extend_from_slice(b"1 0 obj\n");
        bytes.extend_from_slice(
            format!(
                "<< /Type /XRef /W [1 4 1] /Size 6 /Index [5 1] /Length {} >>\nstream\n",
                stream1.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&stream1);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(b"startxref\n999999\n%%EOF\n");
        assert!(!bytes.windows(7).any(|window| window == b"trailer"));
        assert!((bogus_offset as usize) > bytes.len());

        let mut input = Cursor::new(bytes);
        let loaded = load_xref_and_trailer_with_repair(&mut input, true)
            .expect("object 1's own candidate xref stream still decodes cleanly");

        assert_eq!(
            loaded.entries.get(&crate::ObjectRef::new(5, 0)),
            Some(&crate::XrefEntry::Uncompressed {
                offset: bogus_offset as u64
            }),
            "the out-of-range entry is still recorded as-is (qpdf's own \
             insertXrefEntry does not validate offsets; later object reads validate use)"
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
        let options = XrefLoadOptions {
            allow_repair: true,
            ..XrefLoadOptions::default()
        };

        let (_trailer, max_offset, form, _deleted_objects, _first_xref_item_offset) =
            recover_trailer_from_xref_stream_candidate(
                &bytes,
                "1.5",
                options,
                &mut entries,
                &mut parsed_xref_streams,
                &mut repair_diagnostics,
                &mut trailer_references,
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
    fn recover_trailer_from_xref_stream_candidate_filters_free_line_scan_entries() {
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let stale_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"5 0 obj\n(obsolete)\nendobj\n");
        let candidate_offset = bytes.len() as u64;
        bytes.extend(xref_stream_object_bytes(
            9,
            "/Size 10 /Index [5 1]",
            &[0u8, 0, 0],
        ));

        let mut entries = BTreeMap::from([
            (
                ObjectRef::new(5, 0),
                XrefEntry::Uncompressed {
                    offset: stale_offset,
                },
            ),
            (
                ObjectRef::new(9, 0),
                XrefEntry::Uncompressed {
                    offset: candidate_offset,
                },
            ),
        ]);
        let mut parsed_xref_streams = BTreeMap::new();
        let mut repair_diagnostics = Diagnostics::default();
        let mut trailer_references = BTreeSet::new();
        let options = XrefLoadOptions {
            allow_repair: true,
            ..XrefLoadOptions::default()
        };

        let (_trailer, _max_offset, _form, deleted_objects, _first_xref_item_offset) =
            recover_trailer_from_xref_stream_candidate(
                &bytes,
                "1.5",
                options,
                &mut entries,
                &mut parsed_xref_streams,
                &mut repair_diagnostics,
                &mut trailer_references,
            )
            .expect("candidate free row must recover");

        assert!(
            !entries.contains_key(&ObjectRef::new(5, 0)),
            "a candidate free row must suppress the obsolete line-scan entry"
        );
        assert!(deleted_objects.contains(&5));

        let recovered = recover_xref_from_linear_scan(
            &bytes,
            "1.5".to_owned(),
            0,
            Error::parse(0, "forced candidate recovery"),
            None,
            options,
            Diagnostics::default(),
            None,
        )
        .expect("candidate recovery must consume its local tombstone before returning");
        assert!(!recovered.loaded.entries.contains_key(&ObjectRef::new(5, 0)));
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

        let (candidate, _diagnostics) =
            find_xref_stream_trailer_candidate(&bytes, &entries, XrefLoadOptions::default());
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
            find_xref_stream_trailer_candidate(&bytes, &entries, XrefLoadOptions::default());
        candidate.expect("object 2 is still found as the /Type /XRef candidate");
        assert!(
            discovery_diagnostics
                .entries()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("expected endobj")),
            "object 1's own missing-endobj warning must survive even though it isn't a stream"
        );
    }

    #[test]
    fn parse_trailer_candidate_surfaces_a_duplicate_key_warning_at_the_qpdf_offset() {
        let bytes = b"trailer<< /Size 3 /Root 1 0 R /Foo 1 /Foo 2 >>";
        let start = "trailer".len();
        let (trailer, diagnostics) = parse_trailer_candidate(bytes, start);
        let trailer = trailer.expect("dictionary");
        assert_eq!(
            trailer.get("Foo"),
            Some(&Object::Integer(2)),
            "last write wins"
        );

        let dict_open_end = start + "<< ".len() - 1; // byte right after "<<"
        assert_eq!(
            diagnostics,
            vec![Diagnostic::warning(
                format!(
                    "(trailer, offset {dict_open_end}): dictionary has duplicated key /Foo; \
                     last occurrence overrides earlier ones"
                ),
                Some(dict_open_end as u64),
            )]
        );
    }

    #[test]
    fn parse_trailer_candidate_surfaces_warnings_even_when_the_candidate_is_rejected() {
        // qpdf's reconstruct_xref calls readTrailer() unconditionally and only
        // rejects a non-dictionary result afterward ("Oh well.  It was worth a
        // try.", QPDF.cc:566-568); any warning the parser already raised while
        // building that rejected candidate still reaches m->warnings.
        let bytes = b"trailer[ 1 0 << /x 1 /x 2 >> ]";
        let start = "trailer".len();
        let (trailer, diagnostics) = parse_trailer_candidate(bytes, start);
        assert_eq!(trailer, None, "a non-dictionary candidate is rejected");

        let dict_open_end = bytes
            .windows(2)
            .position(|window| window == b"<<")
            .expect("nested dict open")
            + 2;
        assert_eq!(
            diagnostics,
            vec![Diagnostic::warning(
                format!(
                    "(trailer, offset {dict_open_end}): dictionary has duplicated key /x; \
                     last occurrence overrides earlier ones"
                ),
                Some(dict_open_end as u64),
            )]
        );
    }

    #[test]
    fn parse_xref_table_surfaces_a_duplicate_trailer_key_warning() {
        let bytes = b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 /Foo 1 /Foo 2 >>\n";
        let mut cursor = ByteCursor::new(bytes, 4);
        let (entries, trailer, diagnostics, first_xref_item_offset) =
            parse_xref_table(&mut cursor, bytes, None, None).expect("valid classic xref section");
        assert_eq!(entries.len(), 1);
        assert_eq!(first_xref_item_offset, 9);
        assert_eq!(
            trailer.get("Foo"),
            Some(&Object::Integer(2)),
            "last write wins"
        );

        let dict_open_end = bytes
            .windows(2)
            .position(|window| window == b"<<")
            .expect("trailer dict open")
            + 2;
        assert_eq!(
            diagnostics,
            vec![Diagnostic::warning(
                format!(
                    "(trailer, offset {dict_open_end}): dictionary has duplicated key /Foo; \
                     last occurrence overrides earlier ones"
                ),
                Some(dict_open_end as u64),
            )]
        );
    }

    #[test]
    fn parse_xref_table_forwards_diagnostics_to_the_sink_when_the_trailer_parse_fails() {
        // qpdf's `QPDFParser::warn` dispatches synchronously to `QPDF::warn`
        // (`libqpdf/QPDFParser.cc:488-497`) at the moment `warnDuplicateKey`
        // fires, independent of whether the rest of the dictionary parses
        // successfully. A truncated trailer that hits EOF right after a
        // duplicate key must still forward that already-recorded warning to
        // the caller's diagnostics sink, even though `parse_xref_table`
        // itself returns `Err`.
        let bytes = b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Foo 1 /Foo 2";
        let mut cursor = ByteCursor::new(bytes, 4);
        let mut sink = Diagnostics::default();
        let result = parse_xref_table(&mut cursor, bytes, Some(&mut sink), None);
        assert!(result.is_err(), "truncated trailer dictionary must fail");

        let dict_open_end = bytes
            .windows(2)
            .position(|window| window == b"<<")
            .expect("trailer dict open")
            + 2;
        assert_eq!(
            sink.entries(),
            [Diagnostic::warning(
                format!(
                    "(trailer, offset {dict_open_end}): dictionary has duplicated key /Foo; \
                     last occurrence overrides earlier ones"
                ),
                Some(dict_open_end as u64),
            )]
        );
    }

    #[test]
    fn parse_xref_table_fails_without_a_sink_when_the_trailer_parse_fails() {
        // A caller not tracking diagnostics (`error_diagnostics_sink: None`,
        // e.g. a call chain that never threaded a sink) must still see the
        // parse failure -- only the diagnostic-forwarding step is skipped.
        let bytes = b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Foo 1 /Foo 2";
        let mut cursor = ByteCursor::new(bytes, 4);
        let result = parse_xref_table(&mut cursor, bytes, None, None);
        assert!(result.is_err(), "truncated trailer dictionary must fail");
    }

    #[test]
    fn parse_xref_table_forwards_diagnostics_to_the_sink_when_the_trailer_is_not_a_dictionary() {
        // Mirrors `parse_trailer_candidate_surfaces_warnings_even_when_the_candidate_is_rejected`
        // above, but through `parse_xref_table`'s ordinary xref-table path:
        // qpdf's `readTrailer()` shares the same `QPDFParser` (`QPDF.cc:1313-1317`),
        // so a warning raised while parsing a rejected (non-dictionary)
        // trailer candidate still reaches `m->warnings`.
        let bytes = b"xref\n0 1\n0000000000 65535 f \ntrailer\n[ 1 0 << /x 1 /x 2 >> ]";
        let mut cursor = ByteCursor::new(bytes, 4);
        let mut sink = Diagnostics::default();
        let result = parse_xref_table(&mut cursor, bytes, Some(&mut sink), None);
        assert!(
            result.is_err(),
            "a non-dictionary trailer candidate must be rejected"
        );

        let dict_open_end = bytes
            .windows(2)
            .position(|window| window == b"<<")
            .expect("nested dict open")
            + 2;
        assert_eq!(
            sink.entries(),
            [Diagnostic::warning(
                format!(
                    "(trailer, offset {dict_open_end}): dictionary has duplicated key /x; \
                     last occurrence overrides earlier ones"
                ),
                Some(dict_open_end as u64),
            )]
        );
    }

    #[test]
    fn parse_xref_table_fails_without_a_sink_when_the_trailer_is_not_a_dictionary() {
        let bytes = b"xref\n0 1\n0000000000 65535 f \ntrailer\n[ 1 0 << /x 1 /x 2 >> ]";
        let mut cursor = ByteCursor::new(bytes, 4);
        let result = parse_xref_table(&mut cursor, bytes, None, None);
        assert!(
            result.is_err(),
            "a non-dictionary trailer candidate must be rejected"
        );
    }

    #[test]
    fn recovery_line_scan_surfaces_a_duplicate_trailer_key_warning() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        bytes.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R /Foo 1 /Foo 2 >>\n");
        // `startxref 0` points at the header, not an `xref` keyword, so the
        // primary read fails and the line-scan reconstruction path
        // (`recover_xref_from_linear_scan` -> `recover_xref_entries` ->
        // `parse_trailer_candidate`) runs.
        bytes.extend_from_slice(b"startxref\n0\n%%EOF\n");

        let loaded = load_xref_and_trailer_with_repair(&mut Cursor::new(bytes), true)
            .expect("recovery must rebuild the table from the line scan");

        let messages: Vec<&str> = loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert!(
            messages.iter().any(|message| {
                message.starts_with("(trailer, offset ") && message.contains("duplicated key /Foo")
            }),
            "recovery must surface the trailer's duplicate-key warning: {messages:?}"
        );
    }

    #[test]
    fn normal_xref_table_read_surfaces_a_duplicate_trailer_key_warning() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let off1 = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_start = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        bytes.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 2 /Root 1 0 R /Foo 1 /Foo 2 >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );

        let loaded = load_xref_and_trailer_with_repair(&mut Cursor::new(bytes), true)
            .expect("well-formed classic xref must not trigger recovery");

        let messages: Vec<&str> = loaded
            .repair_diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert!(
            messages.iter().any(|message| {
                message.starts_with("(trailer, offset ") && message.contains("duplicated key /Foo")
            }),
            "the ordinary xref-table read must surface the trailer's duplicate-key warning: {messages:?}"
        );
    }

    #[test]
    fn parse_trailer_candidate_handles_a_start_offset_past_the_end_of_the_buffer() {
        let bytes = b"trailer";
        let (trailer, diagnostics) = parse_trailer_candidate(bytes, bytes.len() + 1);
        assert_eq!(trailer, None);
        assert!(diagnostics.is_empty());
    }
}
