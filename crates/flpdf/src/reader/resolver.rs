//! The canonical document resolver: the state `QPDF::resolve` reaches for, and
//! the borrow seam that lets it be reached from an [`ObjectHandle`] alone.
//!
//! qpdf correspondence: `QPDF::resolve` (`libqpdf/QPDF.cc:1700-1753`) and the `QPDF::Members` fields it touches.
//!
//! # Why this exists as its own owner
//!
//! `DocumentResolver::resolve_indirect` takes `&self`
//! (`object_handle.rs`), because a handle reaches its document through a
//! `Weak`, not through a `&mut Pdf` the caller happens to hold. Resolution
//! nevertheless needs the input source, the object cache, and the in-progress
//! set *mutably*. [`ResolverCore`] is that mutable state, [`ResolverHandle`]
//! wraps it in a `RefCell`, and `Pdf` owns the whole thing behind an `Rc` so
//! handles can hold a `Weak` to it.
//!
//! That ownership is also why the canonical handle registry lives here rather
//! than on `Pdf`: resolving an object means minting a canonical handle for
//! every nested `N G R` in its body, and `resolve_indirect` has no `&mut Pdf`
//! to mint through. See [`ResolverCore::object_cache`].
//!
//! # Borrow discipline
//!
//! Every non-test accessor below takes its `RefCell` borrow and drops it
//! within a single expression, and — **for every `R` in this workspace** —
//! calls nothing that could re-enter resolution while that borrow is live.
//! That is not stylistic: resolution is re-entrant (qpdf's own
//! `/Length`-as-indirect-reference case, `QPDF::readStream`,
//! `libqpdf/QPDF.cc:1360-1399`), so a borrow held across a nested resolve is a
//! `RefCell` double-borrow panic in production.
//!
//! **The qualifier is load-bearing and the hazard is real.**
//! [`ResolverCore::seek`], [`ResolverCore::tell`] and [`ResolverCore::read`]
//! each hold `borrow_mut()` across `R::seek`/`R::read`, which is
//! caller-supplied code. `R` is arbitrary and
//! [`ObjectHandle`] is `'static + Clone`, so an `R` that owns a handle from
//! this same document could call `try_dereference` from inside `read` and
//! re-enter — reaching a double-borrow panic from entirely safe Rust, once
//! anything in the resolver path takes a borrow of its own. Nothing in-tree
//! does this (every `R` is a `Cursor` or `BufReader`), so it is unreachable
//! today rather than guarded against.
//!
//! `with_reader_mut` is the one accessor that hands out `&mut R` while the
//! borrow is held, which is why it is `#[cfg(test)]` and carries its own
//! warning.
//!
//! A full uncompressed resolution takes borrows at seven kinds of place, and
//! every one of them begins and ends inside a single expression:
//! [`ResolveMark::begin`] and its `drop`; [`ResolverHandle::xref_entry`];
//! [`ResolverHandle::seek`]/[`ResolverHandle::seek_relative`]/[`ResolverHandle::tell`]/[`ResolverHandle::read`],
//! once per input operation; [`ResolverHandle::get_object_handle`], once per
//! nested `N G R` the parse mints a handle for; [`ResolverHandle::push_warning`]
//! wherever a warning is raised.
//!
//! **This is pinned by tests now, not merely stated**, and the mutation that
//! pins it is the one that actually reaches the seam. Taking a borrow inside
//! [`ResolverHandle::read_stream`] that spans only the `/Length` dereference
//! reddens every fixture that gives a stream an *indirect* `/Length` — six
//! tests today, and by construction whichever tests have that shape later —
//! each panicking `RefCell already borrowed` inside [`ResolveMark::begin`],
//! which is the nested resolution's first borrow. A fixture whose `/Length`
//! is a direct integer cannot catch it: nothing re-enters, so there is no
//! second borrow for it to collide with.
//!
//! **A coarser mutation proves less than it looks, and an earlier revision of
//! this comment mistook one for a verification.** It cited "holding
//! [`ResolverHandle::xref_entry`]'s borrow across `read_object_at_offset`
//! fails six tests". That borrow spans the whole read-and-parse phase, so it
//! panics in [`ResolverHandle::seek`] on the very first input operation,
//! before any nested resolution is reached — it shows that the input wrappers
//! need the borrow free, not that the seam is guarded. Its count was wrong by
//! construction, too: what that mutation reddens is *every* test that drives a
//! real read through this resolver, so the number grows with every fixture
//! added and "six" was already stale when this was read again. That is why the
//! claim above is phrased as which fixtures fail rather than how many. The
//! same revision claimed that wrapping a
//! [`ResolverHandle::push_warning`] call in a borrow "fails five"; it named
//! no call site and could not be reproduced, so it is withdrawn rather than
//! restated.
//!
//! The warning sink stays the easiest way to get this wrong, because
//! `push_warning` needs `borrow_mut` and the code that warns is the code that
//! has just finished reading something. The live file-object parser returns
//! diagnostics to [`ResolverHandle::read_object_at_offset_with_description`] after it has
//! released its input adapter, so each source token contributes at most one
//! document warning.
//!
//! `ResolverHandle::read_window` and its `read_to_owned` helper remain a
//! legacy owned-buffer seam: qpdf reads its live `m->file` source and has no
//! bounded-window helper.

use crate::encryption::crypt_filters::interpret_cf_from_handle;
use crate::encryption::state::{EncryptionMode, EncryptionState};
use crate::object_handle::{DocumentResolver, ObjectValue, StreamDataProvider, NO_PARSED_OFFSET};
use crate::parser::{
    parse_live_file_object_with_decrypter, parse_object_handle_with_context,
    parse_qpdf_direct_object_handle_with_diagnostics, trailing_data_error, LiveInput,
    LiveTokenSource, StringDecrypter,
};
use crate::pipeline::aes::PlAesPdf;
use crate::pipeline::rc4::PlRc4;
use crate::pipeline::Pipeline;
use crate::tokenizer::{Token, TokenType, Tokenizer};
use crate::{Diagnostic, Diagnostics, Error, ObjectHandle, ObjectRef, Result, XrefEntry};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::rc::{Rc, Weak};

/// qpdf's `InvalidInputSource` exception text (`libqpdf/QPDF.cc:55-106`).
pub(crate) const CLOSED_INPUT_SOURCE_ERROR: &str =
    "QPDF operation attempted on a QPDF object with no input source. QPDF operations are invalid before processFile (or another process method) or after closeInputSource";
pub(crate) const CLOSED_INPUT_SOURCE_NAME: &str = "closed input source";

/// The state `QPDF::resolve` and the functions it calls operate on.
///
/// The field list is taken from qpdf's `QPDF::Members`
/// (`include/qpdf/QPDF.hh`) restricted to what `QPDF::resolve`,
/// `QPDF::readObjectAtOffset`, and `QPDF::resolveObjectsInStream` touch.
/// Everything else a document owns — `version`, `trailer`, `startxref`,
/// `foreign_object_maps`, dirty tracking, and every legacy field already
/// carrying a `qpdf-cutover-delete` marker — stays on `Pdf`.
///
/// One member is present for a consumer this slice does not yet have: the
/// encryption parameters (qpdf `m->encp`), added in flpdf-25kg.3.11 so
/// flpdf-25kg.3.10's pipe-time stream decryption has something to read.
/// Resolve-time *string* decryption is unrelated and unchanged — qpdf
/// decrypts strings during `readObjectAtOffset`'s parse
/// (`StringDecrypter`, `libqpdf/QPDF.cc:1337-1339`) but streams only at pipe
/// time (`decryptStream`, `QPDF.cc:2491`). See
/// [`ResolverCore::encryption_parameters`].
///
/// The shared input source corresponding to qpdf's `m->file`.
///
/// `QPDF::ForeignStreamData` retains the same `InputSource` shared pointer as
/// the source document, rather than retaining the source `QPDF` object
/// (`libqpdf/QPDF.cc:2265-2273`). Keeping the reader, header wrapper, and
/// `last_offset` in one `Rc` gives the Rust port the same ownership boundary:
/// a deferred foreign-stream provider can outlive its source resolver while
/// still using the source input and its logical offset coordinates.
#[derive(Debug)]
enum StreamReadError {
    /// The wrapped reader failed while delivering the requested bytes. qpdf
    /// turns this `fread`/`ferror` case into `read N bytes` when the source has
    /// a description (`FileInputSource.cc:116-127`).
    UnderlyingRead(Error),
    /// A different input operation failed while implementing the read
    /// contract, such as qpdf's EOF normalization seek. This must retain its
    /// own error classification and message.
    Operation(Error),
}

impl StreamReadError {
    fn into_error(self) -> Error {
        match self {
            Self::UnderlyingRead(error) | Self::Operation(error) => error,
        }
    }
}

struct StreamInput<R: Read + Seek + 'static> {
    /// `None` is qpdf's `InvalidInputSource`. The input source is held behind
    /// an `Rc` and replaced, rather than mutated, when it is closed so a
    /// deferred foreign-stream provider retains the source it captured before
    /// `QPDF::closeInputSource` (`libqpdf/QPDF.cc:278-281,2265-2273`).
    reader: Option<Rc<RefCell<R>>>,
    header_offset: usize,
    last_offset: Cell<u64>,
}

impl<R: Read + Seek + 'static> StreamInput<R> {
    fn new(reader: R, header_offset: usize) -> Self {
        Self {
            reader: Some(Rc::new(RefCell::new(reader))),
            header_offset,
            last_offset: Cell::new(0),
        }
    }

    fn invalid() -> Self {
        Self {
            reader: None,
            header_offset: 0,
            last_offset: Cell::new(0),
        }
    }

    fn active_reader(&self) -> Result<&Rc<RefCell<R>>> {
        self.reader
            .as_ref()
            .ok_or_else(|| Error::Internal(CLOSED_INPUT_SOURCE_ERROR.to_owned()))
    }

    fn is_closed(&self) -> bool {
        self.reader.is_none()
    }

    fn seek(&self, offset: u64) -> Result<()> {
        let physical = (self.header_offset as u64).saturating_add(offset);
        self.active_reader()?
            .borrow_mut()
            .seek(SeekFrom::Start(physical))?;
        Ok(())
    }

    fn tell(&self) -> Result<u64> {
        Ok(self
            .active_reader()?
            .borrow_mut()
            .stream_position()?
            .saturating_sub(self.header_offset as u64))
    }

    fn source_length(&self) -> Result<u64> {
        let mut reader = self.active_reader()?.borrow_mut();
        let position = reader.stream_position()?;
        let end = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(position))?;
        Ok(end.saturating_sub(self.header_offset as u64))
    }

    fn seek_relative(&self, delta: u64) -> Result<()> {
        const MAX_OFFSET: u64 = i64::MAX as u64;

        let mut reader = self.active_reader()?.borrow_mut();
        let position = reader.stream_position()?;
        if delta > MAX_OFFSET.saturating_sub(position) {
            return Err(Error::parse(
                position as usize,
                format!("adding {delta} to {position} would cause an integer overflow"),
            ));
        }
        reader.seek(SeekFrom::Current(delta as i64))?;
        Ok(())
    }

    fn read(&self, buf: &mut [u8]) -> std::result::Result<usize, StreamReadError> {
        self.last_offset
            .set(self.tell().unwrap_or(self.last_offset.get()));
        let mut reader = self
            .active_reader()
            .map_err(StreamReadError::Operation)?
            .borrow_mut();
        let mut filled = 0;
        while filled < buf.len() {
            match reader.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(StreamReadError::UnderlyingRead(error.into())),
            }
        }
        if filled == 0 && !buf.is_empty() {
            // qpdf FileInputSource::read moves the cursor and last_offset to
            // the physical source end after a zero-byte read at EOF
            // (libqpdf/FileInputSource.cc:115-132). A seekable Rust reader
            // may retain an attempted position beyond EOF, so reproduce that
            // observable InputSource contract explicitly.
            let end = reader
                .seek(SeekFrom::End(0))
                .map_err(|error| StreamReadError::Operation(error.into()))?;
            self.last_offset
                .set(end.saturating_sub(self.header_offset as u64));
        }
        Ok(filled)
    }

    fn rewind_underlying_source(&self) -> Result<()> {
        self.active_reader()?
            .borrow_mut()
            .seek(SeekFrom::Start(0))?;
        Ok(())
    }

    fn read_underlying_bytes(&self) -> Result<Vec<u8>> {
        let pos = self.tell()?;
        self.rewind_underlying_source()?;
        let mut bytes = Vec::new();
        self.active_reader()?.borrow_mut().read_to_end(&mut bytes)?;
        self.seek(pos)?;
        Ok(bytes)
    }

    /// Read the logical PDF source, excluding any leading material that qpdf's
    /// offset input source hides from the document. This is the source-byte
    /// view consumed by qpdf's linearization checker, whose `/L`, `/H`, and
    /// `/T` offsets are relative to the logical input (`QPDF_linearization.cc:
    /// 84-155,159-245`).
    fn read_logical_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = self.read_underlying_bytes()?;
        let header_offset = self.header_offset.min(bytes.len());
        bytes.drain(..header_offset);
        Ok(bytes)
    }

    fn last_offset(&self) -> u64 {
        self.last_offset.get()
    }
}

pub(crate) struct ResolverCore<R: Read + Seek + 'static> {
    /// qpdf `m->file` (`QPDF.hh:1456`).
    /// The current `InputSource` pointer. Closing replaces this pointer with
    /// `InvalidInputSource`; it does not mutate the previously captured
    /// source, which is required by qpdf's foreign-stream ownership contract.
    input: RefCell<Rc<StreamInput<R>>>,
    /// Also qpdf `m->file`: when repair finds a valid header after leading
    /// material, qpdf does not keep the offset beside the input source, it
    /// *wraps* the source so the shift is invisible to every later read —
    /// `m->file = std::shared_ptr<InputSource>(new OffsetInputSource(m->file,
    /// global_offset))` (`libqpdf/QPDF.cc:406`). Keeping the shift in
    /// [`StreamInput`] beside the reader and applying it in
    /// [`Self::seek`] and [`Self::tell`] puts it under the same single owner,
    /// without a second input-source type.
    ///
    /// It is *not* equivalent, and the difference is what
    /// [`StreamInput::rewind_underlying_source`] exists for: wrapping makes
    /// the shift unskippable, so raw-input snapshots reach the bytes before it
    /// through the wrapper's `proxied` member rather than through `m->file`.
    header_offset: usize,
    /// qpdf `m->xref_table` (`QPDF.hh:1465`).
    source_xref_entries: BTreeMap<ObjectRef, XrefEntry>,
    /// qpdf `m->obj_cache` (`QPDF.hh:1467`), and the document's *only*
    /// canonical [`ObjectRef`] → [`ObjectHandle`] map.
    ///
    /// **It absorbed what was `Pdf::handle_registry`.** An earlier revision
    /// of this doc said the map was "empty for the whole of this slice … not
    /// yet a second view of `Pdf::handle_registry`", and flagged a teardown
    /// hazard for whoever populated it, offering "install into both, or move
    /// the teardown walk here". Neither half survives: real resolution has to
    /// mint a handle for every nested `N G R` it parses, and a second map
    /// would hand out handles that are not `is_same_object_as` the ones
    /// [`crate::Pdf::get_object_handle`] vends — breaking
    /// `Pdf::is_canonical_object_handle` and leaking every reference cycle
    /// `Pdf::drop` could no longer reach. So the registry moved here whole,
    /// which is also qpdf's shape: `m->obj_cache` is what `QPDF::getObject`
    /// fills, what `QPDF::getAllObjects` walks (`libqpdf/QPDF.cc:1285-1295`),
    /// and what `QPDF::~QPDF` disconnects.
    ///
    /// The teardown walk moved with it: [`ResolverHandle::disconnect_all`] is
    /// what `Pdf::drop` now calls.
    object_cache: BTreeMap<ObjectRef, ObjectHandle>,
    /// qpdf m->last_object_description (QPDF.hh:1457), retained for
    /// damaged-PDF warnings raised after a cache-preparing operation such as
    /// QPDF::nextObjGen (QPDF.cc:1873-1879). This UTF-8-facing projection is
    /// kept for the existing generic warning sink; the raw qpdf bytes used by
    /// `damagedPDF` are retained beside it.
    last_object_description: String,
    /// The byte-preserving qpdf description state. `setLastObjectDescription`
    /// composes the caller description and object identity in `std::string`,
    /// and an indirect `/Length` resolve intentionally overwrites it with the
    /// length object's identity (`QPDF.cc:1561,1725`).
    last_object_description_bytes: Vec<u8>,
    /// Object-cache entries created by qpdf-shaped allocation/replacement,
    /// rather than by looking up an unresolved reference. qpdf keeps both
    /// cases in `m->obj_cache`, but the provenance is needed by the Pdf
    /// object-ref view to distinguish a real allocated null from a resolved
    /// dangling reference (`QPDF.cc:1882-1894,1986-1993`).
    allocated_object_refs: BTreeSet<ObjectRef>,
    /// qpdf `m->resolving` (`include/qpdf/QPDF.hh:1468`), the set
    /// `QPDF::resolve` tests to detect "an object references itself directly
    /// or indirectly in some key that has to be resolved during object
    /// parsing, such as stream length" (`libqpdf/QPDF.cc:1706-1708`).
    ///
    /// Written only through [`ResolveMark`], never directly.
    resolving: BTreeSet<ObjectRef>,
    /// qpdf `m->resolved_object_streams` (`QPDF.hh:1485`). Keyed by object
    /// stream *number* rather than by `ObjectRef`, matching qpdf's own
    /// `std::set<int>` and `resolveObjectsInStream(int obj_stream_number)`.
    resolved_object_streams: BTreeSet<u32>,
    /// qpdf's transient default `QPDFXRefEntry()` values created by
    /// `m->xref_table[og]` while inspecting an object-stream header
    /// (`QPDF.cc:1823`). They are type-0 entries in qpdf's map, distinct from
    /// an object that was never looked up at all. The effective source table
    /// still contains only live type-1/type-2 entries; this set records the
    /// lookup side effect needed for a later `resolve(og)` warning.
    default_xref_entries: BTreeSet<ObjectRef>,
    /// qpdf `m->attempt_recovery` (`QPDF.hh:1461`).
    ///
    /// Same on/off flag and default: qpdf initialises it to `true` and
    /// `QPDF::setAttemptRecovery(false)` (`QPDF.hh:234`, `QPDF.cc:334`) opts
    /// out; flpdf's [`crate::PdfOpenOptions::repair`] also defaults to `true`,
    /// while an explicit `repair: false` opts out. This records the permission
    /// the document was opened with, not whether recovery actually ran —
    /// qpdf's own flag is likewise setter-controlled, and it tracks a
    /// reconstruct that happened in a separate member
    /// (`m->reconstructed_xref`, `QPDF.hh:1480`).
    attempt_recovery: bool,
    /// qpdf `m->reconstructed_xref` (`include/qpdf/QPDF.hh:1480`).
    ///
    /// Set to `true` when live resolution triggers cross-reference table
    /// reconstruction (`QPDF::reconstruct_xref`, `libqpdf/QPDF.cc:516-530`).
    /// Prevents infinite reconstruction loops if parsing a reconstructed object
    /// fails again.
    reconstructed_xref: bool,
    /// qpdf `m->fixed_dangling_refs` (`include/qpdf/QPDF.hh:1483`). Set only
    /// after the effective xref table has been completely prepared; qpdf
    /// clears it when reconstruction changes that table.
    fixed_dangling_refs: bool,
    /// qpdf `m->warnings` (`include/qpdf/QPDF.hh:1475`), the list `QPDF::warn`
    /// appends to and `QPDF::getWarnings` hands back.
    ///
    /// It lives here rather than on `Pdf` because `QPDF::resolve` warns on a
    /// resolution loop (`libqpdf/QPDF.cc:1710`) and on a damaged object
    /// (`:1738`, `:1740`), and [`DocumentResolver::resolve_indirect`] reaches
    /// its document through a `Weak` — it has no `&mut Pdf` to push into. Every
    /// warning flpdf raises, from the resolver or from any `&mut Pdf` helper
    /// walk, lands in this one collection, matching qpdf's single `m->warnings`;
    /// `Pdf::push_warning` and `Pdf::repair_diagnostics` are its two doors.
    ///
    /// **Borrow discipline.** [`ResolverHandle::push_warning`] takes
    /// `borrow_mut()` and [`ResolverHandle::repair_diagnostics`] takes
    /// `borrow()`, so neither may be called while a borrow of this core is
    /// already held. That constraint binds the read-and-parse phase, which
    /// warns from [`ResolverHandle::validate_stream_line_end`] and from
    /// [`ResolverHandle::read_object_at_offset_with_description`] in between input operations;
    /// each of those takes and drops its own borrow, so nothing is held when
    /// the push happens.
    repair_diagnostics: Diagnostics,
    /// qpdf `m->logger`, shared with callers and replaceable on the live
    /// document.
    logger: crate::QPDFLogger,
    /// qpdf `m->suppress_warnings`; collection remains active while true.
    suppress_warnings: bool,
    /// qpdf input-source description used when formatting warning locations.
    /// The source name is byte-preserving, matching qpdf's `std::string`.
    description: Vec<u8>,
    /// qpdf `m->encp` (`include/qpdf/QPDF.hh:1463`), the encryption
    /// parameters `QPDF::pipeStreamData`'s static overload takes as its first
    /// argument and consults before piping a stream
    /// (`libqpdf/QPDF.cc:2477-2492`) — the primitive flpdf-25kg.3.10 adds.
    ///
    /// `Rc<RefCell<..>>` rather than a bare `Option<EncryptionState>`,
    /// because qpdf's `m->encp` is a `std::shared_ptr<EncryptionParameters>`:
    /// a second owner (`ForeignStreamData::encp`, `QPDF.hh:939`) holds the
    /// *same* allocation, constructed from `QPDF::Members::encp` by copying
    /// the shared_ptr (`QPDF.cc:2266`), not by copying the data. `Pdf::encryption`
    /// holds a clone of this same `Rc`, obtained via
    /// [`ResolverHandle::encryption_parameters`] in `open_with_repair_mode`,
    /// so `Pdf::authenticate_if_encrypted`'s one write is visible here
    /// without a second write site.
    ///
    /// Constructed empty (`None`) in [`ResolverHandle::new_shared`] and
    /// populated later, matching `Members::encp(new EncryptionParameters)`
    /// (`QPDF.cc:201`): default-constructed at document-construction time,
    /// `encrypted = false` until `initializeEncryption()` runs. flpdf has no
    /// separate `encrypted`/`encryption_initialized` pair — the outer
    /// `Option` serves both; see the field-mapping table in this issue's
    /// design (`bd show flpdf-25kg.3.11`) for the disclosed collapse.
    encryption_parameters: Rc<RefCell<Option<crate::encryption::state::EncryptionState>>>,
}

pub(crate) struct ResolverWarningOptions {
    logger: crate::QPDFLogger,
    suppress_warnings: bool,
    description: Vec<u8>,
}

/// Keep failures raised while delivering a warning distinct from failures
/// raised by the object-stream operation itself. qpdf catches the latter in
/// `QPDF::resolve` and converts them to a warning plus a null cache entry, but
/// a logger/pipeline failure is an error of the caller's diagnostic channel and
/// must still propagate.
enum ObjectStreamResolutionError {
    Operation(Error),
    MemberWarning {
        stream_number: u32,
        object_ref: ObjectRef,
        offset: u64,
        message: String,
    },
    WarningDelivery(Error),
}

impl From<Error> for ObjectStreamResolutionError {
    fn from(error: Error) -> Self {
        Self::Operation(error)
    }
}

impl From<crate::pipeline::PipelineError> for ObjectStreamResolutionError {
    fn from(error: crate::pipeline::PipelineError) -> Self {
        Self::Operation(error.into())
    }
}

impl ResolverWarningOptions {
    pub(crate) fn new(
        logger: crate::QPDFLogger,
        suppress_warnings: bool,
        description: Vec<u8>,
    ) -> Self {
        Self {
            logger,
            suppress_warnings,
            description,
        }
    }

    pub(crate) fn replay_warnings(&self, diagnostics: &Diagnostics) -> Result<()> {
        for diagnostic in diagnostics.entries() {
            let description = diagnostic
                .description
                .as_deref()
                .unwrap_or(self.description.as_slice());
            route_warning(
                &self.logger,
                self.suppress_warnings,
                description,
                diagnostic.offset,
                &diagnostic.message,
            )?;
        }
        Ok(())
    }
}

fn route_warning(
    logger: &crate::QPDFLogger,
    suppress_warnings: bool,
    description: &[u8],
    offset: Option<u64>,
    message: &str,
) -> Result<()> {
    if suppress_warnings {
        return Ok(());
    }
    let mut line = b"WARNING: ".to_vec();
    if message.starts_with("(object ") || message.starts_with("(trailer") {
        line.extend_from_slice(description);
        if !description.is_empty() {
            line.push(b' ');
        }
        line.extend_from_slice(message.as_bytes());
        line.push(b'\n');
        return logger.warn(line);
    }
    let positive_offset = offset.filter(|offset| *offset > 0);
    if !description.is_empty() {
        line.extend_from_slice(description);
        if let Some(offset) = positive_offset {
            line.extend_from_slice(b" (offset ");
            line.extend_from_slice(offset.to_string().as_bytes());
            line.push(b')');
        }
    } else if let Some(offset) = positive_offset {
        line.extend_from_slice(b"offset ");
        line.extend_from_slice(offset.to_string().as_bytes());
    }
    if !description.is_empty() || positive_offset.is_some() {
        line.extend_from_slice(if message.starts_with('(') {
            &b" "[..]
        } else {
            &b": "[..]
        });
    }
    line.extend_from_slice(message.as_bytes());
    line.push(b'\n');
    logger.warn(line)
}

/// Format the byte-preserving `QPDFExc::what()` shape for an input-source
/// warning with an explicit object description (`QPDFExc.cc:19-50`).
fn format_input_warning_what(
    filename: &[u8],
    object: &[u8],
    offset: u64,
    message: &[u8],
) -> Vec<u8> {
    let mut result = filename.to_vec();
    if !(object.is_empty() && offset == 0) {
        if !filename.is_empty() {
            result.extend_from_slice(b" (");
        }
        if !object.is_empty() {
            result.extend_from_slice(object);
            if offset > 0 {
                result.extend_from_slice(b", ");
            }
        }
        if offset > 0 {
            result.extend_from_slice(b"offset ");
            result.extend_from_slice(offset.to_string().as_bytes());
        }
        if !filename.is_empty() {
            result.push(b')');
        }
    }
    if !result.is_empty() {
        result.extend_from_slice(b": ");
    }
    result.extend_from_slice(message);
    result
}

fn route_object_warning(
    logger: &crate::QPDFLogger,
    suppress_warnings: bool,
    message: &[u8],
) -> Result<()> {
    if suppress_warnings {
        return Ok(());
    }
    let mut line = b"WARNING: ".to_vec();
    line.extend_from_slice(message);
    line.push(b'\n');
    logger.warn(line)
}

impl<R: Read + Seek> ResolverCore<R> {
    /// Position the input source at qpdf-logical `offset`.
    ///
    /// qpdf `m->file->seek(offset, SEEK_SET)`. The header shift is applied
    /// here for the same reason `OffsetInputSource` applies it inside
    /// `m->file` (`libqpdf/QPDF.cc:406`): every caller above this line works
    /// in qpdf-logical coordinates and never sees the physical position.
    fn seek(&mut self, offset: u64) -> Result<()> {
        self.input.borrow().seek(offset)
    }

    /// The input source's current qpdf-logical position.
    ///
    /// qpdf `m->file->tell()`. This is the live position `QPDF::readStream`
    /// saves before resolving `/Length` and restores afterwards
    /// (`libqpdf/QPDF.cc:1367-1384`) — not a value recomputed from an
    /// argument, which is precisely why the restore is load-bearing.
    fn tell(&mut self) -> Result<u64> {
        self.input.borrow().tell()
    }

    fn source_length(&mut self) -> Result<u64> {
        self.input.borrow().source_length()
    }

    /// Advance the input by `delta` bytes from wherever it currently is,
    /// without naming an absolute position.
    ///
    /// qpdf `m->file->seek(delta, SEEK_CUR)`, and specifically the second half
    /// of the pair `QPDF::readStream` performs under the comment "Seek in two
    /// steps to avoid potential integer overflow"
    /// (`libqpdf/QPDF.cc:1383-1385`). Two steps rather than one because
    /// `stream_offset + length` is computed in `qpdf_offset_t`
    /// (`long long`, `include/qpdf/Types.h:31`) and a declared `/Length` is
    /// attacker-controlled up to that type's maximum.
    ///
    /// The bounds check is qpdf's, not an flpdf invention:
    /// `BufferInputSource::seek`'s `SEEK_CUR` arm calls
    /// `QIntC::range_check(this->cur_offset, offset)`
    /// (`libqpdf/BufferInputSource.cc:95-97`), whose overflow arm is
    /// `(std::numeric_limits<T>::max() - cur) < delta`
    /// (`include/qpdf/QIntC.hh:255-268`, reached through `:270-278`) — the
    /// comparison below, and its message, transcribed. Rust's `Seek` cannot
    /// stand in for it: `Cursor::seek` adds in `u64` and so accepts an offset
    /// that overflows `i64` (measured: from position 236,
    /// `SeekFrom::Current(i64::MAX)` returns `Ok(9223372036854776043)`).
    ///
    /// **Which failure this is matters beyond the DoS**, because in qpdf the
    /// exception *class* picks the recovery fork. `damagedPDF("expected
    /// endstream")` is a `QPDFExc`, caught at `libqpdf/QPDF.cc:1390` and sent
    /// to `recoverStreamLength`; the `std::range_error` this ports, like the
    /// `std::runtime_error` `FileInputSource::seek` throws
    /// (`libqpdf/FileInputSource.cc:100-107`), is not, so it passes that catch
    /// and lands in `QPDF::resolve`'s `catch (std::exception&)`
    /// (`:1739-1741`), which warns and leaves the object to resolve to null.
    /// Collapsing this into "expected endstream" would file the case on the
    /// wrong fork for whichever slice ports recovery.
    fn seek_relative(&mut self, delta: u64) -> Result<()> {
        self.input.borrow().seek_relative(delta)
    }

    /// Fill `buf` from the current position, returning how many bytes were
    /// available, and leave the position advanced by exactly that many.
    ///
    /// qpdf `m->file->read(buf, len)`, whose contract is likewise "returns
    /// the number of bytes read, 0 at EOF" — `FileInputSource::read` loops
    /// over `fread` and `BufferInputSource::read` clamps to what remains
    /// (`libqpdf/FileInputSource.cc`, `libqpdf/BufferInputSource.cc`). The
    /// loop here is what makes a short `Read::read` — legal for any `R` —
    /// indistinguishable from that contract.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let result = self.input.borrow().read(buf);
        match result {
            Ok(read) => Ok(read),
            Err(StreamReadError::UnderlyingRead(Error::Io(_error)))
                if !self.description.is_empty() =>
            {
                let message = format!("read {} bytes", buf.len());
                let offset = self.input.borrow().last_offset();
                let what =
                    format_input_warning_what(&self.description, &[], offset, message.as_bytes());
                // qpdf's FileInputSource converts a failed fread into a
                // QPDFExc carrying only the source name, offset, and
                // requested read length (`FileInputSource.cc:116-132`);
                // the platform errno is intentionally not part of what().
                Err(Error::SystemBytes(what))
            }
            Err(StreamReadError::UnderlyingRead(error))
            | Err(StreamReadError::Operation(error)) => Err(error),
        }
    }

    /// Read all physical bytes of the input source from position 0, restoring the
    /// logical position afterwards.
    fn read_underlying_bytes(&mut self) -> Result<Vec<u8>> {
        self.input.borrow().read_underlying_bytes()
    }
}

/// The record that one reference's resolution is in progress, removed when the
/// record goes out of scope.
///
/// qpdf's `ResolveRecorder` (`include/qpdf/QPDF.hh:980-996`): its constructor
/// inserts into `m->resolving` (`:985`) and its destructor erases (`:990`).
///
/// **A `Drop` guard rather than a matched insert/remove pair**, for the same
/// reason qpdf uses an RAII class rather than two statements: the removal has
/// to happen on *every* exit from the resolution, including the ones the
/// author did not write. qpdf's `try`/`catch` (`libqpdf/QPDF.cc:1718-1742`)
/// brackets only the xref-entry switch; `updateCache` (`:1748`) and
/// `setDefaultDescription` (`:1752`) run outside it, so `~ResolveRecorder` is
/// what erases the mark should either throw. The Rust body has exits of the
/// same shape — an unwinding panic today, plus the `?` returns Task 4's
/// read-and-parse steps will add.
/// `the_in_progress_mark_is_removed_when_a_resolution_unwinds` asserts the
/// unwind case rather than assuming it.
///
/// One divergence in mechanism, none in outcome: qpdf stores the iterator
/// `std::set::insert` returns and erases *by iterator*, while this removes by
/// key. Same element either way — `object_ref` is what was inserted, and
/// [`ResolverCore::resolving`] is not otherwise written.
struct ResolveMark<'a, R: Read + Seek + 'static> {
    core: &'a RefCell<ResolverCore<R>>,
    object_ref: ObjectRef,
}

impl<'a, R: Read + Seek> ResolveMark<'a, R> {
    /// Record `object_ref` as in progress, or report that it already was.
    ///
    /// Folds two qpdf steps into one `BTreeSet::insert`: the loop test
    /// `m->resolving.count(og)` (`libqpdf/QPDF.cc:1706`) and the insert inside
    /// `ResolveRecorder`'s constructor (`include/qpdf/QPDF.hh:985`). `insert`
    /// already answers "was it there?", so asking twice would be redundant.
    ///
    /// **`None` deliberately yields no guard.** qpdf `return`s at
    /// `libqpdf/QPDF.cc:1712`, before `ResolveRecorder rr(this, og)` at
    /// `:1714`, so a loop-detecting call constructs no recorder and destroys
    /// none — the outer resolution's mark survives its inner call. A guard
    /// handed back here regardless would erase that mark on drop.
    ///
    /// Borrow discipline: the `borrow_mut()` is taken and dropped inside this
    /// expression, so no borrow is live when the caller runs its body.
    fn begin(core: &'a RefCell<ResolverCore<R>>, object_ref: ObjectRef) -> Option<Self> {
        if core.borrow_mut().resolving.insert(object_ref) {
            Some(Self { core, object_ref })
        } else {
            None
        }
    }
}

impl<R: Read + Seek> Drop for ResolveMark<'_, R> {
    /// qpdf `~ResolveRecorder` (`include/qpdf/QPDF.hh:988-991`).
    fn drop(&mut self) {
        self.core.borrow_mut().resolving.remove(&self.object_ref);
    }
}

/// The `Rc`-shared, interior-mutable owner of [`ResolverCore`], and the
/// [`DocumentResolver`] a document's handles hold a `Weak` to.
pub(crate) struct ResolverHandle<R: Read + Seek + 'static> {
    core: RefCell<ResolverCore<R>>,
    /// Source line endings included by canonical stream-boundary recovery.
    ///
    /// The recovered length follows qpdf's `recoverStreamLength` coordinate
    /// and therefore includes the line ending immediately before
    /// `endstream`. Retain the observed suffix for inspection consumers that
    /// have their own display-framing policy; the qpdf pipe path always reads
    /// the complete recovered length before any AES/RC4 stage.
    recovered_stream_eols: RefCell<BTreeMap<ObjectRef, crate::parser::RecoveredStreamEol>>,
    /// A `Weak` to this same allocation, so minting a canonical handle can
    /// attach the resolver the handle will later call back into.
    ///
    /// Not a qpdf member. qpdf's `QPDF_Stream`/`QPDFObject` carry a raw
    /// `QPDF*` back-pointer (`libqpdf/QPDFObject.cc:10` calls
    /// `QPDF::Resolver::resolve(value->qpdf, og)`), which needs no such
    /// field. `#![forbid(unsafe_code)]` (`crates/flpdf/src/lib.rs:83`) rules
    /// that out, and the design of record records the raw back-pointer as an
    /// alternative rejected for exactly that reason, so the `Weak` is the
    /// safe-Rust stand-in and this field is the only way to obtain one from
    /// `&self`.
    self_weak: Weak<ResolverHandle<R>>,
    /// qpdf's source-side `setImmediateCopyFrom` flag. It is read by the
    /// destination stream-copy boundary before a lazy source is registered.
    immediate_copy_from: Cell<bool>,
    /// The owning document's [`crate::Pdf`] identity, stamped onto every
    /// handle this minted — `ObjectHandle`'s `pdf_unique_id`, whose own doc
    /// traces it to qpdf's `QPDF::getUniqueId`
    /// (`include/qpdf/QPDF.hh:283`, `libqpdf/QPDF.cc:2294-2296`).
    ///
    /// Duplicated from `Pdf::unique_id` rather than reached through it: the
    /// resolver has no `&Pdf`. The value is assigned before either side is
    /// constructed and is re-bound only when qpdf's process-memory boundary
    /// installs a new source on the same document.
    pdf_unique_id: Cell<u64>,
}

/// The `QPDF::StringDecrypter` qpdf binds to one indirect object immediately
/// before `QPDFParser::parse` (`libqpdf/QPDF.cc:1331-1340`).
struct ResolverStringDecrypter<'resolver, R: Read + Seek + 'static> {
    object_ref: ObjectRef,
    encryption_parameters: Rc<RefCell<Option<crate::encryption::state::EncryptionState>>>,
    resolver: &'resolver ResolverHandle<R>,
}

/// qpdf's `ForeignStreamData` (`include/qpdf/QPDF.hh:925-943`), captured at
/// `QPDF::copyStreamData` time (`libqpdf/QPDF.cc:2265-2273`). The source input
/// and encryption cell are shared independently from the source resolver;
/// only the destination warning sink is retained weakly for the later
/// `pipeForeignStreamData` call.
///
/// `description` mirrors qpdf's `foreign->file` carrying its own name
/// (`InputSource::getName`): `pipeForeignStreamData` throws its damaged-PDF
/// exceptions via `damagedPDF(file, ...)`, which bakes the *source*
/// `InputSource`'s name into the exception before the destination `QPDF`
/// ever sees it (`libqpdf/QPDF.cc:2477-2530,2565-2585`;
/// `libqpdf/QPDF_encryption.cc:1122-1128`). Captured once here rather than
/// read from `input` at pipe time because `StreamInput` (flpdf's
/// `InputSource` stand-in) does not itself carry a name — flpdf keeps that
/// string on the resolver instead.
struct ForeignStreamData<R: Read + Seek + 'static> {
    input: Rc<StreamInput<R>>,
    encryption_parameters: Rc<RefCell<Option<crate::encryption::state::EncryptionState>>>,
    object_ref: ObjectRef,
    parsed_offset: i64,
    stream_length: usize,
    local_dict: ObjectHandle,
    description: Vec<u8>,
}

/// qpdf's `CopiedStreamDataProvider` (`libqpdf/QPDF.cc:126-163`) dispatches
/// captured foreign data through the destination `QPDF`. A weak erased
/// resolver keeps that ownership direction without making the provider keep a
/// destination resolver cycle alive.
struct OriginalStreamDataProvider<R: Read + Seek + 'static> {
    foreign_data: Rc<ForeignStreamData<R>>,
    destination_resolver: Weak<dyn DocumentResolver>,
}

impl<R: Read + Seek + 'static> StreamDataProvider for OriginalStreamDataProvider<R> {
    fn supports_retry(&self) -> bool {
        true
    }

    fn provide_stream_data_with_retry_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        let destination = self.destination_resolver.upgrade().ok_or_else(|| {
            Error::Internal("foreign stream destination resolver is no longer live".to_owned())
        })?;
        pipe_stream_data_from_input(
            &self.foreign_data.input,
            &self.foreign_data.encryption_parameters,
            destination.as_ref(),
            Some(self.foreign_data.description.as_slice()),
            self.foreign_data.object_ref,
            self.foreign_data.parsed_offset,
            self.foreign_data.stream_length,
            &self.foreign_data.local_dict,
            pipeline,
            suppress_warnings,
            will_retry,
        )
    }
}

impl<R: Read + Seek + 'static> StringDecrypter for ResolverStringDecrypter<'_, R> {
    fn decrypt_string(&mut self, bytes: &mut Vec<u8>) -> Result<()> {
        let (use_aes, warn_unknown_string) = {
            let encryption_parameters = self.encryption_parameters.borrow();
            // cov:ignore-start: read_object_at_offset constructs this adapter only after observing Some; parsing cannot mutate the shared slot
            let encryption = encryption_parameters.as_ref().ok_or_else(|| {
                Error::Internal("string decrypter invoked without encryption parameters".into())
            })?;
            // cov:ignore-end
            encryption.string_method()
        };
        if warn_unknown_string {
            self.resolver.push_warning(
                "unknown encryption filter for strings (check /StrF in /Encrypt dictionary); \
                 strings may be decrypted improperly",
            )?;
            let mut encryption_parameters = self.encryption_parameters.borrow_mut();
            // cov:ignore-start: the selection above proves the shared state is present
            let encryption = encryption_parameters.as_mut().ok_or_else(|| {
                Error::Internal("string decrypter lost encryption parameters".into())
            })?;
            // cov:ignore-end
            encryption.commit_string_method();
        }
        let mut encryption_parameters = self.encryption_parameters.borrow_mut();
        // cov:ignore-start: the selection above proves the shared state is present
        let encryption = encryption_parameters
            .as_mut()
            .ok_or_else(|| Error::Internal("string decrypter lost encryption parameters".into()))?;
        // cov:ignore-end
        encryption.decrypt_object_string(self.object_ref, bytes, use_aes)
    }
}

/// The value and source metadata that qpdf's `readObjectAtOffset` hands to
/// `updateCache`. The two end offsets are not the value's parsed/token offset;
/// they bracket the indirect object's terminator and following whitespace.
#[derive(Debug)]
struct ParsedObjectAtOffset {
    object_ref: ObjectRef,
    value: ObjectValue,
    /// Parser diagnostics observed while recovering this object's body. A
    /// recovered null is kept distinct from a literal null at the canonical
    /// cache boundary so legacy tree consumers can preserve their error path.
    malformed: bool,
    parsed_offset: i64,
    description: Vec<u8>,
    end_before_space: i64,
    end_after_space: i64,
    /// The tokenizer start of the token after the parsed object value. qpdf's
    /// `readObject` leaves this in the input source's last-offset state when
    /// the object was already cached before an offset read.
    trailing_start: Option<u64>,
}

/// qpdf only reconstructs the xref table for damage found while reading the
/// indirect-object header (`QPDF.cc:1591-1637`). Errors from the object body,
/// stream framing, or cache-extent scan are caught by `QPDF::resolve` and do
/// not trigger a second xref discovery pass. Keep that boundary explicit so a
/// body parse failure cannot accidentally be treated as a stale xref offset.
enum ReadObjectAtOffsetError {
    Header(Error),
    Body(Error),
}

impl ReadObjectAtOffsetError {
    fn into_error(self) -> Error {
        match self {
            Self::Header(error) | Self::Body(error) => error,
        }
    }
}

impl<R: Read + Seek> ResolverHandle<R> {
    /// Build the resolver already inside its `Rc`.
    ///
    /// `Rc::new_cyclic` rather than `Rc::new` because [`Self::self_weak`]
    /// has to point at this very allocation; there is no way to add it
    /// afterwards without making the field mutable.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_shared(
        reader: R,
        header_offset: usize,
        source_xref_entries: BTreeMap<ObjectRef, XrefEntry>,
        attempt_recovery: bool,
        already_reconstructed: bool,
        repair_diagnostics: Diagnostics,
        warning_options: ResolverWarningOptions,
        pdf_unique_id: u64,
    ) -> Rc<Self> {
        let ResolverWarningOptions {
            logger,
            suppress_warnings,
            description,
        } = warning_options;
        Rc::new_cyclic(|self_weak| Self {
            core: RefCell::new(ResolverCore {
                input: RefCell::new(Rc::new(StreamInput::new(reader, header_offset))),
                header_offset,
                source_xref_entries,
                object_cache: BTreeMap::new(),
                last_object_description: String::new(),
                last_object_description_bytes: Vec::new(),
                allocated_object_refs: BTreeSet::new(),
                resolving: BTreeSet::new(),
                resolved_object_streams: BTreeSet::new(),
                default_xref_entries: BTreeSet::new(),
                attempt_recovery,
                // qpdf `m->reconstructed_xref` (`QPDF.cc:524`): set by
                // `reconstruct_xref` which runs both at open time (`:464`) and
                // during resolution (`:1617`). Carry open-time recovery state
                // so a second full scan is not performed for an object from an
                // already-recovered table.
                reconstructed_xref: already_reconstructed,
                fixed_dangling_refs: false,
                repair_diagnostics,
                logger,
                suppress_warnings,
                description,
                encryption_parameters: Rc::new(RefCell::new(None)),
            }),
            recovered_stream_eols: RefCell::new(BTreeMap::new()),
            self_weak: self_weak.clone(),
            immediate_copy_from: Cell::new(false),
            pdf_unique_id: Cell::new(pdf_unique_id),
        })
    }

    /// Build qpdf's default-constructed document resolver. Its input source
    /// is already the invalid source used before `processFile` and the object
    /// cache is empty, so no reader or sentinel byte buffer is required.
    pub(crate) fn new_uninitialized(
        warning_options: ResolverWarningOptions,
        pdf_unique_id: u64,
    ) -> Rc<Self> {
        let ResolverWarningOptions {
            logger,
            suppress_warnings,
            description,
        } = warning_options;
        Rc::new_cyclic(|self_weak| Self {
            core: RefCell::new(ResolverCore {
                input: RefCell::new(Rc::new(StreamInput::invalid())),
                header_offset: 0,
                source_xref_entries: BTreeMap::new(),
                object_cache: BTreeMap::new(),
                last_object_description: String::new(),
                last_object_description_bytes: Vec::new(),
                allocated_object_refs: BTreeSet::new(),
                resolving: BTreeSet::new(),
                resolved_object_streams: BTreeSet::new(),
                default_xref_entries: BTreeSet::new(),
                attempt_recovery: true,
                reconstructed_xref: false,
                fixed_dangling_refs: false,
                repair_diagnostics: Diagnostics::default(),
                logger,
                suppress_warnings,
                description,
                encryption_parameters: Rc::new(RefCell::new(None)),
            }),
            recovered_stream_eols: RefCell::new(BTreeMap::new()),
            self_weak: self_weak.clone(),
            immediate_copy_from: Cell::new(false),
            pdf_unique_id: Cell::new(pdf_unique_id),
        })
    }

    /// Create qpdf's owned empty stream object at the resolver boundary.
    /// `QPDF::newStream` registers the freshly created stream under a new
    /// generation-zero identity; keeping the allocation and registration here
    /// lets `ObjectHandle::copy_stream` use the same path as `Pdf::new_stream`.
    pub(crate) fn new_stream_handle(&self) -> Result<ObjectHandle> {
        let stream = self.direct_object_handle(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(Vec::new()),
            stream_data: None,
            stream_length: 0,
            stream_provider: None,
            filter_on_write: true,
        });
        stream.set_parsed_offset_if_unset(0);
        self.make_indirect_from_object_handle(stream)
    }

    /// Create qpdf's reserved construction sentinel and register its
    /// document-owned identity in the canonical object cache.
    ///
    /// `QPDF::newReserved` delegates to `makeIndirectFromQPDFObject`, which
    /// allocates `nextObjGen`, stores a `QPDF_Reserved` payload in `obj_cache`,
    /// and returns the same indirect identity
    /// (`libqpdf/QPDF.cc:1882-1888,1900-1903`; `QPDF_Reserved.cc:1-27`).
    pub(crate) fn new_reserved_handle(&self) -> Result<ObjectHandle> {
        let object_ref = self.next_obj_gen()?;
        let resolver: Weak<dyn DocumentResolver> = self.self_weak.clone();
        let reserved =
            ObjectHandle::new_reserved_for_pdf(object_ref, self.pdf_unique_id.get(), resolver);
        let mut core = self.core.borrow_mut();
        let previous = core.object_cache.insert(object_ref, reserved.clone());
        core.allocated_object_refs.insert(object_ref);
        debug_assert!(
            previous.is_none(),
            "next_obj_gen must return a fresh ObjGen"
        );
        Ok(reserved)
    }

    /// Set qpdf's source-side immediate-copy flag.
    pub(crate) fn set_immediate_copy_from(&self, value: bool) {
        self.immediate_copy_from.set(value);
    }

    fn immediate_copy_from(&self) -> bool {
        self.immediate_copy_from.get()
    }

    /// qpdf's `QPDF::copyStreamData` (`libqpdf/QPDF.cc:2216-2272`). Existing
    /// buffers are shared directly; provider/original sources remain lazy and
    /// are dispatched through one retry-aware provider owned by the copied
    /// destination stream. The source-side immediate flag is checked through
    /// the source handle's resolver, matching qpdf's source-QPDF contract.
    pub(crate) fn copy_stream_data(
        &self,
        destination: &ObjectHandle,
        source: &ObjectHandle,
    ) -> Result<()> {
        source.try_dereference()?;
        destination.try_dereference()?;
        let destination_type_name = destination.type_name()?;
        let Some(destination_dict) = destination.as_stream_dict() else {
            return Err(Error::System(format!(
                "operation for stream attempted on object of type {destination_type_name}"
            )));
        };

        let filter = Some(stream_copy_dictionary_value(&destination_dict, b"/Filter")?);
        let decode_parms = stream_copy_dictionary_value(&destination_dict, b"/DecodeParms")?;
        let decode_parms = Some(decode_parms);
        let mut source_data = source.as_stream_data();
        let source_immediate_copy = source
            .context()
            .map(|resolver| resolver.immediate_copy_from())
            .unwrap_or(false);

        if source_data.is_none() && source_immediate_copy {
            let source_type_name = source.type_name()?;
            let Some(source_dict) = source.as_stream_dict() else {
                return Err(Error::System(format!(
                    "operation for stream attempted on object of type {source_type_name}"
                )));
            };
            let raw_data = source.get_raw_stream_data()?;
            source.replace_stream_data(
                raw_data,
                Some(stream_copy_dictionary_value(&source_dict, b"/Filter")?),
                Some(stream_copy_dictionary_value(&source_dict, b"/DecodeParms")?),
            );
            source_data = source.as_stream_data();
        }

        if let Some(data) = source_data {
            destination.replace_stream_data(data, filter, decode_parms);
            return Ok(());
        }

        let provider = if source.has_stream_data_provider() {
            // qpdf's provider-backed source retains the foreign stream/QPDF
            // because the provider itself is the source of the bytes.
            crate::object_handle::copied_stream_data_provider(source.clone())
        } else {
            // qpdf's original-file source captures the input and stream
            // metadata instead, so the source QPDF/handle may be destroyed.
            let source_resolver = source.context().ok_or_else(|| {
                Error::Internal(
                    "original foreign stream has no owning document resolver".to_owned(),
                )
            })?;
            let destination_resolver = self.document_resolver_weak()?;
            source_resolver.original_stream_data_provider_for_destination(
                source,
                &destination_dict,
                destination_resolver,
            )?
        };

        destination.replace_stream_data_provider(provider, filter, decode_parms)
    }

    pub(crate) fn original_stream_data_provider(
        &self,
        source: &ObjectHandle,
        destination_dict: &ObjectHandle,
    ) -> Result<Rc<dyn StreamDataProvider>> {
        let destination_resolver = self.document_resolver_weak()?;
        self.original_stream_data_provider_for_destination(
            source,
            destination_dict,
            destination_resolver,
        )
    }

    pub(crate) fn original_stream_data_provider_for_destination(
        &self,
        source: &ObjectHandle,
        destination_dict: &ObjectHandle,
        destination_resolver: Weak<dyn DocumentResolver>,
    ) -> Result<Rc<dyn StreamDataProvider>> {
        let object_ref = source.object_ref().ok_or_else(|| {
            Error::Internal("original foreign stream has no object reference".to_owned())
        })?;
        let stream_length = source.stream_source_length().ok_or_else(|| {
            Error::Internal("original foreign stream has no stream length".to_owned())
        })?;
        let input = self.stream_input();
        let encryption_parameters = self.encryption_parameters();
        let description = self.core.borrow().description.clone();
        Ok(Rc::new(OriginalStreamDataProvider {
            foreign_data: Rc::new(ForeignStreamData {
                input,
                encryption_parameters,
                object_ref,
                parsed_offset: source.get_parsed_offset(),
                stream_length,
                local_dict: destination_dict.clone(),
                description,
            }),
            destination_resolver,
        }))
    }

    pub(crate) fn document_resolver_weak(&self) -> Result<Weak<dyn DocumentResolver>> {
        let strong = self
            .self_weak
            .upgrade()
            .ok_or_else(|| Error::Internal("document resolver is no longer live".to_owned()))?;
        let erased: Rc<dyn DocumentResolver> = strong;
        Ok(Rc::downgrade(&erased))
    }

    /// The canonical handle for `object_ref`, minting and registering one on
    /// first request.
    ///
    /// qpdf's `QPDF::getObject`: one `QPDFObject` per `QPDFObjGen` in
    /// `m->obj_cache`, handed back on every later lookup. Repeated calls
    /// return handles that are `is_same_object_as` each other, which is what
    /// makes lazy resolution observable through any clone.
    ///
    /// Borrow discipline: the `borrow_mut()` spans only the map lookup and
    /// the `ObjectHandle` construction, neither of which resolves anything.
    pub(crate) fn get_object_handle(&self, object_ref: ObjectRef) -> ObjectHandle {
        self.core
            .borrow_mut()
            .object_cache
            .entry(object_ref)
            .or_insert_with(|| {
                let resolver: Weak<dyn DocumentResolver> = self.self_weak.clone();
                ObjectHandle::new_indirect_for_pdf_with_resolver(
                    object_ref,
                    NO_PARSED_OFFSET,
                    self.pdf_unique_id.get(),
                    resolver,
                )
            })
            .clone()
    }

    /// Return qpdf's `reserveObjectIfNotExists` result for one object identity.
    ///
    /// A source xref entry (including a resolver-created default free row) is
    /// already an object-cache candidate and therefore receives the ordinary
    /// unresolved canonical handle. Only a genuinely new identity gets the
    /// reserved construction sentinel that JSON input later normalizes to
    /// null when no object definition replaces it
    /// (`libqpdf/QPDF.cc:1935-1943`).
    pub(crate) fn reserve_object_if_not_exists(&self, object_ref: ObjectRef) -> ObjectHandle {
        if self.registered_handle(object_ref).is_some()
            || self.xref_entry(object_ref).is_some()
            || self.has_default_xref_entry(object_ref)
        {
            return self.get_object_handle(object_ref);
        }

        let resolver: Weak<dyn DocumentResolver> = self.self_weak.clone();
        let reserved =
            ObjectHandle::new_reserved_for_pdf(object_ref, self.pdf_unique_id.get(), resolver);
        let mut core = self.core.borrow_mut();
        let previous = core.object_cache.insert(object_ref, reserved.clone());
        core.allocated_object_refs.insert(object_ref);
        previous.unwrap_or(reserved)
    }

    /// Construct a direct value with this document's weak context, matching
    /// the resolver-bearing handles minted by [`Self::get_object_handle`].
    /// qpdf stores the same owning `QPDF*` on every non-null value created by
    /// its file parser (`libqpdf/QPDFParser.cc:394-444`).
    pub(crate) fn direct_object_handle(&self, value: ObjectValue) -> ObjectHandle {
        let resolver: Weak<dyn DocumentResolver> = self.self_weak.clone();
        ObjectHandle::from_value_with_resolver(value, resolver)
    }

    /// Construct a direct value from the live file parser. Unlike a
    /// programmatic value lifted through [`Self::direct_object_handle`], qpdf
    /// stamps parser-created direct values with their owning `QPDF*`
    /// (`libqpdf/QPDFParser.cc:394-444`).
    pub(crate) fn parsed_direct_object_handle(&self, value: ObjectValue) -> ObjectHandle {
        let resolver: Weak<dyn DocumentResolver> = self.self_weak.clone();
        ObjectHandle::from_parsed_value_with_resolver(value, resolver)
    }

    /// Parse a caller-provided object string against this document's canonical
    /// object cache, matching `QPDFObjectHandle::parse(QPDF*, ...)`.
    pub(crate) fn parse_object_handle_from_bytes(
        &self,
        input: &[u8],
        object_description: &str,
    ) -> Result<ObjectHandle> {
        let mut handles = ChildHandles {
            resolver: self,
            description_template: format!("parsed object, {object_description} at offset $PO")
                .into_bytes(),
        };
        let parsed = parse_object_handle_with_context(input, &mut handles)?;
        for diagnostic in &parsed.diagnostics {
            self.push_object_warning(qpdf_exception_what(
                "parsed object",
                object_description,
                diagnostic.relative_offset,
                &diagnostic.message,
            ))?;
        }
        if let Some(error) = trailing_data_error(input, parsed.next_offset, parsed.last_offset) {
            return Err(error);
        }
        Ok(parsed.value)
    }

    pub(crate) fn parser_description_template(&self, object_ref: ObjectRef) -> Vec<u8> {
        let core = self.core.borrow();
        let mut description = core.description.clone();
        description.extend_from_slice(
            format!(
                ", object {} {} at offset $PO",
                object_ref.number, object_ref.generation
            )
            .as_bytes(),
        );
        description
    }

    fn parser_description_template_for_read(
        &self,
        object_ref: ObjectRef,
        read_description: &[u8],
    ) -> Vec<u8> {
        let core = self.core.borrow();
        let mut description = core.description.clone();
        description.extend_from_slice(b", ");
        description.extend_from_slice(read_description);
        description.extend_from_slice(
            format!(
                ": object {} {} at offset $PO",
                object_ref.number, object_ref.generation
            )
            .as_bytes(),
        );
        description
    }

    pub(crate) fn stream_description(&self, object_ref: ObjectRef) -> Vec<u8> {
        let core = self.core.borrow();
        let mut description = core.description.clone();
        description.extend_from_slice(
            format!(
                ", stream object {} {}",
                object_ref.number, object_ref.generation
            )
            .as_bytes(),
        );
        description
    }

    /// Keep qpdf's current object description for later damaged-PDF warnings
    /// (`QPDF.hh:1457`). This is the direct Rust equivalent of
    /// `setLastObjectDescription(description, og)` (`QPDF.cc:1298-1310`):
    /// the caller's description is part of the state, not only a formatter
    /// argument at the eventual warning site.
    fn set_last_object_description(&self, object_ref: ObjectRef, description: Option<&[u8]>) {
        let mut rendered = Vec::new();
        if let Some(description) = description.filter(|description| !description.is_empty()) {
            rendered.extend_from_slice(description);
            if object_ref.number != 0 {
                rendered.extend_from_slice(b": ");
            }
        }
        if object_ref.number != 0 {
            rendered.extend_from_slice(
                format!("object {} {}", object_ref.number, object_ref.generation).as_bytes(),
            );
        }

        let mut core = self.core.borrow_mut();
        core.last_object_description = String::from_utf8_lossy(&rendered).into_owned();
        core.last_object_description_bytes = rendered;
    }

    /// Append a generic damaged-PDF warning with the current object
    /// description and input offset. The location is formatted into the
    /// diagnostic when an object description exists so the qtest driver can
    /// add only its filename, matching qpdf's warning formatter.
    pub(crate) fn push_damaged_warning(&self, message: impl Into<String>) -> Result<()> {
        let message = message.into();
        let (diagnostic_message, diagnostic_offset, logger, suppress_warnings, description) = {
            let mut core = self.core.borrow_mut();
            let offset = core.input.borrow().last_offset();
            let object = core.last_object_description.clone();
            let (diagnostic_message, diagnostic_offset) = if object.is_empty() {
                (message.clone(), (offset > 0).then_some(offset))
            } else {
                let offset_text = if offset > 0 {
                    format!(", offset {offset}")
                } else {
                    String::new()
                };
                (format!("({object}{offset_text}): {message}"), None)
            };
            core.repair_diagnostics.push(Diagnostic::warning(
                diagnostic_message.clone(),
                diagnostic_offset,
            ));
            (
                diagnostic_message,
                diagnostic_offset,
                core.logger.clone(),
                core.suppress_warnings,
                core.description.clone(),
            )
        };
        route_warning(
            &logger,
            suppress_warnings,
            &description,
            diagnostic_offset,
            &diagnostic_message,
        )
    }

    /// qpdf's `damagedPDF("expected endobj")` is rendered with the object
    /// identity and the input source's last offset already in the message
    /// (`libqpdf/QPDF.cc:1297-1310,1331-1355,2641-2644`). Keeping that
    /// location in the diagnostic text also lets the qtest driver add only
    /// the filename, as qpdf's warning formatter does.
    fn expected_endobj_warning(object_ref: ObjectRef, offset: u64) -> String {
        format!(
            "(object {} {}, offset {offset}): expected endobj",
            object_ref.number, object_ref.generation
        )
    }

    /// Warn `expected endobj` with qpdf's current object description.
    ///
    /// `damagedPDF("expected endobj")` renders `m->last_object_description`,
    /// which `readObjectAtOffset` and `readObject` build from the caller's
    /// description and the object identity (`libqpdf/QPDF.cc:1298-1310`,
    /// `:1331-1354`, `:2641-2644`), so a described read such as the
    /// `linearization hint stream` keeps its prefix on this warning too.
    fn push_expected_endobj_warning(
        &self,
        object_ref: ObjectRef,
        offset: u64,
        read_description: Option<&[u8]>,
    ) -> Result<()> {
        let last_description = self.core.borrow().last_object_description_bytes.clone();
        if !last_description.is_empty() {
            // qpdf's damagedPDF("expected endobj") renders the current
            // `m->last_object_description`, which may now be the indirect
            // `/Length` object resolved by readStream (`QPDF.cc:1725`), not
            // the stream object that entered readStream.
            if read_description.is_none() {
                let object_description = String::from_utf8_lossy(&last_description);
                return self.push_warning_at(
                    offset,
                    format!("({object_description}, offset {offset}): expected endobj"),
                );
            }
            return self.push_stream_warning_with_object_description(
                &last_description,
                offset,
                "expected endobj",
            );
        }
        if read_description.is_some() {
            return self.push_stream_warning_with_description(
                object_ref,
                offset,
                "expected endobj",
                read_description,
            );
        }
        self.push_warning(Self::expected_endobj_warning(object_ref, offset))
    }

    /// The canonical handle for `object_ref` **if one has already been
    /// minted**, without minting one.
    ///
    /// The read-only counterpart of [`Self::get_object_handle`], for the
    /// `&self` callers that ask whether a reference has a handle at all.
    pub(crate) fn registered_handle(&self, object_ref: ObjectRef) -> Option<ObjectHandle> {
        self.core.borrow().object_cache.get(&object_ref).cloned()
    }

    /// Whether `object_ref` was created through a qpdf-shaped allocation or
    /// replacement path rather than merely cached while resolving a reference.
    /// This is the provenance distinction required when both cases currently
    /// hold a resolved null in the same canonical object cache.
    pub(crate) fn is_allocated_object(&self, object_ref: ObjectRef) -> bool {
        self.core
            .borrow()
            .allocated_object_refs
            .contains(&object_ref)
    }

    /// Every canonical handle minted so far, in [`ObjectRef`] order.
    ///
    /// qpdf `QPDF::getAllObjects` walks `m->obj_cache` the same way
    /// (`libqpdf/QPDF.cc:1285-1295`).
    pub(crate) fn all_object_handles(&self) -> Vec<ObjectHandle> {
        self.core.borrow().object_cache.values().cloned().collect()
    }

    /// The largest object *number* any canonical handle occupies.
    pub(crate) fn max_object_number(&self) -> Option<u32> {
        self.core
            .borrow()
            .object_cache
            .keys()
            .next_back()
            .map(|object_ref| object_ref.number)
    }

    /// Resolve every unresolved entry in the effective xref table, matching
    /// qpdf's `QPDF::resolveXRefTable` (`libqpdf/QPDF.cc:1239-1254`). A
    /// resolution-time xref reconstruction invalidates the in-progress walk;
    /// the caller reruns it against the rebuilt table before marking the cache
    /// prepared.
    fn resolve_xref_table(&self) -> Result<bool> {
        let may_change = !self.reconstructed_xref();
        for object_ref in self.xref_refs() {
            let handle = self.get_object_handle(object_ref);
            if handle.is_resolved() {
                continue;
            }
            handle.try_dereference()?;
            if may_change && self.reconstructed_xref() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Ensure every effective xref object and every parser-discovered
    /// dangling reference is represented in the canonical cache, matching
    /// qpdf's `QPDF::fixDanglingReferences` (`libqpdf/QPDF.cc:1258-1269`).
    /// Repeated calls are a no-op after the fixed state has been recorded.
    pub(crate) fn fix_dangling_references(&self) -> Result<()> {
        if self.core.borrow().fixed_dangling_refs {
            return Ok(());
        }

        if !self.resolve_xref_table()? {
            self.resolve_xref_table()?;
        }

        self.core.borrow_mut().fixed_dangling_refs = true;
        Ok(())
    }

    /// Return the greatest object number in the prepared canonical cache,
    /// matching qpdf's `QPDF::getObjectCount` (`libqpdf/QPDF.cc:1271-1283`).
    pub(crate) fn get_object_count(&self) -> Result<u32> {
        self.fix_dangling_references()?;
        Ok(self.max_object_number().unwrap_or(0))
    }

    /// Return qpdf's next generation-zero object identity.
    ///
    /// `QPDF::nextObjGen` first prepares the effective object cache through
    /// `getObjectCount`, then allocates one above its greatest object number
    /// and rejects the signed `int` boundary
    /// (`libqpdf/QPDF.cc:1271-1283,1872-1880`; `QPDFObjGen.hh:41-74`).
    /// Free xref entries and legacy-only values are deliberately absent from
    /// this cache and therefore cannot raise the allocation ceiling.
    pub(crate) fn next_obj_gen(&self) -> Result<ObjectRef> {
        let max_object_id = self.get_object_count()?;
        if max_object_id >= i32::MAX as u32 {
            return Err(Error::Unsupported(
                "max object id is too high to create new objects".to_string(),
            ));
        }
        Ok(ObjectRef::new(max_object_id + 1, 0))
    }

    /// Register the existing handle allocation under qpdf's fresh object
    /// identity, preserving the handle's shared storage instead of cloning its
    /// value. This is the Rust equivalent of qpdf's higher-level
    /// `QPDF::makeIndirectObject(QPDFObjectHandle)`, which rejects an
    /// uninitialized handle before delegating to the raw-`shared_ptr`
    /// primitive `QPDF::makeIndirectFromQPDFObject`
    /// (`libqpdf/QPDF.cc:1882-1895`): the handle aliases held by the caller
    /// and the canonical cache see the same object after promotion.
    pub(crate) fn make_indirect_from_object_handle(
        &self,
        handle: ObjectHandle,
    ) -> Result<ObjectHandle> {
        if !handle.is_direct() {
            return Err(Error::Unsupported(
                "cannot make an already-indirect ObjectHandle indirect".to_string(),
            ));
        }
        if !handle.is_initialized() {
            return Err(Error::Unsupported(
                "attempted to make an uninitialized QPDFObjectHandle indirect".to_string(),
            ));
        }
        let object_ref = self.next_obj_gen()?;
        // A contextless direct root already claimed by a name/number tree
        // wrapper for a different Pdf must not be silently re-owned here:
        // that wrapper's existing (cached) claim would otherwise go stale,
        // letting it keep operating on what is now this Pdf's object. Run
        // after next_obj_gen so a failed allocation leaves `handle`
        // completely untouched, matching every other fallible step here.
        handle.claim_tree_pdf(self.pdf_unique_id.get())?;
        let promoted = handle.promote_to_indirect(
            object_ref,
            self.pdf_unique_id.get(),
            self.self_weak.clone(),
        );
        let mut core = self.core.borrow_mut();
        let previous = core.object_cache.insert(object_ref, promoted.clone());
        core.allocated_object_refs.insert(object_ref);
        debug_assert!(
            previous.is_none(),
            "next_obj_gen must return a fresh ObjGen"
        );
        Ok(promoted)
    }

    /// Replace the canonical value for `object_ref` without replacing the
    /// canonical handle itself.
    ///
    /// qpdf's `QPDF::replaceObject` rejects an indirect handle or a handle for
    /// which `isInitialized()` is false before mutating the cache, and then
    /// calls `updateCache` (`QPDF.cc:1986-1993`). qpdf's
    /// `QPDFObjectHandle::isInitialized()` is only the non-null object-pointer
    /// check (`QPDFObjectHandle.hh:1636`), so qpdf Reserved/Destroyed values
    /// are not rejected by that guard. flpdf deliberately retains its existing
    /// narrower `Resolved(ObjectValue)` source contract; the preflight below
    /// enforces that contract before minting an absent target cache entry.
    /// `QPDFObject::assign` inside that update shares the replacement's
    /// `QPDFValue` (`QPDF.cc:1986-1993,1835-1857`;
    /// `QPDFObject_private.hh:117-120`), which is represented by
    /// [`ObjectHandle::share_value_state_with`].
    pub(crate) fn replace_object(
        &self,
        object_ref: ObjectRef,
        replacement: ObjectHandle,
    ) -> Result<ObjectHandle> {
        if !replacement.is_direct() {
            return Err(Error::Unsupported(
                "QPDF::replaceObject called with indirect object handle".to_string(),
            ));
        }
        if !replacement.belongs_exclusively_to_pdf(self.pdf_unique_id.get()) {
            return Err(Error::Unsupported(
                "Attempting to add an object from a different QPDF. Use QPDF::copyForeignObject to add objects from another file.".to_string(),
            ));
        }
        // `share_value_state_with` retains the same contract, but this
        // preflight must run before `get_object_handle`: a failed replacement
        // must not leave an absent target in the canonical object cache.
        replacement.validate_replacement_source()?;

        let target = self.get_object_handle(object_ref);
        target.share_value_state_with(&replacement)?;
        target.clear_description();
        target.reset_parsed_offset();
        target.set_end_offsets(NO_PARSED_OFFSET, NO_PARSED_OFFSET);
        if self.xref_entry(object_ref).is_none() {
            self.core
                .borrow_mut()
                .allocated_object_refs
                .insert(object_ref);
        }
        Ok(target)
    }

    /// Swap two canonical object values while retaining their object
    /// generations and every outstanding handle identity.
    ///
    /// This is qpdf's `QPDF::swapObjects` (`libqpdf/QPDF.cc:2279-2289`):
    /// both cache entries are resolved before `QPDFObject::swapWith` exchanges
    /// their value allocations. Unknown object generations therefore resolve
    /// to qpdf's ordinary null object before the swap.
    pub(crate) fn swap_objects(&self, first: ObjectRef, second: ObjectRef) -> Result<()> {
        let first_handle = self.get_object_handle(first);
        let second_handle = self.get_object_handle(second);
        first_handle.try_dereference()?;
        second_handle.try_dereference()?;
        first_handle.swap_value_state_with(&second_handle);
        Ok(())
    }

    /// Remove an object from the canonical xref/cache view and leave any
    /// outstanding handle as a floating null value.
    ///
    /// qpdf erases the exact xref/cache entry after assigning a null value to
    /// the cached object (`QPDF.cc:1996-2005`). This cache mutation is
    /// separate from xref registration's transient `deleted_objects`: qpdf
    /// uses that local set only while loading or reconstructing xrefs
    /// (`QPDF.cc:686-708`, `:1187-1210`), so no mutation-history tombstone
    /// belongs in the resolver. The handle is nullified first so aliases held
    /// by callers observe the transition even after the cache entry is gone.
    #[cfg(test)]
    pub(crate) fn remove_object(&self, object_ref: ObjectRef) -> Result<()> {
        let cached = {
            let mut core = self.core.borrow_mut();
            core.source_xref_entries.remove(&object_ref);
            core.default_xref_entries.remove(&object_ref);
            core.fixed_dangling_refs = false;
            core.allocated_object_refs.remove(&object_ref);
            core.object_cache.remove(&object_ref)
        };
        if let Some(handle) = cached {
            handle.remove_from_document();
        }
        Ok(())
    }

    /// Whether cross-reference table reconstruction has occurred during resolution.
    ///
    /// qpdf `m->reconstructed_xref` (`include/qpdf/QPDF.hh:1480`).
    pub(crate) fn reconstructed_xref(&self) -> bool {
        self.core.borrow().reconstructed_xref
    }

    /// Whether resolution-time damage recovery is enabled.
    ///
    /// qpdf's `m->attempt_recovery` (`include/qpdf/QPDF.hh:1461`) controls
    /// whether a failed object read may trigger `reconstruct_xref`.
    pub(crate) fn attempt_recovery(&self) -> bool {
        self.core.borrow().attempt_recovery
    }

    /// Set qpdf's live `m->attempt_recovery` policy.
    ///
    /// This is the resolver-side state mutation for `QPDF::setAttemptRecovery`
    /// (`include/qpdf/QPDF.hh:234`, `libqpdf/QPDF.cc:334`). The flag is read
    /// at both the xref-load boundary and the later object-resolution retry
    /// boundary, so changing it on the live resolver must affect both paths.
    pub(crate) fn set_attempt_recovery(&self, value: bool) {
        self.core.borrow_mut().attempt_recovery = value;
    }

    fn stream_recovery_enabled(&self) -> bool {
        self.core.borrow().attempt_recovery
    }

    /// qpdf `QPDF::reconstruct_xref` (`libqpdf/QPDF.cc:516-530`) & `QPDF::readObjectAtOffset`
    /// recovery retry (`:1614-1637`).
    ///
    /// Only `Error::Parse` triggers reconstruction, matching qpdf's
    /// `catch (QPDFExc&)` guard at `QPDF.cc:1614` (qpdf's `QPDFExc` covers only
    /// parse-level damage, not I/O or system errors).
    ///
    /// Reconstructs the cross-reference table via line-scan of the logical byte
    /// slice (`bytes[header_offset..]`), matching qpdf's use of
    /// `OffsetInputSource` which presents logical offset 0 to `reconstruct_xref`
    /// (`libqpdf/OffsetInputSource.cc:seek`).  Flips `m->reconstructed_xref` to
    /// `true`, emits repair warnings, and retries reading `object_ref` at the
    /// rebuilt offset. Returns `Ok(Some(parsed))`
    /// on successful retry, `Ok(None)` if the object is absent post-rebuild (to
    /// be warned and resolved to null), or `Err(err)` if recovery fails, if the
    /// trigger is not a parse error, or if a second reconstruction attempt is
    /// made (infinite-loop guard).
    fn reconstruct_xref_and_retry(
        &self,
        trigger_error: Error,
        object_ref: ObjectRef,
    ) -> Result<Option<ParsedObjectAtOffset>> {
        if self.core.borrow().reconstructed_xref {
            // Avoid xref reconstruction infinite loops (QPDF.cc:518-522).
            return Err(trigger_error);
        }

        // qpdf `catch (QPDFExc&)` at QPDF.cc:1614 — only parse damage triggers
        // reconstruction.  I/O, system, and other errors are propagated
        // unchanged so that the reconstructed_xref guard is not tripped by an
        // unrelated failure, and the guard state is not poisoned for a later
        // genuine xref mismatch.
        if !matches!(trigger_error, Error::Parse { .. }) {
            return Err(trigger_error);
        }

        {
            let mut core = self.core.borrow_mut();
            core.reconstructed_xref = true;
            core.fixed_dangling_refs = false;
        }

        // Push repair warnings (QPDF.cc:528-530)
        self.push_warning("file is damaged")?;

        let Error::Parse { offset, message } = &trigger_error else {
            unreachable!("guard above ensures Parse variant"); // cov:ignore: unreachable after guard
        };
        let location = format!(
            "(object {} {}, offset {}): {}",
            object_ref.number, object_ref.generation, offset, message
        );
        self.push_warning(location)?;
        self.push_warning("Attempting to reconstruct cross-reference table")?;

        // Read logical bytes (header_offset already consumed), matching qpdf's
        // OffsetInputSource which seeks to logical-0 at QPDF.cc:543.
        let header_offset = self.core.borrow().header_offset;
        let raw_bytes = self.core.borrow_mut().read_underlying_bytes()?;
        let logical_bytes = raw_bytes.get(header_offset..).ok_or_else(|| {
            Error::parse(
                header_offset,
                "input ended before the detected PDF header offset",
            )
        })?;
        // `reconstruct_xref` rescans every recoverable body after removing
        // only type-1 xref rows (`QPDF.cc:516-575`). Its local
        // `deleted_objects` suppression belongs to xref registration and is
        // cleared after that operation (`QPDF.cc:686-708`, `:1187-1210`);
        // `removeObject` is instead an exact cache/xref mutation
        // (`QPDF.cc:1996-2005`). A prior canonical removal therefore cannot
        // filter this fresh recovery scan.
        let new_entries = crate::xref::recover_xref_entries(logical_bytes, false)?.entries;

        {
            let mut core = self.core.borrow_mut();
            core.source_xref_entries
                .retain(|_, entry| !matches!(entry, XrefEntry::Uncompressed { .. }));
            core.source_xref_entries.extend(new_entries);
        }

        // Lookup object_ref in reconstructed xref table
        let retry_entry = self.xref_entry(object_ref);
        match retry_entry {
            Some(XrefEntry::Uncompressed { offset: new_offset }) => {
                // qpdf QPDF.cc:1622-1628: the retry call has try_recovery=false, so
                // any parse failure propagates as an exception (Err here).
                self.read_object_at_offset_with_description(
                    new_offset,
                    object_ref,
                    true,
                    false,
                    None,
                )
                    .map(Some)
                    .map_err(ReadObjectAtOffsetError::into_error)
            }
            Some(XrefEntry::Compressed { .. }) => Err(Error::Unsupported(format!(
                "canonical resolver cannot yet resolve object {} {}: only uncompressed cross-reference entries are implemented",
                object_ref.number, object_ref.generation
            ))),
            Some(XrefEntry::Free { .. }) | None => Ok(None),
        }
    }

    /// Sever every canonical handle's value, breaking the reference cycles a
    /// resolved object graph forms.
    ///
    /// qpdf `QPDF::~QPDF` walks `m->obj_cache` and replaces each object with
    /// `QPDF_Destroyed` for the same reason. `Pdf::drop` is the sole caller;
    /// see its own comment for why the cycles exist.
    pub(crate) fn disconnect_all(&self) {
        for handle in self.core.borrow().object_cache.values() {
            handle.disconnect();
        }
    }

    /// Append a warning to this document's one diagnostics collection —
    /// the accumulating half of qpdf `QPDF::warn`
    /// (`libqpdf/QPDF.cc:487-494`). qpdf's does two things: `push_back` onto
    /// `m->warnings`, and — unless `m->suppress_warnings` — write
    /// `"WARNING: " << e.what()` straight to its logger (`:492`). Only the
    /// append happens before logger delivery, and the core borrow is released
    /// before the pipeline write so a custom sink can safely call unrelated
    /// document code. Suppression skips only delivery, never collection.
    ///
    /// Takes `&self`, which is the whole reason the sink moved here: the
    /// resolver reaches its document through a `Weak` and never holds a
    /// `&mut Pdf`. `Pdf::push_warning` is the same door for callers that do.
    ///
    /// Borrow discipline: the `borrow_mut()` is taken and dropped inside this
    /// expression, so it composes with a nested resolution — but it must not
    /// be called while a borrow of the core is already held.
    pub(crate) fn push_warning(&self, message: impl Into<String>) -> Result<()> {
        self.push_warning_with_offset(None, None, message)
    }

    /// [`Self::push_warning`] with the offset qpdf attributes the warning to.
    ///
    /// qpdf carries the position inside the exception it throws — every
    /// `damagedPDF(file, object, offset, message)` overload takes one
    /// (`include/qpdf/QPDF.hh:1044-1050`) — where flpdf keeps it in
    /// [`Diagnostic::offset`] beside the text.
    ///
    pub(crate) fn push_warning_at(&self, offset: u64, message: impl Into<String>) -> Result<()> {
        self.push_warning_with_offset(Some(offset), None, message)
    }

    /// Append qpdf's `damagedPDF("trailer", message)` warning shape.
    ///
    /// `QPDF::readTrailer` leaves the parser's input-source offset in
    /// `getLastOffset()` before `initializeEncryption` reports malformed
    /// `/ID` (`libqpdf/QPDF.cc:1313-1327`;
    /// `libqpdf/QPDF_encryption.cc:718-751`). The trailer location is already
    /// part of the exception text, so the generic input warning formatter must
    /// not add a second `(offset ...)` wrapper when this warning is replayed.
    pub(crate) fn push_trailer_warning_at(
        &self,
        offset: u64,
        message: impl Into<String>,
    ) -> Result<()> {
        let (diagnostic_offset, location) = if offset > 0 {
            (Some(offset), format!("(trailer, offset {offset})"))
        } else {
            (None, "(trailer)".to_owned())
        };
        self.push_warning_with_offset(
            diagnostic_offset,
            None,
            format!("{location}: {}", message.into()),
        )
    }

    /// Format the `QPDFExc::what()` bytes emitted by qpdf's JSON reactor.
    fn format_json_warning_what(
        description: &[u8],
        input_name: &[u8],
        object: &str,
        offset: i64,
        message: &[u8],
    ) -> Vec<u8> {
        let mut object = object.as_bytes().to_vec();
        if input_name != description {
            object.extend_from_slice(b" from ");
            object.extend_from_slice(input_name);
        }
        let offset_text = if offset > 0 {
            let mut text = b", offset ".to_vec();
            text.extend_from_slice(offset.to_string().as_bytes());
            text
        } else {
            Vec::new()
        };
        let mut what = Vec::new();
        if object.is_empty() {
            if offset > 0 {
                if description.is_empty() {
                    what.extend_from_slice(b"offset ");
                    what.extend_from_slice(offset.to_string().as_bytes());
                    what.extend_from_slice(b": ");
                } else {
                    what.extend_from_slice(description);
                    what.extend_from_slice(b" (offset ");
                    what.extend_from_slice(offset.to_string().as_bytes());
                    what.extend_from_slice(b"): ");
                }
            } else if !description.is_empty() {
                what.extend_from_slice(description);
                what.extend_from_slice(b": ");
            }
        } else if description.is_empty() {
            what.extend_from_slice(&object);
            what.extend_from_slice(&offset_text);
            what.extend_from_slice(b": ");
        } else {
            what.extend_from_slice(description);
            what.extend_from_slice(b" (");
            what.extend_from_slice(&object);
            what.extend_from_slice(&offset_text);
            what.extend_from_slice(b"): ");
        }
        what.extend_from_slice(message);
        what
    }

    /// Emit a warning from qpdf's JSON input reactor.
    ///
    /// `QPDF::warn(qpdf_e_json, object, offset, message)` formats the PDF
    /// description outside the `(obj:... from input, offset N)` object
    /// context (`libqpdf/QPDF.cc:488-505`; `libqpdf/QPDFExc.cc:19-49`). The
    /// ordinary offset sink cannot express that shape without duplicating the
    /// filename or moving the offset into the message, so JSON input keeps a
    /// dedicated routing door here.
    pub(crate) fn push_json_warning(
        &self,
        input_name: impl AsRef<[u8]>,
        object: &str,
        offset: i64,
        message: impl Into<String>,
    ) -> Result<()> {
        let input_name = input_name.as_ref().to_vec();
        let message = message.into();
        let (logger, suppress_warnings, what) = {
            let mut core = self.core.borrow_mut();
            let what = Self::format_json_warning_what(
                &core.description,
                &input_name,
                object,
                offset,
                message.as_bytes(),
            );
            let mut diagnostic = Diagnostic::object_warning_bytes(&what);
            diagnostic.offset = (offset >= 0).then_some(offset as u64);
            core.repair_diagnostics.push(diagnostic);
            (core.logger.clone(), core.suppress_warnings, what)
        };
        if suppress_warnings {
            return Ok(());
        }

        let mut line = b"WARNING: ".to_vec();
        line.extend_from_slice(&what);
        line.push(b'\n');
        logger.warn(line)
    }

    /// Emit a warning raised while parsing a canonical ObjStm member.
    ///
    /// qpdf parses the member through its `BufferInputSource` whose name is
    /// the source description plus `object stream N`, so the parser offset is
    /// relative to the decoded buffer rather than the source PDF
    /// (`libqpdf/QPDF.cc:1792-1807`). Keep that coordinate in the rendered
    /// qpdf message, but do not publish it as [`Diagnostic::offset`], whose
    /// contract is a source-file position.
    pub(crate) fn push_object_stream_warning(
        &self,
        stream_number: u32,
        object_ref: ObjectRef,
        offset: u64,
        message: impl Into<String>,
    ) -> Result<()> {
        let detail = message.into();
        let diagnostic_message = format!(
            "object stream {stream_number} (object {} {}, offset {offset}): {detail}",
            object_ref.number, object_ref.generation
        );
        let object_message = format!(
            "(object {} {}, offset {offset}): {detail}",
            object_ref.number, object_ref.generation
        );
        let (logger, suppress_warnings, description) = {
            let mut core = self.core.borrow_mut();
            core.repair_diagnostics
                .push(Diagnostic::warning(diagnostic_message, None));
            (
                core.logger.clone(),
                core.suppress_warnings,
                core.description.clone(),
            )
        };
        let mut object_stream_description = description;
        if !object_stream_description.is_empty() {
            object_stream_description.extend_from_slice(b" object stream ");
        } else {
            object_stream_description.extend_from_slice(b"object stream ");
        }
        object_stream_description.extend_from_slice(stream_number.to_string().as_bytes());
        route_warning(
            &logger,
            suppress_warnings,
            &object_stream_description,
            None,
            &object_message,
        )
    }

    /// `description_override` is `Some` only for a foreign stream's deferred
    /// read: qpdf's `pipeForeignStreamData` builds its `QPDFExc` from the
    /// captured source `InputSource`'s name, and `QPDF::warn(QPDFExc const&)`
    /// pushes that exception into the destination's own warning list without
    /// rewriting its filename (`libqpdf/QPDF.cc:488-494,2498-2500,2565-2585`).
    /// `self` (the destination) still owns collection into
    /// [`Diagnostic`]/`repair_diagnostics` and routing through its own
    /// logger/`suppress_warnings`; only the location text substitutes the
    /// source's description for `self`'s own.
    fn push_warning_with_offset(
        &self,
        offset: Option<u64>,
        description_override: Option<&[u8]>,
        message: impl Into<String>,
    ) -> Result<()> {
        let message = message.into();
        let (logger, suppress_warnings, own_description) = {
            let mut core = self.core.borrow_mut();
            let diagnostic = match description_override {
                Some(description) => {
                    Diagnostic::warning_with_description(message.clone(), offset, description)
                }
                None => Diagnostic::warning(message.clone(), offset),
            };
            core.repair_diagnostics.push(diagnostic);
            (
                core.logger.clone(),
                core.suppress_warnings,
                core.description.clone(),
            )
        };
        let description = description_override.unwrap_or(own_description.as_slice());
        route_warning(&logger, suppress_warnings, description, offset, &message)
    }

    /// [`Self::push_warning`] for a warning an object raised about itself.
    ///
    /// Three qpdf emitters reach this sink through `DocumentResolver::warn`,
    /// and they build different exceptions. `typeWarning` and `objectWarning`
    /// use `QPDFExc(qpdf_e_object, "", description, 0, message)`
    /// (`libqpdf/QPDFObjectHandle.cc:2180-2188,2210`); `warnIfPossible`'s
    /// context branch instead uses `qpdf_e_damaged_pdf` (`:2202`). All three
    /// build their exception with an empty filename, so the object
    /// description is the only location `QPDFExc::createWhat`
    /// (`libqpdf/QPDFExc.cc:19-49`) can interpose — that part of the routing
    /// is uniform even though the error code is not. `QPDF::resolve` reads
    /// with an empty description (`libqpdf/QPDF.cc:1725`), which makes
    /// `setLastObjectDescription` (`:1297-1309`) yield `"object N G"` and
    /// never a file name. [`Self::push_warning`]'s input-source description
    /// is the slot qpdf fills for `damagedPDF` warnings instead
    /// (`libqpdf/QPDFParser.cc:512`, `input->getName()`), so reusing it here
    /// would emit a file name qpdf does not. Live parser direct values and
    /// canonical handles now carry their qpdf parser descriptions; this sink
    /// intentionally receives the already-formed message and keeps its
    /// diagnostic offset empty rather than adding a second location.
    ///
    /// Same borrow discipline as [`Self::push_warning`]: the `borrow_mut()`
    /// is taken and dropped before the logger write.
    pub(crate) fn push_object_warning(&self, message: impl AsRef<[u8]>) -> Result<()> {
        let message = message.as_ref().to_vec();
        let (logger, suppress_warnings) = {
            let mut core = self.core.borrow_mut();
            core.repair_diagnostics
                .push(Diagnostic::object_warning_bytes(&message));
            (core.logger.clone(), core.suppress_warnings)
        };
        route_object_warning(&logger, suppress_warnings, &message)
    }

    /// Record and route a complete qpdf warning value whose
    /// `QPDFExc::what()` has already been assembled by the owning consumer.
    /// This is the byte-preserving equivalent of `QPDF::warn(QPDFExc const&)`:
    /// deferred job replay must not prepend a second filename or discard a
    /// non-UTF-8 source description (`libqpdf/QPDF.cc:488-504` and
    /// `QPDFExc.cc:19-50`).
    pub(crate) fn push_qpdf_warning_bytes(&self, message: impl AsRef<[u8]>) -> Result<()> {
        let message = message.as_ref().to_vec();
        let (logger, suppress_warnings) = {
            let mut core = self.core.borrow_mut();
            core.repair_diagnostics
                .push(Diagnostic::object_warning_bytes(&message));
            (core.logger.clone(), core.suppress_warnings)
        };
        route_object_warning(&logger, suppress_warnings, &message)
    }

    pub(crate) fn replay_warnings(&self, diagnostics: &Diagnostics) -> Result<()> {
        let (logger, suppress_warnings, own_description) = {
            let core = self.core.borrow();
            (
                core.logger.clone(),
                core.suppress_warnings,
                core.description.clone(),
            )
        };
        for diagnostic in diagnostics.entries() {
            if diagnostic.is_object_warning() {
                route_object_warning(&logger, suppress_warnings, diagnostic.message_bytes())?;
                continue;
            }
            let description = diagnostic
                .description
                .as_deref()
                .unwrap_or(own_description.as_slice());
            route_warning(
                &logger,
                suppress_warnings,
                description,
                diagnostic.offset,
                &diagnostic.message,
            )?;
        }
        Ok(())
    }

    /// A snapshot of every warning raised on this document so far.
    ///
    /// Returns an owned clone because the collection lives behind a
    /// `RefCell` and a `&Diagnostics` cannot be handed out from one. See
    /// `Pdf::repair_diagnostics`, which is the public door onto this and
    /// carries the trade-off.
    ///
    /// **A snapshot is interchangeable with the borrow it replaced**, which is
    /// what lets callers keep comparing a length captured earlier against a
    /// later one and iterating with `.skip(start)` — `flpdf-cli`'s
    /// `finish_lazy_warnings` and `emit_warnings_since`
    /// (`crates/flpdf-cli/src/main.rs:5341-5364`) do exactly that.
    /// [`Diagnostics`] is append-only: `push` and `push_encrypted` are its
    /// only mutators and its entry vector is private, so an index valid in one
    /// snapshot names the same entry in every later one. Nothing replaces the
    /// collection wholesale either — `xref.rs` does that only on `LoadedXref`,
    /// before the document exists. Were either to change, every
    /// `diagnostics_start` in the CLI would quietly start meaning something
    /// else.
    pub(crate) fn repair_diagnostics(&self) -> Diagnostics {
        self.core.borrow().repair_diagnostics.clone()
    }

    /// qpdf's `QPDF::numWarnings` (`libqpdf/QPDF.cc:360-363`): the size of
    /// the warning collection without copying it.
    pub(crate) fn num_warnings(&self) -> usize {
        self.core.borrow().repair_diagnostics.entries().len()
    }

    pub(crate) fn logger(&self) -> crate::QPDFLogger {
        self.core.borrow().logger.clone()
    }

    pub(crate) fn set_logger(&self, logger: crate::QPDFLogger) {
        self.core.borrow_mut().logger = logger;
    }

    pub(crate) fn suppress_warnings(&self) -> bool {
        self.core.borrow().suppress_warnings
    }

    pub(crate) fn set_suppress_warnings(&self, suppress: bool) {
        self.core.borrow_mut().suppress_warnings = suppress;
    }

    /// The offset repair chose as qpdf-logical zero. See
    /// [`ResolverCore::header_offset`].
    #[cfg(test)]
    pub(crate) fn header_offset(&self) -> usize {
        self.core.borrow().header_offset
    }

    /// This document's encryption parameters, in their shared, mutable-in-
    /// place form — the pipe-side door onto [`ResolverCore::encryption_parameters`].
    ///
    /// Clones the `Rc` under a single short borrow, matching every other
    /// accessor here: nothing is held once this returns, so a caller that
    /// then does I/O through `self` cannot double-borrow.
    ///
    /// Crate-visible because the canonical [`crate::pdf::Pdf`] container is a
    /// sibling of `reader`; the returned [`crate::encryption::state::EncryptionState`]
    /// has the same visibility. No public API exposes either implementation
    /// type.
    pub(crate) fn encryption_parameters(
        &self,
    ) -> Rc<RefCell<Option<crate::encryption::state::EncryptionState>>> {
        self.core.borrow().encryption_parameters.clone()
    }

    fn stream_input(&self) -> Rc<StreamInput<R>> {
        self.core.borrow().input.borrow().clone()
    }

    /// Replace the current source with qpdf's `InvalidInputSource`
    /// (`libqpdf/QPDF.cc:278-281`). The previous `Rc` is deliberately left
    /// alive for any foreign-stream provider that captured it before this
    /// operation, just as qpdf replaces `m->file` without changing the source
    /// pointer stored in `ForeignStreamData`.
    pub(crate) fn close_input_source(&self) {
        self.core
            .borrow_mut()
            .input
            .replace(Rc::new(StreamInput::invalid()));
    }

    pub(crate) fn input_source_closed(&self) -> bool {
        self.core.borrow().input.borrow().is_closed()
    }

    /// Return the name of the current qpdf input source. An active source uses
    /// the caller-provided description; the invalid replacement has qpdf's
    /// fixed `closed input source` name.
    pub(crate) fn input_source_name(&self) -> String {
        let core = self.core.borrow();
        if core.input.borrow().is_closed() {
            CLOSED_INPUT_SOURCE_NAME.to_owned()
        } else {
            String::from_utf8_lossy(&core.description).into_owned()
        }
    }

    /// This document's cross-reference entry for `object_ref`, if the source
    /// declared one.
    pub(crate) fn xref_entry(&self, object_ref: ObjectRef) -> Option<XrefEntry> {
        self.core
            .borrow()
            .source_xref_entries
            .get(&object_ref)
            .copied()
    }

    fn insert_default_xref_entry(&self, object_ref: ObjectRef) {
        self.core
            .borrow_mut()
            .default_xref_entries
            .insert(object_ref);
    }

    #[cfg(test)]
    pub(crate) fn insert_default_xref_entry_for_test(&self, object_ref: ObjectRef) {
        self.insert_default_xref_entry(object_ref);
    }

    fn has_default_xref_entry(&self, object_ref: ObjectRef) -> bool {
        self.core
            .borrow()
            .default_xref_entries
            .contains(&object_ref)
    }

    /// A snapshot of the source cross-reference entries, excluding the
    /// resolver-created default free rows. Resolution decisions use this view
    /// because those rows are lookup side effects rather than source objects.
    pub(crate) fn source_xref_entries(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        self.core.borrow().source_xref_entries.clone()
    }

    /// A snapshot of the whole effective cross-reference table. qpdf inserts
    /// a default type-0 row into `m->xref_table` when an object-stream header
    /// names an absent member (`libqpdf/QPDF.cc:1823`); expose that row in the
    /// public snapshot while preserving any source row for the same identity.
    pub(crate) fn xref_entries(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        let core = self.core.borrow();
        let mut entries = core.source_xref_entries.clone();
        for object_ref in &core.default_xref_entries {
            entries
                .entry(*object_ref)
                .or_insert(XrefEntry::Free { next: 0 });
        }
        entries
    }

    fn object_stream_description_template(
        &self,
        stream_number: u32,
        object_ref: ObjectRef,
    ) -> Vec<u8> {
        let core = self.core.borrow();
        let mut description = core.description.clone();
        description.extend_from_slice(
            format!(
                " object stream {stream_number}, object {} {} at offset $PO",
                object_ref.number, object_ref.generation
            )
            .as_bytes(),
        );
        description
    }

    fn resolve_object_stream_with_failure_kind(
        &self,
        stream_number: u32,
    ) -> std::result::Result<(), ObjectStreamResolutionError> {
        if !self
            .core
            .borrow_mut()
            .resolved_object_streams
            .insert(stream_number)
        {
            return Ok(());
        }

        // qpdf marks the object stream before forcing its own resolution
        // (`QPDF.cc:1760-1767`), so a failed stream remains accounted for and
        // cannot be parsed repeatedly through different member references.
        let stream_ref = ObjectRef::new(stream_number, 0);
        let stream_handle = self.get_object_handle(stream_ref);
        stream_handle.try_dereference()?;
        let (stream_end_before_space, stream_end_after_space) = stream_handle.end_offsets();
        let stream_dict = stream_handle.as_stream_dict().ok_or_else(|| {
            Error::parse(
                0,
                format!("supposed object stream {stream_number} is not a stream"),
            )
        })?;

        if !stream_dict.try_is_dictionary_of_type(b"ObjStm", b"")? {
            // qpdf uses damagedPDF(message) here, so the warning carries the
            // current object description and input-source last offset
            // (`QPDF.cc:1760-1779,2630-2644`) rather than a bare message.
            self.push_damaged_warning(format!(
                "supposed object stream {stream_number} has wrong type"
            ))
            .map_err(ObjectStreamResolutionError::WarningDelivery)?;
        }

        let mut decoded_stream = crate::pipeline::buffer::Buffer::new("object stream", None);
        let mut filtering_attempted = false;
        let _success = stream_handle.pipe_stream_data_for_object_stream(
            &mut decoded_stream,
            &mut filtering_attempted,
            0,
            crate::writer::DecodeLevel::Specialized,
            false,
            false,
        )?;
        if !filtering_attempted {
            return Err(ObjectStreamResolutionError::Operation(Error::Unsupported(
                "getStreamData called on unfilterable stream".to_owned(),
            )));
        }
        let decoded_stream_data = decoded_stream.take_buffer()?;

        let object_count = Self::object_stream_integer(&stream_dict, b"/N", "/N")?;
        let first = Self::object_stream_integer(&stream_dict, b"/First", "/First")?;
        let mut tokenizer = Tokenizer::new(&decoded_stream_data);
        let mut members = BTreeMap::new();
        for _ in 0..object_count {
            let object_number = u32::try_from(tokenizer.next_integer()?)
                .map_err(|_| Error::parse(0, "object stream object number is invalid"))?;
            let object_offset = usize::try_from(tokenizer.next_integer()?)
                .map_err(|_| Error::parse(0, "object stream object offset is invalid"))?;
            // qpdf stores the header in std::map<int, int>, so an object
            // number repeated in the header keeps the last offset and the
            // remaining members are visited in ascending object-number order
            // (`QPDF.cc:1788-1790`).
            members.insert(object_number, object_offset);
        }

        for (object_number, object_offset) in members {
            let object_ref = ObjectRef::new(object_number, 0);
            if !matches!(
                self.xref_entry(object_ref),
                Some(XrefEntry::Compressed { stream, .. }) if stream == stream_number
            ) {
                if self.xref_entry(object_ref).is_none() {
                    // qpdf's `m->xref_table[og]` inserts a default type-0
                    // entry even when the object-stream header mentions an
                    // object absent from the effective xref table.
                    self.insert_default_xref_entry(object_ref);
                }
                // qpdf skips an ObjStm member that has since been overridden
                // by another effective xref entry (`QPDF.cc:1792-1795`).
                continue;
            }

            // A caller replacement is already the effective value for this
            // member. Keep it authoritative when another unresolved member
            // causes the source stream to be decoded; otherwise this pass
            // would overwrite the live cache slot with the stale source
            // bytes before the writer can consume the ObjectHandle. This is
            // the same cache authority that makes qpdf's resolved object
            // handles independent of the source container once materialized.
            let member_handle = self.get_object_handle(object_ref);
            if member_handle.is_resolved() {
                continue;
            }

            let member_start = first
                .checked_add(object_offset)
                .ok_or_else(|| Error::parse(0, "object stream member offset overflow"))?;
            // qpdf seeks the member input even when the declared offset is
            // beyond the decoded ObjStm payload. Let the live parser emit its
            // normal EOF diagnostic instead of manufacturing a range error
            // (`QPDF.cc:1825-1828`).
            let member_data = decoded_stream_data.get(member_start..).unwrap_or_default();
            // `InputSource::getLastOffset()` remains at the decoded EOF after
            // that seek/read attempt; use the same offset for the warning.
            let diagnostic_start = member_start.min(decoded_stream_data.len());
            let member_start = i64::try_from(diagnostic_start)
                .map_err(|_| Error::parse(0, "object stream member offset is too large"))?;
            let description_template =
                self.object_stream_description_template(stream_number, object_ref);
            self.set_last_object_description(object_ref, None);
            let mut handles = ChildHandles {
                resolver: self,
                description_template: description_template.clone(),
            };
            let (value, parsed_offset, diagnostics) =
                match parse_qpdf_direct_object_handle_with_diagnostics(
                    member_data,
                    member_start,
                    Some(member_start),
                    &mut handles,
                ) {
                    Ok(parsed) => parsed,
                    Err(Error::Parse { offset, message }) => {
                        return Err(ObjectStreamResolutionError::MemberWarning {
                            stream_number,
                            object_ref,
                            offset: (member_start as u64).saturating_add(offset as u64),
                            message,
                        });
                    }
                    // cov:ignore-start: SliceLiveInput's parser can only surface Parse or success here; preserve any future non-Parse failure defensively.
                    Err(error) => return Err(ObjectStreamResolutionError::Operation(error)),
                    // cov:ignore-end
                };
            let malformed = !diagnostics.is_empty() && matches!(&value, ObjectValue::Null);
            for diagnostic in diagnostics {
                let offset =
                    (member_start as u64).saturating_add(diagnostic.relative_offset as u64);
                self.push_object_stream_warning(
                    stream_number,
                    object_ref,
                    offset,
                    diagnostic.message,
                )
                .map_err(ObjectStreamResolutionError::WarningDelivery)?;
            }

            if malformed {
                member_handle.set_resolved(ObjectValue::Null);
            } else {
                member_handle.set_resolved(value);
                member_handle.set_parsed_offset_if_unset(parsed_offset);
                member_handle.set_end_offsets(stream_end_before_space, stream_end_after_space);
                if parsed_offset >= 0 && !member_handle.is_null() {
                    member_handle.set_description(
                        self.object_stream_description_template(stream_number, object_ref),
                        parsed_offset,
                    );
                }
            }
        }
        Ok(())
    }

    fn object_stream_integer(stream_dict: &ObjectHandle, key: &[u8], label: &str) -> Result<usize> {
        let value = stream_dict.try_get_key(key)?.try_as_integer()?;
        let Some(value) = value else {
            return Err(Error::parse(
                0,
                format!("object stream {label} is not an integer"),
            ));
        };
        usize::try_from(value)
            .map_err(|_| Error::parse(0, format!("object stream {label} is invalid")))
    }

    fn resolve_object_stream_or_null(
        &self,
        stream_number: u32,
        object_ref: ObjectRef,
        handle: &ObjectHandle,
    ) -> Result<()> {
        match self.resolve_object_stream_with_failure_kind(stream_number) {
            Ok(()) => {
                if !handle.is_resolved() {
                    handle.set_resolved(ObjectValue::Null);
                }
            }
            Err(ObjectStreamResolutionError::WarningDelivery(error)) => return Err(error),
            Err(ObjectStreamResolutionError::MemberWarning {
                stream_number,
                object_ref,
                offset,
                message,
            }) => {
                self.push_object_stream_warning(stream_number, object_ref, offset, message)?;
                if !handle.is_resolved() {
                    handle.set_resolved(ObjectValue::Null);
                }
            }
            Err(ObjectStreamResolutionError::Operation(error))
                if Self::is_qpdf_caught_resolution_error(&error) =>
            {
                // `QPDF::resolve` catches the QPDFExc raised by
                // `resolveObjectsInStream`, warns, and lets its common tail
                // cache the requested object as null (`QPDF.cc:1724-1750`).
                self.push_caught_resolution_warning(error, object_ref)?;
                if !handle.is_resolved() {
                    handle.set_resolved(ObjectValue::Null);
                }
            }
            Err(ObjectStreamResolutionError::Operation(error)) => return Err(error),
        }
        Ok(())
    }

    fn is_qpdf_caught_resolution_error(error: &Error) -> bool {
        match error {
            Error::Parse { .. } | Error::Unsupported(_) => true,
            Error::Internal(message) => message == CLOSED_INPUT_SOURCE_ERROR,
            _ => false,
        }
    }

    /// Preserve the source position carried by qpdf's `QPDFExc` when its
    /// resolve catch turns a structural failure into a warning. qpdf's
    /// `QPDF::warn` receives the exception unchanged (`QPDF.cc:1737-1741`),
    /// so the diagnostic keeps the exception offset rather than rendering it
    /// into the message text. Offsetless failures retain the existing text
    /// path.
    fn push_caught_resolution_warning(&self, error: Error, object_ref: ObjectRef) -> Result<()> {
        match error {
            Error::Parse { offset, message } => {
                let object = self.core.borrow().last_object_description.clone();
                let message = if object.is_empty() {
                    message
                } else if offset > 0 {
                    format!("({object}, offset {offset}): {message}")
                } else {
                    format!("({object}): {message}")
                };
                self.push_warning_at(u64::try_from(offset).unwrap_or(u64::MAX), message)
            }
            Error::Internal(message) if message == CLOSED_INPUT_SOURCE_ERROR => {
                let message = format!(
                    "object {}/{}: error reading object: {message}",
                    object_ref.number, object_ref.generation
                );
                self.push_warning_with_offset(
                    None,
                    Some(CLOSED_INPUT_SOURCE_NAME.as_bytes()),
                    message,
                )
            }
            error => self.push_warning(error.to_string()),
        }
    }

    /// Apply qpdf's `updateCache` result to the already-vended canonical slot.
    /// The parsed header's object generation is authoritative here: with
    /// recovery disabled qpdf warns on an xref/header mismatch, caches the
    /// object under the generation it actually read, and lets the originally
    /// requested slot fall through to the common null fallback.
    fn cache_parsed_object(&self, parsed: ParsedObjectAtOffset) {
        let ParsedObjectAtOffset {
            object_ref,
            value,
            malformed,
            parsed_offset,
            description,
            end_before_space,
            end_after_space,
            trailing_start: _,
        } = parsed;
        let handle = self.get_object_handle(object_ref);
        if malformed && matches!(&value, ObjectValue::Null) {
            // The qpdf parser recovers a damaged scalar/container close as a
            // visible null. Source damage remains observable through the
            // diagnostics; the value itself follows qpdf's null fallback.
            handle.set_resolved(ObjectValue::Null);
            return;
        }
        handle.set_resolved(value);
        handle.set_parsed_offset_if_unset(parsed_offset);
        handle.set_end_offsets(end_before_space, end_after_space);
        if !description.is_empty() {
            handle.set_description(description, parsed_offset);
        }
    }

    /// The refs declared by the effective source cross-reference table,
    /// collected under a single short borrow.
    pub(crate) fn xref_refs(&self) -> Vec<ObjectRef> {
        self.core
            .borrow()
            .source_xref_entries
            .keys()
            .copied()
            .collect()
    }

    /// Read `[offset, next)` — or `[offset, EOF)` when `next` is `None` — in
    /// qpdf-logical coordinates, into an owned buffer.
    ///
    /// It lives on [`ResolverHandle`] rather than on [`ResolverCore`] on
    /// purpose. `ResolverCore`'s method surface is meant to be checkable
    /// against qpdf line by line — it is `m->file`'s operations and nothing
    /// else. This bounded owned-window read remains a helper for the legacy
    /// `Pdf` read paths; hosting it one level out keeps it built *on* the
    /// primitives instead of adding it to the qpdf-shaped resolver surface.
    ///
    /// Do not build on its shape. qpdf streams from `m->file` and brackets the
    /// one re-entrant seam by saving and restoring the position
    /// (`QPDF::readStream`, `libqpdf/QPDF.cc:1360-1398`). The design of record
    /// names generalising this owned-window shape as a wrong turn that would
    /// entrench a divergence, so `readObjectAtOffset`/`readStream` port that
    /// save/restore seam rather than reusing this.
    // Separable at function granularity, so CLAUDE.md's marker policy calls
    // for #[deprecated] here rather than a block comment marker: qpdf reads
    // the live `m->file` source and has no bounded owned-window helper
    // (`InputSource.hh:71-74`; `QPDF.cc:1360-1398`). Callers get
    // #[allow(deprecated)] locally rather than spreading unchecked.
    #[deprecated(
        note = "no qpdf counterpart; qpdf reads m->file live and has no bounded owned-window helper -- do not add new callers"
    )]
    pub(crate) fn read_window(&self, offset: u64, next: Option<u64>) -> Result<Vec<u8>> {
        self.seek(offset)?;
        #[allow(deprecated)]
        self.read_to_owned(next.map(|next| next.saturating_sub(offset)))
    }

    /// Collect `limit` bytes — or everything left when `limit` is `None` —
    /// from the current position into an owned buffer.
    ///
    /// Grows as it goes rather than pre-allocating `limit`. The legacy
    /// [`Self::read_window`] caller's bound comes from the *next*
    /// cross-reference offset, which a corrupt table can make arbitrarily
    /// large on a small file. `std::io::Read::take(n).read_to_end(..)`, which
    /// this replaces, had the same property.
    #[deprecated(
        note = "no qpdf counterpart; only used by the equally legacy read_window -- do not add new callers"
    )]
    fn read_to_owned(&self, limit: Option<u64>) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        loop {
            let remaining =
                limit.map_or(u64::MAX, |limit| limit.saturating_sub(bytes.len() as u64));
            if remaining == 0 {
                return Ok(bytes);
            }
            let filled = bytes.len();
            let want = remaining.min(BULK_READ_CHUNK as u64) as usize;
            bytes.resize(filled + want, 0);
            // `ResolverCore::read` already loops to EOF, so a short answer
            // here means the input is exhausted.
            let read = self.read(&mut bytes[filled..])?;
            bytes.truncate(filled + read);
            if read < want {
                return Ok(bytes);
            }
        }
    }

    /// qpdf `QPDF::pipeStreamData` (`libqpdf/QPDF.cc:2477-2538`), the only
    /// path by which a stream's original bytes reach a consumer: `QPDF_Stream`
    /// keeps no copy of them, so its `pipeStreamData` reads them here from
    /// `parsed_offset` and `length` (`libqpdf/QPDF_Stream.cc:608-620`).
    ///
    /// The inner bool reports whether the bytes were delivered, mirroring
    /// qpdf's `bool`: a damaged source inside `pipeStreamData`'s `try` is a
    /// warning and `false`. Errors preparing `decryptStream` are outside that
    /// catch boundary (`QPDF.cc:2487-2494`) and therefore remain `Err`.
    ///
    /// qpdf allocates `length` and reads it in one operation (`:2497-2501`).
    /// That is deliberate here even though parsing deliberately does *not*
    /// pre-allocate a declared `/Length`: by pipe time the offset and length
    /// have already been validated by the framing scan.
    /// A recovery scan's line ending is part of that length; qpdf does not
    /// subtract it before the decryption stage or for foreign stream data.
    ///
    /// qpdf prepends a decryption stage before it touches the input source
    /// (`:2490-2492`, `QPDF::decryptStream`). The legacy `/V < 4` form has no
    /// crypt-filter lookup: it always uses RC4 (`QPDF_encryption.cc:1062-1064`,
    /// `:1146-1151`). Later branches below extend that same pipe-time seam for
    /// `/V >= 4` crypt filters.
    //
    // The parameter list is qpdf's (`QPDF.cc:2542-2550` passes seven beyond
    // the receiver); bundling them would be a shape this port does not have a
    // counterpart for. The production stream consumers call this resolver
    // seam directly so source reads and pipe-time decryption stay centralized.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pipe_stream_data(
        &self,
        object_ref: ObjectRef,
        offset: i64,
        length: usize,
        stream_dict: &ObjectHandle,
        pipeline: &mut dyn Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        let input = self.stream_input();
        let encryption_parameters = self.encryption_parameters();
        pipe_stream_data_from_input(
            &input,
            &encryption_parameters,
            self,
            None,
            object_ref,
            offset,
            length,
            stream_dict,
            pipeline,
            suppress_warnings,
            will_retry,
        )
    }

    /// Test-only mutable access to the input source itself, for fixtures that
    /// arm a reader to start failing partway through a resolution.
    ///
    /// `f` must not resolve anything: it runs while the core's borrow is held,
    /// so a nested resolve would double-borrow. Every in-tree caller only
    /// flips a field on a fault-injecting cursor.
    #[cfg(test)]
    pub(crate) fn with_reader_mut<T>(&self, f: impl FnOnce(&mut R) -> T) -> T {
        let input = self.core.borrow().input.borrow().clone();
        let reader = input
            .reader
            .as_ref()
            .expect("with_reader_mut requires an active input source");
        let mut guard = reader.borrow_mut();
        f(&mut *guard)
    }

    /// Test-only: install a cross-reference entry the source did not declare,
    /// for fixtures that drive resolution of a hand-built object.
    #[cfg(test)]
    pub(crate) fn insert_xref_entry(&self, object_ref: ObjectRef, entry: XrefEntry) {
        let mut core = self.core.borrow_mut();
        core.source_xref_entries.insert(object_ref, entry);
    }

    // ---- the input source, streamed ----
    //
    // Each of the four wrappers below takes its borrow and drops it inside
    // its own expression, so the input *position* — not a borrow — is what
    // survives between them. That is the whole point: qpdf's re-entrancy seam
    // is a save/restore of `m->file`'s position (`QPDF::readStream`,
    // `libqpdf/QPDF.cc:1367-1384`), and a seam can only be ported onto a
    // position that a nested resolution can actually disturb.

    /// See [`ResolverCore::seek`].
    fn seek(&self, offset: u64) -> Result<()> {
        self.core.borrow_mut().seek(offset)
    }

    /// See [`ResolverCore::seek_relative`].
    fn seek_relative(&self, delta: u64) -> Result<()> {
        self.core.borrow_mut().seek_relative(delta)
    }

    /// See [`ResolverCore::tell`].
    fn tell(&self) -> Result<u64> {
        self.core.borrow_mut().tell()
    }

    /// See [`ResolverCore::read`].
    fn read(&self, buf: &mut [u8]) -> Result<usize> {
        self.core.borrow_mut().read(buf)
    }

    /// Capture qpdf's `end_before_space`/`end_after_space` pair after an
    /// indirect object's `endobj` framing has been consumed. The first value
    /// is the position immediately after the terminator; the second is the
    /// position of the first following non-whitespace byte. qpdf throws
    /// `EOF after endobj` if the source ends while looking for that byte
    /// (`QPDF.cc:1651-1663`), so this is deliberately part of the read/cache
    /// operation rather than best-effort metadata.
    fn object_end_offsets(&self) -> Result<(i64, i64)> {
        let end_before_space = self.tell()?;
        let end_before_space = i64::try_from(end_before_space)
            .map_err(|_| Error::parse(usize::MAX, "object end offset is out of range"))?;

        let end_after_space = loop {
            let mut byte = [0u8; 1];
            if self.read(&mut byte)? == 0 {
                let offset = usize::try_from(self.tell()?).unwrap_or(usize::MAX);
                return Err(Error::parse(offset, "EOF after endobj"));
            }
            if !byte[0].is_ascii_whitespace() {
                let position = self.tell()?.checked_sub(1).ok_or_else(|| {
                    Error::parse(0, "object end offset is before the input start")
                })?;
                self.seek(position)?;
                break i64::try_from(position)
                    .map_err(|_| Error::parse(usize::MAX, "object end offset is out of range"))?;
            }
        };

        Ok((end_before_space, end_after_space))
    }

    /// Locate qpdf's linearization candidate using the same two-stage input
    /// shape as `QPDF::isLinearized` (`libqpdf/QPDF_linearization.cc:95-125`):
    /// scan only the first 1024 bytes for a digit, then read real source
    /// tokens from that position until the first `integer integer obj <<`
    /// sequence is found.
    pub(crate) fn linearization_candidate(&self) -> Result<Option<i64>> {
        const PREFIX_LENGTH: usize = 1024;

        self.seek(0)?;
        let mut prefix = [0u8; PREFIX_LENGTH];
        let read = self.read(&mut prefix)?;
        let mut scan = 0;

        while scan < read {
            while scan < read && !prefix[scan].is_ascii_digit() {
                scan += 1;
            }
            if scan == read {
                break;
            }

            let candidate_start = scan;
            while scan < read && prefix[scan].is_ascii_digit() {
                scan += 1;
            }

            self.seek(candidate_start as u64)?;
            let tokens = {
                let mut input = self.live_input();
                let mut tokenizer = LiveTokenSource::new(&mut input);
                let result = (|| -> Result<Option<i64>> {
                    let first = tokenizer.next_token()?;
                    if !first.is_integer() {
                        return Ok(None);
                    }

                    let second = tokenizer.next_token()?;
                    if !second.is_integer() {
                        return Ok(None);
                    }

                    let object = tokenizer.next_token()?;
                    if !object.is_word_value(b"obj") {
                        return Ok(None);
                    }

                    let dictionary = tokenizer.next_token()?;
                    if dictionary.token_type != TokenType::DictOpen {
                        return Ok(None);
                    }

                    Ok(std::str::from_utf8(&first.value)
                        .ok()
                        .and_then(|value| value.parse::<i64>().ok()))
                })();
                drop(tokenizer);
                input.finish()?;
                result
            };

            let Some(object_number) = tokens? else {
                continue;
            };
            return Ok(Some(object_number));
        }

        Ok(None)
    }

    /// Return the qpdf-logical source length used by `/L` in
    /// `QPDF::isLinearized` (`QPDF_linearization.cc:141-150`). Preserve the
    /// caller's live position because the Rust resolver may be re-entered by
    /// later lazy resolution.
    pub(crate) fn source_length(&self) -> Result<u64> {
        self.core.borrow_mut().source_length()
    }

    /// Return qpdf's `InputSource::getLastOffset()` value for the most recent
    /// source read (`QPDF.cc:2624-2628`). Linearization diagnostics use this
    /// physical read offset when wrapping a damaged parameter error.
    pub(crate) fn last_offset(&self) -> u64 {
        self.core.borrow().input.borrow().last_offset()
    }

    /// Seed the shared input source's qpdf-style last-read position after the
    /// initial xref/trailer snapshot has been loaded outside the resolver.
    pub(crate) fn set_last_offset(&self, offset: u64) {
        self.core.borrow().input.borrow().last_offset.set(offset);
    }

    /// Return the logical source bytes while restoring the resolver's current
    /// input position. This is intentionally a reader-owned seam rather than a
    /// second file open: qpdf's `QPDF::checkLinearization` reads the same
    /// `m->file` that resolution and stream providers use.
    pub(crate) fn source_bytes(&self) -> Result<Vec<u8>> {
        self.core.borrow().input.borrow().read_logical_bytes()
    }

    /// Append the next chunk of input to `bytes`, reporting whether anything
    /// was left to append.
    ///
    /// The position advances by exactly what was appended, so `bytes` always
    /// mirrors `[scan start, current position)`.
    ///
    /// This belongs to the bounded framing-token consumer below. File-object
    /// body parsing and stream recovery use the live InputSource adapter and
    /// do not accumulate the source prefix.
    fn refill(&self, bytes: &mut Vec<u8>) -> Result<bool> {
        let filled = bytes.len();
        let want = filled.max(INPUT_CHUNK);
        bytes.resize(filled + want, 0);
        let read = self.read(&mut bytes[filled..])?;
        bytes.truncate(filled + read);
        Ok(read != 0)
    }

    /// Pull the input forward from the current position until `attempt`
    /// reports a complete result, then leave the position exactly where that
    /// result stopped consuming.
    ///
    /// This is only a bounded framing-token helper. `QPDFTokenizer` and
    /// `QPDFParser` consume `m->file` a character at a time
    /// (`QPDFTokenizer::presentCharacter`, and `unreadCh` to give back the
    /// one character of overshoot — `libqpdf/QPDF.cc:1656` uses
    /// `seek(-1, SEEK_CUR)` for the same purpose), so qpdf never has to know
    /// in advance how far an object reaches. The file-object body path
    /// advances through `LiveTokenSource` directly; only its stream framing
    /// token uses this bounded pull adapter.
    ///
    /// Its position remains live and its pull leaves the source after the
    /// consumed token. [`Self::read_window`]'s bounded-window shape is not
    /// this helper: it takes its extent from the next xref entry and leaves no
    /// position behind, so it remains a legacy tenant rather than a
    /// `ResolverCore` method.
    ///
    /// `attempt` reports `(value, end)` where `end` is how many bytes it
    /// consumed. A result that consumed the *whole* buffer is treated as
    /// possibly truncated and re-attempted against more input, because a
    /// token cut short by the buffer's end can parse as a shorter valid one
    /// (`12345` cut to `1234`, `endobj` cut to `endob`). Only when the buffer
    /// has reached EOF is such a result accepted.
    fn scan_forward<T>(&self, mut attempt: impl FnMut(&[u8]) -> Result<(T, usize)>) -> Result<T> {
        let start = self.tell()?;
        let mut bytes = Vec::new();
        let mut more = true;
        loop {
            let outcome = attempt(&bytes);
            let complete = matches!(&outcome, Ok((_, end)) if *end < bytes.len());
            if complete || !more {
                let (value, end) = outcome?;
                self.seek(start.saturating_add(end as u64))?;
                return Ok(value);
            }
            more = self.refill(&mut bytes)?;
        }
    }

    /// Read one token from the current position, leaving the input just past
    /// it.
    ///
    /// qpdf `QPDF::readToken(m->file)` — `readObject` calls it to decide
    /// between `stream` and `endobj` (`libqpdf/QPDF.cc:1347-1354`) and
    /// `readStream` calls it to check for `endstream` (`:1386`).
    ///
    /// **EOF is a token here, not an error**, because it is one in qpdf: the
    /// document's shared tokenizer has `m->tokenizer.allowEOF()` applied at
    /// construction (`libqpdf/QPDF.cc:208`), so `readToken` at end of input
    /// returns `tt_eof` rather than the `tt_bad` "unexpected EOF" the
    /// un-permitted tokenizer would produce (`libqpdf/QPDFTokenizer.cc:930-939`).
    /// That is what makes both of the checks after it report the framing
    /// keyword they wanted — `expected endstream` at `:1386-1389`, `expected
    /// endobj` at `:1352-1355` — instead of complaining about the EOF, and it
    /// is the difference between a `/Length` that runs off the end of the
    /// input being diagnosed as qpdf diagnoses it or not.
    ///
    /// **Both halves are pinned, because each is a different outcome.** Delete
    /// the `allow_eof` below and every fixture whose input ends where a
    /// framing keyword was expected reddens. The `endstream` ones swap the
    /// missing keyword for `unexpected EOF`, a wrong message;
    /// `a_stream_ending_the_input_after_endstream_warns_and_still_resolves`,
    /// the `endobj` half, stops *resolving at all* — there the difference is
    /// not the message but whether the object comes back.
    ///
    /// `allow_eof` is also used by the live file-object tokenizer, whose
    /// trailing token is the direct equivalent of this stream-framing read.
    ///
    /// qpdf passes `allow_bad = true` (`QPDF::readToken`, `:1536-1539`), so a
    /// *malformed* token is returned as `tt_bad` and likewise reported as the
    /// missing keyword. Keep that policy here so the canonical stream path
    /// can enter its recovery arm. qpdf's `QPDFTokenizer::nextToken` advances
    /// its last offset while it skips whitespace and comments
    /// (`libqpdf/QPDFTokenizer.cc:926-961`), so return the token start after
    /// that skipped prefix rather than the attempted-read position.
    fn read_token_from_input(&self) -> Result<(Token, u64)> {
        let start = self.tell()?;
        let token = self.scan_forward(|bytes| {
            let mut tokenizer = Tokenizer::new(bytes);
            tokenizer.allow_eof();
            let token = tokenizer.read_token(true, 0)?;
            Ok((token, tokenizer.position()))
        })?;
        let token_start = if token.token_type == TokenType::Eof {
            // FileInputSource::read records the physical source end after a
            // zero-byte read, even when the preceding seek requested a
            // position beyond EOF (libqpdf/FileInputSource.cc:115-132).
            let eof = self.last_offset();
            self.seek(eof)?;
            eof
        } else {
            start.saturating_add(token.start as u64)
        };
        Ok((token, token_start))
    }

    /// Read one byte from the current position, or `None` at EOF.
    fn read_byte(&self) -> Result<Option<u8>> {
        let mut byte = [0u8; 1];
        Ok((self.read(&mut byte)? == 1).then_some(byte[0]))
    }

    /// One live `InputSource` view over this resolver's document input.
    ///
    /// This is intentionally a tiny adapter rather than another owner: qpdf
    /// gives `QPDFParser` the same `m->file` that stream length resolution can
    /// move, so the resolver remains the sole owner of position and its
    /// canonical object cache.
    fn live_input(&self) -> ResolverLiveInput<'_, R> {
        ResolverLiveInput {
            resolver: self,
            buffer: [0; LIVE_INPUT_BUFFER],
            buffer_start: 0,
            buffer_len: 0,
            buffer_index: 0,
        }
    }

    /// qpdf `QPDF::readObjectAtOffset` (`libqpdf/QPDF.cc:1591-1637`),
    /// `QPDF::reconstruct_xref` (`libqpdf/QPDF.cc:516-530`), and `QPDF::resolve` fallback (`:1745-1748`).
    ///
    /// This is the type-1 read/retry operation used by `QPDF::resolve`. If
    /// reconstruction changes the target to a type-2 entry, the caller turns
    /// the signal into the object-stream path rather than reading the old
    /// offset again.
    ///
    /// Returns the parsed value, source description, and qpdf's cached source
    /// extent metadata in qpdf-logical coordinates.
    ///
    /// When `attempt_recovery` is enabled and the indirect-object header at
    /// `offset` is damaged, recovery reconstructs the cross-reference table
    /// via line scan, retries at the newly found offset, or warns and resolves
    /// to null if absent post-rebuild. Body, stream-framing, and cache-extent
    /// failures stay on the resolve catch/null path and do not trigger xref
    /// reconstruction.
    ///
    /// **Positions reported from here are the file's.** The live parser reads
    /// the resolver-owned input directly, as qpdf does; `QPDFParser::warn`
    /// takes its position from `input->getLastOffset()`
    /// (`libqpdf/QPDFParser.cc:516-519`).
    /// `a_recovered_malformed_body_reports_its_warning_at_the_file_offset`
    /// pins the absolute diagnostic. The mismatch error is likewise anchored
    /// directly at `offset`.
    #[cfg(test)]
    fn read_object_at_offset(
        &self,
        offset: u64,
        expected: ObjectRef,
    ) -> Result<(ObjectValue, i64)> {
        let parsed = self
            .read_object_at_offset_with_description(offset, expected, false, false, None)
            .map_err(ReadObjectAtOffsetError::into_error)?;
        Ok((parsed.value, parsed.parsed_offset))
    }

    /// Read an object at a physical offset and return the source position that
    /// qpdf's `readHintStream` would use for a subsequent damage warning.
    ///
    /// qpdf only updates an unresolved object's cached end offsets in
    /// `readObjectAtOffset`. If the object was already cached, the input's last
    /// tokenizer offset remains the trailing-token start. If it was unresolved,
    /// the direct whitespace scan advances the last offset to
    /// `end_after_space`. Keep both cases as operation metadata rather than
    /// storing a hint-specific field on every `ObjectHandle`.
    /// Read an object at a physical offset using qpdf's caller-provided
    /// description. `QPDF::readHintStream` supplies `linearization hint
    /// stream` to `readObjectAtOffset`, so stream framing and recovery warnings
    /// must retain that description instead of falling back to `object N G`
    /// (`libqpdf/QPDF_linearization.cc:241-245` and
    /// `libqpdf/QPDF.cc:1297-1339`).
    pub(crate) fn resolve_at_offset_with_description(
        &self,
        offset: u64,
        expected: ObjectRef,
        description: impl AsRef<[u8]>,
    ) -> Result<(ObjectHandle, Option<u64>)> {
        self.resolve_at_offset_with_optional_description(
            offset,
            expected,
            Some(description.as_ref().to_vec()),
        )
    }

    /// Read an object at a physical offset, optionally retaining qpdf's
    /// caller-provided description in stream diagnostics. The existing
    /// no-description offset route and the linearization hint-stream route
    /// share this canonical primitive.
    pub(crate) fn resolve_at_offset_with_optional_description(
        &self,
        offset: u64,
        expected: ObjectRef,
        description: Option<Vec<u8>>,
    ) -> Result<(ObjectHandle, Option<u64>)> {
        let was_resolved = self.get_object_handle(expected).is_resolved();
        let parsed = self
            .read_object_at_offset_with_description(offset, expected, true, false, description)
            .map_err(ReadObjectAtOffsetError::into_error)?;
        let damage_offset = if was_resolved {
            parsed.trailing_start
        } else {
            u64::try_from(parsed.end_after_space).ok()
        };
        let object_ref = parsed.object_ref;
        self.cache_parsed_object(parsed);
        Ok((self.get_object_handle(object_ref), damage_offset))
    }

    fn read_object_at_offset_with_description(
        &self,
        offset: u64,
        expected: ObjectRef,
        capture_end_offsets: bool,
        try_recovery: bool,
        read_description: Option<Vec<u8>>,
    ) -> std::result::Result<ParsedObjectAtOffset, ReadObjectAtOffsetError> {
        if expected.number != 0 {
            self.set_last_object_description(expected, read_description.as_deref());
        }
        self.seek(offset).map_err(ReadObjectAtOffsetError::Body)?;
        let (found, parsed, trailing, trailing_start, object_header_offset) = {
            let mut input = self.live_input();
            let mut tokenizer = LiveTokenSource::new(&mut input);
            let number_token = tokenizer
                .next_token()
                .map_err(ReadObjectAtOffsetError::Header)?;
            let generation_token = tokenizer
                .next_token()
                .map_err(ReadObjectAtOffsetError::Header)?;
            let obj = tokenizer
                .next_token()
                .map_err(ReadObjectAtOffsetError::Header)?;
            let (number, generation) = match (
                read_live_header_integer(number_token),
                read_live_header_integer(generation_token),
                obj.is_word_value(b"obj"),
            ) {
                (Ok(number), Ok(generation), true) => (number, generation),
                _ => {
                    // qpdf reads all three header tokens before reporting
                    // the single damagedPDF("expected n n obj") error at
                    // the object's xref offset (QPDF.cc:1589-1594).
                    return Err(ReadObjectAtOffsetError::Header(Error::parse(
                        offset as usize,
                        "expected n n obj",
                    )));
                }
            };
            // qpdf consumes the object header before entering QPDF::readObject,
            // which captures `m->file->tell()` at this exact point
            // (`libqpdf/QPDF.cc:1331-1335`). Keep this separate from `offset`,
            // the xref/object-start position used by header diagnostics.
            drop(tokenizer);
            let object_header_offset = input.tell().map_err(ReadObjectAtOffsetError::Header)?;

            let found = u32::try_from(number)
                .ok()
                .zip(u16::try_from(generation).ok())
                .map(|(number, generation)| ObjectRef::new(number, generation));
            if try_recovery && found != Some(expected) {
                return Err(ReadObjectAtOffsetError::Header(Error::parse(
                    offset as usize,
                    format!("expected {} {} obj", expected.number, expected.generation),
                )));
            }
            let mut minter = ChildHandles {
                resolver: self,
                description_template: match read_description.as_deref() {
                    Some(description) => self.parser_description_template_for_read(
                        found.unwrap_or(expected),
                        description,
                    ),
                    None => self.parser_description_template(found.unwrap_or(expected)),
                },
            };
            let encryption_parameters = self.encryption_parameters();
            // qpdf reads and caches the `/Encrypt` dictionary before it marks
            // the document encrypted (`QPDF::initializeEncryption`,
            // `libqpdf/QPDF_encryption.cc:753-908`). Its later object parser
            // therefore never decrypts `/O`, `/U`, `/OE`, or `/UE` when that
            // dictionary is observed through the canonical object cache. The
            // resolver's encryption state is shared with the parser, so make
            // the same cache-timing rule explicit here instead of decrypting
            // the dictionary a second time after authentication.
            let (has_encryption, encrypt_ref) = encryption_parameters
                .borrow()
                .as_ref()
                .map(|encryption| (true, encryption.encrypt_ref))
                .unwrap_or((false, None));
            let mut decrypter = if has_encryption && encrypt_ref != found {
                found.map(|object_ref| ResolverStringDecrypter {
                    object_ref,
                    encryption_parameters,
                    resolver: self,
                })
            } else {
                None
            };
            let parsed = parse_live_file_object_with_decrypter(
                &mut input,
                &mut minter,
                decrypter
                    .as_mut()
                    .map(|decrypter| decrypter as &mut dyn StringDecrypter),
            )
            .map_err(ReadObjectAtOffsetError::Body)?;
            let trailing = if parsed.empty.is_none() {
                let mut trailing_tokens = LiveTokenSource::new(&mut input);
                let trailing = trailing_tokens
                    .next_token()
                    .map_err(ReadObjectAtOffsetError::Body)?;
                drop(trailing_tokens);
                Some(trailing)
            } else {
                None
            };
            let trailing_start = trailing
                .as_ref()
                .and_then(|token| u64::try_from(token.start).ok());
            input.finish().map_err(ReadObjectAtOffsetError::Body)?;
            (
                found,
                parsed,
                trailing,
                trailing_start,
                object_header_offset,
            )
        };

        if found.is_some_and(|object_ref| object_ref.number == 0) {
            return Err(ReadObjectAtOffsetError::Header(Error::parse(
                offset as usize,
                "object with ID 0",
            )));
        }

        if found.is_none() {
            return Err(ReadObjectAtOffsetError::Header(Error::parse(
                offset as usize,
                "object reference is out of range",
            )));
        }
        let found = found.expect("the range check above establishes the header object reference");
        self.set_last_object_description(found, read_description.as_deref());
        if found != expected {
            self.push_warning_at(
                offset,
                format!(
                    "(object {} {}, offset {offset}): expected {} {} obj",
                    expected.number, expected.generation, expected.number, expected.generation
                ),
            )
            .map_err(ReadObjectAtOffsetError::Body)?;
        }

        let malformed = !parsed.diagnostics.is_empty();
        for warning in parsed.diagnostics {
            let warning_offset = warning.relative_offset as u64;
            self.push_warning_at(
                warning_offset,
                format!(
                    "(object {} {}, offset {warning_offset}): {}",
                    found.number, found.generation, warning.message
                ),
            )
            .map_err(ReadObjectAtOffsetError::Body)?;
        }

        if let Some(empty_offset) = parsed.empty {
            self.push_warning_at(
                empty_offset,
                format!(
                    "(object {} {}, offset {empty_offset}): empty object treated as null",
                    found.number, found.generation
                ),
            )
            .map_err(ReadObjectAtOffsetError::Body)?;
            let (value, parsed_offset) = parsed
                .value
                .into_direct_value()
                .expect("live file parser's recovered empty object is always a direct null");
            debug_assert_eq!(parsed_offset, parsed.parsed_offset);
            let (end_before_space, end_after_space) = if capture_end_offsets {
                self.object_end_offsets()
                    .map_err(ReadObjectAtOffsetError::Body)?
            } else {
                (NO_PARSED_OFFSET, NO_PARSED_OFFSET)
            };
            return Ok(ParsedObjectAtOffset {
                object_ref: found,
                value,
                malformed: false,
                parsed_offset,
                description: Vec::new(),
                end_before_space,
                end_after_space,
                trailing_start,
            });
        }

        // Keep the parser's raw template through the canonical transfer.
        // Rendering here would turn an escaped caller literal such as
        // `input-$$PO.pdf` back into `$PO`, and a later set_description on the
        // canonical handle would interpret that literal as a fresh offset
        // placeholder.
        let description = parsed
            .value
            .description_template()
            .unwrap_or_else(|| parsed.value.description());
        let (value, parsed_offset) = parsed.value.into_direct_value().expect(
            "live file parser's top-level bare-reference recovery always returns a direct value",
        );
        debug_assert_eq!(parsed_offset, parsed.parsed_offset);
        let trailing = trailing.expect("non-empty parse must have a framing token");
        if trailing.is_word_value(b"stream") {
            let stream_description = self.stream_description(found);
            let (value, parsed_offset) = self
                .read_stream(
                    value,
                    parsed_offset,
                    description,
                    object_header_offset,
                    found,
                    read_description.as_deref(),
                )
                .map_err(ReadObjectAtOffsetError::Body)?;
            let (end_before_space, end_after_space) = if capture_end_offsets {
                self.object_end_offsets()
                    .map_err(ReadObjectAtOffsetError::Body)?
            } else {
                (NO_PARSED_OFFSET, NO_PARSED_OFFSET)
            };
            Ok(ParsedObjectAtOffset {
                object_ref: found,
                value,
                malformed,
                parsed_offset,
                description: stream_description,
                end_before_space,
                end_after_space,
                trailing_start,
            })
        } else {
            if !trailing.is_word_value(b"endobj") {
                self.push_expected_endobj_warning(
                    found,
                    u64::try_from(trailing.start).unwrap_or(u64::MAX),
                    read_description.as_deref(),
                )
                .map_err(ReadObjectAtOffsetError::Body)?;
            }
            let (end_before_space, end_after_space) = if capture_end_offsets {
                self.object_end_offsets()
                    .map_err(ReadObjectAtOffsetError::Body)?
            } else {
                (NO_PARSED_OFFSET, NO_PARSED_OFFSET)
            };
            Ok(ParsedObjectAtOffset {
                object_ref: found,
                value,
                malformed,
                parsed_offset,
                description,
                end_before_space,
                end_after_space,
                trailing_start,
            })
        }
    }

    /// qpdf `QPDF::readStream` (`libqpdf/QPDF.cc:1360-1399`), entered with the
    /// input positioned immediately after the `stream` keyword.
    ///
    /// **This is the resolver's one re-entrancy seam**, and the reason
    /// [`ResolverHandle::resolve_indirect`] is split into phases at all. No
    /// borrow of [`ResolverCore`] is held anywhere in this method: every input
    /// operation takes and drops its own, so the `/Length` dereference below
    /// is free to re-enter `resolve_indirect` for another reference, seek the
    /// input somewhere else entirely, and return. Seeking back to
    /// `stream_offset` before validating the declared length is what restores
    /// the qpdf input position after that nested resolution.
    ///
    /// # The declared `/Length` is validated before it is believed
    ///
    /// qpdf never allocates here. It seeks past the declared length, reads one
    /// token, and hands `stream_offset` and `length` to
    /// `QPDF_Stream::create` (`:1398`); the payload is read later by
    /// `QPDF::pipeStreamData`, which is where the allocation
    /// (`std::make_unique<char[]>(length)`, `:2497`) and the short-read
    /// diagnosis ("unexpected EOF reading stream data", `:2498-2500`) live.
    ///
    /// flpdf follows qpdf's lazy shape: `ObjectValue::Stream` stores the
    /// source offset and validated length with `stream_data: None`, and
    /// [`ObjectHandle::get_raw_stream_data`] reads the bytes later through
    /// `pipeStreamData`. Consequently this phase never allocates according to
    /// an attacker-controlled `/Length`; short reads are diagnosed at pipe
    /// time, as qpdf does at `QPDF.cc:2495-2504`.
    ///
    /// qpdf catches the `QPDFExc` raised by an unusable length or a missing
    /// `endstream` at `QPDF.cc:1390-1397`, warns it, and calls
    /// `recoverStreamLength`. The same recovery is used here when the
    /// document was opened with `PdfOpenOptions::repair`: the recovered length
    /// includes the bytes between the stream data start and the first token,
    /// exactly as qpdf's `tell() - stream_offset` does.
    ///
    /// The non-parse failures remain outside this catch. In particular,
    /// [`ResolverCore::seek_relative`]'s overflow refusal is not a qpdf
    /// `QPDFExc`, so `QPDF::resolve` warns and leaves the object unresolved
    /// rather than entering stream-length recovery (`QPDF.cc:1739-1748`).
    fn read_stream(
        &self,
        dict: ObjectValue,
        dict_offset: i64,
        dict_description: Vec<u8>,
        object_header_offset: u64,
        object_ref: ObjectRef,
        read_description: Option<&[u8]>,
    ) -> Result<(ObjectValue, i64)> {
        self.validate_stream_line_end()?;

        // qpdf `:1365-1367`: "Must get offset before accessing any additional
        // objects since resolving a previously unresolved indirect object
        // will change file position."
        let stream_offset = self.tell()?;

        let mut recovered = false;
        // `/Length` is the re-entrant call into `resolve_indirect` described
        // above. Check the stack immediately before it, after this method's
        // locals are live, so object-attributed recovery diagnostics cannot
        // make a small caller stack overflow before the resolver hub grows it.
        let mut length = match stacker::maybe_grow(
            super::READER_STACK_RED_ZONE,
            super::READER_STACK_GROWTH_SIZE,
            || Self::stream_length(&dict, object_header_offset),
        ) {
            Ok(length) => length,
            Err(error) if self.is_recoverable_stream_error(&error) => {
                if !self.stream_recovery_enabled() {
                    return Err(error);
                }
                self.warn_stream_failure(
                    &error,
                    object_header_offset,
                    object_ref,
                    read_description,
                )?; // cov:ignore: LLVM maps this covered multiline recovery call terminator to a zero-count continuation region
                recovered = true;
                self.recover_stream_length(stream_offset, object_ref, read_description)?
            }
            Err(error) => return Err(error),
        };

        if !recovered {
            let span = length as u64;

            // qpdf `:1383-1385`: "Seek in two steps to avoid potential integer
            // overflow", `m->file->seek(stream_offset, SEEK_SET)` then
            // `m->file->seek(toO(length), SEEK_CUR)`. Nothing is read and nothing
            // is allocated yet — the declared length has only moved the position.
            self.seek(stream_offset)?;
            self.seek_relative(span)?;

            // qpdf `:1386-1389`. A mismatched framing token is a QPDFExc and
            // enters the same recovery arm as an unusable `/Length`.
            let (framing_token, framing_offset) = self.read_token_from_input()?;
            if !framing_token.is_word_value(b"endstream") {
                let error = Error::parse(framing_offset as usize, "expected endstream");
                if !self.stream_recovery_enabled() {
                    return Err(error);
                }
                self.warn_stream_failure(
                    &error,
                    object_header_offset,
                    object_ref,
                    read_description,
                )?; // cov:ignore: LLVM maps this covered multiline framing-recovery call terminator to a zero-count continuation region
                length = self.recover_stream_length(stream_offset, object_ref, read_description)?;
            }
        }

        // qpdf `:1350-1354`: `readObject` reads one more token after
        // `readStream` returns and warns if it is not `endobj`.
        let (trailing, trailing_offset) = self.read_token_from_input()?;
        if !trailing.is_word_value(b"endobj") {
            self.push_expected_endobj_warning(object_ref, trailing_offset, read_description)?;
        }

        let dict = self.direct_object_handle(dict);
        dict.set_parsed_offset_if_unset(dict_offset);
        if !dict_description.is_empty() {
            // QPDF_Stream::setDescription leaves an already-described stream
            // dictionary untouched (`QPDF_Stream.cc:299-312`). The parser's
            // dictionary description was rendered before the value was
            // unwrapped, so restore that metadata on the rewrapped handle.
            dict.set_description(dict_description, dict_offset);
        }
        Ok((
            ObjectValue::Stream {
                stream_dict: dict,
                stream_data: None,
                stream_length: length,
                stream_provider: None,
                filter_on_write: true,
            },
            i64::try_from(stream_offset).unwrap_or(i64::MAX),
        ))
    }

    /// Whether `error` is one of the parse failures qpdf catches around
    /// `readStream` (`libqpdf/QPDF.cc:1370-1397`). Seek and I/O failures stay
    /// on the caller's normal error path; recovering those would change qpdf's
    /// exception class and its resolve-to-null behavior.
    fn is_recoverable_stream_error(&self, error: &Error) -> bool {
        matches!(
            error,
            Error::Parse { message, .. }
                if message == "stream dictionary lacks /Length key"
                    || message == "/Length key in stream dictionary is not an integer"
                    || message == "/Length key in stream dictionary is out of range"
                    || message == "expected endstream"
        )
    }

    /// Emit the parse failure qpdf catches before entering
    /// `recoverStreamLength`. Length failures are attributed to the indirect
    /// object's header; `expected endstream` is attributed to the attempted
    /// framing read at the stream data position.
    fn warn_stream_failure(
        &self,
        error: &Error,
        object_header_offset: u64,
        object_ref: ObjectRef,
        read_description: Option<&[u8]>,
    ) -> Result<()> {
        let Error::Parse { offset, message } = error else {
            return Ok(());
        };
        let warning_offset = if message == "expected endstream" {
            u64::try_from(*offset).unwrap_or(object_header_offset)
        } else {
            object_header_offset
        };
        self.push_stream_warning_with_description(
            object_ref,
            warning_offset,
            message,
            read_description,
        )
    }

    /// Port qpdf's `recoverStreamLength` (`libqpdf/QPDF.cc:1482-1524`).
    ///
    /// qpdf asks its input source to find the first `endstream` or `endobj`
    /// token after `stream_offset` (`QPDF.cc:1482-1497`). The scan below is
    /// the same `findFirst("end", ...)` shape, but consumes the resolver's
    /// live source instead of copying the complete document for every damaged
    /// stream. Candidate tokens are limited to qpdf's `max_len = 20`, and a
    /// failed candidate advances by one byte so a valid token nested inside a
    /// longer word remains discoverable. For `endobj`, qpdf rewinds to the
    /// token start so the outer `readObject` consumes it; for `endstream`, it
    /// leaves the input after the token.
    fn recover_stream_length(
        &self,
        stream_offset: u64,
        object_ref: ObjectRef,
        read_description: Option<&[u8]>,
    ) -> Result<usize> {
        let warning = self.push_stream_warning_with_description(
            object_ref,
            stream_offset,
            "attempting to recover stream length",
            read_description,
        );
        warning?;

        let terminator = self.find_stream_recovery_terminator(stream_offset)?;
        let (length, next_position, recovered_eol) = match terminator {
            Some((position, next_position)) => {
                let recovered_eol = self.recovered_stream_eol_at(stream_offset, position)?;
                (
                    usize::try_from(position.saturating_sub(stream_offset)).map_err(|_| {
                        // cov:ignore-start: u64-to-usize overflow is unreachable on supported 64-bit CI; retain the defensive error for narrower targets
                        Error::parse(usize::MAX, "recovered stream length is out of range")
                    })?, // cov:ignore-end
                    Some(next_position),
                    recovered_eol,
                )
            }
            None => (0, None, None),
        };
        if let Some(eol) = recovered_eol {
            self.recovered_stream_eols
                .borrow_mut()
                .insert(object_ref, eol);
        } else {
            self.recovered_stream_eols.borrow_mut().remove(&object_ref);
        }
        if let Some(next_position) = next_position {
            self.seek(next_position)?;
        }

        if length == 0 {
            self.push_stream_warning_with_description(
                object_ref,
                stream_offset,
                "unable to recover stream data; treating stream as empty",
                read_description,
            )?;
        } else {
            let message = format!("recovered stream length: {length}");
            self.push_stream_warning_with_description(
                object_ref,
                stream_offset,
                message,
                read_description,
            )?; // cov:ignore: LLVM maps this covered multiline recovery-warning call terminator to a zero-count continuation region
        }
        Ok(length)
    }

    fn recovered_stream_eol_at(
        &self,
        stream_offset: u64,
        data_end: u64,
    ) -> Result<Option<crate::parser::RecoveredStreamEol>> {
        if data_end <= stream_offset {
            return Ok(None);
        }
        let start = data_end.saturating_sub(2).max(stream_offset);
        let width = usize::try_from(data_end - start).unwrap_or(2).min(2);
        self.seek(start)?;
        let mut suffix = [0u8; 2];
        let read = self.read(&mut suffix[..width])?;
        self.seek(data_end)?;
        Ok(match read {
            2 if suffix == *b"\r\n" => Some(crate::parser::RecoveredStreamEol::CrLf),
            1..=2 if suffix[read - 1] == b'\n' => Some(crate::parser::RecoveredStreamEol::Lf),
            1..=2 if suffix[read - 1] == b'\r' => Some(crate::parser::RecoveredStreamEol::Cr),
            _ => None,
        })
    }

    pub(crate) fn recovered_stream_eol(
        &self,
        object_ref: ObjectRef,
    ) -> Option<crate::parser::RecoveredStreamEol> {
        self.recovered_stream_eols
            .borrow()
            .get(&object_ref)
            .copied()
    }

    /// Whether the canonical `decryptStream` route transforms the complete
    /// recovered source span for this stream. Inspection uses this
    /// classification to avoid applying its separate display-framing trim;
    /// the pipe itself still passes the full length to the decrypt stage.
    pub(crate) fn recovered_stream_eol_is_transformed(
        &self,
        stream_dict: &ObjectHandle,
    ) -> Result<bool> {
        let encryption = self.encryption_parameters().borrow().as_ref().cloned();
        let Some(encryption) = encryption else {
            return Ok(false);
        };
        let inspection = inspect_stream_encryption(&encryption, stream_dict)?;
        Ok(!inspection.is_xref && encryption.stream_method_transforms(inspection.method))
    }

    /// qpdf's `damagedPDF(input, offset, message)` warning carries the
    /// resolved object's `QPDFObjGen` in the rendered message while leaving
    /// the logger's input-source prefix separate (`QPDF.cc:1482-1529`). Keep
    /// that same shape instead of passing the offset as a bare diagnostic,
    /// which would lose the object identity at the canonical ObjectHandle
    /// boundary.
    fn push_stream_warning(
        &self,
        object_ref: ObjectRef,
        offset: u64,
        message: impl Into<String>,
    ) -> Result<()> {
        self.push_warning_at(
            offset,
            format!(
                "(object {} {}, offset {offset}): {}",
                object_ref.number,
                object_ref.generation,
                message.into()
            ),
        )
    }

    /// Emit a stream warning from an offset read that supplied qpdf's own
    /// object description. qpdf stores the complete `QPDFExc::what()` in its
    /// warning list and writes that same value to the logger
    /// (`libqpdf/QPDF.cc:488-504`, `QPDFExc.cc:19-50`). Keeping the complete
    /// bytes as an object-origin diagnostic makes live delivery and deferred
    /// job replay identical, including arbitrary input-description bytes.
    fn push_stream_warning_with_description(
        &self,
        object_ref: ObjectRef,
        offset: u64,
        message: impl Into<String>,
        read_description: Option<&[u8]>,
    ) -> Result<()> {
        let Some(read_description) = read_description else {
            return self.push_stream_warning(object_ref, offset, message);
        };
        let message = message.into();
        let mut object_description = read_description.to_vec();
        object_description.extend_from_slice(
            format!(": object {} {}", object_ref.number, object_ref.generation).as_bytes(),
        );
        self.push_stream_warning_with_object_description(&object_description, offset, message)
    }

    /// Emit a stream warning using qpdf's already-composed object description.
    ///
    /// This is the `QPDF::warn(damagedPDF(...))` boundary: the caller may have
    /// entered the stream through a described offset read, and a nested
    /// indirect `/Length` resolution may have replaced that description before
    /// the warning is emitted (`QPDF.cc:1298-1310,1725,2641-2644`).
    fn push_stream_warning_with_object_description(
        &self,
        object_description: &[u8],
        offset: u64,
        message: impl Into<String>,
    ) -> Result<()> {
        let message = message.into();
        let (logger, suppress_warnings, what) = {
            let mut core = self.core.borrow_mut();
            let what = if core.description.is_empty() {
                let mut what = b"(".to_vec();
                what.extend_from_slice(object_description);
                if offset > 0 {
                    what.extend_from_slice(b", offset ");
                    what.extend_from_slice(offset.to_string().as_bytes());
                }
                what.extend_from_slice(b"): ");
                what.extend_from_slice(message.as_bytes());
                what
            } else {
                format_input_warning_what(
                    &core.description,
                    object_description,
                    offset,
                    message.as_bytes(),
                )
            };
            // `QPDFExc` keeps the file position beside the rendered text
            // (`QPDFExc.cc:19-50`); retain it for `repair_diagnostics()`.
            let mut diagnostic = Diagnostic::object_warning_bytes(&what);
            diagnostic.offset = Some(offset);
            core.repair_diagnostics.push(diagnostic);
            (core.logger.clone(), core.suppress_warnings, what)
        };
        if suppress_warnings {
            return Ok(());
        }
        route_object_warning(&logger, false, &what)
    }

    /// The `PatternFinder`/`findEndstream` pair used by qpdf's
    /// `recoverStreamLength`. `findFirst` first checks the complete `end`
    /// prefix, so a byte that is not followed by `nd` never enters the
    /// bounded tokenizer. A rejected full candidate still resumes at
    /// `candidate + 1` rather than after the candidate token; the live input
    /// rewinds within its buffer, crossing a buffer boundary only when that
    /// is unavoidable.
    fn find_stream_recovery_terminator(&self, stream_offset: u64) -> Result<Option<(u64, u64)>> {
        let mut input = self.live_input();
        input.seek(stream_offset)?;

        loop {
            let candidate = input.tell()?;
            let Some(byte) = input.read_byte()? else {
                return Ok(None);
            };
            if byte != b'e' {
                continue;
            }

            let Some(next) = input.read_byte()? else {
                return Ok(None);
            };
            if next != b'n' {
                input.unread_byte()?;
                continue;
            }
            let Some(next) = input.read_byte()? else {
                return Ok(None);
            };
            if next != b'd' {
                input.unread_byte()?;
                input.unread_byte()?;
                continue;
            }

            let (token, token_len, token_end) = read_stream_recovery_token(&mut input, b"end")?;
            if &token[..token_len] == b"endstream" {
                return Ok(Some((candidate, token_end)));
            }
            if &token[..token_len] == b"endobj" {
                return Ok(Some((candidate, candidate)));
            }

            for _ in 1..token_len {
                input.unread_byte()?;
            }
        }
    }

    /// qpdf `QPDF::validateStreamLineEnd` (`libqpdf/QPDF.cc:1400-1448`),
    /// byte for byte: a newline ends it; a carriage return ends it, consuming
    /// a following newline or warning if there is none; any other
    /// non-whitespace is pushed back with a warning; whitespace warns and
    /// keeps going; EOF just returns, because "a premature EOF here will
    /// result in some other problem that will get reported at another time"
    /// (`:1413-1415`).
    fn validate_stream_line_end(&self) -> Result<()> {
        loop {
            let Some(byte) = self.read_byte()? else {
                return Ok(());
            };
            if byte == b'\n' {
                return Ok(());
            }
            if byte == b'\r' {
                match self.read_byte()? {
                    Some(b'\n') | None => {}
                    Some(_) => {
                        self.unread_byte()?;
                        self.push_warning("stream keyword followed by carriage return only")?;
                    }
                }
                return Ok(());
            }
            if !crate::tokenizer::is_ws(byte) {
                self.unread_byte()?;
                self.push_warning("stream keyword not followed by proper line terminator")?;
                return Ok(());
            }
            self.push_warning("stream keyword followed by extraneous whitespace")?;
        }
    }

    /// Give back the byte just read. qpdf `InputSource::unreadCh`, which
    /// `validateStreamLineEnd` uses at `libqpdf/QPDF.cc:1432` and `:1442`.
    fn unread_byte(&self) -> Result<()> {
        let position = self.tell()?;
        self.seek(position.saturating_sub(1))
    }

    /// qpdf `readStream`'s `/Length` lookup (`libqpdf/QPDF.cc:1371-1382`).
    ///
    /// Takes no `&self` on purpose: [`ObjectHandle::try_dereference`] below is
    /// the re-entry point, and a method with no access to [`ResolverCore`]
    /// cannot be holding a borrow of it when that happens.
    ///
    /// This helper only validates and dereferences `/Length`; it receives
    /// qpdf's post-`obj` header position so malformed `/Length` exceptions
    /// retain the offset captured by `readObject`. [`Self::read_stream`] passes
    /// that position to this helper and to [`Self::warn_stream_failure`]. A
    /// framing failure instead uses the attempted `endstream` token's offset,
    /// while [`Self::recover_stream_length`] reports its recovery warning at
    /// the stream-data offset.
    fn stream_length(dict: &ObjectValue, object_header_offset: u64) -> Result<usize> {
        let error_offset = usize::try_from(object_header_offset).unwrap_or(usize::MAX);
        let ObjectValue::Dictionary(entries) = dict else {
            return Err(Error::parse(
                error_offset,
                "stream keyword follows an object that is not a dictionary",
            ));
        };
        let length = entries.get(b"/Length".as_slice());
        if let Some(length) = length {
            length.try_dereference()?;
        }
        // qpdf tests `isNull()` before `isInteger()` and reports the two
        // separately (`:1373-1380`); an absent key reads as null there, so
        // both routes land on the same message here.
        match length.map(ObjectHandle::as_integer) {
            Some(Some(value)) => usize::try_from(value).map_err(|_| {
                Error::parse(
                    error_offset,
                    "/Length key in stream dictionary is out of range",
                )
            }),
            Some(None) if length.is_some_and(|length| !length.is_null()) => Err(Error::parse(
                error_offset,
                "/Length key in stream dictionary is not an integer",
            )),
            _ => Err(Error::parse(
                error_offset,
                "stream dictionary lacks /Length key",
            )),
        }
    }
}

/// Bytes pulled from the input source per [`ResolverHandle::refill`].
const INPUT_CHUNK: usize = 4096;

/// What qpdf's `pipeStreamData` `try` block would have thrown
/// (`libqpdf/QPDF.cc:2495-2504`), carrying the position each exception is
/// attributed to. The two variants are qpdf's two `catch` arms.
/// Shared qpdf-shaped implementation for original and foreign source data.
/// The source input/encryption owners and destination warning sink are
/// deliberately separate, matching `QPDF::pipeForeignStreamData`'s call into
/// static `QPDF::pipeStreamData` (`libqpdf/QPDF.cc:2477-2585`).
///
/// `description_override` is qpdf's explicit `file` argument to `damagedPDF`
/// and to the unknown-encryption-filter `QPDFExc` constructor
/// (`libqpdf/QPDF.cc:2498-2500,2517-2529`; `libqpdf/QPDF_encryption.cc:
/// 1122-1128`): both build the exception's filename from the *source*
/// `InputSource`, never from `qpdf_for_warning`. `None` here reproduces
/// qpdf's non-foreign overload, where `file` and `qpdf_for_warning` are the
/// same `QPDF` (`libqpdf/QPDF.cc:2541-2552`) and `warning_sink`'s own
/// description is already correct.
/// The `length` argument remains the complete source span supplied by qpdf's
/// `QPDF_Stream`, including bytes found by recovered stream framing; the
/// recovery-EOL metadata is not a pipe-time decryption adjustment.
#[allow(clippy::too_many_arguments)]
fn pipe_stream_data_from_input<R: Read + Seek + 'static>(
    input: &StreamInput<R>,
    encryption_parameters: &Rc<RefCell<Option<crate::encryption::state::EncryptionState>>>,
    warning_sink: &dyn DocumentResolver,
    description_override: Option<&[u8]>,
    object_ref: ObjectRef,
    offset: i64,
    length: usize,
    stream_dict: &ObjectHandle,
    pipeline: &mut dyn Pipeline,
    suppress_warnings: bool,
    will_retry: bool,
) -> Result<bool> {
    let encryption_snapshot = encryption_parameters.borrow().as_ref().cloned();
    let Some(encryption_snapshot) = encryption_snapshot else {
        return pipe_stream_data_to_pipeline_for_input(
            input,
            warning_sink,
            description_override,
            object_ref,
            offset,
            length,
            pipeline,
            suppress_warnings,
            will_retry,
        );
    };

    let inspection = inspect_stream_encryption(&encryption_snapshot, stream_dict)?;
    if inspection.is_xref {
        return pipe_stream_data_to_pipeline_for_input(
            input,
            warning_sink,
            description_override,
            object_ref,
            offset,
            length,
            pipeline,
            suppress_warnings,
            will_retry,
        );
    }

    let (use_aes, warn_unknown) = {
        let encryption = encryption_parameters.borrow();
        match encryption.as_ref() {
            None => (None, false),
            Some(encryption) => encryption.stream_method(inspection.method),
        }
    };
    if warn_unknown {
        warning_sink.warn_stream_data(
            input.last_offset(),
            description_override,
            format!(
                "unknown encryption filter for streams (check {}); \
                 streams may be decrypted improperly",
                inspection.method_source
            ),
        )?;
        let mut encryption = encryption_parameters.borrow_mut();
        if let Some(encryption) = encryption.as_mut() {
            encryption.commit_stream_method(inspection.method);
        }
    }

    let decryption = {
        let mut encryption = encryption_parameters.borrow_mut();
        match encryption.as_mut() {
            None => None,
            Some(encryption) => {
                let stage = match use_aes {
                    None => StreamDecryption::None,
                    Some(false) => {
                        StreamDecryption::Rc4(encryption.key_for_object(object_ref, false).to_vec())
                    }
                    Some(true) => {
                        StreamDecryption::Aes(encryption.key_for_object(object_ref, true).to_vec())
                    }
                };
                Some(stage)
            }
        }
    };
    let Some(decryption) = decryption else {
        return pipe_stream_data_to_pipeline_for_input(
            input,
            warning_sink,
            description_override,
            object_ref,
            offset,
            length,
            pipeline,
            suppress_warnings,
            will_retry,
        );
    };

    match decryption {
        StreamDecryption::None => pipe_stream_data_to_pipeline_for_input(
            input,
            warning_sink,
            description_override,
            object_ref,
            offset,
            length,
            pipeline,
            suppress_warnings,
            will_retry,
        ),
        StreamDecryption::Rc4(key) => {
            let mut decrypt = PlRc4::new("RC4 stream decryption", pipeline, &key)?;
            pipe_stream_data_to_pipeline_for_input(
                input,
                warning_sink,
                description_override,
                object_ref,
                offset,
                length,
                &mut decrypt,
                suppress_warnings,
                will_retry,
            )
        }
        StreamDecryption::Aes(key) => {
            let mut decrypt = PlAesPdf::new_decrypt("AES stream decryption", pipeline, &key)?;
            pipe_stream_data_to_pipeline_for_input(
                input,
                warning_sink,
                description_override,
                object_ref,
                offset,
                length,
                &mut decrypt,
                suppress_warnings,
                will_retry,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn pipe_stream_data_to_pipeline_for_input<R: Read + Seek + 'static>(
    input: &StreamInput<R>,
    warning_sink: &dyn DocumentResolver,
    description_override: Option<&[u8]>,
    object_ref: ObjectRef,
    offset: i64,
    length: usize,
    pipeline: &mut dyn Pipeline,
    suppress_warnings: bool,
    will_retry: bool,
) -> Result<bool> {
    let mut attempted_finish = false;
    let Some(failure) =
        attempt_pipe_stream_data_for_input(input, offset, length, pipeline, &mut attempted_finish)
    else {
        return Ok(true);
    };

    // `description_override` is fixed for the whole call (it names the
    // source input this piping reads from, not the failure), so it is
    // captured once here rather than threaded as a fourth positional
    // argument to every `warn_stream_data` call below.
    let warn =
        |at: u64, message: String| warning_sink.warn_stream_data(at, description_override, message);

    if !suppress_warnings {
        match failure {
            PipeFailure::ShortRead { at } => {
                warn(at, "unexpected EOF reading stream data".to_owned())?;
            }
            PipeFailure::Decoding { at, ref detail } => {
                let og = format!("{} {}", object_ref.number, object_ref.generation);
                warn(
                    at,
                    format!("error decoding stream data for object {og}: {detail}"),
                )?;
                if will_retry {
                    warn(
                        at,
                        "stream will be re-processed without filtering to avoid data loss"
                            .to_owned(),
                    )?;
                }
            }
        }
    }

    if !attempted_finish {
        let _ = pipeline.finish();
    }
    Ok(false)
}

fn attempt_pipe_stream_data_for_input<R: Read + Seek + 'static>(
    input: &StreamInput<R>,
    offset: i64,
    length: usize,
    pipeline: &mut dyn Pipeline,
    attempted_finish: &mut bool,
) -> Option<PipeFailure> {
    let start = match u64::try_from(offset) {
        Ok(start) => start,
        Err(_) => {
            return Some(PipeFailure::Decoding {
                at: input.last_offset(),
                detail: format!("stream offset {offset} is negative"),
            })
        }
    };

    if let Err(error) = input.seek(start) {
        return Some(PipeFailure::Decoding {
            at: input.last_offset(),
            detail: error.to_string(),
        });
    }

    let mut buf: Vec<u8> = Vec::new();
    if let Err(error) = buf.try_reserve_exact(length) {
        return Some(PipeFailure::Decoding {
            at: input.last_offset(),
            detail: format!("cannot allocate {length} bytes of stream data: {error}"),
        });
    }
    buf.resize(length, 0);

    match input.read(&mut buf) {
        Ok(read) if read == length => {}
        Ok(read) => {
            return Some(PipeFailure::ShortRead {
                at: start.saturating_add(read as u64),
            })
        }
        Err(error) => {
            return Some(PipeFailure::Decoding {
                at: input.last_offset(),
                detail: error.into_error().to_string(),
            })
        }
    }

    if let Err(error) = pipeline.write(&buf) {
        return Some(PipeFailure::Decoding {
            at: input.last_offset(),
            detail: error.to_string(),
        });
    }
    *attempted_finish = true;
    if let Err(error) = pipeline.finish() {
        return Some(PipeFailure::Decoding {
            at: input.last_offset(),
            detail: error.to_string(),
        });
    }
    None
}

enum PipeFailure {
    /// `damagedPDF(file, "", offset + read, "unexpected EOF reading stream
    /// data")` (`:2498-2500`), caught as a `QPDFExc` (`:2505-2509`).
    ShortRead { at: u64 },
    /// Anything else (`:2510-2530`).
    Decoding { at: u64, detail: String },
}

/// The replacement value of qpdf's local `Pipeline*` after `decryptStream`.
/// The key is owned because the encryption-state borrow must end before this
/// stage reads the source or calls a downstream pipeline.
enum StreamDecryption {
    /// qpdf's `e_none` early return; retain the caller's pipeline unchanged.
    None,
    /// qpdf's `Pl_RC4` branch (`QPDF_encryption.cc:1146-1151`).
    Rc4(Vec<u8>),
    /// qpdf's shared `Pl_AES_PDF` branch for `e_aes` and `e_aesv3`
    /// (`QPDF_encryption.cc:1136-1145`).
    Aes(Vec<u8>),
}

/// The stream-dictionary half of qpdf's `decryptStream` method choice
/// (`libqpdf/QPDF_encryption.cc:1055-1103`). `method: None` means that qpdf
/// reached its `/StmF` fallback; a concrete `Identity` is instead the
/// deliberate no-op selected by `/Crypt` or cleartext metadata.
struct StreamEncryptionInspection {
    is_xref: bool,
    method: Option<EncryptionMode>,
    method_source: &'static str,
}

fn inspect_stream_encryption(
    encryption: &EncryptionState,
    stream_dict: &ObjectHandle,
) -> Result<StreamEncryptionInspection> {
    let stream_type = stream_dict.try_get_key(b"/Type")?.try_as_name()?;
    let is_xref = stream_type.as_deref() == Some(b"XRef");
    let is_metadata = stream_type.as_deref() == Some(b"Metadata");
    let default_source = "/StmF from /Encrypt dictionary";

    // qpdf's `/Type /XRef` return is outside this gate, but all subsequent
    // stream-local Crypt inspection is strictly `/V >= 4` (`:1057-1064`).
    if is_xref || encryption.encryption_v < 4 {
        return Ok(StreamEncryptionInspection {
            is_xref,
            method: None,
            method_source: default_source,
        });
    }

    let filter = stream_dict.try_get_key(b"/Filter")?;
    let mut method = None;
    let mut method_source = default_source;
    if filter.try_is_or_has_name(b"Crypt")? {
        let decode_params = stream_dict.try_get_key(b"/DecodeParms")?;
        // qpdf's `if (isDictionary()) { if (isDictionaryOfType()) ... }
        // else if (isArray() && filter.isArray()) ...` shape matters: a
        // dictionary of the wrong type never falls through to array pairing.
        if decode_params.try_is_dictionary_of_type(b"", b"")? {
            if decode_params.try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"")? {
                let name = decode_params.try_get_key(b"/Name")?;
                method = Some(interpret_cf_from_handle(encryption, &name)?);
                method_source = "stream's Crypt decode parameters";
            } // cov:ignore: llvm maps the covered typed-dictionary branch to its closing brace
        } else if let (Some(filter_len), Some(decode_len)) =
            (filter.try_array_len()?, decode_params.try_array_len()?)
        {
            if filter_len == decode_len {
                for index in 0..filter_len {
                    let (Some(filter_item), Some(crypt_params)) = (
                        filter.try_array_item(index)?,
                        decode_params.try_array_item(index)?,
                    ) else {
                        continue; // cov:ignore: equal-length in-range array indexes always exist
                    };
                    let is_crypt = filter_item.try_is_name_and_equals(b"Crypt")?;
                    let has_dictionary_params = crypt_params.try_is_dictionary_of_type(b"", b"")?;
                    if !is_crypt || !has_dictionary_params {
                        continue;
                    }
                    let name = crypt_params.try_get_key(b"/Name")?;
                    if name.try_as_name()?.is_some() {
                        method = Some(interpret_cf_from_handle(encryption, &name)?);
                        method_source = "stream's Crypt decode parameters (array)";
                    } // cov:ignore: llvm maps the covered assignment branch to its closing brace
                }
            } // cov:ignore: llvm maps the covered equal-length branch to its closing brace
        } // cov:ignore: llvm maps the covered Crypt-filter branch exit to this closing brace
    }
    // qpdf begins with `e_unknown`, so both a missing stream-local method and
    // a selected unknown name take this fallback. The source remains local
    // when an explicit `/Crypt` lookup produced that unknown method, which is
    // observable in the eventual warning text.
    if matches!(method, None | Some(EncryptionMode::Unknown)) {
        method = if !encryption.encrypt_metadata && is_metadata {
            Some(EncryptionMode::Identity)
        } else {
            None
        };
    }

    Ok(StreamEncryptionInspection {
        is_xref,
        method,
        method_source,
    })
}

/// Bytes pulled per iteration by [`ResolverHandle::read_to_owned`].
///
/// Larger than [`INPUT_CHUNK`] because its callers are the legacy tenants,
/// one of which reads the whole file: the chunked loop replaced a single
/// `read_to_end`, and this keeps the iteration count in the same order.
/// It dies with those callers.
const BULK_READ_CHUNK: usize = 64 * 1024;

/// A live, one-byte-at-a-time parser view over [`ResolverHandle`]'s owned
/// source. `LiveTokenSource` owns token state; this adapter owns no bytes and
/// therefore cannot replay an already completed token.
struct ResolverLiveInput<'a, R: Read + Seek + 'static> {
    resolver: &'a ResolverHandle<R>,
    buffer: [u8; LIVE_INPUT_BUFFER],
    /// Logical source offset of `buffer[0]`.
    buffer_start: u64,
    buffer_len: usize,
    buffer_index: usize,
}

impl<R: Read + Seek> LiveInput for ResolverLiveInput<'_, R> {
    fn tell(&mut self) -> Result<u64> {
        if self.buffer_len == 0 {
            self.resolver.tell()
        } else {
            Ok(self.buffer_start.saturating_add(self.buffer_index as u64))
        }
    }

    fn seek(&mut self, offset: u64) -> Result<()> {
        self.resolver.seek(offset)?;
        self.buffer_start = offset;
        self.buffer_len = 0;
        self.buffer_index = 0;
        Ok(())
    }

    fn read_byte(&mut self) -> Result<Option<u8>> {
        if self.buffer_index == self.buffer_len {
            self.buffer_start = self.resolver.tell()?;
            self.buffer_len = self.resolver.read(&mut self.buffer)?;
            self.buffer_index = 0;
            if self.buffer_len == 0 {
                return Ok(None);
            }
        }
        let byte = self.buffer[self.buffer_index];
        self.buffer_index += 1;
        Ok(Some(byte))
    }

    fn unread_byte(&mut self) -> Result<()> {
        if self.buffer_index != 0 {
            self.buffer_index -= 1;
            Ok(())
        } else {
            let position = self.tell()?;
            let previous = position
                .checked_sub(1)
                .ok_or_else(|| Error::parse(0, "cannot unread before the start of input"))?;
            self.seek(previous)
        }
    }
}

impl<R: Read + Seek> ResolverLiveInput<'_, R> {
    /// Flush a speculative fast-read buffer before another resolver consumer
    /// observes `m->file`'s position. Qpdf's `InputSource::fastUnread` does
    /// the same seek after tokenizer use (`InputSource.hh:148-153`).
    fn finish(&mut self) -> Result<()> {
        let position = self.tell()?;
        self.seek(position)
    }
}

/// qpdf's `findEndstream` reads each complete `end` candidate with
/// `readToken(input, 20)`. Recovery only needs to distinguish two word values,
/// so retain at most the same twenty token bytes while leaving the live source
/// at the equivalent post-token position. A delimiter is unread just as
/// qpdf's `InputSource::fastUnread` does.
const STREAM_RECOVERY_TOKEN_LIMIT: usize = 20;

fn read_stream_recovery_token<I: LiveInput>(
    input: &mut I,
    prefix: &[u8],
) -> Result<([u8; STREAM_RECOVERY_TOKEN_LIMIT], usize, u64)> {
    let mut token = [0; STREAM_RECOVERY_TOKEN_LIMIT];
    token[..prefix.len()].copy_from_slice(prefix);
    let mut token_len = prefix.len();

    loop {
        if token_len == STREAM_RECOVERY_TOKEN_LIMIT {
            return Ok((token, token_len, input.tell()?));
        }

        let Some(byte) = input.read_byte()? else {
            return Ok((token, token_len, input.tell()?));
        };
        if crate::tokenizer::is_ws(byte) || crate::tokenizer::is_delimiter(byte) {
            input.unread_byte()?;
            return Ok((token, token_len, input.tell()?));
        }
        token[token_len] = byte;
        token_len += 1;
    }
}

/// qpdf's `InputSource::buf_size` (`include/qpdf/InputSource.hh:92`).
const LIVE_INPUT_BUFFER: usize = 128;

fn read_live_header_integer(token: Token) -> Result<i64> {
    if token.token_type != crate::tokenizer::TokenType::Integer {
        return Err(Error::parse(token.start, "expected integer"));
    }
    std::str::from_utf8(&token.value)
        .ok()
        .and_then(|text| text.parse::<i64>().ok())
        .ok_or_else(|| Error::parse(token.start, "invalid integer"))
}

/// Fetch a stream-copy dictionary value with qpdf's initialized-null semantics.
///
/// `QPDF::copyStreamData` (`libqpdf/QPDF.cc:2216-2276`) always passes
/// `getKey("/Filter")` and `getKey("/DecodeParms")` into
/// `QPDF_Stream::replaceStreamData`; a missing key is therefore an initialized
/// contextual null, not the preserve-existing sentinel used by other callers.
fn stream_copy_dictionary_value(dictionary: &ObjectHandle, key: &[u8]) -> Result<ObjectHandle> {
    dictionary.try_get_key(key)
}

/// Lets the parser mint a canonical handle for a nested `N G R` through the
/// resolver's own registry.
///
/// The adapter exists only because [`crate::parser::HandleResolver`] takes
/// `&mut self` while [`DocumentResolver::resolve_indirect`] has `&self`;
/// holding `&ResolverHandle` in a struct and taking `&mut` of *that* bridges
/// the two without any interior mutability of its own.
///
/// qpdf's parser reaches the same map directly: `QPDFParser` calls
/// `QPDF::getObject(og)`, which inserts a `QPDF_Unresolved` into
/// `m->obj_cache` if absent (`libqpdf/QPDF.cc:1952-1959`) — and notes there
/// that it "must not resolve any objects", which is exactly why the handle
/// this hands back is unresolved.
struct ChildHandles<'a, R: Read + Seek + 'static> {
    resolver: &'a ResolverHandle<R>,
    description_template: Vec<u8>,
}

fn qpdf_exception_what(filename: &str, object: &str, offset: usize, message: &str) -> String {
    let mut result = String::new();
    if !filename.is_empty() {
        result.push_str(filename);
    }
    if !(object.is_empty() && offset == 0) {
        if !filename.is_empty() {
            result.push_str(" (");
        }
        if !object.is_empty() {
            result.push_str(object);
            if offset > 0 {
                result.push_str(", ");
            }
        }
        if offset > 0 {
            result.push_str(&format!("offset {offset}"));
        }
        if !filename.is_empty() {
            result.push(')');
        }
    }
    if !result.is_empty() {
        result.push_str(": ");
    }
    result.push_str(message);
    result
}

impl<R: Read + Seek> crate::parser::HandleResolver for ChildHandles<'_, R> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.resolver.get_object_handle(object_ref)
    }

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        self.resolver.parsed_direct_object_handle(value)
    }

    fn description_template(&self) -> Option<Vec<u8>> {
        Some(self.description_template.clone())
    }
}

impl<R: Read + Seek> DocumentResolver for ResolverHandle<R> {
    fn pdf_unique_id(&self) -> Option<u64> {
        Some(self.pdf_unique_id.get())
    }

    fn input_description(&self) -> Vec<u8> {
        self.core.borrow().description.clone()
    }

    fn new_stream(&self) -> Result<ObjectHandle> {
        self.new_stream_handle()
    }

    fn copy_stream_data(&self, destination: &ObjectHandle, source: &ObjectHandle) -> Result<()> {
        ResolverHandle::copy_stream_data(self, destination, source)
    }

    fn original_stream_data_provider(
        &self,
        source: &ObjectHandle,
        destination_dict: &ObjectHandle,
    ) -> Result<Rc<dyn StreamDataProvider>> {
        ResolverHandle::original_stream_data_provider(self, source, destination_dict)
    }

    fn original_stream_data_provider_for_destination(
        &self,
        source: &ObjectHandle,
        destination_dict: &ObjectHandle,
        destination_resolver: Weak<dyn DocumentResolver>,
    ) -> Result<Rc<dyn StreamDataProvider>> {
        ResolverHandle::original_stream_data_provider_for_destination(
            self,
            source,
            destination_dict,
            destination_resolver,
        )
    }

    fn immediate_copy_from(&self) -> bool {
        self.immediate_copy_from()
    }

    fn warn(&self, message: Vec<u8>) -> Result<()> {
        self.push_object_warning(message)
    }

    fn warn_stream_data(
        &self,
        offset: u64,
        description_override: Option<&[u8]>,
        message: String,
    ) -> Result<()> {
        self.push_warning_with_offset(Some(offset), description_override, message)
    }

    fn pipe_stream_data(
        &self,
        object_ref: ObjectRef,
        offset: i64,
        length: usize,
        stream_dict: &ObjectHandle,
        pipeline: &mut dyn Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        ResolverHandle::pipe_stream_data(
            self,
            object_ref,
            offset,
            length,
            stream_dict,
            pipeline,
            suppress_warnings,
            will_retry,
        )
    }

    /// Resolve `object_ref`'s slot in place.
    ///
    /// This ports the source classes reached by qpdf's `QPDF::resolve`
    /// (`libqpdf/QPDF.cc:1700-1753`): type 1 reads one file object, type 2
    /// resolves the complete object stream, and free/absent entries become
    /// null. No branch delegates to `Pdf`'s legacy `Object` resolver.
    ///
    /// Reaching this at all is what distinguishes an attached handle from a
    /// detached one — [`ObjectHandle::try_dereference`] reports
    /// `"belongs to a dropped PDF"` when it cannot upgrade its `Weak`, which
    /// is a different failure from this one.
    ///
    /// # The null fallback
    ///
    /// qpdf's loop and catch branches (`libqpdf/QPDF.cc:1706-1749`) warn,
    /// call `updateCache(og, QPDF_Null::create(), -1, -1)`, and return without
    /// throwing. The canonical handle route uses
    /// `set_resolved(ObjectValue::Null)`, which updates the shared value and
    /// clears its source provenance before any parsed literal-null offset is
    /// installed. There is deliberately no value-layer distinction between a
    /// dangling reference, a resolution loop, damaged input, and a literal
    /// null; xref/cache metadata remains the source-of-truth for whether a
    /// reference existed (`QPDF::Members::xref_table`, `QPDF.cc:1716-1748`).
    ///
    /// *The canonical cache* needs no separate write. The `handle` this was
    /// called with is already the [`ResolverCore::object_cache`] entry for
    /// `object_ref` — [`ObjectHandle::try_dereference`] can only reach here
    /// through a handle carrying this resolver's `Weak`, and
    /// [`Self::get_object_handle`] is the only thing that mints one — so
    /// `set_resolved(ObjectValue::Null)` writes straight through the cached
    /// slot, which is what qpdf's `updateCache` achieves with
    /// `cache.object->assign(...)` (`libqpdf/QPDF.cc:1849-1853`).
    ///
    /// qpdf's other `updateCache` branch, the insert, is unreachable from
    /// `QPDF::resolve`: `QPDF::getObject` has already put a `QPDF_Unresolved`
    /// in `m->obj_cache` (`:1955-1957`) before anything can ask for the object
    /// to be resolved, so `isCached(og)` is always true here. flpdf is the
    /// same — [`Self::get_object_handle`] is the only minter and it is
    /// entry-or-insert — which is why neither branch of this method has a
    /// separate counterpart in `resolve_indirect`.
    ///
    /// *The warning* is `"loop detected resolving object N G"`, qpdf's own
    /// text from `libqpdf/QPDF.cc:1710` with `og.unparse(' ')`
    /// (`libqpdf/QPDFObjGen.cc:19-22`) expanded — that separator is why the
    /// number and generation are space-joined rather than carrying qpdf's
    /// default `,`. It goes through [`ResolverHandle::push_warning`] into the
    /// same collection `Pdf::repair_diagnostics` hands back, so a loop is
    /// visible to exactly the callers that see every other flpdf warning.
    ///
    /// The text is stored bare, without qpdf's `QPDFExc` framing.
    /// `damagedPDF("", message)` (`:2625-2628`) fills in the filename and
    /// `m->file->getLastOffset()`, and `QPDFExc::createWhat`
    /// (`libqpdf/QPDFExc.cc:19-49`) renders `"<filename>: <message>"` — the
    /// object description is empty here, so nothing else is interposed.
    /// flpdf keeps the filename out of [`Diagnostic::message`] throughout
    /// (`xref.rs`'s `"file is damaged"` is the same shape), so matching that
    /// convention *is* matching qpdf's inner text. The offset is `None`
    /// rather than qpdf's `getLastOffset()`: `getLastOffset` is the start of
    /// the last token the tokenizer produced, which flpdf does not track —
    /// the resolver's own input position (see [`ResolverCore::tell`]) is a
    /// different quantity, and reporting it instead would be a fabrication.
    ///
    /// Not ported for a different reason: qpdf's `isUnresolved(og)` early
    /// return (`:1702-1704`). [`ObjectHandle::try_dereference`] makes the same
    /// test before calling in, so it is not reachable through that path, but
    /// this method does not make it itself.
    ///
    /// # The type-1 branch
    ///
    /// qpdf's `case 1:` (`libqpdf/QPDF.cc:1720-1727`) calls
    /// `readObjectAtOffset`, which caches the object it read.
    /// [`Self::read_object_at_offset_with_description`] is that call; the value it returns is
    /// written into `handle`'s slot, which *is* the
    /// [`ResolverCore::object_cache`] entry (see the loop branch above).
    ///
    /// **The three phases, and where each borrow of [`ResolverCore`] lives.**
    ///
    /// 1. *Short borrows.* [`ResolveMark::begin`] takes one to test and set
    ///    `resolving`; [`Self::xref_entry`] takes one to read the entry. Both
    ///    end inside their own call.
    /// 2. *No borrow at all.* `read_object_at_offset_with_description` runs here. It reads and
    ///    parses through [`Self::scan_forward`], every step of which takes and
    ///    drops its own borrow, so the `/Length` dereference inside
    ///    [`Self::read_stream`] is free to re-enter this very method.
    /// 3. *Short borrows.* `set_resolved`/`set_parsed_offset_if_unset` touch
    ///    only the handle's own cell — which is the canonical cache entry, so
    ///    they are the `updateCache` equivalent — and the mark's `drop` takes
    ///    the last borrow of the core.
    ///
    /// Structural parse and unsupported-feature failures take qpdf's
    /// `catch (QPDFExc& e) { warn(e); }` (`:1740-1743`) followed by the
    /// resolve-to-null fallback (`:1745-1749`). I/O, encryption, and
    /// diagnostic-channel failures remain errors because they are outside the
    /// equivalent operation boundary in this crate. The damaged-object route
    /// reaches this catch only after the `attempt_recovery` header gate above.
    ///
    /// # Why the body is wrapped in `stacker::maybe_grow`
    ///
    /// **This method is re-entrant, and nothing bounds how deep.** A stream
    /// whose `/Length` is an indirect reference to another stream whose
    /// `/Length` is an indirect reference to another … recurses
    /// `resolve_indirect` → [`Self::read_object_at_offset_with_description`] →
    /// [`Self::read_stream`] → [`Self::stream_length`] →
    /// [`ObjectHandle::try_dereference`] → `resolve_indirect` once per link.
    /// The loop branch above cannot stop it: `resolving` holds *references*,
    /// and every link is a different one, so no repeat is ever seen.
    ///
    /// **A depth limit would be the wrong fix, because qpdf has none.**
    /// `QPDF::resolve`'s `m->resolving` test (`libqpdf/QPDF.cc:1706-1712`) is
    /// the same reference-repeat check and likewise carries no counter; qpdf
    /// recurses this chain until its own stack runs out. Measured on qpdf
    /// 11.9.0 over generated fixtures of this exact shape: 4000 links
    /// (314,083 bytes) exits 3 with recovery warnings, 20,000 links and
    /// 100,000 links both segfault (exit 139). Refusing at a fixed depth would
    /// therefore reject documents qpdf accepts.
    ///
    /// What is taken instead is this crate's own established answer, the one
    /// `parser.rs`'s recursive-descent hub already uses: grow the stack rather than
    /// bound the recursion, so the depth a caller survives follows available
    /// memory instead of the thread's initial stack. The rationale beside
    /// `lift_bounded`'s own call applies verbatim — a production caller on a
    /// small-stack thread must not abort the process where a value could be
    /// returned — and this path was the one place in the reader still
    /// inconsistent with it.
    ///
    /// The wrap goes here, not on [`Self::read_stream`], for the same reason
    /// `parser.rs` wraps `Parser::object`: this is the frame that appears
    /// exactly once per level, so protecting it protects every level.
    fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
        // Keep this boundary small: enter the large dispatch frame only after
        // maybe_grow has had a chance to switch away from a small caller stack.
        let result = stacker::maybe_grow(
            super::READER_STACK_RED_ZONE,
            super::READER_STACK_GROWTH_SIZE,
            || self.resolve_indirect_inner(object_ref, handle),
        );
        self.finish_indirect_resolution(object_ref, handle, result)
    }
}

impl<R: Read + Seek> ResolverHandle<R> {
    /// Keep the resolve-time catch and null fallback out of the recursive
    /// dispatch frame. The `/Length` resolver can re-enter this frame once
    /// per indirect link, so even a small local-layout change compounds on
    /// the deep-chain path.
    #[inline(never)]
    fn finish_indirect_resolution(
        &self,
        object_ref: ObjectRef,
        handle: &ObjectHandle,
        result: Result<()>,
    ) -> Result<()> {
        match result {
            Ok(()) => Ok(()),
            Err(error) if Self::is_qpdf_caught_resolution_error(&error) => {
                // qpdf catches QPDFExc/std::exception around both
                // resolve dispatch arms, warns, and lets the common
                // tail install a null cache value
                // (`QPDF.cc:1737-1749`). Parse/unsupported errors are
                // flpdf's structural equivalent; I/O, encryption,
                // and diagnostic-channel failures remain caller errors.
                self.push_caught_resolution_warning(error, object_ref)?;
                handle.set_resolved(ObjectValue::Null);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    #[inline(never)]
    fn resolve_indirect_inner(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
        // ---- phase 1: short borrows only ----

        // Bound to a named local, not to `_`: the mark must live until
        // this method returns or unwinds, and `let Some(_) = ..` would
        // drop it at the end of this statement.
        let Some(_mark) = ResolveMark::begin(&self.core, object_ref) else {
            // qpdf's order: warn, then cache null
            // (`libqpdf/QPDF.cc:1710-1711`). Neither call may hold a
            // borrow across the other — `push_warning` takes its own
            // `borrow_mut`.
            self.push_warning(format!(
                "loop detected resolving object {} {}",
                object_ref.number, object_ref.generation
            ))?;
            handle.set_resolved(ObjectValue::Null);
            return Ok(());
        };
        let entry = self.xref_entry(object_ref);

        if entry.is_none() && self.has_default_xref_entry(object_ref) {
            self.push_warning(format!(
                "object {}/{} has unexpected xref entry type",
                object_ref.number, object_ref.generation
            ))?;
            handle.set_resolved(ObjectValue::Null);
            return Ok(());
        }

        // ---- phase 2: no borrow is held across this ----
        let result = match entry {
            Some(XrefEntry::Uncompressed { offset }) => {
                // qpdf's dedicated zero-offset arm
                // (`libqpdf/QPDF.cc:1571-1575`) treats a bogus live
                // type-1 entry as null before attempting I/O. In
                // particular, it must not turn this known sentinel
                // into a resolution-time xref-recovery trigger.
                if offset == 0 {
                    self.push_warning_at(0, "object has offset 0")?;
                    handle.set_resolved(ObjectValue::Null);
                    return Ok(());
                }
                let attempt_recovery = self.attempt_recovery();
                match self.read_object_at_offset_with_description(
                    offset,
                    object_ref,
                    true,
                    attempt_recovery,
                    None,
                ) {
                    Ok(parsed) => {
                        let parsed_ref = parsed.object_ref;
                        self.cache_parsed_object(parsed);
                        if parsed_ref != object_ref {
                            // qpdf's common resolve tail sees the
                            // requested slot still unresolved after
                            // caching the header's actual generation.
                            handle.set_resolved(ObjectValue::Null);
                        }
                        Ok(())
                    }
                    Err(ReadObjectAtOffsetError::Body(error)) => Err(error),
                    Err(ReadObjectAtOffsetError::Header(error)) if attempt_recovery => {
                        match self.reconstruct_xref_and_retry(error, object_ref) {
                            Ok(Some(parsed)) => {
                                let parsed_ref = parsed.object_ref;
                                self.cache_parsed_object(parsed);
                                if parsed_ref != object_ref {
                                    handle.set_resolved(ObjectValue::Null); // cov:ignore: reconstructed xref keys the exact parsed object reference
                                }
                                Ok(())
                            }
                            Ok(None) => {
                                let warning = format!(
                                            "object {} {} not found in file after regenerating cross reference table",
                                            object_ref.number, object_ref.generation
                                        );
                                self.push_warning(warning)?;
                                handle.set_resolved(ObjectValue::Null);
                                Ok(())
                            }
                            // cov:ignore-start: resolution-time reconstruct_xref records only type-1 entries; type-2 retry handoff belongs to xref-stream recovery before resolution
                            Err(err) if matches!(&err, Error::Unsupported(_)) => {
                                // Reconstruction can replace the
                                // requested type-1 entry with a
                                // type-2 entry. qpdf's retry then
                                // enters `resolveObjectsInStream`
                                // rather than treating the source
                                // class as unsupported.
                                if let Some(XrefEntry::Compressed { stream, .. }) =
                                    self.xref_entry(object_ref)
                                {
                                    self.resolve_object_stream_or_null(stream, object_ref, handle)
                                } else {
                                    Err(err)
                                }
                            }
                            // cov:ignore-end
                            Err(err) => Err(err),
                        }
                    }
                    Err(ReadObjectAtOffsetError::Header(error)) => Err(error),
                }
            }
            Some(XrefEntry::Compressed { stream, .. }) => {
                self.resolve_object_stream_or_null(stream, object_ref, handle)
            }
            Some(XrefEntry::Free { .. }) => {
                handle.set_resolved(ObjectValue::Null);
                Ok(())
            }
            None => {
                // qpdf QPDF::resolve fallback (QPDF.cc:1745-1748):
                // Absent entries resolve to null without invoking reconstruction.
                handle.set_resolved(ObjectValue::Null);
                Ok(())
            }
        };

        result
    }
}

#[cfg(test)]
mod tests {
    use super::pipe_stream_data_from_input;
    use super::ObjectStreamResolutionError;
    use super::ResolveMark;
    use super::ResolverHandle;
    use super::ResolverWarningOptions;
    use super::CLOSED_INPUT_SOURCE_ERROR;
    use crate::encryption::state::{EncryptionMode, EncryptionState};
    use crate::object_handle::{DocumentResolver, ObjectValue, NO_PARSED_OFFSET};
    use crate::{
        Diagnostic, Diagnostics, Error, ObjectHandle, ObjectRef, Pdf, Severity, XrefEntry,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Cursor;
    use std::process::Command; // cov:ignore: test-only import has no executable LLVM counter.
    use std::rc::Rc;
    use std::sync::Arc;

    /// A three-object document with a classic cross-reference table: catalog,
    /// page tree, and one page. Every object is uncompressed (xref type 1).
    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let catalog = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let pages = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let page = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{catalog:010} 00000 n \n{pages:010} 00000 n \n{page:010} 00000 n \n",
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn dangling_reference_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let catalog = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Dangling 99 0 R >>\nendobj\n",
        );
        let pages = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let page = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{catalog:010} 00000 n \n{pages:010} 00000 n \n{page:010} 00000 n \n",
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn free_reference_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let catalog = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Free 4 1 R >>\nendobj\n");
        let pages = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let page = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "xref\n0 5\n0000000000 65535 f \n{catalog:010} 00000 n \n{pages:010} 00000 n \n{page:010} 00000 n \n0000000000 00001 f \n",
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn unreferenced_high_free_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let catalog = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let pages = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let page = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 100\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{catalog:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{pages:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{page:010} 00000 n \n").as_bytes());
        for _ in 4..100 {
            pdf.extend_from_slice(b"0000000000 00000 f \n");
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 100 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn compressed_object_stream_fixture() -> (Vec<u8>, u64) {
        let member_data = b"<< /Value 1 >> [7 0 R]";
        let header = b"7 0 8 14 ";
        let first = header.len();
        let mut stream_data = header.to_vec();
        stream_data.extend_from_slice(member_data);

        let mut pdf = b"%PDF-1.5\n".to_vec();
        let stream_offset = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "4 0 obj\n<< /Type /ObjStm /N 2 /First {first} /Length {} >>\nstream\n",
                stream_data.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&stream_data);
        // qpdf's readObjectAtOffset scans past endobj and requires one
        // following non-whitespace byte to establish end_after_space. Keep a
        // harmless trailer marker so this hand-built source exercises the
        // successful cache path rather than qpdf's EOF-after-endobj failure.
        pdf.extend_from_slice(b"\nendstream\nendobj\n%tail\n");
        (pdf, stream_offset)
    }

    /// A resolver built directly, bypassing `Pdf::open` entirely — the state
    /// qpdf's `Members::encp(new EncryptionParameters)` is in immediately
    /// after `QPDF` construction, before `initializeEncryption()` has run.
    fn bare_resolver() -> std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>> {
        ResolverHandle::new_shared(
            Cursor::new(minimal_pdf_bytes()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        )
    }

    #[test]
    fn foreign_copy_stream_requires_an_owning_source_resolver() {
        let resolver = bare_resolver();
        let destination = resolver.new_stream_handle().expect("destination stream");
        let source = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(Vec::new()),
            stream_data: None,
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });

        let error = resolver
            .copy_stream_data(&destination, &source)
            .expect_err("an original stream without a document cannot be copied lazily");
        assert!(matches!(error, Error::Internal(message)
            if message == "original foreign stream has no owning document resolver"));
    }

    #[test]
    fn foreign_original_stream_provider_reports_invalid_source_shapes() {
        let resolver = bare_resolver();
        let destination_dict = ObjectHandle::dictionary(Vec::new());
        let direct_stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: destination_dict.clone(),
            stream_data: None,
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });
        let error = resolver
            .original_stream_data_provider(&direct_stream, &destination_dict)
            .expect_err("a direct stream has no source object identity");
        assert!(matches!(error, Error::Internal(message)
            if message == "original foreign stream has no object reference"));

        let unresolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(99, 0), -1);
        let error = resolver
            .original_stream_data_provider(&unresolved, &destination_dict)
            .expect_err("an unresolved non-stream handle has no source stream length");
        assert!(matches!(error, Error::Internal(message)
            if message == "original foreign stream has no stream length"));
    }

    #[test]
    fn foreign_original_stream_provider_reports_a_dropped_destination() {
        let source = bare_resolver();
        let source_stream = source.new_stream_handle().expect("source stream");
        let destination = bare_resolver();
        let destination_erased: Rc<dyn DocumentResolver> = destination.clone();
        let destination_resolver = Rc::downgrade(&destination_erased);
        drop(destination_erased);
        drop(destination);

        let provider = source
            .original_stream_data_provider_for_destination(
                &source_stream,
                &ObjectHandle::dictionary(Vec::new()),
                destination_resolver,
            )
            .expect("foreign source metadata");
        let mut sink = crate::pipeline::buffer::Buffer::new("foreign stream", None);
        let error = provider
            .provide_stream_data_with_retry_by_id(0, 0, &mut sink, false, false)
            .expect_err("destination lifetime must be checked at pipe time");
        assert!(matches!(error, Error::Internal(message)
            if message == "foreign stream destination resolver is no longer live"));
    }

    /// qpdf's `pipeForeignStreamData` builds the damaged-PDF exception from
    /// the captured source `InputSource` (`foreign->file`), not from the
    /// destination `QPDF`: `pipeStreamData`'s static overload throws
    /// `damagedPDF(file, "", offset, message)`, which bakes `file->getName()`
    /// in as the exception's filename before `qpdf_for_warning.warn(e)` ever
    /// runs (`libqpdf/QPDF.cc:2477-2585`). `QPDF::warn(QPDFExc const&)` just
    /// pushes that already-built exception into the destination's warning
    /// list and logs it — it never reconstructs the filename from its own
    /// `m->file` (`libqpdf/QPDF.cc:488-494`). So a stream copied from a named
    /// source into a differently named destination must still report the
    /// source's name when its deferred read fails, even though the failure
    /// is collected and logged through the destination.
    #[test]
    fn foreign_deferred_read_failure_reports_the_source_description() {
        let source_bytes = b"%PDF-1.4\nshort".to_vec();
        let parsed_offset = 9i64;
        let available = source_bytes.len() - 9;
        let declared_length = available + 100;

        let source = ResolverHandle::new_shared(
            Cursor::new(source_bytes),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, b"source.pdf".to_vec()),
            0,
        );
        let source_stream = source.direct_object_handle(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(Vec::new()),
            stream_data: None,
            stream_length: declared_length,
            stream_provider: None,
            filter_on_write: true,
        });
        source_stream.set_parsed_offset_if_unset(parsed_offset);
        let source_stream = source
            .make_indirect_from_object_handle(source_stream)
            .expect("source stream registers under a fresh identity");

        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let destination = ResolverHandle::new_shared(
            Cursor::new(Vec::new()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, b"destination.pdf".to_vec()),
            0,
        );
        let destination_erased: Rc<dyn DocumentResolver> = destination.clone();
        let destination_resolver = Rc::downgrade(&destination_erased);

        let provider = source
            .original_stream_data_provider_for_destination(
                &source_stream,
                &ObjectHandle::dictionary(Vec::new()),
                destination_resolver,
            )
            .expect("original foreign source metadata");

        let mut sink = crate::pipeline::buffer::Buffer::new("foreign stream", None);
        let ok = provider
            .provide_stream_data_with_retry_by_id(0, 0, &mut sink, false, false)
            .expect("decryptStream preparation");
        assert!(!ok, "a truncated foreign read fails");

        let logged = String::from_utf8(output.lock().unwrap().clone()).expect("utf8 log");
        assert!(
            logged.contains("source.pdf"),
            "warning must name the source document: {logged}"
        );
        assert!(
            !logged.contains("destination.pdf"),
            "warning must not attribute the source's location to the destination: {logged}"
        );

        let diagnostics = destination.repair_diagnostics();
        assert_eq!(diagnostics.entries().len(), 1);
        assert_eq!(
            diagnostics.entries()[0].description.as_deref(),
            Some(b"source.pdf".as_slice())
        );
    }

    #[test]
    fn canonical_stream_recovery_identifies_the_included_line_ending() {
        let recover = |source: &[u8], data_end: u64| {
            let resolver = ResolverHandle::new_shared(
                Cursor::new(source.to_vec()),
                0,
                BTreeMap::new(),
                false,
                false,
                Diagnostics::default(),
                ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
                0,
            );
            resolver
                .recovered_stream_eol_at(0, data_end)
                .expect("line-ending probe")
        };

        assert_eq!(
            recover(b"payload\r\nendstream", 9),
            Some(crate::parser::RecoveredStreamEol::CrLf)
        );
        assert_eq!(
            recover(b"payload\nendstream", 8),
            Some(crate::parser::RecoveredStreamEol::Lf)
        );
        assert_eq!(
            recover(b"payload\rendstream", 8),
            Some(crate::parser::RecoveredStreamEol::Cr)
        );
        assert_eq!(recover(b"endstream", 0), None);
    }

    #[test]
    fn recovered_stream_eol_lookup_returns_the_recorded_value() {
        let resolver = resolver_over(Vec::new());
        let object_ref = ObjectRef::new(1, 0);
        resolver
            .recovered_stream_eols
            .borrow_mut()
            .insert(object_ref, crate::parser::RecoveredStreamEol::CrLf);

        assert_eq!(
            resolver.recovered_stream_eol(object_ref),
            Some(crate::parser::RecoveredStreamEol::CrLf)
        );
        assert_eq!(resolver.recovered_stream_eol(ObjectRef::new(2, 0)), None);
    }

    /// AC6 case 4: a resolver on which no authentication step has run at all
    /// reports no encryption parameters. This is qpdf's
    /// `encryption_initialized == false` state.
    ///
    /// **This does not discriminate that state from qpdf's
    /// `encryption_initialized == true, encrypted == false` state** (an
    /// authenticated-but-unencrypted document, see
    /// [`an_unencrypted_document_reports_no_encryption_parameters`] below).
    /// This test builds its resolver directly via `bare_resolver()`,
    /// bypassing `Pdf::open` entirely, so `Pdf::authenticate_if_encrypted`
    /// never runs here and both states collapse to the same `None`
    /// regardless of `Pdf` sharing this cell — that collapse is qpdf's
    /// `encrypted`/`encryption_initialized` pair mapping onto one
    /// `Option<EncryptionState>` (see the field-mapping table in this
    /// issue's design, `bd show flpdf-25kg.3.11`), not evidence one way or
    /// the other about whether `Pdf` and `ResolverCore` share an allocation
    /// — [`an_rc4_document_reports_its_encryption_parameters_through_the_shared_cell`]
    /// and its AES sibling are what pin the sharing itself.
    #[test]
    fn a_resolver_with_no_authentication_attempted_reports_no_encryption_parameters() {
        let resolver = bare_resolver();
        assert!(resolver.encryption_parameters().borrow().is_none());
    }

    /// How many times a [`crate::pipeline::test_support::RecordingSink`] was
    /// finished. Shared so every finish-counting test agrees on what it means.
    fn finish_count(
        trace: &std::rc::Rc<std::cell::RefCell<crate::pipeline::test_support::Trace>>,
    ) -> usize {
        trace
            .borrow()
            .calls
            .iter()
            .filter(|call| {
                matches!(
                    call,
                    crate::pipeline::test_support::TraceCall::Finish { .. }
                )
            })
            .count()
    }

    struct WarningRecordingSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl crate::pipeline::Pipeline for WarningRecordingSink {
        // cov:ignore-start: PipelineHandle does not inspect a custom sink identifier in warning routing
        fn identifier(&self) -> &str {
            "warning recording sink"
        }
        // cov:ignore-end

        fn write(&mut self, data: &[u8]) -> crate::pipeline::PipelineResult<()> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        // cov:ignore-start: warning routing deliberately leaves caller-owned custom sinks unfinished
        fn finish(&mut self) -> crate::pipeline::PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    #[test]
    fn warning_location_omits_empty_description_and_zero_offset_like_qpdf() {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));

        for (description, offset, message) in [
            ("", None, "no location"),
            ("", Some(0), "zero offset"),
            ("", Some(7), "positive offset"),
            ("input.pdf", Some(0), "named zero offset"),
            ("input.pdf", Some(7), "named positive offset"),
            (
                "input.pdf",
                None,
                "(object 5 0, offset 232): expected endobj",
            ),
        ] {
            super::route_warning(&logger, false, description.as_bytes(), offset, message).unwrap();
        }

        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: no location\n\
              WARNING: zero offset\n\
              WARNING: offset 7: positive offset\n\
              WARNING: input.pdf: named zero offset\n\
              WARNING: input.pdf (offset 7): named positive offset\n\
              WARNING: input.pdf (object 5 0, offset 232): expected endobj\n"
        );
    }

    #[test]
    fn input_warning_what_matches_qpdf_context_shapes() {
        for (filename, object, offset, expected) in [
            (b"".as_slice(), b"".as_slice(), 0, b"message".as_slice()),
            (
                b"".as_slice(),
                b"object 1 0".as_slice(),
                0,
                b"object 1 0: message".as_slice(),
            ),
            (
                b"input.pdf".as_slice(),
                b"".as_slice(),
                7,
                b"input.pdf (offset 7): message".as_slice(),
            ),
            (
                b"input.pdf".as_slice(),
                b"object 1 0".as_slice(),
                0,
                b"input.pdf (object 1 0): message".as_slice(),
            ),
            (
                b"input.pdf".as_slice(),
                b"object 1 0".as_slice(),
                7,
                b"input.pdf (object 1 0, offset 7): message".as_slice(),
            ),
        ] {
            assert_eq!(
                super::format_input_warning_what(filename, object, offset, b"message"),
                expected
            );
        }
    }

    #[test]
    fn warning_location_does_not_repeat_offset_for_object_prefixed_message() {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));

        super::route_warning(
            &logger,
            false,
            b"input.pdf",
            Some(123),
            "(object 5 0, offset 123): expected endobj",
        )
        .unwrap();

        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: input.pdf (object 5 0, offset 123): expected endobj\n"
        );
    }

    #[test]
    fn warning_location_does_not_repeat_offset_for_trailer_prefixed_message() {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));

        super::route_warning(
            &logger,
            false,
            b"input.pdf",
            Some(416),
            "(trailer, offset 416): invalid /ID in trailer dictionary",
        )
        .unwrap();

        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: input.pdf (trailer, offset 416): invalid /ID in trailer dictionary\n"
        );
    }

    #[test]
    fn trailer_warning_without_positive_offset_omits_offset_like_qpdf() {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::new()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, Vec::new()),
            0,
        );

        resolver
            .push_trailer_warning_at(0, "invalid /ID in trailer dictionary")
            .expect("warning delivery");

        assert_eq!(
            resolver.repair_diagnostics().entries()[0].message,
            "(trailer): invalid /ID in trailer dictionary"
        );
        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: (trailer): invalid /ID in trailer dictionary\n"
        );
    }

    #[test]
    fn damaged_warning_keeps_object_context_without_positive_offset() {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::new()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, Vec::new()),
            0,
        );
        {
            let mut core = resolver.core.borrow_mut();
            core.last_object_description = "object 7 0".to_owned();
            core.last_object_description_bytes = b"object 7 0".to_vec();
            core.input.borrow().last_offset.set(0);
        }

        resolver
            .push_damaged_warning("no offset")
            .expect("warning delivery");

        assert_eq!(
            resolver.repair_diagnostics().entries()[0].message,
            "(object 7 0): no offset"
        );
        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: (object 7 0): no offset\n"
        );
    }

    #[test]
    fn expected_endobj_warning_falls_back_when_description_state_is_empty() {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::new()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, Vec::new()),
            0,
        );

        resolver
            .push_expected_endobj_warning(ObjectRef::new(7, 0), 12, None)
            .expect("warning delivery");

        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: (object 7 0, offset 12): expected endobj\n"
        );
    }

    #[test]
    fn json_warning_location_omits_wrapping_parens_for_empty_description() {
        // `QPDFExc::createWhat` (`libqpdf/QPDFExc.cc:19-49`) only wraps the
        // object/offset detail in parentheses when a non-empty filename
        // precedes it. With an empty filename and no object, a positive
        // offset must stand alone as "offset N: message", not
        // "(offset N): message" -- the previously-buggy shape.
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::new()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, Vec::new()),
            0,
        );

        for (object, offset, message, expected) in [
            ("", 0, "zero offset", "WARNING: zero offset\n"),
            (
                "",
                5,
                "positive offset",
                "WARNING: offset 5: positive offset\n",
            ),
            (
                "obj:1 0 R",
                0,
                "object only",
                "WARNING: obj:1 0 R: object only\n",
            ),
            (
                "obj:1 0 R",
                5,
                "object and offset",
                "WARNING: obj:1 0 R, offset 5: object and offset\n",
            ),
        ] {
            output.lock().unwrap().clear();
            resolver
                .push_json_warning("", object, offset, message)
                .unwrap();
            assert_eq!(
                output.lock().unwrap().as_slice(),
                expected.as_bytes(),
                "object={object:?} offset={offset}"
            );
        }
    }

    #[test]
    fn json_warning_location_wraps_object_and_offset_for_named_description() {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::new()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, b"document.pdf".to_vec()),
            0,
        );

        for (object, offset, message, expected) in [
            ("", 0, "zero offset", "WARNING: document.pdf: zero offset\n"),
            (
                "",
                5,
                "positive offset",
                "WARNING: document.pdf (offset 5): positive offset\n",
            ),
            (
                "obj:1 0 R",
                0,
                "object only",
                "WARNING: document.pdf (obj:1 0 R): object only\n",
            ),
            (
                "obj:1 0 R",
                5,
                "object and offset",
                "WARNING: document.pdf (obj:1 0 R, offset 5): object and offset\n",
            ),
        ] {
            output.lock().unwrap().clear();
            resolver
                .push_json_warning("document.pdf", object, offset, message)
                .unwrap();
            assert_eq!(
                output.lock().unwrap().as_slice(),
                expected.as_bytes(),
                "object={object:?} offset={offset}"
            );
        }
    }

    type CapturedWarnings = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    /// A named resolver whose warnings are delivered rather than suppressed,
    /// paired with the buffer its logger writes to.
    fn named_resolver_with_captured_warnings() -> (
        std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>>,
        CapturedWarnings,
    ) {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::new()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, b"input.pdf".to_vec()),
            0,
        );
        (resolver, output)
    }

    #[test]
    fn a_handle_warns_through_the_live_resolver_it_was_minted_from() {
        // The end-to-end route acceptance asks for: an object emits through
        // its own context and lands in the collection `Pdf::repair_diagnostics`
        // hands back, without the caller holding a `&mut Pdf`.
        let (resolver, output) = named_resolver_with_captured_warnings();
        let erased: std::rc::Rc<dyn DocumentResolver> = resolver.clone();
        let handle = crate::object_handle::ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(3, 0),
            std::rc::Rc::downgrade(&erased),
        );
        handle.set_resolved(ObjectValue::Integer(7));

        handle
            .type_warning("dictionary", "treating as empty")
            .unwrap();

        assert_eq!(
            resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            ["object 3 0: operation for dictionary attempted on object of type integer: treating as empty"]
        );
        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: object 3 0: operation for dictionary attempted on object of type integer: \
              treating as empty\n"
        );
    }

    #[test]
    fn live_parser_binds_deep_direct_values_to_the_owning_resolver() {
        let resolver =
            resolver_over(b"1 0 obj\n<< /L1 << /L2 << /Value 7 >> >> >>\nendobj\n".to_vec());
        let (object, _) = resolver
            .read_object_at_offset(0, ObjectRef::new(1, 0))
            .expect("live object should parse");
        let values = ObjectHandle::from_value(object)
            .as_dictionary()
            .expect("expected a dictionary object");
        let level_one = values
            .get(b"/L1".as_slice())
            .and_then(ObjectHandle::as_dictionary)
            .and_then(|values| values.get(b"/L2".as_slice()).cloned())
            .expect("level two dictionary");
        let value = level_one
            .as_dictionary()
            .and_then(|values| values.get(b"/Value".as_slice()).cloned())
            .expect("deep scalar");

        assert_eq!(level_one.get_parsed_offset(), 22);
        assert_eq!(value.get_parsed_offset(), 32);

        level_one
            .object_warning("deep container warning")
            .expect("deep container keeps the document context");
        value
            .object_warning("deep scalar warning")
            .expect("deep scalar keeps the document context");

        assert_eq!(
            resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            [
                ", object 1 0 at offset 24: deep container warning",
                ", object 1 0 at offset 32: deep scalar warning",
            ]
        );
    }

    #[test]
    fn canonical_live_parser_stamps_root_and_nested_descriptions() {
        let resolver = resolver_over_named_object(
            b"\n1 0 obj\n<< /L1 << /L2 << /Value 7 >> >> /Items [8 null] >>\nendobj\n%tail\n"
                .to_vec(),
            ObjectRef::new(1, 0),
            "input.pdf",
        );
        let root = resolver.get_object_handle(ObjectRef::new(1, 0));
        root.try_dereference().expect("live object should resolve");

        assert_eq!(root.description(), b"input.pdf, object 1 0 at offset 11");
        let level_one = root
            .as_dictionary()
            .and_then(|values| values.get(b"/L1".as_slice()).cloned())
            .expect("level one dictionary");
        assert_eq!(
            level_one.description(),
            b"input.pdf, object 1 0 at offset 18"
        );

        let level_two = level_one
            .as_dictionary()
            .and_then(|values| values.get(b"/L2".as_slice()).cloned())
            .expect("level two dictionary");
        assert_eq!(
            level_two.description(),
            b"input.pdf, object 1 0 at offset 25"
        );

        let value = level_two
            .as_dictionary()
            .and_then(|values| values.get(b"/Value".as_slice()).cloned())
            .expect("deep scalar");
        assert_eq!(value.description(), b"input.pdf, object 1 0 at offset 33");

        let items = root
            .as_dictionary()
            .and_then(|values| values.get(b"/Items".as_slice()).cloned())
            .expect("array value");
        assert_eq!(items.description(), b"input.pdf, object 1 0 at offset 49");
        let array_scalar = items
            .as_array()
            .and_then(|values| values.first().cloned())
            .expect("array scalar");
        assert_eq!(
            array_scalar.description(),
            b"input.pdf, object 1 0 at offset 49"
        );
        let array_null = items
            .as_array()
            .and_then(|values| values.get(1).cloned())
            .expect("array null");
        assert!(array_null.description().is_empty());

        level_two
            .object_warning("deep container description")
            .expect("nested dictionary keeps the document context");
        value
            .object_warning("deep scalar description")
            .expect("nested scalar keeps the document context");
        assert_eq!(
            resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            [
                "input.pdf, object 1 0 at offset 25: deep container description",
                "input.pdf, object 1 0 at offset 33: deep scalar description",
            ]
        );
    }

    #[test]
    fn parser_created_direct_values_do_not_keep_a_dropped_resolver_alive() {
        let value = {
            let resolver = resolver_over(b"1 0 obj\n<< /Value 7 >>\nendobj\n".to_vec());
            let (object, _) = resolver
                .read_object_at_offset(0, ObjectRef::new(1, 0))
                .expect("live object should parse");
            let values = ObjectHandle::from_value(object)
                .as_dictionary()
                .expect("expected a dictionary object");
            values
                .get(b"/Value".as_slice())
                .cloned()
                .expect("direct value")
        };

        let error = value
            .object_warning("dropped document")
            .expect_err("a weak context must be gone with its resolver");
        assert!(matches!(
            error,
            Error::System(message) if message == ", object 1 0 at offset 18: dropped document"
        ));
    }

    #[test]
    fn live_parser_stream_dictionary_keeps_the_owning_resolver_context() {
        let resolver =
            resolver_over(b"1 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n".to_vec());
        let (object, _) = resolver
            .read_object_at_offset(0, ObjectRef::new(1, 0))
            .expect("live stream should parse");
        let stream = ObjectHandle::from_value(object);
        let stream_dict = stream.as_stream_dict().expect("stream dictionary");

        stream_dict
            .object_warning("stream dictionary warning")
            .expect("stream dictionary keeps the document context");
        assert_eq!(
            resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            [", object 1 0 at offset 10: stream dictionary warning"]
        );
    }

    #[test]
    fn canonical_live_parser_stream_stamps_root_and_dictionary_descriptions() {
        let object_ref = ObjectRef::new(1, 0);
        let resolver = resolver_over_named_object(
            b"\n1 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n%tail\n".to_vec(),
            object_ref,
            "input.pdf",
        );
        let stream = resolver.get_object_handle(object_ref);
        stream
            .try_dereference()
            .expect("live stream should resolve");
        let stream_dict = stream.as_stream_dict().expect("stream dictionary");

        assert_eq!(stream.description(), b"input.pdf, stream object 1 0");
        assert!(
            stream.get_parsed_offset() > stream_dict.get_parsed_offset(),
            "the stream description must retain the stream-data offset"
        );
        let dictionary_description = stream_dict.description();
        assert!(
            dictionary_description.starts_with(b"input.pdf, object 1 0 at offset "),
            "the stream dictionary must retain its parser description, got {dictionary_description:?}"
        );

        stream
            .object_warning("stream root warning")
            .expect("stream root warning should reach the resolver");
        stream_dict
            .object_warning("stream dictionary warning")
            .expect("stream dictionary warning should reach the resolver");
        let expected_dictionary_warning = format!(
            "{}: stream dictionary warning",
            String::from_utf8_lossy(&dictionary_description)
        );
        assert_eq!(
            resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.clone())
                .collect::<Vec<_>>(),
            vec![
                "input.pdf, stream object 1 0: stream root warning".to_owned(),
                expected_dictionary_warning,
            ]
        );
    }

    #[test]
    fn an_object_warning_carries_no_location_even_when_the_document_is_named() {
        let (resolver, output) = named_resolver_with_captured_warnings();

        resolver
            .push_object_warning(
                "operation for dictionary attempted on object of type integer: treating as empty",
            )
            .unwrap();

        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: operation for dictionary attempted on object of type integer: \
              treating as empty\n"
        );
    }

    #[test]
    fn an_object_warning_is_collected_with_document_warnings_in_emission_order() {
        let (resolver, output) = named_resolver_with_captured_warnings();

        resolver.push_warning("from the document").unwrap();
        resolver.push_object_warning("from the object").unwrap();

        let collected: Vec<_> = resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| (entry.message.clone(), entry.offset))
            .collect();
        assert_eq!(
            collected,
            vec![
                ("from the document".to_owned(), None),
                ("from the object".to_owned(), None),
            ]
        );
        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: input.pdf: from the document\n\
              WARNING: from the object\n"
        );
    }

    #[test]
    fn replaying_an_object_warning_preserves_raw_message_bytes() {
        let (resolver, output) = named_resolver_with_captured_warnings();
        let mut diagnostics = Diagnostics::default();
        diagnostics.push(Diagnostic::object_warning_bytes(
            b"object-warning-\xff.pdf: malformed object",
        ));

        resolver.replay_warnings(&diagnostics).unwrap();

        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: object-warning-\xff.pdf: malformed object\n"
        );
    }

    #[test]
    fn a_suppressed_object_warning_is_still_collected() {
        let resolver = resolver_over(Vec::new());

        resolver.push_object_warning("from the object").unwrap();

        assert_eq!(
            resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            ["from the object"]
        );
    }

    /// A resolver over `bytes`, so a test can hand `pipe_stream_data` an
    /// offset and length directly instead of parsing a document to reach one.
    fn resolver_over(bytes: Vec<u8>) -> std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>> {
        ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        )
    }

    #[test]
    fn close_input_source_replaces_only_the_document_source_pointer() {
        let resolver = resolver_over(b"retained source".to_vec());
        let captured_source = resolver.stream_input();

        assert_eq!(resolver.input_source_name(), "");
        resolver.close_input_source();

        assert_eq!(
            captured_source
                .tell()
                .expect("captured source remains live"),
            0
        );
        assert_eq!(
            resolver
                .stream_input()
                .tell()
                .expect_err("the document source must now be invalid")
                .to_string(),
            CLOSED_INPUT_SOURCE_ERROR
        );
    }

    fn resolver_over_named_object(
        bytes: Vec<u8>,
        object_ref: ObjectRef,
        description: &str,
    ) -> std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>> {
        ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            BTreeMap::from([(object_ref, XrefEntry::Uncompressed { offset: 1 })]),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(
                crate::QPDFLogger::create(),
                true,
                description.as_bytes().to_vec(),
            ),
            0,
        )
    }

    fn resolver_over_with_failing_warning(
        bytes: Vec<u8>,
        fail_at: usize,
    ) -> std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>> {
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(fail_at),
        )));
        ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, b"stream.pdf".to_vec()),
            0,
        )
    }

    fn authenticated_v2_rc4_encryption() -> EncryptionState {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v2-rc4-128-r3.pdf"
        );
        let fixture = std::fs::read(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v2-rc4-128-r3.pdf");
        let pdf = Pdf::open_mem_owned_with_options(
            fixture,
            crate::PdfOpenOptions {
                password: b"user-v2".to_vec(),
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open RC4 fixture");
        pdf.resolver
            .encryption_parameters()
            .borrow()
            .as_ref()
            .cloned()
            .expect("RC4 fixture must authenticate")
    }

    fn authenticated_v5_aes256_encryption() -> EncryptionState {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v5-aes-256-r6.pdf"
        );
        let fixture = std::fs::read(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v5-aes-256-r6.pdf");
        let pdf = Pdf::open_mem_owned_with_options(
            fixture,
            crate::PdfOpenOptions {
                password: b"user-v5-r6".to_vec(),
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open AES-256 fixture");
        pdf.resolver
            .encryption_parameters()
            .borrow()
            .as_ref()
            .cloned()
            .expect("AES-256 fixture must authenticate")
    }

    fn rc4_stream_ciphertext(
        object_ref: ObjectRef,
        plaintext: &[u8],
        encryption: &EncryptionState,
    ) -> Vec<u8> {
        let mut key_derivation = encryption.clone();
        let key = key_derivation.key_for_object(object_ref, false).to_vec();
        let mut ciphertext = plaintext.to_vec();
        crate::encryption::rc4::Rc4::new(&key)
            .expect("authenticated RC4 key is non-empty")
            .process_in_place(&mut ciphertext);
        ciphertext
    }

    fn v4_encryption(cf_stream: EncryptionMode) -> EncryptionState {
        let mut encryption = authenticated_v2_rc4_encryption();
        encryption.encryption_v = 4;
        encryption.cf_stream = cf_stream;
        encryption
    }

    fn crypt_filter_decode_params(name: &[u8]) -> crate::ObjectHandle {
        crate::ObjectHandle::dictionary(vec![
            (
                b"Type".to_vec(),
                crate::ObjectHandle::name(b"CryptFilterDecodeParms".to_vec()),
            ),
            (b"Name".to_vec(), crate::ObjectHandle::name(name.to_vec())),
        ])
    }

    fn crypt_params(name: &[u8]) -> crate::ObjectHandle {
        crate::ObjectHandle::dictionary(vec![(
            b"Name".to_vec(),
            crate::ObjectHandle::name(name.to_vec()),
        )])
    }

    struct EncryptionClearingResolver {
        target: std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>>,
    }

    impl DocumentResolver for EncryptionClearingResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            handle: &crate::ObjectHandle,
        ) -> crate::Result<()> {
            *self.target.encryption_parameters().borrow_mut() = None;
            handle.set_resolved(ObjectValue::Name(b"Metadata".to_vec()));
            Ok(())
        }

        fn warn_stream_data(
            &self,
            _offset: u64,
            _description_override: Option<&[u8]>,
            _message: String,
        ) -> crate::Result<()> {
            *self.target.encryption_parameters().borrow_mut() = None;
            Ok(())
        }
    }

    struct FailingStreamWarningResolver;

    impl DocumentResolver for FailingStreamWarningResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            _handle: &ObjectHandle,
        ) -> crate::Result<()> {
            Ok(())
        }

        fn warn_stream_data(
            &self,
            _offset: u64,
            _description_override: Option<&[u8]>,
            _message: String,
        ) -> crate::Result<()> {
            Err(Error::Internal("stream warning sink failed".to_owned()))
        }
    }

    /// `QPDF::pipeStreamData` seeks to the parsed offset, reads exactly
    /// `length` bytes, writes them to the pipeline and finishes it
    /// (`libqpdf/QPDF.cc:2496-2504`). It reads the declared length and nothing
    /// around it.
    #[test]
    fn piping_copies_the_declared_length_from_the_parsed_offset() {
        let source = b"%PDF-1.4\nbefore<payload bytes>after".to_vec();
        let offset = source
            .windows(7)
            .position(|w| w == b"payload")
            .expect("marker") as i64;
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        let ok = resolver
            .pipe_stream_data(
                ObjectRef::new(4, 0),
                offset,
                b"payload bytes".len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation");

        assert!(ok, "an in-bounds read succeeds");
        assert_eq!(sink.take_buffer().expect("buffer"), b"payload bytes");
    }

    // Removing the pipe-side decrypt stage leaves this ciphertext at the sink.
    #[test]
    fn piping_an_encrypted_v2_rc4_stream_delivers_plaintext_to_the_sink() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = authenticated_v2_rc4_encryption();
        let plaintext = b"pipe-time RC4 plaintext";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);

        let resolver = resolver_over(ciphertext);
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                plaintext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    #[test]
    fn pipe_releases_the_encryption_borrow_while_stream_dict_values_resolve() {
        let object_ref = ObjectRef::new(4, 0);
        let source = b"reentrant dictionary inspection".to_vec();
        let resolver = resolver_over(source.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(v4_encryption(EncryptionMode::Rc4));
        let clearing: std::rc::Rc<dyn DocumentResolver> =
            std::rc::Rc::new(EncryptionClearingResolver {
                target: resolver.clone(),
            });
        let stream_type = crate::ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            std::rc::Rc::downgrade(&clearing),
        );
        let dict = crate::ObjectHandle::dictionary(vec![(b"Type".to_vec(), stream_type)]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(object_ref, 0, source.len(), &dict, &mut sink, false, false,)
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), source);
        assert!(resolver.encryption_parameters().borrow().is_none());
    }

    #[test]
    fn a_stream_dictionary_resolution_error_propagates_before_the_pipe_try() {
        let resolver = resolver_over(b"ciphertext".to_vec());
        *resolver.encryption_parameters().borrow_mut() =
            Some(v4_encryption(EncryptionMode::Identity));
        let (stream_type, stream_type_resolver) =
            crate::object_handle::identity_tests::resolver_bearing_handle(ObjectValue::Name(
                b"Metadata".to_vec(),
            ));
        drop(stream_type_resolver);
        let dict = crate::ObjectHandle::dictionary(vec![(b"Type".to_vec(), stream_type)]);
        let mut sink = crate::pipeline::test_support::RecordingSink::new(&[], &[]);
        let trace = sink.trace();

        let error = resolver
            .pipe_stream_data(
                ObjectRef::new(4, 0),
                0,
                b"ciphertext".len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect_err("decryptStream preparation errors propagate");
        assert!(error
            .to_string()
            .contains("object 20 0 belongs to a dropped PDF"));
        assert!(trace.borrow().calls.is_empty(), "the sink is untouched");
        assert!(resolver.repair_diagnostics().entries().is_empty());
    }

    #[test]
    fn an_invalid_aes_object_key_propagates_before_the_pipe_try() {
        let mut encryption = v4_encryption(EncryptionMode::Aes128);
        encryption.file_key.clear();
        encryption.cached_object_encryption_key.clear();
        encryption.cached_key_og = None;
        let resolver = resolver_over(b"ciphertext".to_vec());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::test_support::RecordingSink::new(&[], &[]);
        let trace = sink.trace();

        let error = resolver
            .pipe_stream_data(
                ObjectRef::new(4, 0),
                0,
                b"ciphertext".len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect_err("decryptStream stage-construction errors propagate");
        assert!(error
            .to_string()
            .contains("Pl_AES_PDF: key must be at least 16 bytes"));
        assert!(trace.borrow().calls.is_empty(), "the sink is untouched");
        assert!(resolver.repair_diagnostics().entries().is_empty());
    }

    #[test]
    fn an_empty_v5_rc4_object_key_propagates_before_the_pipe_try() {
        let mut encryption = authenticated_v5_aes256_encryption();
        encryption.cf_stream = EncryptionMode::Rc4;
        encryption.file_key.clear();
        encryption.cached_object_encryption_key.clear();
        encryption.cached_key_og = None;
        let resolver = resolver_over(b"ciphertext".to_vec());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::test_support::RecordingSink::new(&[], &[]);
        let trace = sink.trace();

        let error = resolver
            .pipe_stream_data(
                ObjectRef::new(4, 0),
                0,
                b"ciphertext".len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect_err("decryptStream stage-construction errors propagate");
        assert!(error.to_string().contains("invalid key/IV length"));
        assert!(trace.borrow().calls.is_empty(), "the sink is untouched");
        assert!(resolver.repair_diagnostics().entries().is_empty());
    }

    /// qpdf returns before the `/V` gate for an encrypted cross-reference
    /// stream (`QPDF_encryption.cc:1057-1061`), leaving its payload untouched.
    #[test]
    fn piping_an_encrypted_xref_stream_leaves_its_ciphertext_untouched() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = authenticated_v2_rc4_encryption();
        let plaintext = b"xref bytes are never decrypted";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            crate::ObjectHandle::name(b"XRef".to_vec()),
        )]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), ciphertext);
    }

    /// With `/V >= 4`, qpdf falls back to `/StmF` only after finding no
    /// stream-local `/Crypt` filter (`QPDF_encryption.cc:1063-1103`).
    #[test]
    fn piping_an_encrypted_v4_rc4_default_stream_delivers_plaintext() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = authenticated_v2_rc4_encryption();
        encryption.encryption_v = 4;
        encryption.cf_stream = EncryptionMode::Rc4;
        let plaintext = b"V4 /StmF RC4 plaintext";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext);
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                plaintext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    #[test]
    fn piping_an_encrypted_v4_aes_default_stream_delivers_plaintext() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = authenticated_v2_rc4_encryption();
        encryption.encryption_v = 4;
        encryption.cf_stream = EncryptionMode::Aes128;
        let mut key_derivation = encryption.clone();
        let key: [u8; 16] = key_derivation
            .key_for_object(object_ref, true)
            .try_into()
            .expect("V4 AES object key");
        let plaintext = b"V4 /StmF AES plaintext";
        let mut ciphertext = plaintext.to_vec();
        crate::encryption::standard::encrypt_cipher_bytes(
            &mut ciphertext,
            crate::encryption::standard::StringEncryptCipher::Aes128 { key: &key },
            &[0x5a; 16],
        )
        .expect("build AES ciphertext");
        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    #[test]
    fn piping_an_encrypted_v5_aes256_default_stream_delivers_plaintext() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = authenticated_v5_aes256_encryption();
        let mut key_derivation = encryption.clone();
        let key: [u8; 32] = key_derivation
            .key_for_object(object_ref, true)
            .try_into()
            .expect("V5 AES object key");
        let plaintext = b"V5 /StmF AES-256 plaintext";
        let mut ciphertext = plaintext.to_vec();
        crate::encryption::standard::encrypt_cipher_bytes(
            &mut ciphertext,
            crate::encryption::standard::StringEncryptCipher::Aes256 { key: &key },
            &[0x5a; 16],
        )
        .expect("build AES-256 ciphertext");
        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    /// qpdf's object-key cache is shared by `decryptString` and
    /// `decryptStream`, and is keyed by object/generation without `use_aes`
    /// (`QPDF_encryption.cc:955-974,1008,1135`). A string encountered first
    /// therefore makes an AES stream on the same object use the string's
    /// unsalted RC4 key.
    #[test]
    fn string_then_stream_share_qpdfs_object_key_cache() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = v4_encryption(EncryptionMode::Aes128);
        encryption.cf_string = EncryptionMode::Rc4;

        let mut oracle_key_state = encryption.clone();
        let cached_string_key = oracle_key_state.key_for_object(object_ref, false).to_vec();
        let plaintext = b"AES stream with the string's cached key";
        let key: [u8; 16] = cached_string_key.clone().try_into().expect("V4 object key");
        let mut ciphertext = plaintext.to_vec();
        crate::encryption::standard::encrypt_cipher_bytes(
            &mut ciphertext,
            crate::encryption::standard::StringEncryptCipher::Aes128 { key: &key },
            &[0x5a; 16],
        )
        .expect("build AES ciphertext with qpdf's cached key");

        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let mut encrypted_string = b"string first".to_vec();
        crate::encryption::rc4::Rc4::new(&cached_string_key)
            .expect("RC4 object key")
            .process_in_place(&mut encrypted_string);
        resolver
            .encryption_parameters()
            .borrow_mut()
            .as_mut()
            .expect("encryption state")
            .decrypt_object_string(object_ref, &mut encrypted_string, Some(false))
            .expect("decrypt string");
        assert_eq!(encrypted_string, b"string first");

        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    /// The same qpdf cache rule is order-sensitive in the other direction:
    /// an AES stream encountered first makes an RC4 string on the same object
    /// use the stream's salted key.
    #[test]
    fn stream_then_string_share_qpdfs_object_key_cache() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = v4_encryption(EncryptionMode::Aes128);
        encryption.cf_string = EncryptionMode::Rc4;

        let mut oracle_key_state = encryption.clone();
        let cached_stream_key = oracle_key_state.key_for_object(object_ref, true).to_vec();
        let key: [u8; 16] = cached_stream_key
            .clone()
            .try_into()
            .expect("V4 AES object key");
        let stream_plaintext = b"stream first";
        let mut stream_ciphertext = stream_plaintext.to_vec();
        crate::encryption::standard::encrypt_cipher_bytes(
            &mut stream_ciphertext,
            crate::encryption::standard::StringEncryptCipher::Aes128 { key: &key },
            &[0x5a; 16],
        )
        .expect("build AES ciphertext");

        let resolver = resolver_over(stream_ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                stream_ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), stream_plaintext);

        let string_plaintext = b"RC4 string with the stream's cached key";
        let mut string_ciphertext = string_plaintext.to_vec();
        crate::encryption::rc4::Rc4::new(&cached_stream_key)
            .expect("cached object key")
            .process_in_place(&mut string_ciphertext);
        resolver
            .encryption_parameters()
            .borrow_mut()
            .as_mut()
            .expect("encryption state")
            .decrypt_object_string(object_ref, &mut string_ciphertext, Some(false))
            .expect("decrypt string");
        assert_eq!(string_ciphertext, string_plaintext);
    }

    /// qpdf warns at the source's last offset, switches the shared `/StmF`
    /// state to AES, and consequently emits that warning only once
    /// (`QPDF_encryption.cc:1121-1133`).
    #[test]
    fn an_unknown_v4_default_stream_filter_warns_once_then_decrypts_as_aes() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = authenticated_v2_rc4_encryption();
        encryption.encryption_v = 4;
        encryption.cf_stream = EncryptionMode::Unknown;
        let mut key_derivation = encryption.clone();
        let key: [u8; 16] = key_derivation
            .key_for_object(object_ref, true)
            .try_into()
            .expect("V4 AES object key");
        let plaintext = b"unknown /StmF defaults to AES";
        let mut ciphertext = plaintext.to_vec();
        crate::encryption::standard::encrypt_cipher_bytes(
            &mut ciphertext,
            crate::encryption::standard::StringEncryptCipher::Aes128 { key: &key },
            &[0x5a; 16],
        )
        .expect("build AES ciphertext");
        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![]);

        for _ in 0..2 {
            let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
            assert!(resolver
                .pipe_stream_data(
                    object_ref,
                    0,
                    ciphertext.len(),
                    &dict,
                    &mut sink,
                    false,
                    false,
                )
                .expect("decryptStream preparation"));
            assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
        }

        let diagnostics = resolver.repair_diagnostics();
        assert_eq!(
            diagnostics
                .entries()
                .iter()
                .map(|diagnostic| (diagnostic.message.as_str(), diagnostic.offset))
                .collect::<Vec<_>>(),
            vec![(
                "unknown encryption filter for streams (check /StmF from /Encrypt dictionary); \
                 streams may be decrypted improperly",
                Some(0),
            )]
        );
        assert_eq!(
            resolver
                .encryption_parameters()
                .borrow()
                .as_ref()
                .expect("encryption retained")
                .cf_stream,
            EncryptionMode::Aes128
        );
    }

    #[test]
    fn unknown_stream_filter_warning_delivery_failure_propagates() {
        let resolver = resolver_over(Vec::new());
        *resolver.encryption_parameters().borrow_mut() =
            Some(v4_encryption(EncryptionMode::Unknown));
        let input = resolver.stream_input();
        let encryption_parameters = resolver.encryption_parameters();
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let warning_sink = FailingStreamWarningResolver;
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
        warning_sink
            .resolve_indirect(
                ObjectRef::new(4, 0),
                &ObjectHandle::new_indirect_unresolved(ObjectRef::new(4, 0), 0),
            )
            .expect("the warning sink resolver is otherwise a no-op");

        let error = pipe_stream_data_from_input(
            &input,
            &encryption_parameters,
            &warning_sink,
            None,
            ObjectRef::new(4, 0),
            0,
            0,
            &dict,
            &mut sink,
            false,
            false,
        )
        .expect_err("warning sink failure must propagate");
        assert!(matches!(error, Error::Internal(message)
            if message == "stream warning sink failed"));
        assert_eq!(
            resolver
                .encryption_parameters()
                .borrow()
                .as_ref()
                .expect("encryption state")
                .cf_stream,
            EncryptionMode::Unknown,
            "qpdf commits the unknown-filter fallback only after warning delivery"
        );

        let mut retry_sink = crate::pipeline::buffer::Buffer::new("stream data", None);
        assert!(pipe_stream_data_from_input(
            &input,
            &encryption_parameters,
            resolver.as_ref(),
            None,
            ObjectRef::new(4, 0),
            0,
            0,
            &dict,
            &mut retry_sink,
            false,
            false,
        )
        .expect("healthy warning sink should allow the retry"));
        assert_eq!(
            resolver
                .encryption_parameters()
                .borrow()
                .as_ref()
                .expect("encryption state")
                .cf_stream,
            EncryptionMode::Aes128
        );
    }

    /// A bare `/Crypt` only overrides `/StmF` when its dictionary has qpdf's
    /// required `/Type /CryptFilterDecodeParms` marker
    /// (`QPDF_encryption.cc:1067-1074`).
    #[test]
    fn unknown_stream_filter_warning_can_clear_state_before_commit() {
        let resolver = resolver_over(Vec::new());
        *resolver.encryption_parameters().borrow_mut() =
            Some(v4_encryption(EncryptionMode::Unknown));
        let input = resolver.stream_input();
        let encryption_parameters = resolver.encryption_parameters();
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let warning_sink = EncryptionClearingResolver {
            target: resolver.clone(),
        };
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(pipe_stream_data_from_input(
            &input,
            &encryption_parameters,
            &warning_sink,
            None,
            ObjectRef::new(4, 0),
            0,
            0,
            &dict,
            &mut sink,
            false,
            false,
        )
        .expect("a warning sink may clear the shared state before commit"));
        assert!(
            resolver.encryption_parameters().borrow().is_none(),
            "commit must tolerate a state cleared while warning delivery ran"
        );
    }

    #[test]
    fn a_typed_bare_crypt_filter_can_select_identity_over_an_rc4_stmf() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = v4_encryption(EncryptionMode::Rc4);
        let plaintext = b"bare Crypt selects Identity";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                crate::ObjectHandle::name(b"Crypt".to_vec()),
            ),
            (
                b"DecodeParms".to_vec(),
                crypt_filter_decode_params(b"Identity"),
            ),
        ]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), ciphertext);
    }

    /// qpdf walks equally-sized `/Filter` and `/DecodeParms` arrays in order;
    /// every `/Crypt` match overwrites the local method, so the last matching
    /// index supplies the one prepended pipeline stage (`:1077-1094`).
    #[test]
    fn a_paired_crypt_filter_array_selects_its_last_crypt_method() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = v4_encryption(EncryptionMode::Identity);
        encryption
            .crypt_filters
            .insert(b"TestRc4".to_vec(), EncryptionMode::Rc4);
        let plaintext = b"array Crypt selects RC4";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext);
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                crate::ObjectHandle::array(vec![
                    crate::ObjectHandle::name(b"FlateDecode".to_vec()),
                    crate::ObjectHandle::name(b"Crypt".to_vec()),
                    crate::ObjectHandle::name(b"Crypt".to_vec()),
                ]),
            ),
            (
                b"DecodeParms".to_vec(),
                crate::ObjectHandle::array(vec![
                    crate::ObjectHandle::null(),
                    crypt_filter_decode_params(b"Identity"),
                    crypt_params(b"TestRc4"),
                ]),
            ),
        ]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                plaintext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    #[test]
    fn an_untyped_bare_crypt_decode_params_dict_does_not_override_stmf() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = v4_encryption(EncryptionMode::Rc4);
        let plaintext = b"untyped bare Crypt falls back to /StmF";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext);
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                crate::ObjectHandle::name(b"Crypt".to_vec()),
            ),
            (b"DecodeParms".to_vec(), crypt_params(b"Identity")),
        ]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                plaintext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    #[test]
    fn unequal_crypt_filter_arrays_fall_back_to_stmf() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = v4_encryption(EncryptionMode::Identity);
        let plaintext = b"unequal Crypt arrays remain /StmF identity";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                crate::ObjectHandle::array(vec![crate::ObjectHandle::name(b"Crypt".to_vec())]),
            ),
            (b"DecodeParms".to_vec(), crate::ObjectHandle::array(vec![])),
        ]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), ciphertext);
    }

    #[test]
    fn a_v2_crypt_filter_does_not_bypass_the_pre_v4_rc4_gate() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = authenticated_v2_rc4_encryption();
        let plaintext = b"V2 always uses RC4";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext);
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                crate::ObjectHandle::name(b"Crypt".to_vec()),
            ),
            (
                b"DecodeParms".to_vec(),
                crypt_filter_decode_params(b"Identity"),
            ),
        ]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                plaintext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    #[test]
    fn an_unknown_bare_crypt_filter_uses_its_own_warning_source() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = v4_encryption(EncryptionMode::Unknown);
        let mut key_derivation = encryption.clone();
        let key: [u8; 16] = key_derivation
            .key_for_object(object_ref, true)
            .try_into()
            .expect("V4 AES object key");
        let plaintext = b"unknown Crypt falls through to unknown /StmF";
        let mut ciphertext = plaintext.to_vec();
        crate::encryption::standard::encrypt_cipher_bytes(
            &mut ciphertext,
            crate::encryption::standard::StringEncryptCipher::Aes128 { key: &key },
            &[0x5a; 16],
        )
        .expect("build AES ciphertext");
        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                crate::ObjectHandle::name(b"Crypt".to_vec()),
            ),
            (
                b"DecodeParms".to_vec(),
                crypt_filter_decode_params(b"NoSuchCF"),
            ),
        ]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
        let diagnostics = resolver.repair_diagnostics();
        assert_eq!(
            diagnostics
                .entries()
                .iter()
                .map(|diagnostic| (diagnostic.message.as_str(), diagnostic.offset))
                .collect::<Vec<_>>(),
            vec![(
                "unknown encryption filter for streams (check stream's Crypt decode parameters); \
                 streams may be decrypted improperly",
                Some(0),
            )]
        );
    }

    #[test]
    fn cleartext_metadata_without_crypt_skips_the_stmf_stage() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = v4_encryption(EncryptionMode::Rc4);
        encryption.encrypt_metadata = false;
        let plaintext = b"cleartext metadata";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext.clone());
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            crate::ObjectHandle::name(b"Metadata".to_vec()),
        )]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                ciphertext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), ciphertext);
    }

    /// `EncryptMetadata true` disables qpdf's cleartext-Metadata exception,
    /// so a Metadata stream without a local `/Crypt` uses `/StmF`
    /// (`QPDF_encryption.cc:1096-1102`).
    #[test]
    fn metadata_with_encrypt_metadata_true_uses_stmf() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = v4_encryption(EncryptionMode::Rc4);
        encryption.encrypt_metadata = true;
        let plaintext = b"encrypted metadata";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext);
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            crate::ObjectHandle::name(b"Metadata".to_vec()),
        )]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                plaintext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    /// qpdf checks `/Crypt` before applying the cleartext-Metadata fallback.
    /// A local RC4 filter therefore wins over `EncryptMetadata false`
    /// (`QPDF_encryption.cc:1067-1103`).
    #[test]
    fn a_crypt_filter_on_cleartext_metadata_still_decrypts() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = v4_encryption(EncryptionMode::Identity);
        encryption.encrypt_metadata = false;
        encryption
            .crypt_filters
            .insert(b"TestRc4".to_vec(), EncryptionMode::Rc4);
        let plaintext = b"Crypt metadata is not cleartext";
        let ciphertext = rc4_stream_ciphertext(object_ref, plaintext, &encryption);
        let resolver = resolver_over(ciphertext);
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let dict = crate::ObjectHandle::dictionary(vec![
            (
                b"Type".to_vec(),
                crate::ObjectHandle::name(b"Metadata".to_vec()),
            ),
            (
                b"Filter".to_vec(),
                crate::ObjectHandle::name(b"Crypt".to_vec()),
            ),
            (
                b"DecodeParms".to_vec(),
                crypt_filter_decode_params(b"TestRc4"),
            ),
        ]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        assert!(resolver
            .pipe_stream_data(
                object_ref,
                0,
                plaintext.len(),
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation"));
        assert_eq!(sink.take_buffer().expect("buffer"), plaintext);
    }

    /// qpdf throws `damagedPDF(file, "", offset + read, "unexpected EOF
    /// reading stream data")` when the source runs out before `length`
    /// (`libqpdf/QPDF.cc:2498-2500`), catches it as a `QPDFExc`, warns unless
    /// suppressed (`:2505-2509`), and returns false. The offset it attributes
    /// the warning to is where the read stopped, not where it started.
    #[test]
    fn a_source_shorter_than_the_declared_length_warns_and_fails() {
        let source = b"%PDF-1.4\nshort".to_vec();
        let offset = 9i64;
        let available = source.len() - 9;
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        let ok = resolver
            .pipe_stream_data(
                ObjectRef::new(4, 0),
                offset,
                available + 100,
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation");

        assert!(!ok, "a truncated read fails");
        let diagnostics = resolver.repair_diagnostics();
        let entries: Vec<_> = diagnostics
            .entries()
            .iter()
            .map(|d| (d.message.as_str(), d.offset))
            .collect();
        assert_eq!(
            entries,
            [(
                "unexpected EOF reading stream data",
                #[allow(clippy::cast_possible_truncation)]
                Some(offset as u64 + available as u64)
            )]
        );
    }

    /// `suppress_warnings` silences the report but not the failure
    /// (`libqpdf/QPDF.cc:2506`).
    #[test]
    fn suppressed_warnings_still_fail_but_report_nothing() {
        let source = b"%PDF-1.4\nshort".to_vec();
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        let ok = resolver
            .pipe_stream_data(
                ObjectRef::new(4, 0),
                9,
                1_000,
                &dict,
                &mut sink,
                true,
                false,
            )
            .expect("decryptStream preparation");

        assert!(!ok);
        assert!(resolver.repair_diagnostics().entries().is_empty());
    }

    /// A failure downstream of the read is qpdf's `catch (std::exception&)`
    /// arm (`libqpdf/QPDF.cc:2510-2530`): it warns "error decoding stream data
    /// for object N G: `<what>`" at `file->getLastOffset()`, which after a
    /// successful read is where that read began
    /// (`BufferInputSource::read` sets `last_offset = cur_offset`,
    /// `libqpdf/BufferInputSource.cc:128`) — the stream's own offset, not the
    /// position afterwards.
    #[test]
    fn a_failing_sink_warns_against_the_streams_offset() {
        let source = b"%PDF-1.4\npayload".to_vec();
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::test_support::RecordingSink::new(&[1], &[]);

        let ok = resolver
            .pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, false, false)
            .expect("decryptStream preparation");

        assert!(!ok);
        let diagnostics = resolver.repair_diagnostics();
        let entries: Vec<_> = diagnostics
            .entries()
            .iter()
            .map(|d| (d.message.as_str(), d.offset))
            .collect();
        assert_eq!(
            entries,
            [(
                "error decoding stream data for object 4 0: sink write failure 1",
                Some(9)
            )]
        );
    }

    /// `will_retry` adds a second warning at the same position telling the
    /// reader why the data is about to be produced again
    /// (`libqpdf/QPDF.cc:2521-2529`). It is a separate warning, after the
    /// first, not a suffix on it.
    #[test]
    fn a_retrying_caller_is_told_the_stream_will_be_reprocessed() {
        let source = b"%PDF-1.4\npayload".to_vec();
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::test_support::RecordingSink::new(&[1], &[]);

        let ok = resolver
            .pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, false, true)
            .expect("decryptStream preparation");

        assert!(!ok);
        let diagnostics = resolver.repair_diagnostics();
        let messages: Vec<_> = diagnostics
            .entries()
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(
            messages,
            [
                "error decoding stream data for object 4 0: sink write failure 1",
                "stream will be re-processed without filtering to avoid data loss",
            ]
        );
        assert!(diagnostics.entries().iter().all(|d| d.offset == Some(9)));
    }

    #[test]
    fn decoding_warning_sink_failures_propagate_from_each_warning() {
        for fail_at in [1, 2] {
            let resolver =
                resolver_over_with_failing_warning(b"%PDF-1.4\npayload".to_vec(), fail_at);
            let dict = crate::ObjectHandle::dictionary(vec![]);
            let mut sink = crate::pipeline::test_support::RecordingSink::new(&[1], &[]);

            assert!(matches!(
                resolver.pipe_stream_data(
                    ObjectRef::new(4, 0),
                    9,
                    7,
                    &dict,
                    &mut sink,
                    false,
                    true,
                ),
                Err(Error::System(ref message)) if message == &format!("sink write failure {fail_at}")
            ));
            assert_eq!(resolver.repair_diagnostics().entries().len(), fail_at);
        }
    }

    /// qpdf sets `attempted_finish` immediately *before* calling
    /// `pipeline->finish()` (`libqpdf/QPDF.cc:2502-2503`), so the tail that
    /// runs after a failure (`:2531-2537`) only finishes a pipeline that never
    /// got the call. A write failure therefore still finishes the sink once —
    /// and any error from that attempt is swallowed.
    #[test]
    fn a_write_failure_still_finishes_the_sink_once() {
        let source = b"%PDF-1.4\npayload".to_vec();
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        // The recovery finish fails too; qpdf ignores that (`:2534-2536`).
        let mut sink = crate::pipeline::test_support::RecordingSink::new(&[1], &[1]);
        let trace = sink.trace();

        let ok = resolver
            .pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, true, false)
            .expect("decryptStream preparation");

        assert!(!ok);
        let finishes = finish_count(&trace);
        assert_eq!(finishes, 1, "the sink is finished exactly once");
    }

    /// The other half of the same rule: a `finish` that fails has already been
    /// attempted, so the tail must not call it again.
    #[test]
    fn a_finish_failure_is_not_retried() {
        let source = b"%PDF-1.4\npayload".to_vec();
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::test_support::RecordingSink::new(&[], &[1]);
        let trace = sink.trace();

        let ok = resolver
            .pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, true, false)
            .expect("decryptStream preparation");

        assert!(!ok);
        let finishes = finish_count(&trace);
        assert_eq!(
            finishes, 1,
            "a failed finish is not attempted a second time"
        );
    }

    /// An offset past the end reads nothing, so qpdf's short-read throw fires
    /// with `read == 0` and attributes the warning to the requested offset
    /// itself (`libqpdf/QPDF.cc:2498-2500`).
    #[test]
    fn an_offset_past_the_end_warns_against_the_requested_offset() {
        let source = b"%PDF-1.4\npayload".to_vec();
        let past_end = source.len() as i64 + 500;
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        let ok = resolver
            .pipe_stream_data(
                ObjectRef::new(4, 0),
                past_end,
                7,
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation");

        assert!(!ok);
        let diagnostics = resolver.repair_diagnostics();
        let entries: Vec<_> = diagnostics
            .entries()
            .iter()
            .map(|d| (d.message.as_str(), d.offset))
            .collect();
        assert_eq!(
            entries,
            [(
                "unexpected EOF reading stream data",
                #[allow(clippy::cast_sign_loss)]
                Some(past_end as u64)
            )]
        );
    }

    /// A negative offset is not silently unreadable: qpdf's input source
    /// throws `std::logic_error("INTERNAL ERROR: BufferInputSource offset <
    /// 0")` (`libqpdf/BufferInputSource.cc:119-121`), which lands in the
    /// decoding-failure arm and produces a warning. Only the detail after the
    /// colon is flpdf's own, since the text qpdf appends there names a C++
    /// input-source class this port does not have.
    #[test]
    fn a_negative_offset_warns_rather_than_failing_silently() {
        let source = b"%PDF-1.4\npayload".to_vec();
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        let ok = resolver
            .pipe_stream_data(ObjectRef::new(4, 0), -1, 7, &dict, &mut sink, false, false)
            .expect("decryptStream preparation");

        assert!(!ok);
        let messages: Vec<_> = resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|d| d.message.clone())
            .collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with("error decoding stream data for object 4 0: "),
            "{messages:?}"
        );
    }

    /// A zero-length stream is not a failure: qpdf allocates nothing, reads
    /// nothing, writes nothing and finishes (`libqpdf/QPDF.cc:2497-2504`).
    #[test]
    fn a_zero_length_stream_succeeds_and_finishes_the_sink() {
        let source = b"%PDF-1.4\npayload".to_vec();
        let resolver = resolver_over(source);
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        let ok = resolver
            .pipe_stream_data(ObjectRef::new(4, 0), 9, 0, &dict, &mut sink, false, false)
            .expect("decryptStream preparation");

        assert!(ok);
        assert!(sink.take_buffer().expect("buffer").is_empty());
        assert!(resolver.repair_diagnostics().entries().is_empty());
    }

    /// An input source that fails is reported, not swallowed: qpdf's seek and
    /// read both throw and land in the decoding-failure arm
    /// (`libqpdf/QPDF.cc:2510-2520`). Exercised through the same
    /// fault-injecting reader shape the resolution tests use.
    #[test]
    fn an_input_source_failure_is_reported_through_the_decoding_arm() {
        // Reading always fails; seeking fails only in the second case, so one
        // pass exercises a successful seek followed by a failing read and the
        // other stops at the seek.
        struct Broken {
            inner: Cursor<Vec<u8>>,
            fail_seeks: bool,
        }

        impl std::io::Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("read went away"))
            }
        }

        impl std::io::Seek for Broken {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                if self.fail_seeks {
                    return Err(std::io::Error::other("seek went away"));
                }
                self.inner.seek(position)
            }
        }

        for fail_seeks in [false, true] {
            let resolver = ResolverHandle::new_shared(
                Broken {
                    inner: Cursor::new(b"%PDF-1.4\npayload".to_vec()),
                    fail_seeks,
                },
                0,
                BTreeMap::<ObjectRef, XrefEntry>::new(),
                false,
                false, // already_reconstructed
                Diagnostics::default(),
                ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
                0,
            );
            let dict = crate::ObjectHandle::dictionary(vec![]);
            let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

            let ok = resolver
                .pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, false, false)
                .expect("decryptStream preparation");

            assert!(!ok, "seeks={fail_seeks}");
            let messages: Vec<_> = resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|d| d.message.clone())
                .collect();
            assert_eq!(messages.len(), 1, "{messages:?}");
            assert!(
                messages[0].starts_with("error decoding stream data for object 4 0: "),
                "seeks={fail_seeks}: {messages:?}"
            );
        }
    }

    /// qpdf's cleanup tail (`libqpdf/QPDF.cc:2531-2537`) sits after *both*
    /// catch arms, so every failure that never reached `finish()` still gets
    /// one — not just a failing write. A downstream stage that buffers or
    /// releases resources in `finish` would otherwise be left hanging before
    /// the caller retries.
    #[test]
    fn every_failure_before_finish_still_finishes_the_sink_once() {
        // (offset, length): a short read, an offset past the end, and a
        // negative offset. None of them reaches qpdf's `finish()` call.
        for (offset, length) in [(9i64, 1_000usize), (10_000, 7), (-1, 7)] {
            let resolver = resolver_over(b"%PDF-1.4\npayload".to_vec());
            let dict = crate::ObjectHandle::dictionary(vec![]);
            let mut sink = crate::pipeline::test_support::RecordingSink::new(&[], &[]);
            let trace = sink.trace();

            let ok = resolver
                .pipe_stream_data(
                    ObjectRef::new(4, 0),
                    offset,
                    length,
                    &dict,
                    &mut sink,
                    true,
                    false,
                )
                .expect("decryptStream preparation");

            assert!(!ok, "offset={offset} length={length}");
            let finishes = finish_count(&trace);
            assert_eq!(finishes, 1, "offset={offset} length={length}");
        }
    }

    /// A rejected seek is attributed to `getLastOffset()`, which a seek never
    /// updates (`include/qpdf/InputSource.hh:88`; neither input source touches
    /// it in `seek`). So the warning names the byte the reader actually last
    /// reached, not the one it was asked to jump to.
    #[test]
    fn a_rejected_seek_is_attributed_to_the_last_read_not_the_requested_target() {
        struct Fickle {
            inner: Cursor<Vec<u8>>,
            fail_seeks: bool,
        }

        impl std::io::Read for Fickle {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.inner.read(buf)
            }
        }

        impl std::io::Seek for Fickle {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                if self.fail_seeks {
                    return Err(std::io::Error::other("seek went away"));
                }
                self.inner.seek(position)
            }
        }

        let resolver = ResolverHandle::new_shared(
            Fickle {
                inner: Cursor::new(b"%PDF-1.4\npayload".to_vec()),
                fail_seeks: false,
            },
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        let dict = crate::ObjectHandle::dictionary(vec![]);

        // One good pipe, so the last read is at offset 9.
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
        assert!(resolver
            .pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, false, false)
            .expect("decryptStream preparation"));

        resolver.with_reader_mut(|reader| reader.fail_seeks = true);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
        assert!(!resolver
            .pipe_stream_data(
                ObjectRef::new(5, 0),
                1_000,
                7,
                &dict,
                &mut sink,
                false,
                false
            )
            .expect("decryptStream preparation"));

        let offsets: Vec<_> = resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|d| d.offset)
            .collect();
        assert_eq!(offsets, [Some(9)], "not the requested 1000");
    }

    /// qpdf allocates the declared length with `make_unique<char[]>`, whose
    /// `std::bad_alloc` its decoding-failure arm reports
    /// (`libqpdf/QPDF.cc:2497`, `:2510-2520`). An infallible allocation would
    /// abort the process on a hostile length instead of warning.
    #[test]
    fn a_length_too_large_to_allocate_warns_rather_than_aborting() {
        let resolver = resolver_over(b"%PDF-1.4\npayload".to_vec());
        let dict = crate::ObjectHandle::dictionary(vec![]);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

        let ok = resolver
            .pipe_stream_data(
                ObjectRef::new(4, 0),
                9,
                usize::MAX,
                &dict,
                &mut sink,
                false,
                false,
            )
            .expect("decryptStream preparation");

        assert!(!ok);
        let messages: Vec<_> = resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|d| d.message.clone())
            .collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with("error decoding stream data for object 4 0: cannot allocate "),
            "{messages:?}"
        );
    }

    /// AC6 case 1: a document with no `/Encrypt` entry authenticates
    /// (`Pdf::authenticate_if_encrypted` runs and returns early) and reports
    /// no encryption parameters through the resolver-side handle — the
    /// `encryption_initialized == true, encrypted == false` qpdf state,
    /// pinned separately from
    /// `a_resolver_with_no_authentication_attempted_reports_no_encryption_parameters`'s
    /// `encryption_initialized == false`, even though both observe `None`.
    #[test]
    fn an_unencrypted_document_reports_no_encryption_parameters() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        assert!(pdf.resolver.encryption_parameters().borrow().is_none());
    }

    /// AC6 case 2: an RC4-encrypted document authenticates through `Pdf`,
    /// and the resolver-side handle observes what that authentication wrote
    /// — proving `Pdf::encryption` and `ResolverCore::encryption_parameters`
    /// are the same allocation, not two independently-written copies. (A
    /// design where `Pdf` held its own separate cell would also authenticate
    /// successfully but leave this resolver-side read at `None`.)
    #[test]
    fn an_rc4_document_reports_its_encryption_parameters_through_the_shared_cell() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v2-rc4-128-r3.pdf"
        );
        let bytes = std::fs::read(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v2-rc4-128-r3.pdf");
        let options = crate::PdfOpenOptions {
            password: b"user-v2".to_vec(),
            ..crate::PdfOpenOptions::default()
        };
        let pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open RC4 fixture");
        let cell = pdf.resolver.encryption_parameters();
        let guard = cell.borrow();
        let encryption = guard.as_ref().expect("RC4 fixture must authenticate");
        // `/V 2`, so qpdf never runs `interpretCF` and leaves `cf_stream` at
        // its `EncryptionParameters` constructor default of `e_none`
        // (`libqpdf/QPDF.cc:190`, gated at `libqpdf/QPDF_encryption.cc:860`).
        // RC4 comes from the consumers' `/V >= 4` gate, not from this field.
        assert_eq!(encryption.encryption_v, 2);
        assert_eq!(
            encryption.cf_stream,
            crate::encryption::state::EncryptionMode::Identity
        );
        // RC4-128 derives a 16-byte file key (qpdf Algorithm 2, key length
        // bits / 8). cf_stream and file_key are sibling fields of one
        // EncryptionState behind one Option, so `Some` already proves both
        // arrived through the shared cell together; this assertion instead
        // checks the derived payload itself -- a real consumer (the
        // per-object key derivation a decryptStream port would need) cares
        // about the key's actual length, not just that a key is present.
        assert_eq!(encryption.file_key.len(), 16);
    }

    /// AC6 case 3: same shape, an AES-128 document.
    #[test]
    fn an_aes_document_reports_its_encryption_parameters_through_the_shared_cell() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v4-aes-128-r4.pdf"
        );
        let bytes = std::fs::read(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v4-aes-128-r4.pdf");
        let options = crate::PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..crate::PdfOpenOptions::default()
        };
        let pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open AES fixture");
        let cell = pdf.resolver.encryption_parameters();
        let guard = cell.borrow();
        let encryption = guard.as_ref().expect("AES fixture must authenticate");
        assert_eq!(
            encryption.cf_stream,
            crate::encryption::state::EncryptionMode::Aes128
        );
        assert_eq!(encryption.file_key.len(), 16);
    }

    // This catches a production regression where canonical resolver parsing
    // exposes ciphertext because it has no object-bound StringDecrypter.
    // Removing the decrypter passed to `parse_live_file_object` makes either
    // the top-level or nested string assertion fail.
    #[test]
    fn canonical_resolver_decrypts_strings_at_parse_time() {
        use crate::encryption::EncryptParams;
        use crate::writer::{emit_canonical_pdf, CompressStreams, WriterOptions};

        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut entries: Vec<(u16, usize)> = Vec::new();
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Title (TopSecretTitle) /Metadata << /Label (NestedSecret) >> >>\nendobj\n",
        );
        let startxref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", entries.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for (generation, offset) in &entries {
            bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
                entries.len() + 1
            )
            .as_bytes(),
        );

        let mut plaintext = Pdf::open(Cursor::new(bytes)).expect("open plaintext fixture");
        let mut encrypted = Vec::new();
        emit_canonical_pdf(
            &mut plaintext,
            &mut encrypted,
            &WriterOptions {
                compress_streams: CompressStreams::No,
                encrypt: Some(EncryptParams::v4_aes128(
                    b"user-pw".to_vec(),
                    b"owner-pw".to_vec(),
                )),
                ..WriterOptions::default()
            },
        )
        .expect("V=4 AES128 encrypted write");
        assert!(!encrypted
            .windows(b"TopSecretTitle".len())
            .any(|bytes| bytes == b"TopSecretTitle"));
        assert!(!encrypted
            .windows(b"NestedSecret".len())
            .any(|bytes| bytes == b"NestedSecret"));

        let mut rt = Pdf::open_with_options(
            Cursor::new(encrypted),
            crate::PdfOpenOptions {
                password: b"user-pw".to_vec(),
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("re-open of V=4 output with user-pw");
        let info_ref = rt
            .trailer()
            .try_get_key(b"/Info")
            .expect("read trailer /Info")
            .object_ref()
            .expect("trailer /Info must be a reference");

        let info: ObjectHandle = rt.get_object_handle(info_ref);
        rt.resolve(&info)
            .expect("canonical resolver must resolve /Info");
        let values = info.as_dictionary().expect("/Info must be a dictionary");
        assert_eq!(
            values
                .get(b"/Title".as_slice())
                .and_then(crate::ObjectHandle::as_string),
            Some(b"TopSecretTitle".to_vec())
        );
        assert_eq!(
            values
                .get(b"/Metadata".as_slice())
                .and_then(crate::ObjectHandle::as_dictionary)
                .and_then(|metadata| metadata.get(b"/Label".as_slice()).cloned())
                .and_then(|label| label.as_string()),
            Some(b"NestedSecret".to_vec())
        );
    }

    fn encrypted_info_fixture(
        info_body: &[u8],
        encrypt: crate::encryption::EncryptParams,
    ) -> Vec<u8> {
        use crate::writer::{emit_canonical_pdf, CompressStreams, WriterOptions};

        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut entries: Vec<(u16, usize)> = Vec::new();
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(b"3 0 obj\n");
        bytes.extend_from_slice(info_body);
        bytes.extend_from_slice(b"\nendobj\n");
        let startxref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", entries.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for (generation, offset) in &entries {
            bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
                entries.len() + 1
            )
            .as_bytes(),
        );

        let mut plaintext = Pdf::open(Cursor::new(bytes)).expect("open plaintext fixture");
        let mut encrypted = Vec::new();
        emit_canonical_pdf(
            &mut plaintext,
            &mut encrypted,
            &WriterOptions {
                compress_streams: CompressStreams::No,
                encrypt: Some(encrypt),
                ..WriterOptions::default()
            },
        )
        .expect("encrypted write");
        encrypted
    }

    fn encrypted_stream_fixture(
        encrypt: crate::encryption::EncryptParams,
    ) -> (Vec<u8>, &'static [u8]) {
        use crate::writer::{emit_canonical_pdf, CompressStreams, WriterOptions};

        const PAYLOAD: &[u8] = b"qpdf 11.9.0 decryptStream differential payload";
        let mut stream = format!("3 0 obj\n<< /Length {} >>\nstream\n", PAYLOAD.len()).into_bytes();
        stream.extend_from_slice(PAYLOAD);
        stream.extend_from_slice(b"\nendstream\nendobj\n");
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Probe 3 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n".to_vec(),
            stream,
        ]);

        let mut plaintext = Pdf::open(Cursor::new(bytes)).expect("open plaintext stream fixture");
        let mut encrypted = Vec::new();
        emit_canonical_pdf(
            &mut plaintext,
            &mut encrypted,
            &WriterOptions {
                compress_streams: CompressStreams::No,
                encrypt: Some(encrypt),
                ..WriterOptions::default()
            },
        )
        .expect("encrypted stream write");
        (encrypted, PAYLOAD)
    }

    /// Exact `/usr/bin/qpdf` 11.9.0 differential for the real encrypted-file
    /// path. Each writer-produced RC4/AES stream is independently decrypted
    /// by qpdf's `--raw-stream-data` and by this pipe-time primitive from the
    /// original source offset; both byte sequences must equal the plaintext.
    #[test]
    fn pipe_time_rc4_and_aes_streams_match_pinned_qpdf_11_9_0() {
        let version = match Command::new("/usr/bin/qpdf").arg("--version").output() {
            Ok(version) => version,
            // cov:ignore-start: CI provides the pinned qpdf binary; this is a developer-host skip
            Err(error) => {
                eprintln!(
                    "/usr/bin/qpdf unavailable; skipping decryptStream differential: {error}"
                );
                return;
            } // cov:ignore-end
        };
        let first_line = String::from_utf8_lossy(&version.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        if first_line != "qpdf version 11.9.0" {
            // cov:ignore-start: CI provides exactly qpdf 11.9.0; this is a developer-host skip
            eprintln!(
                "expected /usr/bin/qpdf 11.9.0; skipping decryptStream differential: {first_line}"
            );
            return;
            // cov:ignore-end
        }

        for (name, encrypt) in [
            (
                "rc4-v2",
                crate::encryption::EncryptParams::rc4(
                    crate::encryption::EncryptMethod::V2Rc4128,
                    b"user-pw",
                    b"owner-pw",
                ),
            ),
            (
                "aes128-v4",
                crate::encryption::EncryptParams::v4_aes128(b"user-pw", b"owner-pw"),
            ),
            (
                "aes256-v5",
                crate::encryption::EncryptParams::v5_r6(b"user-pw", b"owner-pw"),
            ),
        ] {
            let (encrypted, plaintext) = encrypted_stream_fixture(encrypt);
            let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
            let path = directory.path().join(format!("{name}.pdf"));
            fs::write(&path, &encrypted).expect("write encrypted stream fixture");

            let qpdf = Command::new("/usr/bin/qpdf")
                .arg("--password=user-pw")
                .arg("--show-object=3")
                .arg("--raw-stream-data")
                .arg(&path)
                .output()
                .expect("run pinned qpdf stream probe");
            if !qpdf.status.success() {
                // cov:ignore-start: failure-only qpdf diagnostic
                panic!(
                    "{name}: qpdf stream probe failed:\n{}",
                    String::from_utf8_lossy(&qpdf.stderr)
                );
                // cov:ignore-end
            }
            assert_eq!(qpdf.stdout, plaintext, "{name}: qpdf oracle bytes");

            let mut pdf = Pdf::open_with_options(
                Cursor::new(encrypted),
                crate::PdfOpenOptions {
                    password: b"user-pw".to_vec(),
                    ..crate::PdfOpenOptions::default()
                },
            )
            .expect("open encrypted stream fixture");
            let object_ref = ObjectRef::new(3, 0);
            let stream: ObjectHandle = pdf.get_object_handle(object_ref);
            pdf.resolve(&stream)
                .expect("resolve encrypted stream handle");
            let offset = stream.get_parsed_offset();
            let dict = stream.as_stream_dict().expect("stream dictionary");
            let length = usize::try_from(
                dict.try_get_key(b"/Length")
                    .expect("resolve /Length")
                    .try_as_integer()
                    .expect("inspect /Length")
                    .expect("integer /Length"),
            )
            .expect("non-negative stream length");
            let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
            assert!(pdf
                .resolver
                .pipe_stream_data(object_ref, offset, length, &dict, &mut sink, false, false,)
                .expect("decryptStream preparation"));
            assert_eq!(
                sink.take_buffer().expect("flpdf stream bytes"),
                qpdf.stdout,
                "{name}: flpdf and qpdf decrypted bytes"
            );
        }
    }

    fn canonical_info_dictionary(
        bytes: Vec<u8>,
    ) -> (
        ObjectRef,
        std::collections::BTreeMap<Vec<u8>, crate::ObjectHandle>,
    ) {
        let mut pdf = Pdf::open_with_options(
            Cursor::new(bytes),
            crate::PdfOpenOptions {
                password: b"user-pw".to_vec(),
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open encrypted fixture");
        let info_ref = pdf
            .trailer()
            .try_get_key(b"/Info")
            .expect("read trailer /Info")
            .object_ref()
            .expect("trailer /Info must be a reference");
        let info: ObjectHandle = pdf.get_object_handle(info_ref);
        pdf.resolve(&info)
            .expect("canonical resolver must resolve /Info");
        (
            info_ref,
            info.as_dictionary().expect("/Info must be a dictionary"),
        )
    }

    fn qpdf_show_object(path: &std::path::Path, object_ref: ObjectRef) -> String {
        let qpdf = Command::new("qpdf")
            .arg("--password=user-pw")
            .arg(format!(
                "--show-object={},{}",
                object_ref.number, object_ref.generation
            ))
            .arg(path)
            .output()
            .expect("run pinned qpdf");
        assert!(
            qpdf.status.success(),
            "qpdf --show-object failed:\nstdout:\n{}\nstderr:\n{}", // cov:ignore: failure-only qpdf diagnostic
            String::from_utf8_lossy(&qpdf.stdout), // cov:ignore: failure-only qpdf diagnostic
            String::from_utf8_lossy(&qpdf.stderr)  // cov:ignore: failure-only qpdf diagnostic
        );
        String::from_utf8(qpdf.stdout).expect("qpdf object display is text")
    }

    fn qpdf_contents_bytes(object: &str) -> Vec<u8> {
        if let Some((_, after_contents)) = object.split_once("/Contents <") {
            let (hex, _) = after_contents
                .split_once('>')
                .expect("qpdf contents hex must terminate");
            assert_eq!(hex.len() % 2, 0, "qpdf emitted an even-length hex string");
            return hex
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    u8::from_str_radix(std::str::from_utf8(pair).expect("qpdf hex is ASCII"), 16)
                        .expect("qpdf contents must be hexadecimal")
                })
                .collect();
        }

        let (_, after_contents) = object
            .split_once("/Contents (")
            .expect("qpdf must display signature contents as a string or hex");
        let (literal, _) = after_contents
            .split_once(')')
            .expect("qpdf contents literal must terminate");
        literal.as_bytes().to_vec()
    }

    #[test]
    fn qpdf_contents_bytes_accepts_hex_and_literal_forms() {
        assert_eq!(
            qpdf_contents_bytes("<< /Contents <000102ff> >>"),
            vec![0, 1, 2, 255]
        );
        assert_eq!(qpdf_contents_bytes("<< /Contents (literal) >>"), b"literal");
    }

    fn aes128_encryption_state() -> crate::encryption::state::EncryptionState {
        let encrypted = encrypted_info_fixture(
            b"<< /Title (TopSecretTitle) >>",
            crate::encryption::EncryptParams::v4_aes128(b"user-pw", b"owner-pw"),
        );
        // cov:ignore-start: encrypted fixture construction is a shared test precondition; only its success path is meaningful to the parser regressions
        let pdf = Pdf::open_with_options(
            Cursor::new(encrypted),
            crate::PdfOpenOptions {
                password: b"user-pw".to_vec(),
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open encrypted fixture");
        // cov:ignore-end
        let encryption_parameters = pdf.resolver.encryption_parameters();
        let encryption = encryption_parameters.borrow();
        encryption
            .as_ref()
            .cloned()
            .expect("encrypted fixture has encryption parameters")
    }

    // This catches a production regression where the resolver adapter accepts
    // a string cipher failure and returns ciphertext. Replacing the parser
    // callback's `?` with recovery makes this resolution succeed instead.
    //
    // The failure has to come from key derivation rather than from the
    // ciphertext: `Pl_AES_PDF` never rejects a payload for its length or its
    // padding (see the lenient-acceptance test below), so a short file key —
    // which leaves `compute_data_key` returning something that is neither an
    // AES-128 nor an AES-256 key — is what a string cipher failure looks like.
    #[test]
    fn canonical_resolver_propagates_string_decryption_errors() {
        let resolver = ResolverHandle::new_shared(
            Cursor::new(b"1 0 obj\n(bad AES ciphertext)\nendobj\n".to_vec()),
            0,
            BTreeMap::from([(ObjectRef::new(1, 0), XrefEntry::Uncompressed { offset: 0 })]),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        let mut state = aes128_encryption_state();
        state.file_key.truncate(5);
        *resolver.encryption_parameters().borrow_mut() = Some(state);

        let error = resolver
            .read_object_at_offset(0, ObjectRef::new(1, 0))
            .expect_err("an underivable AES object key must fail object parsing");

        assert!(matches!(error, Error::Encrypted(_)), "got {error:?}");
    }

    /// qpdf's `decryptString` runs the stored bytes through `Pl_AES_PDF` into a
    /// `Pl_Buffer` (`libqpdf/QPDF_encryption.cc:1013-1021`) and only converts a
    /// `std::runtime_error` into a damaged-PDF error (`:1097-1100`). The stage
    /// raises neither for a payload that is not a whole number of blocks nor
    /// for one whose trailer is not valid padding: it zero-pads the tail
    /// (`libqpdf/Pl_AES_PDF.cc:107-118`) and leaves an implausible trailer
    /// alone (`:183-196`). So a malformed AES string resolves rather than
    /// failing, and the leading block is consumed as the vector.
    #[test]
    fn canonical_resolver_accepts_malformed_aes_strings_like_qpdf() {
        let resolver = ResolverHandle::new_shared(
            Cursor::new(b"1 0 obj\n(bad AES ciphertext)\nendobj\n".to_vec()),
            0,
            BTreeMap::from([(ObjectRef::new(1, 0), XrefEntry::Uncompressed { offset: 0 })]),
            false,
            false, // already_reconstructed
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        *resolver.encryption_parameters().borrow_mut() = Some(aes128_encryption_state());

        let (object, _) = resolver
            .read_object_at_offset(0, ObjectRef::new(1, 0))
            .expect("a malformed AES string is tolerated, not rejected");

        // "bad AES ciphertext" is 18 bytes: the leading 16 are consumed as the
        // vector and the remaining 2 are zero-padded up to a block, so one
        // block of plaintext comes back. The bytes themselves vary with the
        // fixture's randomly generated `/O`/`/U` (and therefore file key), so
        // this cannot pin an exact length: `Pl_AES_PDF`'s lenient unpad also
        // strips a trailing byte whenever it happens to look like valid
        // PKCS#7 padding (most commonly a lone `0x01`, ~1/256 of random
        // blocks), which this same tolerant behavior requires accepting, not
        // rejecting. Assert only the tolerance property itself -- resolution
        // succeeded and returned plaintext -- not a specific byte count.
        assert!(
            matches!(&object, ObjectValue::String(bytes) if !bytes.is_empty()),
            "expected leniently decrypted plaintext, got {object:?}"
        );
    }

    // This catches a production regression where canonical parsing omits the
    // qpdf unknown-`/StrF` warning, or emits it for every string token. The
    // filter is changed after successful authentication so the real parser
    // callback must take qpdf's Unknown -> AES fallback and reset the mode.
    #[test]
    fn canonical_resolver_warns_once_for_an_unknown_string_filter() {
        let encrypted = encrypted_info_fixture(
            b"<< /Title (TopSecretTitle) /Metadata << /Label (NestedSecret) >> >>",
            crate::encryption::EncryptParams::v4_aes128(b"user-pw", b"owner-pw"),
        );
        let mut pdf = Pdf::open_with_options(
            Cursor::new(encrypted),
            crate::PdfOpenOptions {
                password: b"user-pw".to_vec(),
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open encrypted fixture");
        let encryption_parameters = pdf.resolver.encryption_parameters();
        encryption_parameters
            .borrow_mut()
            .as_mut()
            .expect("encryption parameters")
            .cf_string = crate::encryption::state::EncryptionMode::Unknown;

        let info_ref = pdf
            .trailer()
            .try_get_key(b"/Info")
            .expect("read trailer /Info")
            .object_ref()
            .expect("trailer /Info must be a reference");
        let info: ObjectHandle = pdf.get_object_handle(info_ref);
        pdf.resolve(&info)
            .expect("canonical resolver must decrypt with qpdf's AES fallback");
        assert_eq!(
            info.as_dictionary()
                .and_then(|values| values.get(b"/Title".as_slice()).cloned())
                .and_then(|title| title.as_string()),
            Some(b"TopSecretTitle".to_vec()) // cov:ignore: llvm maps the assertion's mismatch-only diagnostic region to this expected value
        );
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .filter(|entry| entry
                    .message
                    .contains("unknown encryption filter for strings"))
                .count(),
            1,
            "qpdf rewrites the unknown string filter after its first warning"
        );
    }

    #[test]
    fn unknown_string_filter_warning_sink_failure_propagates() {
        let encrypted = encrypted_info_fixture(
            b"<< /Title (TopSecretTitle) >>",
            crate::encryption::EncryptParams::v4_aes128(b"user-pw", b"owner-pw"),
        );
        let mut pdf = Pdf::open_with_options(
            Cursor::new(encrypted),
            crate::PdfOpenOptions {
                password: b"user-pw".to_vec(),
                ..crate::PdfOpenOptions::default()
            },
        )
        .unwrap();
        pdf.resolver
            .encryption_parameters()
            .borrow_mut()
            .as_mut()
            .unwrap()
            .cf_string = crate::encryption::state::EncryptionMode::Unknown;
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(logger);
        let info_ref = pdf
            .trailer()
            .try_get_key(b"/Info")
            .unwrap()
            .object_ref()
            .unwrap();

        let info: ObjectHandle = pdf.get_object_handle(info_ref);
        assert!(matches!(
            pdf.resolve(&info),
            Err(Error::System(ref message)) if message == "sink write failure 1"
        ));
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
        assert_eq!(
            pdf.resolver
                .encryption_parameters()
                .borrow()
                .as_ref()
                .expect("encryption state")
                .cf_string,
            crate::encryption::state::EncryptionMode::Unknown,
            "qpdf commits the unknown-filter fallback only after warning delivery"
        );

        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let retry_logger = crate::QPDFLogger::create();
        retry_logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        pdf.set_logger(retry_logger);
        pdf.resolve(&info)
            .expect("healthy warning sink should allow the retry");
        assert_eq!(
            pdf.resolver
                .encryption_parameters()
                .borrow()
                .as_ref()
                .expect("encryption state")
                .cf_string,
            crate::encryption::state::EncryptionMode::Aes128
        );
        let warning_output = output.lock().unwrap();
        assert_eq!(
            warning_output
                .windows(b"unknown encryption filter for strings".len())
                .filter(|window| *window == b"unknown encryption filter for strings")
                .count(),
            1
        );
    }

    // This catches a production regression where a cipher-mode dispatch
    // selects the wrong object key or bypasses nested parser tokens. The
    // qpdf command independently parses the exact encrypted object that the
    // canonical resolver dereferences; disabling the callback makes flpdf's
    // plaintext assertions fail while qpdf continues to show the literals.
    #[test]
    fn canonical_resolver_string_ciphers_match_pinned_qpdf() {
        use crate::encryption::{EncryptMethod, EncryptParams};

        // cov:ignore-start: CI has pinned qpdf; this fallback is for developer hosts only.
        if Command::new("qpdf").arg("--version").output().is_err() {
            eprintln!("qpdf not available; skipping string decryption differential");
            return;
        }
        // cov:ignore-end

        for (name, encrypt) in [
            (
                "rc4",
                EncryptParams::rc4(EncryptMethod::V2Rc4128, b"user-pw", b"owner-pw"),
            ),
            ("aes128", EncryptParams::v4_aes128(b"user-pw", b"owner-pw")),
            ("aes256", EncryptParams::v5_r6(b"user-pw", b"owner-pw")),
        ] {
            let encrypted = encrypted_info_fixture(
                b"<< /Title (TopSecretTitle) /Metadata << /Label (NestedSecret) >> >>",
                encrypt,
            );
            let (info_ref, values) = canonical_info_dictionary(encrypted.clone());
            assert_eq!(
                values
                    .get(b"/Title".as_slice())
                    .and_then(crate::ObjectHandle::as_string),
                Some(b"TopSecretTitle".to_vec()),
                "{name}: canonical resolver title"
            );
            assert_eq!(
                values
                    .get(b"/Metadata".as_slice())
                    .and_then(crate::ObjectHandle::as_dictionary)
                    .and_then(|metadata| metadata.get(b"/Label".as_slice()).cloned())
                    .and_then(|label| label.as_string()),
                Some(b"NestedSecret".to_vec()),
                "{name}: canonical resolver nested label"
            );

            let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
            let path = directory.path().join(format!("{name}.pdf"));
            fs::write(&path, encrypted).expect("write encrypted qpdf fixture");
            let qpdf = qpdf_show_object(&path, info_ref);
            assert!(
                qpdf.contains("TopSecretTitle"),
                "{name}: qpdf object:\n{qpdf}"
            );
            assert!(
                qpdf.contains("NestedSecret"),
                "{name}: qpdf object:\n{qpdf}"
            );
        }
    }

    // This catches a production regression where the parser loses the raw
    // signature Contents value after it has recognized the whole dictionary.
    // qpdf's writer leaves this field unencrypted and its textual unparser
    // chooses hex, which lets the test compare the preserved bytes directly.
    #[test]
    fn canonical_resolver_signature_contents_matches_pinned_qpdf() {
        // cov:ignore-start: CI has pinned qpdf; this fallback is for developer hosts only.
        if Command::new("qpdf").arg("--version").output().is_err() {
            eprintln!("qpdf not available; skipping signature string differential");
            return;
        }
        // cov:ignore-end

        let encrypted = encrypted_info_fixture(
            b"<< /Type /Sig /ByteRange [0 10 20 30] /Contents (SignatureCipher) /Reason (ReasonPlain) >>",
            crate::encryption::EncryptParams::v4_aes128(b"user-pw", b"owner-pw"),
        );
        let (info_ref, values) = canonical_info_dictionary(encrypted.clone());
        assert_eq!(
            values
                .get(b"/Reason".as_slice())
                .and_then(crate::ObjectHandle::as_string),
            Some(b"ReasonPlain".to_vec())
        );
        let contents = values
            .get(b"/Contents".as_slice())
            .and_then(crate::ObjectHandle::as_string)
            .expect("signature /Contents is a string");
        assert_eq!(contents, b"SignatureCipher");

        let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
        let path = directory.path().join("signature.pdf");
        fs::write(&path, encrypted).expect("write encrypted qpdf fixture");
        let qpdf = qpdf_show_object(&path, info_ref);
        assert!(qpdf.contains("ReasonPlain"), "qpdf object:\n{qpdf}");
        assert_eq!(contents, qpdf_contents_bytes(&qpdf));

        let no_byte_range = encrypted_info_fixture(
            b"<< /Type /Sig /Contents (SignatureCipher) >>",
            crate::encryption::EncryptParams::v4_aes128(b"user-pw", b"owner-pw"),
        );
        let (_, values) = canonical_info_dictionary(no_byte_range);
        assert_eq!(
            values
                .get(b"/Contents".as_slice())
                .and_then(crate::ObjectHandle::as_string),
            Some(b"SignatureCipher".to_vec())
        );
    }

    /// The attach itself: a handle vended by a live document must reach that
    /// document's resolver, and now that uncompressed objects are implemented,
    /// come back resolved.
    ///
    /// The failure this still discriminates against is the one that existed
    /// before the attach landed: `Error::Internal("... belongs to a dropped
    /// PDF")` is `try_dereference` failing to upgrade its `Weak`, i.e. no
    /// resolver attached at all. `expect("...")` alone would report that as a
    /// generic failure; naming it keeps the two apart.
    #[test]
    fn a_vended_handle_reaches_its_documents_resolver_rather_than_reporting_a_dropped_pdf() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));

        pdf.resolve(&handle).expect(
            "an attached resolver resolves an uncompressed object; \
             `belongs to a dropped PDF` here would instead mean `get_object_handle` \
             vended a handle whose `Weak` could not be upgraded, i.e. no resolver \
             was attached at all",
        );

        assert!(handle.is_resolved());
        assert_eq!(
            handle
                .as_dictionary()
                .expect("the catalog is a dictionary")
                .get(b"/Type".as_slice())
                .and_then(crate::ObjectHandle::as_name),
            Some(b"Catalog".to_vec())
        );
    }

    /// Attaching a resolver must not disturb `Pdf::drop`'s teardown.
    ///
    /// A handle that outlives its document is destroyed and does *not* error,
    /// because `Pdf::drop` disconnects every registry entry into
    /// `IndirectState::Destroyed` before the `Rc<ResolverHandle>` drops, and
    /// `try_dereference` short-circuits on any non-`NotYetResolved` state
    /// without consulting the resolver. qpdf's `isNull()` accepts `ot_null`,
    /// not `ot_destroyed` (`libqpdf/QPDFObjectHandle.cc:352-356`).
    #[test]
    fn a_handle_outliving_its_document_is_destroyed_not_null() {
        let handle = {
            let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
            pdf.get_object_handle(ObjectRef::new(1, 0))
        };

        handle
            .try_dereference()
            .expect("a destroyed slot is terminal, not an error");
        assert_eq!(
            handle.type_code().expect("type code"),
            14,
            "qpdf ot_destroyed"
        );
        assert!(!handle.is_null());
    }

    /// The resolver cannot outlive the document that owns it, observed through
    /// the input source it owns rather than through an internal count.
    ///
    /// A handle is deliberately kept alive *across* `drop(pdf)` here. If a
    /// vended handle's link back to its document were strong rather than
    /// `Weak`, the surviving handle would keep the `ResolverHandle` alive, the
    /// `ResolverCore` with it, and therefore the `Arc<[u8]>` the document was
    /// reading — so `Arc::strong_count` would stay at 2. Watching it fall to 1
    /// is what makes this a leak test and not a restatement of the types.
    ///
    /// `Rc::strong_count` on the resolver is asserted too, but only while the
    /// document is alive: after the drop there is no `Pdf` left to ask.
    #[test]
    fn a_handle_outliving_its_document_does_not_keep_the_input_source_alive() {
        let bytes: Arc<[u8]> = Arc::from(&minimal_pdf_bytes()[..]);
        let kept = Arc::clone(&bytes);

        let handle = {
            let mut pdf = Pdf::open_mem(bytes).expect("open");
            let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
            assert!(
                pdf.resolver_is_uniquely_owned(),
                "the document must hold the only strong reference to its resolver"
            );
            assert_eq!(
                Arc::strong_count(&kept),
                2,
                "the live document holds the buffer"
            );
            handle
        };

        // The handle is still alive right here — that is the whole point.
        // Disconnect has cleared its document-owned indirect metadata while
        // preserving the shared slot in the terminal `Destroyed` state.
        assert!(handle.is_direct());
        assert_eq!(
            Arc::strong_count(&kept),
            1,
            "a surviving handle must not keep its dropped document's input source alive"
        );
        drop(handle);
    }

    /// Re-entering resolution for a reference already in progress takes qpdf's
    /// loop branch, and the outer resolution's mark survives that inner call.
    ///
    /// The outer resolution is staged by holding a [`ResolveMark`] — the same
    /// production guard [`ResolverHandle::resolve_indirect`] takes — rather
    /// than by a genuinely nested call, because nothing in this slice resolves
    /// far enough to re-enter on its own. Task 5's `/Length` regression drives
    /// the same guard through a real nested resolution.
    ///
    /// Three separate things are asserted, because a guard can get any one of
    /// them wrong on its own:
    ///
    /// 1. the inner call is not an error — qpdf's loop branch `return`s after
    ///    warning and caching null (`libqpdf/QPDF.cc:1706-1712`), it does not
    ///    throw;
    /// 2. the handle reads as null and its parsed offset has been *cleared*,
    ///    qpdf's `updateCache(og, QPDF_Null::create(), -1, -1)` (`:1711`),
    ///    which writes `-1` over whatever the cache entry held. The offset is
    ///    pre-set below so the assertion proves that the null cache update
    ///    clears an existing source offset rather than merely observing the
    ///    freshly vended `NO_PARSED_OFFSET` sentinel;
    /// 3. the mark is *still* held after the inner call returns. qpdf reaches
    ///    its `return` at `:1712` before constructing `ResolveRecorder rr` at
    ///    `:1714`, so the inner call never owns a recorder and never erases
    ///    the outer's entry. A guard that inserted unconditionally and erased
    ///    on drop would clear the outer resolution's mark from underneath it.
    #[test]
    fn a_reference_already_being_resolved_takes_the_loop_branch_and_leaves_the_outer_mark() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let handle: ObjectHandle = pdf.get_object_handle(object_ref);
        handle.set_parsed_offset_if_unset(100);

        let resolver = Rc::clone(&pdf.resolver);
        let outer = ResolveMark::begin(&resolver.core, object_ref)
            .expect("the first mark for a reference must be recorded, not reported as a loop");

        pdf.resolve(&handle)
            .expect("a resolution loop is qpdf's null outcome, not an error");

        assert!(handle.is_null(), "a detected loop resolves to null");
        assert!(
            handle.is_resolved(),
            "and is terminal, so it is not re-read"
        );
        assert_eq!(
            handle.get_parsed_offset(),
            NO_PARSED_OFFSET,
            "qpdf caches the loop null at offset -1, overwriting what was there"
        );
        assert!(
            pdf.resolver.core.borrow().resolving.contains(&object_ref),
            "the inner loop-detecting call must not erase the outer resolution's mark"
        );

        drop(outer);
        assert!(
            !pdf.resolver.core.borrow().resolving.contains(&object_ref),
            "the outer guard must remove its own mark when it goes out of scope"
        );
    }

    /// The mark is removed when the resolution returns an *error*, not only
    /// when it returns successfully.
    ///
    /// A matched insert/remove pair placed only on the success path would fail
    /// this. Left behind, the mark would make the very next attempt at the
    /// same reference report a phantom loop and resolve it to null.
    ///
    /// Driven through an object-stream (type 2) entry whose supposed parent
    /// is not a stream. The canonical class is implemented, so this exercises
    /// the error exit without delegating to the raw-object resolver.
    #[test]
    fn a_resolution_failure_warns_and_resolves_null_without_leaking_in_progress_mark() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        pdf.resolver.insert_xref_entry(
            object_ref,
            crate::XrefEntry::Compressed {
                stream: 9,
                index: 0,
            },
        );
        let handle: ObjectHandle = pdf.get_object_handle(object_ref);

        pdf.resolve(&handle)
            .expect("qpdf catches the stream-shape error and resolves the member to null");
        assert!(handle.is_null());
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("supposed object stream 9 is not a stream")));

        assert!(
            pdf.resolver.core.borrow().resolving.is_empty(),
            "a failed resolution must not leave its reference marked in progress"
        );
    }

    /// A malformed compressed source goes through the canonical resolver's
    /// warning/null path rather than being handed to `Pdf`'s raw-object route.
    #[test]
    fn a_malformed_compressed_class_resolves_to_null_without_the_legacy_route() {
        let object_ref = ObjectRef::new(1, 0);

        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.resolver.insert_xref_entry(
            object_ref,
            crate::XrefEntry::Compressed {
                stream: 9,
                index: 0,
            },
        );
        let handle: ObjectHandle = pdf.get_object_handle(object_ref);

        pdf.resolve(&handle)
            .expect("qpdf resolves a malformed compressed object to null");
        assert!(handle.is_null());
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("supposed object stream 9 is not a stream")));
    }

    #[test]
    fn a_compressed_object_resolves_through_the_canonical_object_stream_path() {
        let (bytes, stream_offset) = compressed_object_stream_fixture();
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let array_ref = ObjectRef::new(8, 0);
        let entries = BTreeMap::from([
            (
                stream_ref,
                XrefEntry::Uncompressed {
                    offset: stream_offset,
                },
            ),
            (
                member_ref,
                XrefEntry::Compressed {
                    stream: stream_ref.number,
                    index: 0,
                },
            ),
            (
                array_ref,
                XrefEntry::Compressed {
                    stream: stream_ref.number,
                    index: 1,
                },
            ),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            entries,
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, b"input.pdf".to_vec()),
            0,
        );

        let member = resolver.get_object_handle(member_ref);
        let member_alias = resolver.get_object_handle(member_ref);
        assert!(member_alias.is_same_object_as(&member));
        member
            .try_dereference()
            .expect("canonical resolver must parse ObjStm members");

        assert_eq!(
            member
                .as_dictionary()
                .and_then(|dict| dict.get(b"/Value".as_slice()).cloned())
                .and_then(|value| value.as_integer()),
            Some(1)
        );
        assert_eq!(member.get_parsed_offset(), 9);
        assert!(member
            .description()
            .windows(b"object 7 0 at offset".len())
            .any(|window| window == b"object 7 0 at offset"));
        assert_eq!(
            member_alias.get_parsed_offset(),
            member.get_parsed_offset(),
            "the pre-vended alias must observe the in-place cache update"
        );
        assert!(member.end_offsets().0 >= 0);
        assert_eq!(
            member.end_offsets(),
            resolver.get_object_handle(stream_ref).end_offsets(),
            "ObjStm members inherit the source stream cache extent"
        );

        let array = resolver.get_object_handle(array_ref);
        array
            .try_dereference()
            .expect("all active members should be cached together");
        let first = array
            .as_array()
            .expect("the second ObjStm member is an array")
            .into_iter()
            .next()
            .expect("array member");
        assert!(first.is_same_object_as(&member));
    }

    #[test]
    fn an_objstm_duplicate_header_keeps_the_last_offset_like_qpdfs_map() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let rejected_first = b"[ 2147483648 0 R ]";
        let accepted_last = b"<< /Value 2 >>";
        // Header offsets are relative to `/First`, so the duplicate's final
        // entry points immediately after the malformed first body.
        let header = format!("7 0 7 {} ", rejected_first.len()).into_bytes();
        let mut stream_data = header.clone();
        stream_data.extend_from_slice(rejected_first);
        stream_data.extend_from_slice(accepted_last);

        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(2)),
            (
                b"First".to_vec(),
                ObjectHandle::integer(header.len() as i64),
            ),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        let member = resolver.get_object_handle(member_ref);
        member
            .try_dereference()
            .expect("the duplicate's final header entry is the one qpdf parses");
        assert_eq!(member.try_get_key(b"/Value").unwrap().as_integer(), Some(2));
        assert!(!resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("2147483648")));
    }

    #[test]
    fn an_object_stream_is_marked_resolved_and_not_parsed_twice() {
        let (bytes, stream_offset) = compressed_object_stream_fixture();
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            BTreeMap::from([
                (
                    stream_ref,
                    XrefEntry::Uncompressed {
                        offset: stream_offset,
                    },
                ),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("first object-stream resolution");
        assert!(
            resolver
                .resolve_object_stream_with_failure_kind(stream_ref.number)
                .is_ok(),
            "the qpdf resolved-object-stream guard is idempotent"
        );
    }

    #[test]
    fn an_object_stream_resolves_a_decode_filter_error_to_null_with_a_warning() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"7 0 << /Value 1 >>".to_vec();
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"UnknownFilter".to_vec()),
            ),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("qpdf catches an object-stream filter error and resolves the member to null");
        assert!(resolver.get_object_handle(member_ref).is_null());
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("getStreamData called on unfilterable stream")));
    }

    #[test]
    fn an_object_stream_resolves_a_codec_failure_to_null_with_a_warning() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"not zlib data".to_vec();
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("qpdf catches a codec failure and resolves the member to null");
        assert!(resolver.get_object_handle(member_ref).is_null());
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("incorrect header check")));
    }

    #[test]
    fn an_object_stream_codec_warning_delivery_failure_still_propagates() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = vec![0x78];
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
        ]);
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        assert!(matches!(
            resolver
                .get_object_handle(member_ref)
                .try_dereference()
                .expect_err("codec warning delivery must remain a caller error"),
            Error::System(message) if message == "sink write failure 1"
        ));
    }

    #[test]
    fn an_object_stream_source_codec_warning_delivery_failure_still_propagates() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = vec![0x78];
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
        ]);
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        let mut source = vec![0];
        source.extend_from_slice(&stream_data);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(source),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, Vec::new()),
            0,
        );
        let stream = resolver.get_object_handle(stream_ref);
        stream.set_resolved(ObjectValue::Stream {
            stream_dict,
            stream_data: None,
            stream_length: stream_data.len(),
            stream_provider: None,
            filter_on_write: true,
        });
        stream.set_parsed_offset_if_unset(1);

        assert!(matches!(
            resolver
                .get_object_handle(member_ref)
                .try_dereference()
                .expect_err("source-backed codec warning delivery must remain a caller error"),
            Error::System(message) if message == "sink write failure 1"
        ));
    }

    #[test]
    fn an_object_stream_with_wrong_type_warns_and_still_resolves() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"7 0 << /Value 1 >>".to_vec();
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"NotObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("qpdf warns for a wrong ObjStm type but continues parsing");
        assert_eq!(
            resolver
                .get_object_handle(member_ref)
                .try_get_key(b"/Value")
                .expect("resolved member dictionary")
                .as_integer(),
            Some(1)
        );
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("supposed object stream 4 has wrong type")));
    }

    #[test]
    fn an_object_stream_operation_error_propagates_after_filter_inspection() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"7 0 << /Value 1 >>".to_vec();
        let (broken_filter, _filter_resolver) =
            crate::object_handle::identity_tests::error_resolving_handle(ObjectRef::new(20, 0));
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
            (b"Filter".to_vec(), broken_filter),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        let error = resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect_err("an operation error in filter inspection must propagate");
        assert!(matches!(error, Error::System(message) if message == "resolver failed"));
    }

    #[test]
    fn an_object_stream_skips_a_member_overridden_by_the_effective_xref() {
        let (bytes, stream_offset) = compressed_object_stream_fixture();
        let stream_ref = ObjectRef::new(4, 0);
        let overridden_ref = ObjectRef::new(7, 0);
        let array_ref = ObjectRef::new(8, 0);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            BTreeMap::from([
                (
                    stream_ref,
                    XrefEntry::Uncompressed {
                        offset: stream_offset,
                    },
                ),
                (overridden_ref, XrefEntry::Uncompressed { offset: 0 }),
                (
                    array_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 1,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );

        resolver
            .get_object_handle(array_ref)
            .try_dereference()
            .expect("the active ObjStm member must still resolve");
        assert!(
            !resolver.get_object_handle(overridden_ref).is_resolved(),
            "an overridden member must not be populated from the object stream"
        );
    }

    #[test]
    fn an_object_stream_reports_eof_for_a_member_offset_out_of_range() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"7 0 << /Value 1 >>".to_vec();
        let expected_eof_offset = stream_data.len();
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(999)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("qpdf catches an out-of-range member offset and resolves it to null");
        assert!(resolver.get_object_handle(member_ref).is_null());
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains(&format!(
                "object stream 4 (object 7 0, offset {expected_eof_offset}): unexpected EOF"
            ))));
    }

    #[test]
    fn an_object_stream_resolves_a_member_parse_error_to_null() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"7 0 [ 2147483648 0 R ]".to_vec();
        let decoded_offset = stream_data
            .windows(b"2147483648".len())
            .position(|window| window == b"2147483648")
            .expect("the fixture must contain the malformed integer");
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let (resolver, output) = named_resolver_with_captured_warnings();
        resolver.insert_xref_entry(stream_ref, XrefEntry::Uncompressed { offset: 1 });
        resolver.insert_xref_entry(
            member_ref,
            XrefEntry::Compressed {
                stream: stream_ref.number,
                index: 0,
            },
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("qpdf catches a member parse error and resolves it to null");
        assert!(resolver.get_object_handle(member_ref).is_null());
        let diagnostics = resolver.repair_diagnostics();
        let warning = diagnostics
            .entries()
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .message
                    .contains("integer out of range converting 2147483648")
            })
            .expect("the caught parse failure must be warned");
        assert_eq!(warning.offset, None);
        assert_eq!(
            warning.message,
            format!(
                "object stream 4 (object 7 0, offset {decoded_offset}): integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type"
            )
        );
        assert_eq!(
            output.lock().unwrap().as_slice(),
            format!(
                "WARNING: input.pdf object stream 4 (object 7 0, offset {decoded_offset}): integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type\n"
            )
            .as_bytes()
        );
    }

    #[test]
    fn canonical_object_stream_diagnostics_keep_decoded_offsets_out_of_source_locations() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"7 0 << /A#zB 1 >>".to_vec();
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let (resolver, output) = named_resolver_with_captured_warnings();
        resolver.insert_xref_entry(stream_ref, XrefEntry::Uncompressed { offset: 1 });
        resolver.insert_xref_entry(
            member_ref,
            XrefEntry::Compressed {
                stream: stream_ref.number,
                index: 0,
            },
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("qpdf preserves the malformed member and warns");

        let diagnostics = resolver.repair_diagnostics();
        assert_eq!(
            diagnostics
                .entries()
                .iter()
                .map(|diagnostic| (diagnostic.offset, diagnostic.message.as_str()))
                .collect::<Vec<_>>(),
            vec![(
                None,
                "object stream 4 (object 7 0, offset 7): name with stray # will not work with PDF >= 1.2"
            )]
        );
        assert_eq!(
            output.lock().unwrap().as_slice(),
            b"WARNING: input.pdf object stream 4 (object 7 0, offset 7): name with stray # will not work with PDF >= 1.2\n"
        );
    }

    #[test]
    fn canonical_object_stream_warning_without_source_description_stays_contextual() {
        let resolver = bare_resolver();
        resolver
            .push_object_stream_warning(
                4,
                ObjectRef::new(7, 0),
                7,
                "name with stray # will not work with PDF >= 1.2",
            )
            .expect("suppressed warning still records its diagnostic");

        assert_eq!(
            resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|diagnostic| (diagnostic.offset, diagnostic.message.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (
                    None,
                    "object stream 4 (object 7 0, offset 7): name with stray # will not work with PDF >= 1.2"
                )
            ]
        );
    }

    #[test]
    fn an_objstm_failure_does_not_overwrite_a_member_cached_before_it() {
        let stream_ref = ObjectRef::new(4, 0);
        let requested_ref = ObjectRef::new(7, 0);
        let malformed_ref = ObjectRef::new(8, 0);
        let valid_member = b"<< /Value 1 >>";
        let malformed_member = b"[ 2147483648 0 R ]";
        let header = format!("7 0 8 {} ", valid_member.len()).into_bytes();
        let mut stream_data = header.clone();
        stream_data.extend_from_slice(valid_member);
        stream_data.extend_from_slice(malformed_member);
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(2)),
            (
                b"First".to_vec(),
                ObjectHandle::integer(header.len() as i64),
            ),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    requested_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
                (
                    malformed_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 1,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        let requested = resolver.get_object_handle(requested_ref);
        requested
            .try_dereference()
            .expect("qpdf warns about a later member and keeps the earlier cache entry");
        assert_eq!(
            requested
                .try_get_key(b"/Value")
                .expect("the earlier member remains a dictionary")
                .as_integer(),
            Some(1)
        );
        assert!(!requested.is_null());
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| {
                diagnostic
                    .message
                    .contains("integer out of range converting 2147483648")
            }));
    }

    #[test]
    fn an_empty_object_stream_member_warns_and_resolves_to_null() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"7 0 endobj".to_vec();
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("qpdf treats an empty ObjStm member as null");
        assert!(resolver.get_object_handle(member_ref).is_null());
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("empty object treated as null")));
    }

    #[test]
    fn an_object_stream_warning_sink_failure_propagates_parser_diagnostics() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_data = b"7 0 << /A#zB 1 >>".to_vec();
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        assert!(matches!(
            resolver
                .get_object_handle(member_ref)
                .try_dereference()
                .expect_err("diagnostic delivery failure must propagate"),
            Error::System(message) if message == "sink write failure 1"
        ));
    }

    #[test]
    fn object_stream_integer_rejects_a_non_integer_metadata_value() {
        let dictionary = ObjectHandle::dictionary(vec![(
            b"N".to_vec(),
            ObjectHandle::name(b"not-an-integer".to_vec()),
        )]);
        let error =
            ResolverHandle::<Cursor<Vec<u8>>>::object_stream_integer(&dictionary, b"/N", "/N")
                .expect_err("object-stream metadata must be integer");
        assert!(matches!(
            error,
            Error::Parse { message, .. } if message == "object stream /N is not an integer"
        ));
    }

    #[test]
    fn stream_recovery_without_a_terminator_treats_the_payload_as_empty() {
        for payload in [b"abc".as_slice(), b"e", b"enx", b"end"] {
            let resolver = resolver_over(payload.to_vec());
            assert_eq!(
                resolver
                    .recover_stream_length(0, ObjectRef::new(1, 0), None)
                    .expect("recovery without a terminator"),
                0,
                "payload {payload:?} must not produce a terminator"
            );
            assert!(resolver
                .repair_diagnostics()
                .entries()
                .iter()
                .any(|entry| entry.message
                    == "(object 1 0, offset 0): unable to recover stream data; treating stream as empty"));
        }
    }

    #[test]
    fn empty_stream_recovery_warning_sink_failure_propagates() {
        let resolver =
            resolver_over_with_failing_warning(b"payload without a framing token".to_vec(), 2);
        assert!(matches!(
            resolver
                .recover_stream_length(0, ObjectRef::new(1, 0), None)
                .expect_err("the empty-recovery warning must reach the sink"),
            Error::System(message) if message == "sink write failure 2"
        ));
    }

    #[test]
    fn stream_failure_warning_ignores_non_parse_errors() {
        let resolver = resolver_over(Vec::new());
        resolver
            .warn_stream_failure(
                &Error::Unsupported("not a parse failure".to_owned()),
                7,
                ObjectRef::new(1, 0),
                None,
            )
            .expect("non-parse failures do not emit stream-recovery warnings");
        assert!(resolver.repair_diagnostics().entries().is_empty());
    }

    #[test]
    fn trailing_object_warning_sink_failure_propagates() {
        let resolver = resolver_over_with_failing_warning(b"1 0 obj\n42\nnot-endobj\n".to_vec(), 1);
        assert!(matches!(
            resolver
                .read_object_at_offset(0, ObjectRef::new(1, 0))
                .expect_err("a warning sink failure must propagate"),
            Error::System(message) if message == "sink write failure 1"
        ));
    }

    #[test]
    fn compressed_member_missing_from_an_object_stream_resolves_to_null() {
        let (bytes, stream_offset) = compressed_object_stream_fixture();
        let stream_ref = ObjectRef::new(4, 0);
        let missing_ref = ObjectRef::new(9, 0);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            BTreeMap::from([
                (
                    stream_ref,
                    XrefEntry::Uncompressed {
                        offset: stream_offset,
                    },
                ),
                (
                    missing_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );

        let missing = resolver.get_object_handle(missing_ref);
        missing
            .try_dereference()
            .expect("a missing compressed member resolves to null");
        assert!(missing.is_resolved());
        assert!(missing.is_null());
    }

    #[test]
    fn an_absent_objstm_header_entry_uses_qpdfs_default_xref_and_warns_when_resolved() {
        let (bytes, stream_offset) = compressed_object_stream_fixture();
        let stream_ref = ObjectRef::new(4, 0);
        let present_ref = ObjectRef::new(7, 0);
        let absent_ref = ObjectRef::new(8, 0);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            BTreeMap::from([
                (
                    stream_ref,
                    XrefEntry::Uncompressed {
                        offset: stream_offset,
                    },
                ),
                (
                    present_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );

        resolver
            .get_object_handle(present_ref)
            .try_dereference()
            .expect("the present member triggers ObjStm header inspection");

        assert_eq!(
            resolver.xref_entries().get(&absent_ref),
            Some(&XrefEntry::Free { next: 0 })
        );
        assert!(!resolver.source_xref_entries().contains_key(&absent_ref));

        let absent = resolver.get_object_handle(absent_ref);
        absent
            .try_dereference()
            .expect("qpdf's default xref entry resolves to null after warning");
        assert!(absent.is_null());
        assert!(resolver
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("object 8/0 has unexpected xref entry type")));
    }

    #[test]
    fn an_objstm_default_xref_warning_delivery_error_propagates() {
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let absent_ref = ObjectRef::new(8, 0);
        let header = b"7 0 8 14 ";
        let mut stream_data = header.to_vec();
        stream_data.extend_from_slice(b"<< /Value 1 >>");
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(2)),
            (
                b"First".to_vec(),
                ObjectHandle::integer(header.len() as i64),
            ),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
        ]);
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, Vec::new()),
            0,
        );
        resolver
            .get_object_handle(stream_ref)
            .set_resolved(ObjectValue::Stream {
                stream_dict,
                stream_data: Some(Rc::new(stream_data)),
                stream_length: 0,
                stream_provider: None,
                filter_on_write: true,
            });

        resolver
            .get_object_handle(member_ref)
            .try_dereference()
            .expect("the present member creates qpdf's default xref entry");
        assert!(matches!(
            resolver
                .get_object_handle(absent_ref)
                .try_dereference()
                .expect_err("default-xref warning delivery must propagate"),
            Error::System(message) if message == "sink write failure 1"
        ));
    }

    #[test]
    fn object_stream_pipeline_errors_are_operation_failures() {
        let error: ObjectStreamResolutionError =
            crate::pipeline::PipelineError::runtime("pipeline failed").into();
        assert!(matches!(
            error,
            ObjectStreamResolutionError::Operation(Error::System(message))
                if message == "pipeline failed"
        ));
    }

    #[test]
    fn a_free_xref_entry_resolves_to_null() {
        let object_ref = ObjectRef::new(9, 0);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(Vec::<u8>::new()),
            0,
            BTreeMap::from([(object_ref, XrefEntry::Free { next: 0 })]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );

        let handle = resolver.get_object_handle(object_ref);
        handle
            .try_dereference()
            .expect("a free xref entry resolves to null");
        assert!(handle.is_resolved());
        assert!(handle.is_null());
    }

    #[test]
    fn a_compressed_object_stream_skips_crypt_after_raw_stream_read() {
        let member_data = b"<< /Value 1 >>";
        let header = b"7 0 ";
        let first = header.len();
        let mut stream_data = header.to_vec();
        stream_data.extend_from_slice(member_data);
        let stream_ref = ObjectRef::new(4, 0);
        let member_ref = ObjectRef::new(7, 0);
        let stream_dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"ObjStm".to_vec())),
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(first as i64)),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(stream_data.len() as i64),
            ),
            (b"Filter".to_vec(), ObjectHandle::name(b"Crypt".to_vec())),
            (
                b"DecodeParms".to_vec(),
                crypt_filter_decode_params(b"StdCF"),
            ),
        ]);
        let encryption = v4_encryption(EncryptionMode::Rc4);
        let ciphertext = rc4_stream_ciphertext(stream_ref, &stream_data, &encryption);
        let mut source = vec![0];
        source.extend_from_slice(&ciphertext);
        let resolver = ResolverHandle::new_shared(
            Cursor::new(source),
            0,
            BTreeMap::from([
                (stream_ref, XrefEntry::Uncompressed { offset: 1 }),
                (
                    member_ref,
                    XrefEntry::Compressed {
                        stream: stream_ref.number,
                        index: 0,
                    },
                ),
            ]),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        *resolver.encryption_parameters().borrow_mut() = Some(encryption);
        let stream = resolver.get_object_handle(stream_ref);
        stream.set_resolved(ObjectValue::Stream {
            stream_dict,
            stream_data: None,
            stream_length: ciphertext.len(),
            stream_provider: None,
            filter_on_write: true,
        });
        stream.set_parsed_offset_if_unset(1);

        let member = resolver.get_object_handle(member_ref);
        member
            .try_dereference()
            .expect("qpdf's already-consumed Crypt stage must not reject ObjStm data");
        assert_eq!(
            member
                .as_dictionary()
                .and_then(|dict| dict.get(b"/Value".as_slice()).cloned())
                .and_then(|value| value.as_integer()),
            Some(1)
        );
    }

    /// A detected loop warns, with qpdf's own message text.
    ///
    /// qpdf: `warn(damagedPDF("", "loop detected resolving object " +
    /// og.unparse(' ')))` (`libqpdf/QPDF.cc:1710`). `og.unparse(' ')`
    /// (`libqpdf/QPDFObjGen.cc:19-22`) is `number + separator + generation`,
    /// so for object 1 generation 0 the message is exactly
    /// `"loop detected resolving object 1 0"` — asserted here as a whole
    /// string, not a `contains`, because the point is the wording and the
    /// space separator rather than the mere presence of a warning.
    ///
    /// The assertion that the collection was empty beforehand matters as much
    /// as the one after it: this fixture opens cleanly, so a warning appearing
    /// at all is attributable to the loop and not to the parse.
    #[test]
    fn a_detected_loop_warns_with_qpdfs_message_text() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let handle: ObjectHandle = pdf.get_object_handle(object_ref);
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "this fixture opens without warnings, so the loop is the only source"
        );

        let resolver = Rc::clone(&pdf.resolver);
        let outer = ResolveMark::begin(&resolver.core, object_ref).expect("first mark");
        pdf.resolve(&handle).expect("a loop is not an error");
        drop(outer);

        let diagnostics = pdf.repair_diagnostics();
        let messages = diagnostics
            .entries()
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, ["loop detected resolving object 1 0"]);
        assert_eq!(
            diagnostics.entries()[0].severity,
            Severity::Warning,
            "qpdf warns and continues here rather than failing the resolution"
        );
        assert_eq!(
            diagnostics.entries()[0].offset,
            None,
            "the resolver tracks no input position in this slice, so qpdf's \
             getLastOffset() has no counterpart to report"
        );
    }

    #[test]
    fn loop_warning_sink_failure_propagates_after_collection() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).unwrap();
        let object_ref = ObjectRef::new(1, 0);
        let handle: ObjectHandle = pdf.get_object_handle(object_ref);
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(logger);

        let resolver = Rc::clone(&pdf.resolver);
        let outer = ResolveMark::begin(&resolver.core, object_ref).unwrap();
        assert!(matches!(
            pdf.resolve(&handle),
            Err(Error::System(ref message)) if message == "sink write failure 1"
        ));
        drop(outer);
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
    }

    /// A warning raised through the resolver and one raised through
    /// `Pdf::push_warning` land in the same collection, in the order they were
    /// raised — qpdf keeps one `m->warnings` (`include/qpdf/QPDF.hh:1475`) and
    /// `QPDF::warn` only ever `push_back`s onto it (`libqpdf/QPDF.cc:490`).
    ///
    /// This is what the sink move bought, and the ordering is the part a
    /// second sink could not have delivered.
    #[test]
    fn resolver_warnings_and_document_warnings_share_one_ordered_collection() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let handle: ObjectHandle = pdf.get_object_handle(object_ref);

        pdf.push_warning("before the loop").unwrap();
        let resolver = Rc::clone(&pdf.resolver);
        let outer = ResolveMark::begin(&resolver.core, object_ref).expect("first mark");
        pdf.resolve(&handle).expect("a loop is not an error");
        drop(outer);
        pdf.push_warning("after the loop").unwrap();

        let diagnostics = pdf.repair_diagnostics();
        let messages = diagnostics
            .entries()
            .iter()
            .map(|entry| entry.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            [
                "before the loop",
                "loop detected resolving object 1 0",
                "after the loop"
            ]
        );
    }

    /// The mark is removed when a resolution *unwinds*, which is the reason
    /// [`ResolveMark`] is a `Drop` guard rather than a matched insert/remove
    /// pair around the body — qpdf gets the same property from
    /// `~ResolveRecorder` (`include/qpdf/QPDF.hh:988-991`).
    ///
    /// The panic stands in for any panic raised between the mark being taken
    /// and the resolution finishing. [`std::panic::AssertUnwindSafe`] is
    /// required, not decorative: without it rustc rejects the closure with
    /// E0277, because the captured `Rc<ResolverHandle<_>>` reaches both the
    /// `UnsafeCell` inside [`ResolverHandle`]'s `RefCell` and the one holding
    /// the `Rc` strong count, neither of which is
    /// [`std::panic::RefUnwindSafe`].
    #[test]
    fn the_in_progress_mark_is_removed_when_a_resolution_unwinds() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _mark = ResolveMark::begin(&pdf.resolver.core, object_ref).expect("first mark");
            panic!("simulated failure part-way through a resolution");
        }));

        assert!(unwound.is_err(), "the body must actually have panicked");
        assert!(
            !pdf.resolver.core.borrow().resolving.contains(&object_ref),
            "an unwind must leave the reference resolvable, not permanently marked in progress"
        );
    }

    /// A three-object document plus a content stream whose `/Length` is an
    /// *indirect* reference, and the object holding that length.
    ///
    /// Object 4's `/Length 5 0 R` is the case `QPDF::readStream` brackets
    /// (`libqpdf/QPDF.cc:1361-1381`): resolving it re-enters the resolver
    /// mid-parse, and object 5 sits *after* object 4 in the file, so the
    /// nested read genuinely moves the input position past the payload.
    fn indirect_length_pdf_bytes() -> Vec<u8> {
        let payload = STREAM_PAYLOAD;
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let mut offsets = Vec::new();
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>\nendobj\n",
        );
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(b"4 0 obj\n<< /Length 5 0 R >>\nstream\n");
        pdf.extend_from_slice(payload);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(format!("5 0 obj\n{}\nendobj\n", payload.len()).as_bytes());

        let xref_start = pdf.len() as u64;
        let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
        for offset in &offsets {
            xref.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    const STREAM_PAYLOAD: &[u8] = b"BT (hi) Tj ET";

    /// A `Cursor` that counts how many times something pulled bytes from it,
    /// so "did not re-read" can be asserted rather than inferred.
    struct CountingReader {
        inner: std::io::Cursor<Vec<u8>>,
        reads: usize,
        seeks: usize,
    }

    impl CountingReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                inner: std::io::Cursor::new(bytes),
                reads: 0,
                seeks: 0,
            }
        }
    }

    impl std::io::Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            self.inner.read(buf)
        }
    }

    impl std::io::Seek for CountingReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.seeks += 1;
            self.inner.seek(position)
        }
    }

    struct RejectRewindReader {
        inner: std::io::Cursor<Vec<u8>>,
        reject_rewind: bool,
    }

    impl std::io::Read for RejectRewindReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl std::io::Seek for RejectRewindReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            if self.reject_rewind && matches!(position, std::io::SeekFrom::Start(0)) {
                return Err(std::io::Error::other("recovery must not rewind the source"));
            }
            self.inner.seek(position)
        }
    }

    /// An uncompressed (xref type 1) object resolves, and a second
    /// `try_dereference` is a no-op that touches the input source not at all.
    ///
    /// The read count is what makes the second half real. `is_resolved()`
    /// alone would pass even if the second call re-read and re-parsed the
    /// object, because it would land on the same value; only counting pulls
    /// distinguishes "cached" from "recomputed". qpdf gets the same property
    /// from `isUnresolved(og)` (`libqpdf/QPDF.cc:1702-1704`).
    #[test]
    fn an_uncompressed_object_resolves_and_a_second_dereference_does_not_re_read() {
        let mut pdf = Pdf::open(CountingReader::new(minimal_pdf_bytes())).expect("open");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
        let before = pdf.resolver.with_reader_mut(|reader| reader.reads);

        pdf.resolve(&handle)
            .expect("an uncompressed object resolves");

        let after_first = pdf.resolver.with_reader_mut(|reader| reader.reads);
        assert!(
            after_first > before,
            "the first dereference must actually have read the input"
        );
        assert_eq!(
            handle
                .as_dictionary()
                .expect("the catalog is a dictionary")
                .get(b"/Type".as_slice())
                .and_then(crate::ObjectHandle::as_name),
            Some(b"Catalog".to_vec())
        );

        pdf.resolve(&handle).expect("a resolved slot is terminal");

        assert_eq!(
            pdf.resolver.with_reader_mut(|reader| reader.reads),
            after_first,
            "a second dereference must not touch the input source again"
        );
    }

    /// A nested `N G R` inside a resolved object is the *same* handle
    /// `Pdf::get_object_handle` vends for that reference.
    ///
    /// This is the property that made the registry move necessary: a
    /// resolver-local second map would satisfy every other assertion in this
    /// file and fail only this one. qpdf gets it from `QPDF::getObject`
    /// (`libqpdf/QPDF.cc:1952-1959`), which the parser calls for exactly this
    /// purpose and which is entry-or-insert over the one `m->obj_cache`.
    ///
    /// Asserted in both directions — child first, then parent first — because
    /// a map that minted on parse but was not consulted by
    /// `get_object_handle` would still pass the second ordering.
    #[test]
    fn a_nested_reference_resolves_to_the_documents_canonical_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");

        let catalog: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&catalog).expect("resolve the catalog");
        let minted_child = catalog
            .as_dictionary()
            .expect("the catalog is a dictionary")
            .get(b"/Pages".as_slice())
            .expect("the catalog has /Pages")
            .clone();
        assert!(
            minted_child.is_same_object_as(&pdf.get_object_handle(ObjectRef::new(2, 0))),
            "a child minted during the parse must be the canonical handle"
        );

        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        let pages: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve(&pages).expect("resolve the page tree");
        let kid = pages
            .as_dictionary()
            .expect("the page tree is a dictionary")
            .get(b"/Kids".as_slice())
            .and_then(crate::ObjectHandle::as_array)
            .expect("/Kids is an array")
            .first()
            .expect("/Kids has one entry")
            .clone();
        assert!(
            kid.is_same_object_as(&page),
            "an already-registered ref must not be re-minted during a parse"
        );
    }

    /// The canonical resolver records the same qpdf parsed offset for a plain
    /// object and for a stream. The stream case is the one that can diverge
    /// quietly: its parsed offset is the data start, not the dictionary's, so
    /// it depends on `validate_stream_line_end` consuming the EOL after
    /// `stream` exactly as qpdf does.
    #[test]
    fn the_canonical_resolver_records_qpdf_offsets_for_plain_and_stream_objects() {
        for object_ref in [ObjectRef::new(1, 0), ObjectRef::new(4, 0)] {
            let mut pdf = Pdf::open_mem_owned(indirect_length_pdf_bytes()).expect("open");
            let handle: ObjectHandle = pdf.get_object_handle(object_ref);
            pdf.resolve(&handle).expect("canonical resolution");
            assert_ne!(
                handle.get_parsed_offset(),
                NO_PARSED_OFFSET,
                "canonical resolution must record an offset for {object_ref:?}"
            );
        }
    }

    /// A stream whose `/Length` is an indirect reference resolves without
    /// retaining its original payload, then reads that payload from the
    /// position the resolver explicitly restored.
    ///
    /// This is `QPDF::readStream`'s bracketed seam
    /// (`libqpdf/QPDF.cc:1361-1381`) driven end to end: object 5 lives after
    /// object 4 in the file, so the nested resolution leaves the input
    /// somewhere past the payload. Deleting the `seek(stream_offset)` restore
    /// makes this read the wrong bytes.
    ///
    /// **What this guards is a `RefCell` double-borrow, not a wrong value.**
    /// A wrong value is what deleting the restore produces; the failure this
    /// test exists for is the other one. Any borrow of
    /// [`super::ResolverCore`] left live across the `/Length` dereference
    /// makes this test *panic* — `RefCell already borrowed`, raised inside
    /// [`ResolveMark::begin`] as the nested resolution takes the very first
    /// borrow of its own — rather than fail an assertion. The panic site names
    /// the colliding borrow, so the message is the diagnosis.
    ///
    /// A fixture whose `/Length` is a direct integer cannot stand in for this
    /// one, however similar it looks: nothing re-enters, so no borrow can
    /// collide and the hazard passes unnoticed. That is the whole reason
    /// object 5 exists.
    #[test]
    fn a_streams_indirect_length_resolves_mid_parse_and_raw_read_uses_the_restored_position() {
        let mut pdf = Pdf::open_mem_owned(indirect_length_pdf_bytes()).expect("open");
        let stream: ObjectHandle = pdf.get_object_handle(ObjectRef::new(4, 0));

        pdf.resolve(&stream)
            .expect("a stream with an indirect /Length resolves");

        assert!(
            stream.as_stream_data().is_none(),
            "a parsed stream has no replacement buffer"
        );
        stream
            .as_stream_dict()
            .expect("stream dictionary")
            .replace_key(b"/Length", ObjectHandle::integer(0))
            .unwrap();
        assert_eq!(
            stream
                .get_raw_stream_data()
                .expect("raw stream data")
                .as_slice(),
            STREAM_PAYLOAD,
            "the original branch must use its stored parse-time length and \
             restored stream offset, not a fresh /Length lookup"
        );
        let canonical_stream: ObjectHandle = pdf.get_object_handle(ObjectRef::new(4, 0));
        assert!(
            canonical_stream.is_same_object_as(&stream),
            "re-fetching the stream must return the canonical handle"
        );
        assert_eq!(
            canonical_stream
                .get_raw_stream_data()
                .expect("canonical raw stream data")
                .as_slice(),
            STREAM_PAYLOAD,
            "the canonical handle must retain the original source boundary"
        );
        assert!(
            pdf.get_object_handle(ObjectRef::new(5, 0)).is_resolved(),
            "the nested /Length resolution really did run"
        );
        assert!(
            pdf.resolver.core.borrow().resolving.is_empty(),
            "both marks must be gone once the outer resolution returns"
        );
    }

    #[test]
    fn raw_stream_data_reports_a_short_original_source_as_unsupported() {
        let mut pdf = Pdf::open_mem_owned(indirect_length_pdf_bytes()).expect("open");
        let stream: ObjectHandle = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve(&stream).expect("resolve stream");
        let stream_dict = stream.as_stream_dict().expect("stream dictionary");

        // `read_stream` cannot produce this shape: it validates the declared
        // length against `endstream` before constructing the stream. This
        // post-parse construction proves the *original source* branch reaches
        // `QPDF::Pipe::pipeStreamData`'s false result when a later caller has
        // a length exceeding the input, rather than mistaking it for an eager
        // parse-time failure or rereading /Length from the dictionary.
        stream.set_resolved(ObjectValue::Stream {
            stream_dict,
            stream_data: None,
            stream_length: 1_000,
            stream_provider: None,
            filter_on_write: true,
        });

        let error = stream
            .get_raw_stream_data()
            .expect_err("a short original source must fail the raw request");
        assert!(matches!(error, Error::Unsupported(message)
            if message == "error getting raw stream data"));
        assert!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .any(|entry| entry.message == "unexpected EOF reading stream data"),
            "the existing source pipe owns its warning before returning false"
        );
    }

    /// A stream whose `/Length` points at the very object being resolved —
    /// qpdf's own example of why the in-progress set exists: "an object
    /// references itself directly or indirectly in some key that has to be
    /// resolved during object parsing, such as stream length"
    /// (`libqpdf/QPDF.cc:1706-1708`).
    ///
    /// **The only fixture where the nested resolution re-enters on the *same*
    /// handle**, which is what makes it more than a restatement of the
    /// sibling-`/Length` seam test above.
    /// [`crate::ObjectHandle::set_resolved`] takes `borrow_mut()` on the very
    /// slot the outer frame is part-way through resolving — a second
    /// `RefCell`, distinct from [`super::ResolverCore`]'s. That much is
    /// latent rather than load-bearing, and the reason is checkable rather
    /// than assumed: no [`crate::ObjectHandle`] method visible outside its own
    /// module takes a caller closure or hands back a `std::cell::Ref`, so no
    /// code *here* can be holding a slot borrow when the seam is crossed. The
    /// two accessors that do run a closure under the borrow, `with_value` and
    /// `with_value_mut`, are private to `object_handle.rs`, which confines the
    /// hazard there — and the one place in that file which could hold a borrow
    /// across resolution, `try_dereference`'s own state check, reddens tests
    /// across the whole crate if it does, so it needs no fixture here. Should
    /// a closure-scoped accessor ever become crate-visible, this is the
    /// fixture positioned to catch a `read_stream` that wrapped the `/Length`
    /// dereference in one.
    ///
    /// What this *does* catch today is the [`super::ResolverCore`] borrow its
    /// sibling catches, on the same terms: a borrow spanning the `/Length`
    /// dereference makes this panic `RefCell already borrowed` rather than
    /// return a wrong value. And it is the only test that reaches the loop
    /// guard through a production resolution — the three that assert the loop
    /// branch's outcome all stage the mark by hand, so all three would still
    /// pass if the guard were unreachable from the parse. Remove the guard and
    /// this fixture recurses until the stack runs out, aborting the test
    /// binary rather than failing a test.
    ///
    /// This test deliberately opens with explicit `repair = false`: the inner
    /// qpdf-style loop guard caches the `/Length` reference as null, the stream
    /// parser reports the unusable length, and the resolver catches that
    /// structural failure and caches the outer stream as null. The ordinary
    /// default now enables the canonical `recoverStreamLength` arm, matching
    /// qpdf's default attempt-recovery behavior.
    #[test]
    fn a_self_referential_length_takes_the_loop_branch_instead_of_recursing_forever() {
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n".to_vec(),
        ]);
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open strict fixture");
        let object_ref = ObjectRef::new(2, 0);
        let handle: ObjectHandle = pdf.get_object_handle(object_ref);

        pdf.resolve(&handle)
            .expect("qpdf catches the null /Length after the loop");
        assert!(handle.is_null());

        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert!(messages
            .iter()
            .any(|message| message == "loop detected resolving object 2 0"));
        assert!(messages
            .iter()
            .any(|message| message.contains("stream dictionary lacks /Length key")));

        assert!(
            handle.is_null() && handle.is_resolved(),
            "the inner call caches qpdf's loop null through the same slot the \
             outer call was resolving"
        );
        pdf.resolve(&handle)
            .expect("and that null is terminal, so nothing is re-read");
        assert!(
            pdf.resolver.core.borrow().resolving.is_empty(),
            "the mark must be gone once the outer resolution returns its error"
        );
    }

    /// The resolver's own reads leave no warnings behind on a clean document.
    ///
    /// The framing checks it performs — the object-id match
    /// (`libqpdf/QPDF.cc:1600-1608`) and the `endobj` check (`:1352-1355`) —
    /// each warn when they fail. The live parser emits its own diagnostics
    /// once after its source borrow is released. This pins that a well-formed
    /// document produces exactly zero warnings.
    #[test]
    fn resolving_a_well_formed_document_raises_no_warnings() {
        let mut pdf = Pdf::open_mem_owned(indirect_length_pdf_bytes()).expect("open");
        for number in 1..=5 {
            let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(number, 0));
            pdf.resolve(&handle).expect("resolve");
        }

        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert_eq!(
            messages,
            Vec::<String>::new(),
            "a clean document must resolve without warnings"
        );
    }

    /// An object whose body is larger than the legacy input chunk still
    /// resolves through the live parser without truncation.
    #[test]
    fn an_object_longer_than_one_input_chunk_resolves_completely() {
        let filler = "x".repeat(super::INPUT_CHUNK * 2);
        let body = format!("1 0 obj\n<< /Type /Catalog /Filler ({filler}) >>\nendobj\n");
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let catalog = pdf.len() as u64;
        pdf.extend_from_slice(body.as_bytes());
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{catalog:010} 00000 n \n").as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open_mem_owned(pdf).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&handle).expect("resolve");

        assert_eq!(
            handle
                .as_dictionary()
                .expect("dictionary")
                .get(b"/Filler".as_slice())
                .and_then(crate::ObjectHandle::as_string)
                .map(|value| value.len()),
            Some(filler.len()),
            "a value spanning several input chunks must not be truncated"
        );
    }

    /// A large direct object is read through qpdf's 128-byte InputSource fast
    /// buffer, not one source read per byte.
    ///
    /// `InputSource::loadBuffer` fixes `buf_size` at 128 bytes
    /// (`include/qpdf/InputSource.hh:92-96,115-121`); `QPDFTokenizer` then
    /// advances its in-memory cursor (`QPDFTokenizer.cc:912-964`). The live
    /// parser must retain that boundary while processing every token once.
    #[test]
    fn a_large_direct_object_uses_the_inputsource_fast_read_buffer() {
        // 1 MiB of value.
        let filler = "x".repeat(super::INPUT_CHUNK * 256);
        let body = format!("1 0 obj\n<< /Type /Catalog /Filler ({filler}) >>\nendobj\n");
        let mut bytes = Vec::from(*b"%PDF-1.4\n");
        let catalog = bytes.len() as u64;
        bytes.extend_from_slice(body.as_bytes());
        let xref_start = bytes.len() as u64;
        bytes.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{catalog:010} 00000 n \n").as_bytes(),
        );
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(CountingReader::new(bytes)).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        // `Pdf::open`'s own xref load has already pulled; only the resolution
        // is under test.
        let before = pdf.resolver.with_reader_mut(|reader| reader.reads);

        pdf.resolve(&handle).expect("resolve");

        let pulls = pdf.resolver.with_reader_mut(|reader| reader.reads) - before;
        assert!(
            pulls <= filler.len() / 128 + 32,
            "a 1 MiB object must use InputSource's 128-byte fast-read buffer, \
             not one source read per byte: took {pulls} pulls"
        );
        assert_eq!(
            handle
                .as_dictionary()
                .expect("dictionary")
                .get(b"/Filler".as_slice())
                .and_then(crate::ObjectHandle::as_string)
                .map(|value| value.len()),
            Some(filler.len()),
            "and the value it reached that way must still be whole"
        );
    }

    /// Build a classic-xref document out of already-framed object bodies,
    /// `bodies[i]` being object `i + 1`.
    fn pdf_with_bodies(bodies: &[Vec<u8>]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::new();
        for body in bodies {
            offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(body);
        }

        let xref_start = pdf.len() as u64;
        let size = bodies.len() + 1;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for offset in &offsets {
            xref.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    // qpdf's file-object parser does not reject a non-name dictionary entry:
    // it retains the value under `/QPDFFake1` and warns
    // (`QPDFParser::fixMissingKeys`, `libqpdf/QPDFParser.cc:430-452`). This
    // must travel through the live resolver path, not merely a parser unit
    // test, or the resolver could regress to its former strict slice parser.
    #[test]
    fn a_live_resolver_recovers_a_non_name_dictionary_entry_once() {
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< (orphan) >>\nendobj\n".to_vec(),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle).expect("qpdf-style recovery");

        let dictionary = handle.as_dictionary().expect("recovered dictionary");
        assert_eq!(
            dictionary
                .get(b"/QPDFFake1".as_slice())
                .and_then(crate::ObjectHandle::as_string),
            Some(b"orphan".to_vec())
        );
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "(object 2 0, offset 55): expected dictionary key but found non-name object; inserting key /QPDFFake1"
            ]
        );
    }

    #[test]
    fn live_object_header_and_fast_unread_fail_at_qpdfs_boundary() {
        let bad_keyword = resolver_over(b"1 0 nope".to_vec());
        assert!(matches!(
            bad_keyword.read_object_at_offset(0, ObjectRef::new(1, 0)),
            Err(Error::Parse { offset: 0, ref message }) if message == "expected n n obj"
        ));

        let bad_number = resolver_over(b"/N 0 obj".to_vec());
        assert!(matches!(
            bad_number.read_object_at_offset(0, ObjectRef::new(1, 0)),
            Err(Error::Parse { offset: 0, ref message }) if message == "expected n n obj"
        ));

        let resolver = resolver_over(b"x".to_vec());
        let mut input = resolver.live_input();
        crate::parser::LiveInput::seek(&mut input, 1).expect("seek after the byte");
        crate::parser::LiveInput::unread_byte(&mut input).expect("unread from an empty buffer");
        assert_eq!(
            crate::parser::LiveInput::tell(&mut input).expect("position"),
            0
        );
        assert!(matches!(
            crate::parser::LiveInput::unread_byte(&mut input),
            Err(Error::Parse { offset: 0, ref message }) if message == "cannot unread before the start of input"
        ));
    }

    #[test]
    fn live_object_header_rejects_qpdfs_object_zero() {
        let resolver = resolver_over(b"0 0 obj\n42\nendobj\n".to_vec());

        let error = resolver
            .read_object_at_offset(0, ObjectRef::new(0, 0))
            .expect_err("qpdf rejects object ID zero");
        assert_eq!(error.to_string(), "parse error at byte 0: object with ID 0");
    }

    #[test]
    fn live_object_header_rejects_an_object_reference_out_of_range() {
        let resolver = resolver_over(b"4294967296 0 obj\n42\nendobj\n".to_vec());

        let error = resolver
            .read_object_at_offset(0, ObjectRef::new(1, 0))
            .expect_err("qpdf rejects an object number outside its ObjGen range");
        assert_eq!(
            error.to_string(),
            "parse error at byte 0: object reference is out of range"
        );
    }

    #[test]
    fn direct_empty_object_read_skips_cache_extent_capture() {
        let resolver = resolver_over(b"1 0 obj\nendobj\n".to_vec());

        let (value, parsed_offset) = resolver
            .read_object_at_offset(0, ObjectRef::new(1, 0))
            .expect("qpdf accepts an empty object as null");
        assert!(matches!(value, ObjectValue::Null));
        assert_eq!(parsed_offset, NO_PARSED_OFFSET);
    }

    #[test]
    fn object_end_offsets_rejects_a_nonadvancing_reader() {
        struct NonAdvancingReader;

        impl std::io::Read for NonAdvancingReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if buffer.is_empty() {
                    Ok(0)
                } else {
                    buffer[0] = b'x';
                    Ok(1)
                }
            }
        }

        impl std::io::Seek for NonAdvancingReader {
            fn seek(&mut self, _position: std::io::SeekFrom) -> std::io::Result<u64> {
                Ok(0)
            }
        }

        let mut reader = NonAdvancingReader;
        let mut empty = [];
        assert_eq!(
            std::io::Read::read(&mut reader, &mut empty).expect("empty reads succeed"),
            0
        );

        let resolver = ResolverHandle::new_shared(
            reader,
            0,
            BTreeMap::new(),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, Vec::new()),
            0,
        );
        assert!(matches!(
            resolver.object_end_offsets(),
            Err(Error::Parse { message, .. })
                if message == "object end offset is before the input start"
        ));
    }

    /// Differential check against the pinned qpdf binary. `--json-object=2`
    /// forces qpdf to parse the damaged object rather than relying on generic
    /// document traversal to reach it.
    #[test]
    fn live_dictionary_recovery_matches_pinned_qpdf_warning_text() {
        // cov:ignore-start: CI has pinned qpdf; this fallback exists only for developer hosts without it.
        if Command::new("qpdf").arg("--version").output().is_err() {
            eprintln!("qpdf not available; skipping live parser differential");
            return;
        }
        // cov:ignore-end

        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< (orphan) >>\nendobj\n".to_vec(),
        ]);
        let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
        let path = directory.path().join("live-file-object-recovery.pdf");
        fs::write(&path, &bytes).expect("write qpdf fixture");
        let qpdf = Command::new("qpdf")
            .args(["--json=2", "--json-object=2"])
            .arg(&path)
            .output()
            .expect("run pinned qpdf");
        let qpdf_diagnostics = String::from_utf8_lossy(&qpdf.stderr);
        let expected =
            "expected dictionary key but found non-name object; inserting key /QPDFFake1";
        assert!(
            qpdf_diagnostics.contains(expected),
            "qpdf must report the oracle recovery warning (status {}):\n{qpdf_diagnostics}",
            qpdf.status
        );

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve(&handle).expect("flpdf recovery");
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec![format!("(object 2 0, offset 55): {expected}")]
        );
    }

    /// A framing keyword straddling an input-chunk boundary is not accepted as
    /// the shorter word the truncation makes it look like.
    ///
    /// `stream` is placed so that only `str` lands in the first chunk. `str`
    /// is a perfectly good word token, so an attempt that accepted a parse
    /// consuming the whole buffer would frame this object as a plain
    /// dictionary and lose the payload entirely — a wrong answer, not an
    /// error. [`ResolverHandle::scan_forward`]'s `end < bytes.len()` test is
    /// what prevents it; qpdf needs no counterpart because its tokenizer
    /// consumes `m->file` directly and has no buffer to be cut short by.
    #[test]
    fn a_framing_keyword_split_across_an_input_chunk_is_not_read_as_a_shorter_word() {
        let head: &[u8] = b"2 0 obj\n<< /Length 13 /Filler (";
        let separator: &[u8] = b") >>\n";
        // The `stream` keyword must begin three bytes before the boundary.
        let filler = vec![b'x'; super::INPUT_CHUNK - 3 - head.len() - separator.len()];

        let mut body = Vec::new();
        body.extend_from_slice(head);
        body.extend_from_slice(&filler);
        body.extend_from_slice(separator);
        let keyword_start = body.len();
        body.extend_from_slice(b"stream\n");
        body.extend_from_slice(STREAM_PAYLOAD);
        body.extend_from_slice(b"\nendstream\nendobj\n");
        assert_eq!(
            keyword_start,
            super::INPUT_CHUNK - 3,
            "the fixture must actually straddle the chunk boundary"
        );

        let bytes = pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(), body]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle).expect("resolve");

        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("read raw stream data")
                .as_slice(),
            STREAM_PAYLOAD,
            "a `stream` keyword cut by the chunk boundary must not frame the \
             object as a plain dictionary"
        );
    }

    /// Resolve object 2 of a two-object document whose second body is `body`,
    /// and hand the outcome to `check` **while the document is still alive**.
    ///
    /// The closure is not decoration: `Pdf::drop` disconnects every canonical
    /// handle, so a helper that returned the handle instead would hand back a
    /// `Destroyed` slot that reads as a resolved null whatever the resolution
    /// actually did.
    fn with_second_object<T>(
        body: &[u8],
        check: impl FnOnce(&crate::ObjectHandle, Result<(), Error>, Vec<String>) -> T,
    ) -> T {
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            body.to_vec(),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let handle: crate::ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
        let outcome = pdf.resolve(&handle);
        let warnings = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        check(&handle, outcome, warnings)
    }

    /// Variant of [`with_second_object`] for tests that pin qpdf's explicit
    /// no-recovery path. The public default follows qpdf and enables recovery;
    /// strict parser/null-cache assertions must opt out deliberately.
    fn with_second_object_strict<T>(
        body: &[u8],
        check: impl FnOnce(&crate::ObjectHandle, Result<(), Error>, Vec<String>) -> T,
    ) -> T {
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            body.to_vec(),
        ]);
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open strict fixture");
        let handle: crate::ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
        let outcome = pdf.resolve(&handle);
        let warnings = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        check(&handle, outcome, warnings)
    }

    /// An object with no body at all resolves to null.
    ///
    /// qpdf: "Nothing in the PDF spec appears to allow empty objects, but they
    /// have been encountered in actual PDF files and Adobe Reader appears to
    /// ignore them" (`libqpdf/QPDF.cc:1341-1344`). flpdf's parser recognises
    /// the case by peeking `endobj` without consuming it, which is what lets
    /// the framing read below still find that same token.
    #[test]
    fn an_empty_object_body_resolves_to_null() {
        with_second_object(b"2 0 obj\nendobj\n", |handle, outcome, warnings| {
            outcome.expect("an empty object is recovered, not rejected");
            assert!(handle.is_null());
            assert_eq!(
                handle.get_parsed_offset(),
                NO_PARSED_OFFSET,
                "a recovered empty object has no source position of its own"
            );
            assert_eq!(
                warnings,
                ["(object 2 0, offset 53): empty object treated as null"],
                "qpdf warns and returns before framing the `endobj` token"
            );
        });
    }

    /// A value followed by something other than `endobj` warns and is kept.
    ///
    /// qpdf `libqpdf/QPDF.cc:1352-1355`: `if (!token.isWord("endobj")) { warn(
    /// damagedPDF("expected endobj")); }` and the object is returned anyway.
    #[test]
    fn a_value_not_followed_by_endobj_warns_and_is_still_resolved() {
        with_second_object(b"2 0 obj\n42\nenddobj\n", |handle, outcome, warnings| {
            outcome.expect("qpdf warns here rather than failing");
            assert_eq!(handle.as_integer(), Some(42));
            assert_eq!(warnings, ["(object 2 0, offset 56): expected endobj"]);
        });
    }

    /// A *direct* value that ends the input warns about the `endobj` that
    /// never arrives. `readObject` itself returns that value, but the enclosing
    /// resolve operation subsequently records the cache extent and falls back
    /// to null when it reaches EOF.
    ///
    /// qpdf makes no distinction between the two: `QPDF::readObject` reads one
    /// token after the value and again after `readStream`, through the very
    /// same `QPDF::readToken` (`libqpdf/QPDF.cc:1347-1354`), which passes
    /// `allow_bad = true` (`:1536-1539`) over a tokenizer that had
    /// `allowEOF()` applied at construction (`:208`). End of input therefore
    /// arrives as `tt_eof`, the check reports the keyword it wanted, and the
    /// object comes back.
    ///
    /// **Observed on qpdf 11.9.0**, over a generated fixture of exactly this
    /// shape — `3 0 obj << /Type /Whatever /A (bcd) >>` appended past `%%EOF`
    /// with the xref entry pointing at it:
    /// `WARNING: (object 3 0, offset 291): expected endobj`. The matching
    /// stream fixture — `<< /Length 3 >> stream abc endstream`, no `endobj` —
    /// produces the identical warning, which pins the parser/framing warning
    /// before the enclosing resolver applies its null fallback.
    ///
    /// qpdf does not stop at that warning when the object is the last thing in
    /// the file: `readObjectAtOffset` skips trailing whitespace to record
    /// `end_after_space` (`:1651-1663`) and throws
    /// `damagedPDF(tell(), "EOF after endobj")` (`:1660`) when the skip reaches
    /// the end. `QPDF::resolve` catches that (`:1737-1738`) and turns it into a
    /// resolve-to-null (`:1745-1748`). flpdf now records both cache-extent
    /// offsets and applies the same structural-error catch, so this test pins
    /// qpdf's two-warning, null outcome.
    #[test]
    fn a_direct_value_ending_the_input_warns_and_resolves_to_null() {
        let mut bytes = pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec()]);
        let appended_at = bytes.len() as u64;
        bytes.extend_from_slice(b"9 0 obj\n<< /Type /Whatever /A (bcd) >>");

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(9, 0),
            crate::XrefEntry::Uncompressed {
                offset: appended_at,
            },
        );
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(9, 0));

        pdf.resolve(&handle)
            .expect("qpdf catches EOF while recording the cache extent");
        assert!(handle.is_null());
        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert!(messages
            .iter()
            .any(|message| message == "(object 9 0, offset 185): expected endobj"));
        assert!(messages
            .iter()
            .any(|message| message.contains("EOF after endobj")));
    }

    /// A value that crosses the old slice window boundary, with more file
    /// after it, still finds its `endobj`. The live tokenizer is independent
    /// of that window: it retains only qpdf's one-byte delimiter unread and
    /// continues until the real framing token.
    #[test]
    fn a_value_ending_on_a_buffer_boundary_still_finds_its_endobj() {
        let open = b"2 0 obj\n<< /Type /Whatever /F (";
        let close = b") >>";
        // Retain the historic window boundary as a regression input.
        let filler = vec![b'x'; super::INPUT_CHUNK - open.len() - close.len()];
        let mut body = Vec::new();
        body.extend_from_slice(open);
        body.extend_from_slice(&filler);
        body.extend_from_slice(close);
        assert_eq!(
            body.len(),
            super::INPUT_CHUNK,
            "the fixture must land the value's end exactly on the boundary"
        );
        body.extend_from_slice(b"\nendobj\n");

        // A third object keeps the input going past the historic boundary.
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            body,
            b"3 0 obj\n<< /Type /Filler >>\nendobj\n".to_vec(),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle).expect("resolve");

        assert_eq!(
            handle
                .as_dictionary()
                .expect("the value is a dictionary")
                .get(b"/F".as_slice())
                .and_then(crate::ObjectHandle::as_string)
                .map(|value| value.len()),
            Some(filler.len()),
            "the value must be whole"
        );
        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert_eq!(
            messages,
            Vec::<String>::new(),
            "the buffer's end is not the input's end, so the framing must \
             refill and find `endobj` rather than report it missing"
        );
    }

    /// A malformed body at a nonzero xref offset reports the offending byte's
    /// position **in the file**, not in the window the resolver read it into.
    ///
    /// qpdf's warning position is an input position, not a parser-window
    /// position: `QPDFParser::warn` takes it from `InputSource::getLastOffset`
    /// (`libqpdf/QPDFParser.cc:516-519`). The live parser must preserve that
    /// absolute coordinate when it passes diagnostics to the document.
    ///
    /// **The number below is qpdf's, measured rather than derived.** The
    /// fixture this builds is byte-identical, over its header and both bodies,
    /// to one run through qpdf 11.9.0: object 2 begins at 256, the stray `)`
    /// sits 30 bytes into it, and qpdf reports
    /// `WARNING: (object 2 0, offset 286): unexpected )` — the same message
    /// flpdf produces, at the same position, once rebased.
    ///
    /// Header errors use qpdf's generic `expected n n obj` message and the
    /// object's xref offset (`libqpdf/QPDF.cc:1589-1594`); the live parser
    /// retains that same boundary before it enters the body parser.
    #[test]
    fn a_recovered_malformed_body_reports_its_warning_at_the_file_offset() {
        // 200 bytes of filler put object 2 far enough into the file that a
        // window-relative position could not be mistaken for a file one.
        let filler = "z".repeat(200);
        let bytes = pdf_with_bodies(&[
            format!("1 0 obj\n<< /Type /Catalog /Filler ({filler}) >>\nendobj\n").into_bytes(),
            b"2 0 obj\n<< /Type /Whatever /A )x >>\nendobj\n".to_vec(),
        ]);
        let malformed_at = bytes
            .windows(2)
            .position(|pair| pair == b")x")
            .expect("the fixture must contain the offending token");
        assert_eq!(
            malformed_at, 286,
            "the fixture must stay byte-compatible with the one qpdf was run \
             against, or the position below is no longer qpdf's"
        );

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle)
            .expect("qpdf recovers a stray close parenthesis");

        let dictionary = handle.as_dictionary().expect("recovered dictionary");
        assert!(
            dictionary
                .get(b"/QPDFFake1".as_slice())
                .is_some_and(|value| value.as_string().as_deref() == Some(b"x".as_slice())),
            "qpdf retains the orphan word under /QPDFFake1"
        );
        let diagnostics = pdf.repair_diagnostics();
        let warning = diagnostics
            .entries()
            .iter()
            .find(|entry| entry.message.contains("unexpected )"))
            .expect("qpdf tokenizer warning");
        assert_eq!(
            warning.message,
            format!("(object 2 0, offset {malformed_at}): unexpected )")
        );
        assert_eq!(warning.offset, Some(malformed_at as u64));
    }

    #[test]
    fn a_caught_parse_failure_preserves_its_warning_offset() {
        let malformed_body = b"2 0 obj\n[ 2147483648 0 R ]\nendobj\n";
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            malformed_body.to_vec(),
        ]);
        let malformed_at = bytes
            .windows(b"2147483648".len())
            .position(|window| window == b"2147483648")
            .expect("the fixture must contain the malformed integer");

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve(&handle)
            .expect("qpdf catches a body parse failure and resolves to null");
        assert!(handle.is_null());

        let diagnostics = pdf.repair_diagnostics();
        let warning = diagnostics
            .entries()
            .iter()
            .find(|entry| {
                entry
                    .message
                    .contains("integer out of range converting 2147483648")
            })
            .expect("the caught parse failure must be warned");
        assert_eq!(warning.offset, Some(malformed_at as u64));
    }

    #[test]
    fn a_caught_offsetless_resolution_failure_keeps_the_existing_warning_path() {
        let resolver = bare_resolver();
        resolver
            .push_caught_resolution_warning(
                Error::Unsupported("unfilterable object stream".to_owned()),
                ObjectRef::new(1, 0),
            )
            .expect("the offsetless warning should reach the document sink");

        let diagnostics = resolver.repair_diagnostics();
        assert_eq!(diagnostics.entries()[0].offset, None);
        assert_eq!(
            diagnostics.entries()[0].message,
            "unsupported PDF feature: unfilterable object stream"
        );
    }

    /// A recoverable diagnostic raised *inside* the body's parse reaches the
    /// document's warnings, and is not dropped with the parser that raised it.
    ///
    /// A name with a stray `#` is the reachable case, and qpdf both recovers it
    /// and warns about it: `QPDFTokenizer::inNameHex1`
    /// (`libqpdf/QPDFTokenizer.cc:448`) and `inNameHex2` (`:463`) set
    /// `"name with stray # will not work with PDF >= 1.2"` while leaving the
    /// token a name, `QPDFTokenizer::nextToken` returns false whenever a
    /// message was set (`:964`), and `QPDFParser::parseRemainder` turns that
    /// into `warn(tokenizer.getErrorMessage())`
    /// (`libqpdf/QPDFParser.cc:141-143`), which reaches the enclosing document
    /// through `QPDFParser::warn` (`:488`), which forwards to `context->warn`
    /// (`:494`).
    ///
    /// Asserted as whole vectors rather than with `contains`, because both the
    /// count and the position matter: the parse's diagnostics precede the
    /// framing's, since qpdf finishes parsing the value before it reads the
    /// token that decides `stream` from `endobj` (`libqpdf/QPDF.cc:1345-1354`).
    #[test]
    fn a_stray_hash_in_a_name_warns_with_qpdfs_tokenizer_wording() {
        with_second_object(
            b"2 0 obj\n<< /A#zB 1 >>\nendobj\n",
            |handle, outcome, warnings| {
                outcome.expect("qpdf recovers the name rather than rejecting the object");
                assert!(
                    handle.as_dictionary().is_some(),
                    "the object still resolves; the warning is the only difference"
                );
                assert_eq!(
                    warnings,
                    ["(object 2 0, offset 56): name with stray # will not work with PDF >= 1.2"]
                );
            },
        );

        with_second_object(
            b"2 0 obj\n<< /A#zB 1 >>\nenddobj\n",
            |_, outcome, warnings| {
                outcome.expect("qpdf recovers the name rather than rejecting the object");
                assert_eq!(
                    warnings,
                    [
                        "(object 2 0, offset 56): name with stray # will not work with PDF >= 1.2",
                        "(object 2 0, offset 67): expected endobj",
                    ]
                );
            },
        );
    }

    /// A body diagnostic is raised once even when the object crosses the old
    /// slice window boundary. The live parser has no discarded parse attempts,
    /// so it must not duplicate an early tokenizer warning while it reaches
    /// the final `endobj`.
    #[test]
    fn a_live_parse_diagnostic_is_raised_once_across_the_old_window_boundary() {
        let head: &[u8] = b"2 0 obj\n<< /A#zB 1 /Filler (";
        let separator: &[u8] = b") >>\n";
        let filler = vec![b'x'; super::INPUT_CHUNK - 3 - head.len() - separator.len()];

        let mut body = Vec::new();
        body.extend_from_slice(head);
        body.extend_from_slice(&filler);
        body.extend_from_slice(separator);
        let keyword_start = body.len();
        body.extend_from_slice(b"endobj\n");
        assert_eq!(
            keyword_start,
            super::INPUT_CHUNK - 3,
            "the fixture must actually straddle the chunk boundary"
        );

        with_second_object(&body, |handle, outcome, warnings| {
            outcome.expect("resolve");
            assert!(handle.as_dictionary().is_some());
            assert_eq!(
                warnings,
                ["(object 2 0, offset 56): name with stray # will not work with PDF >= 1.2"],
                "one diagnostic per object, not one per scan_forward attempt"
            );
        });
    }

    /// An object whose header carries a different `N G` than the xref table
    /// asked for warns and resolves the requested slot to null. The canonical
    /// resolver must not silently route this malformed source through the
    /// legacy raw-object reader.
    #[test]
    fn an_object_whose_header_names_a_different_reference_warns_and_resolves_to_null() {
        with_second_object(b"7 0 obj\n42\nendobj\n", |handle, outcome, warnings| {
            outcome.expect("qpdf catches an object-id mismatch");
            assert!(handle.is_null());
            assert!(warnings
                .iter()
                .any(|warning| warning.contains("expected 2 0 obj")));
        });
    }

    #[test]
    fn a_header_mismatch_caches_the_object_under_its_actual_objgen() {
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            b"7 0 obj\n42\nendobj\n".to_vec(),
        ]);
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open strict mismatch fixture");
        let requested: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve(&requested)
            .expect("qpdf resolves the requested slot to null after warning");

        assert!(requested.is_null());
        let found = pdf.get_object_handle(ObjectRef::new(7, 0));
        assert_eq!(found.as_integer(), Some(42));
        assert!(found.is_resolved());
        assert!(found.is_same_object_as(&pdf.get_object_handle(ObjectRef::new(7, 0))));
    }

    /// qpdf captures the offset after the indirect-object header before it
    /// enters `QPDF::readObject`; stream `/Length` failures therefore use the
    /// post-`obj` position rather than the xref/object-start position
    /// (`libqpdf/QPDF.cc:1331-1335,1360-1399`). Recovery warnings still use
    /// the stream data position captured by `readStream`.
    #[test]
    fn canonical_stream_length_warnings_use_qpdfs_post_header_offset() {
        let catalog = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let object_start = b"%PDF-1.4\n".len() as u64 + catalog.len() as u64;
        let header_offset = object_start + b"2 0 obj".len() as u64;

        for dictionary in [
            b"<< /Type /X >>".as_slice(),
            b"<< /Length /X >>".as_slice(),
            b"<< /Length -5 >>".as_slice(),
        ] {
            let mut body = b"2 0 obj\n".to_vec();
            body.extend_from_slice(dictionary);
            body.extend_from_slice(b"\nstream\nabc\nendstream\nendobj\n");
            let stream_offset = object_start
                + body
                    .windows(b"stream\n".len())
                    .position(|window| window == b"stream\n")
                    .expect("stream keyword") as u64
                + b"stream\n".len() as u64;
            let expected_length_warning = match dictionary {
                b"<< /Type /X >>" => "stream dictionary lacks /Length key",
                b"<< /Length /X >>" => "/Length key in stream dictionary is not an integer",
                b"<< /Length -5 >>" => "/Length key in stream dictionary is out of range",
                _ => unreachable!("all malformed length cases are listed"), // cov:ignore: dictionary is one of the three literals above
            }; // cov:ignore: match terminator has no executable coverage region

            let mut pdf = Pdf::open_mem_owned_with_options(
                pdf_with_bodies(&[catalog.to_vec(), body]),
                crate::PdfOpenOptions {
                    repair: true,
                    ..crate::PdfOpenOptions::default()
                },
            )
            .expect("open repair fixture");
            let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
            pdf.resolve(&handle)
                .expect("repair mode recovers an unusable stream length");

            let messages: Vec<_> = pdf
                .repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.clone())
                .collect();
            assert_eq!(
                messages,
                vec![
                    format!("(object 2 0, offset {header_offset}): {expected_length_warning}"),
                    format!(
                        "(object 2 0, offset {stream_offset}): attempting to recover stream length"
                    ),
                    format!("(object 2 0, offset {stream_offset}): recovered stream length: 4"),
                ]
            );
        }
    }

    #[test]
    fn stream_input_read_at_or_beyond_eof_records_source_end_like_qpdf() {
        let resolver = resolver_over(b"abc".to_vec());
        let input = resolver.stream_input();
        input
            .seek(100)
            .expect("seek beyond the source is permitted");

        let mut byte = [0u8; 1];
        assert_eq!(input.read(&mut byte).expect("read at EOF"), 0);
        assert_eq!(input.last_offset(), 3);
        assert_eq!(input.tell().expect("EOF position"), 3);
    }

    #[test]
    fn offset_read_preserves_qpdf_caller_description_in_stream_warnings() {
        let bytes = b"2 0 obj\n<< /Length 100000 >>\nstream\nabc\nendstream\nendobj\n%%EOF\n";
        let stream_offset = bytes
            .windows(b"stream\n".len())
            .position(|window| window == b"stream\n")
            .expect("stream keyword")
            + b"stream\n".len();
        let recovered_length = bytes
            .windows(b"endstream".len())
            .position(|window| window == b"endstream")
            .expect("endstream keyword")
            - stream_offset;

        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes.to_vec()),
            0,
            BTreeMap::from([(ObjectRef::new(2, 0), XrefEntry::Uncompressed { offset: 0 })]),
            true,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, b"input.pdf".to_vec()),
            0,
        );

        let (handle, _) = resolver
            .resolve_at_offset_with_optional_description(
                0,
                ObjectRef::new(2, 0),
                Some(b"linearization hint stream".to_vec()),
            )
            .expect("qpdf's described offset read resolves");
        assert!(handle.as_stream_dict().is_some());

        let expected = format!(
            "WARNING: input.pdf (linearization hint stream: object 2 0, offset {}): expected endstream\n\
             WARNING: input.pdf (linearization hint stream: object 2 0, offset {}): attempting to recover stream length\n\
             WARNING: input.pdf (linearization hint stream: object 2 0, offset {}): recovered stream length: {}\n",
            bytes.len(), stream_offset, stream_offset, recovered_length
        );
        assert_eq!(
            output.lock().expect("warning output").as_slice(),
            expected.as_bytes()
        );
    }

    type RecordedWarnings = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

    fn described_read_warning_output(
        bytes: &[u8],
    ) -> (RecordedWarnings, Rc<ResolverHandle<Cursor<Vec<u8>>>>) {
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes.to_vec()),
            0,
            BTreeMap::from([(ObjectRef::new(2, 0), XrefEntry::Uncompressed { offset: 0 })]),
            true,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, b"input.pdf".to_vec()),
            0,
        );
        (output, resolver)
    }

    /// qpdf: `qpdf --check` on a linearized file whose hint stream lacks
    /// `endobj` warns `(linearization hint stream: object 7 0, offset 722):
    /// expected endobj`; the description set by `readObjectAtOffset` is
    /// still `m->last_object_description` when `readObject` checks the
    /// trailing token (`libqpdf/QPDF.cc:1298-1310`, `:2641-2644`).
    #[test]
    fn described_offset_read_preserves_context_for_stream_expected_endobj() {
        let bytes = b"2 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobX\n%%EOF\n";
        let trailing_offset = bytes
            .windows(b"endobX".len())
            .position(|window| window == b"endobX")
            .expect("trailing token");
        let (output, resolver) = described_read_warning_output(bytes);

        let (handle, _) = resolver
            .resolve_at_offset_with_optional_description(
                0,
                ObjectRef::new(2, 0),
                Some(b"linearization hint stream".to_vec()),
            )
            .expect("qpdf's described offset read resolves");
        assert!(handle.as_stream_dict().is_some());

        let expected = format!(
            "WARNING: input.pdf (linearization hint stream: object 2 0, offset {trailing_offset}): expected endobj\n"
        );
        assert_eq!(
            output.lock().expect("warning output").as_slice(),
            expected.as_bytes()
        );
        let diagnostics = resolver.repair_diagnostics();
        let diagnostic = diagnostics.entries().last().expect("recorded warning");
        assert!(diagnostic.is_object_warning());
        assert_eq!(diagnostic.offset, Some(trailing_offset as u64));
    }

    #[test]
    fn indirect_length_resolution_clobbers_a_described_read_context_like_qpdf() {
        let bytes = b"2 0 obj\n<< /Length 3 0 R >>\nstream\nabc\nendstream\nendobX\n3 0 obj\n3\nendobj\n%%EOF\n";
        let length_offset = bytes
            .windows(b"3 0 obj".len())
            .position(|window| window == b"3 0 obj")
            .expect("indirect length object");
        let trailing_offset = bytes
            .windows(b"endobX".len())
            .position(|window| window == b"endobX")
            .expect("trailing token");
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes.to_vec()),
            0,
            BTreeMap::from([
                (ObjectRef::new(2, 0), XrefEntry::Uncompressed { offset: 0 }),
                (
                    ObjectRef::new(3, 0),
                    XrefEntry::Uncompressed {
                        offset: length_offset as u64,
                    },
                ),
            ]),
            true,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, b"input.pdf".to_vec()),
            0,
        );

        let (handle, _) = resolver
            .resolve_at_offset_with_optional_description(
                0,
                ObjectRef::new(2, 0),
                Some(b"linearization hint stream".to_vec()),
            )
            .expect("described stream read resolves");
        assert!(handle.as_stream_dict().is_some());

        let expected =
            format!("WARNING: input.pdf (object 3 0, offset {trailing_offset}): expected endobj\n");
        assert_eq!(
            output.lock().expect("warning output").as_slice(),
            expected.as_bytes()
        );
    }

    #[test]
    fn described_offset_read_preserves_context_for_dictionary_expected_endobj() {
        let bytes = b"2 0 obj\n<< /Type /Test >>\nendobX\n%%EOF\n";
        let trailing_offset = bytes
            .windows(b"endobX".len())
            .position(|window| window == b"endobX")
            .expect("trailing token");
        let (output, resolver) = described_read_warning_output(bytes);

        let (handle, _) = resolver
            .resolve_at_offset_with_optional_description(
                0,
                ObjectRef::new(2, 0),
                Some(b"linearization hint stream".to_vec()),
            )
            .expect("qpdf's described offset read resolves");
        assert!(handle.as_dictionary().is_some());

        let expected = format!(
            "WARNING: input.pdf (linearization hint stream: object 2 0, offset {trailing_offset}): expected endobj\n"
        );
        assert_eq!(
            output.lock().expect("warning output").as_slice(),
            expected.as_bytes()
        );
    }

    #[test]
    fn described_offset_read_preserves_context_for_framing_recovery() {
        let bytes = b"2 0 obj\n<< /Length 1 >>\nstream\nabc\nendstream\nendobj\n%%EOF\n";
        let stream_offset = bytes
            .windows(b"stream\n".len())
            .position(|window| window == b"stream\n")
            .expect("stream keyword")
            + b"stream\n".len();

        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes.to_vec()),
            0,
            BTreeMap::from([(ObjectRef::new(2, 0), XrefEntry::Uncompressed { offset: 0 })]),
            true,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, false, b"input.pdf".to_vec()),
            0,
        );

        let (handle, _) = resolver
            .resolve_at_offset_with_optional_description(
                0,
                ObjectRef::new(2, 0),
                Some(b"linearization hint stream".to_vec()),
            )
            .expect("qpdf's described offset read resolves");
        assert!(handle.as_stream_dict().is_some());

        let logged = String::from_utf8(output.lock().expect("warning output").clone())
            .expect("warning output is utf8");
        assert!(logged.contains(&format!(
            "input.pdf (linearization hint stream: object 2 0, offset {}): expected endstream",
            stream_offset + 1
        )));
        assert!(logged.contains(&format!(
            "input.pdf (linearization hint stream: object 2 0, offset {stream_offset}): recovered stream length: 4"
        )));
    }

    #[test]
    fn described_offset_read_collects_but_suppresses_stream_warnings() {
        let bytes = b"2 0 obj\n<< /Length 100000 >>\nstream\nabc\nendstream\nendobj\n%%EOF\n";
        let logger = crate::QPDFLogger::create();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            WarningRecordingSink(std::sync::Arc::clone(&output)),
        )));
        let resolver = ResolverHandle::new_shared(
            Cursor::new(bytes.to_vec()),
            0,
            BTreeMap::from([(ObjectRef::new(2, 0), XrefEntry::Uncompressed { offset: 0 })]),
            true,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(logger, true, b"input.pdf".to_vec()),
            0,
        );

        resolver
            .resolve_at_offset_with_optional_description(
                0,
                ObjectRef::new(2, 0),
                Some(b"linearization hint stream".to_vec()),
            )
            .expect("suppressed described offset read resolves");
        assert!(output.lock().expect("warning output").is_empty());
        assert_eq!(resolver.repair_diagnostics().entries().len(), 3);
    }

    /// With repair disabled, qpdf rethrows an unusable `/Length` exception
    /// from `readStream`; `resolve` warns that unchanged exception and then
    /// resolves the requested object to null. The exception still carries
    /// `readObject`'s post-header offset (`libqpdf/QPDF.cc:1331-1399,
    /// 1695-1749`).
    #[test]
    fn no_recovery_stream_length_warnings_use_qpdfs_post_header_offset() {
        let catalog = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let object_start = b"%PDF-1.4\n".len() as u64 + catalog.len() as u64;
        let header_offset = object_start + b"2 0 obj".len() as u64;

        for (dictionary, expected_message) in [
            (
                b"<< /Type /X >>".as_slice(),
                "stream dictionary lacks /Length key",
            ),
            (
                b"<< /Length /X >>".as_slice(),
                "/Length key in stream dictionary is not an integer",
            ),
            (
                b"<< /Length -5 >>".as_slice(),
                "/Length key in stream dictionary is out of range",
            ),
        ] {
            let mut body = b"2 0 obj\n".to_vec();
            body.extend_from_slice(dictionary);
            body.extend_from_slice(b"\nstream\nabc\nendstream\nendobj\n");
            let mut pdf = Pdf::open_mem_owned_with_options(
                pdf_with_bodies(&[catalog.to_vec(), body]),
                crate::PdfOpenOptions {
                    repair: false,
                    ..crate::PdfOpenOptions::default()
                },
            )
            .expect("open strict malformed-length fixture");
            let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
            pdf.resolve(&handle)
                .expect("qpdf catches the strict malformed-length failure");
            assert!(handle.is_null());

            let diagnostics = pdf.repair_diagnostics();
            assert_eq!(diagnostics.entries().len(), 1);
            assert_eq!(
                diagnostics.entries()[0].message,
                format!("(object 2 0, offset {header_offset}): {expected_message}")
            );
            assert_eq!(diagnostics.entries()[0].offset, Some(header_offset));
        }
    }

    /// Every way `/Length` can fail to yield a byte count.
    ///
    /// qpdf reports the null and non-integer cases separately
    /// (`libqpdf/QPDF.cc:1370-1377`); an absent key reads as null there, so it
    /// shares the first message.
    #[test]
    fn a_stream_whose_length_is_unusable_resolves_to_null_with_qpdfs_own_distinction() {
        for (dict, expected) in [
            (
                &b"<< /Type /X >>"[..],
                "stream dictionary lacks /Length key",
            ),
            (b"<< /Length null >>", "stream dictionary lacks /Length key"),
            (
                b"<< /Length /X >>",
                "/Length key in stream dictionary is not an integer",
            ),
            (
                b"<< /Length -5 >>",
                "/Length key in stream dictionary is out of range",
            ),
        ] {
            let mut body = b"2 0 obj\n".to_vec();
            body.extend_from_slice(dict);
            body.extend_from_slice(b"\nstream\nabc\nendstream\nendobj\n");

            with_second_object_strict(&body, |handle, outcome, warnings| {
                outcome.expect("qpdf catches an unusable /Length");
                assert!(handle.is_null());
                assert!(warnings.iter().any(|warning| warning.contains(expected)));
            });
        }
    }

    /// A `stream` keyword after a non-dictionary value resolves to null.
    ///
    /// qpdf cannot reach its own equivalent: `readObject` only calls
    /// `readStream` when `object.isDictionary()` (`libqpdf/QPDF.cc:1350`), so
    /// this guard stands in for a branch qpdf expresses as a condition.
    #[test]
    fn a_stream_keyword_after_a_non_dictionary_resolves_to_null() {
        with_second_object(
            b"2 0 obj\n42\nstream\nabc\nendstream\nendobj\n",
            |handle, outcome, warnings| {
                outcome.expect("qpdf catches a stream keyword after a non-dictionary");
                assert!(handle.is_null());
                assert!(warnings.iter().any(|warning| warning
                    .contains("stream keyword follows an object that is not a dictionary")));
            },
        );
    }

    /// A `/Length` running past the end of the input, and a payload not
    /// followed by `endstream`, are both `expected endstream`.
    ///
    /// One message for both because qpdf has one: neither case reaches a
    /// length check, they reach the token check at `libqpdf/QPDF.cc:1386-1389`
    /// from opposite sides. Measured on qpdf 11.9.0 — `/Length 100000` over a
    /// 420-byte file warns `expected endstream`, and so does `/Length 1` in
    /// front of `abc`; each then goes on to `attempting to recover stream
    /// length` / `recovered stream length: 4`, which the canonical resolver
    /// records when `repair` is enabled. With recovery disabled, the resolver
    /// catches the framing parse error and caches the stream as null. An
    /// earlier revision reported the first case as "stream data ends before
    /// its declared /Length", a message with no counterpart anywhere in
    /// `libqpdf/` — it came from reading the payload before checking the
    /// framing, which is what made the absurd-length allocation reachable.
    #[test]
    fn a_stream_payload_that_does_not_match_its_length_resolves_to_null() {
        for body in [
            &b"2 0 obj\n<< /Length 100000 >>\nstream\nabc\nendstream\nendobj\n"[..],
            b"2 0 obj\n<< /Length 1 >>\nstream\nabc\nendstream\nendobj\n",
        ] {
            with_second_object_strict(body, |handle, outcome, warnings| {
                outcome.expect("qpdf catches a missing endstream");
                assert!(handle.is_null());
                assert!(
                    warnings
                        .iter()
                        .any(|warning| warning.contains("expected endstream")),
                    "for {body:?}: {warnings:?}"
                );
            });
        }
    }

    #[test]
    fn canonical_stream_reuses_the_length_recovered_from_bad_framing() {
        let body = b"2 0 obj\n<< /Length 1 >>\nstream\nabc\nendstream\nendobj\n".to_vec();
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            body.clone(),
        ]);
        let body_offset = b"%PDF-1.4\n".len() + b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".len();
        let stream_offset = body_offset
            + body
                .windows(b"stream\n".len())
                .position(|window| window == b"stream\n")
                .expect("stream keyword")
            + b"stream\n".len();
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle)
            .expect("bad framing enters qpdf's recovery arm");
        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("read recovered stream data")
                .as_slice(),
            b"abc\n"
        );
        assert!(pdf.repair_diagnostics().entries().iter().any(|entry| {
            entry.message
                == format!("(object 2 0, offset {stream_offset}): recovered stream length: 4")
        }));
    }

    #[test]
    fn canonical_stream_allows_a_malformed_framing_token_to_recover() {
        let body = b"2 0 obj\n<< /Length 0 >>\nstream\n(\nendstream\nendobj\n".to_vec();
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            body.clone(),
        ]);
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle)
            .expect("qpdf allows a malformed framing token into recovery");
        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("read recovered stream data")
                .as_slice(),
            b"(\n"
        );
        let messages: Vec<_> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert!(messages
            .iter()
            .any(|message| message == "(object 2 0, offset 76): expected endstream"));
        assert!(messages.iter().any(|message| {
            message == "(object 2 0, offset 76): attempting to recover stream length"
        }));
        assert!(messages
            .iter()
            .any(|message| { message == "(object 2 0, offset 76): recovered stream length: 2" }));
    }

    #[test]
    fn canonical_stream_recovery_repositions_before_endobj() {
        let body = b"2 0 obj\n<< /Length 1 >>\nstream\nabc\nendobj\n".to_vec();
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            body.clone(),
        ]);
        let body_offset = b"%PDF-1.4\n".len() + b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".len();
        let stream_offset = body_offset
            + body
                .windows(b"stream\n".len())
                .position(|window| window == b"stream\n")
                .expect("stream keyword")
            + b"stream\n".len();
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle)
            .expect("endobj is a valid qpdf recovery terminator");
        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("read recovered stream data")
                .as_slice(),
            b"abc\n"
        );
        assert!(pdf.repair_diagnostics().entries().iter().any(|entry| {
            entry.message
                == format!("(object 2 0, offset {stream_offset}): recovered stream length: 4")
        }));
    }

    #[test]
    fn canonical_stream_recovery_respects_qpdf_candidate_token_limit() {
        let mut payload = b"end".to_vec();
        payload.extend_from_slice(&[b'x'; 17]);
        let mut body = b"2 0 obj\n<< /Length 0 >>\nstream\n".to_vec();
        body.extend_from_slice(&payload);
        body.extend_from_slice(b"endstream\nendobj\n");
        let bytes = pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(), body]);
        let body_offset = b"%PDF-1.4\n".len() + b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".len();
        let stream_offset = body_offset + b"2 0 obj\n<< /Length 0 >>\nstream\n".len();
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle)
            .expect("a long rejected candidate still finds nested endstream");
        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("read recovered stream data")
                .as_slice(),
            payload.as_slice()
        );
        assert!(pdf.repair_diagnostics().entries().iter().any(|entry| {
            entry.message
                == format!("(object 2 0, offset {stream_offset}): recovered stream length: 20")
        }));
    }

    #[test]
    fn canonical_stream_recovery_does_not_seek_for_each_non_prefix_candidate() {
        let payload = vec![b'e'; 96];
        let mut body = b"2 0 obj\n<< /Length 0 >>\nstream\n".to_vec();
        body.extend_from_slice(&payload);
        body.extend_from_slice(b"\nendstream\nendobj\n");
        let bytes = pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(), body]);
        let mut pdf = Pdf::open_with_options(
            CountingReader::new(bytes),
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        let before = pdf.resolver.with_reader_mut(|reader| reader.seeks);
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle)
            .expect("recovery must scan a long non-prefix payload");

        let seeks = pdf.resolver.with_reader_mut(|reader| reader.seeks) - before;
        assert!(seeks < payload.len() / 2);
    }

    #[test]
    fn canonical_stream_malformed_framing_matches_pinned_qpdf() {
        // cov:ignore-start: CI has pinned qpdf; this fallback is for developer hosts only.
        if Command::new("qpdf").arg("--version").output().is_err() {
            eprintln!("qpdf not available; skipping stream recovery differential");
            return;
        }
        // cov:ignore-end

        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Length 0 >>\nstream\n(\nendstream\nendobj\n".to_vec(),
        ]);
        let body_offset = b"%PDF-1.4\n".len() + b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".len();
        let stream_offset = body_offset + b"2 0 obj\n<< /Length 0 >>\nstream\n".len();
        let directory = tempfile::tempdir().expect("temporary qpdf fixture directory");
        let path = directory.path().join("malformed-stream-framing.pdf");
        fs::write(&path, &bytes).expect("write qpdf fixture");
        let qpdf = Command::new("qpdf")
            .args(["--check", "--warning-exit-0"])
            .arg(&path)
            .output()
            .expect("run pinned qpdf");
        let qpdf_diagnostics = String::from_utf8_lossy(&qpdf.stderr);
        for expected in [
            "expected endstream",
            "attempting to recover stream length",
            "recovered stream length: 2",
        ] {
            assert!(
                qpdf_diagnostics.contains(expected),
                "qpdf must report {expected:?} (status {}):\n{qpdf_diagnostics}",
                qpdf.status
            );
        }

        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve(&handle).expect("flpdf recovery");
        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("read recovered stream data")
                .as_slice(),
            b"(\n"
        );
        let flpdf_diagnostics: Vec<_> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert_eq!(
            flpdf_diagnostics,
            vec![
                format!("(object 2 0, offset {stream_offset}): expected endstream"),
                format!(
                    "(object 2 0, offset {stream_offset}): attempting to recover stream length"
                ),
                format!("(object 2 0, offset {stream_offset}): recovered stream length: 2"),
            ]
        );
    }

    #[test]
    fn canonical_stream_diagnoses_the_attempted_framing_token_offset() {
        let body = b"2 0 obj\n<< /Length 1 >>\nstream\nabc\nendstream\nendobj\n".to_vec();
        let body_offset = b"%PDF-1.4\n".len() + b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".len();
        let stream_offset = body_offset
            + body
                .windows(b"stream\n".len())
                .position(|window| window == b"stream\n")
                .expect("stream keyword")
            + b"stream\n".len();
        let attempted_offset =
            u64::try_from(stream_offset + 1).expect("fixture offset fits qpdf offset");
        let bytes = pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(), body]);
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle).expect("repair the bad framing length");

        let diagnostics: Vec<_> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| (entry.message.clone(), entry.offset))
            .collect();
        assert_eq!(
            diagnostics.first(),
            Some(&(
                format!("(object 2 0, offset {attempted_offset}): expected endstream"),
                Some(attempted_offset)
            ))
        );
    }

    #[test]
    fn canonical_stream_diagnoses_framing_before_ignored_whitespace_and_comments() {
        let body =
            b"2 0 obj\n<< /Length 1 >>\nstream\nx \n% ignored\nnot-endstream\nendstream\nendobj\n";
        let body_offset = b"%PDF-1.4\n".len() + b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".len();
        let stream_offset = body_offset
            + body
                .windows(b"stream\n".len())
                .position(|window| window == b"stream\n")
                .expect("stream keyword")
            + b"stream\n".len();
        let attempted_offset = u64::try_from(stream_offset + b"x \n% ignored\n".len())
            .expect("fixture offset fits qpdf offset");
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            body.to_vec(),
        ]);
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle).expect("repair the bad framing length");

        let diagnostics: Vec<_> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| (entry.message.clone(), entry.offset))
            .collect();
        assert_eq!(
            diagnostics.first(),
            Some(&(
                format!("(object 2 0, offset {attempted_offset}): expected endstream"),
                Some(attempted_offset)
            ))
        );
    }

    #[test]
    fn canonical_stream_recovery_does_not_rewind_the_whole_source() {
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Length 1 >>\nstream\nabc\nendstream\nendobj\n".to_vec(),
        ]);
        let mut pdf = Pdf::open_with_options(
            RejectRewindReader {
                inner: std::io::Cursor::new(bytes),
                reject_rewind: false,
            },
            crate::PdfOpenOptions {
                repair: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");
        pdf.resolver.with_reader_mut(|reader| {
            reader.reject_rewind = true;
            assert!(
                std::io::Seek::seek(reader, std::io::SeekFrom::Start(0)).is_err(),
                "the guard must reject a whole-source rewind"
            );
        });
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle)
            .expect("stream recovery scans the live source");
        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("read recovered stream data")
                .as_slice(),
            b"abc\n"
        );
    }

    /// A `/Length` too large to be a position in the input is diagnosed
    /// without ever being allocated.
    ///
    /// **This is a process-lifetime assertion before it is a message
    /// assertion.** Reading the payload before checking the framing made the
    /// first case `vec![0u8; 9223372036854775000]`, and an allocation that
    /// large does not fail — it aborts: `memory allocation of
    /// 9223372036854775000 bytes failed`, `SIGABRT`, taking the whole test
    /// binary and every other result in it. Restore that `vec!` and this file
    /// stops producing a test report at all.
    ///
    /// The two values are two different qpdf mechanisms, and the split is by
    /// input source rather than by value, in flpdf exactly as in qpdf:
    ///
    /// - `9223372036854775000` added to a small position does not overflow
    ///   `qpdf_offset_t`, so an in-memory source takes the seek and the
    ///   following read finds nothing — `expected endstream`. That is
    ///   `BufferInputSource::seek` (`libqpdf/BufferInputSource.cc:83-108`) and
    ///   [`std::io::Cursor`] alike; qpdf's CLI cannot open an in-memory
    ///   document, so it was measured instead at `/Length 1099511627776`,
    ///   which a file-backed source *can* seek to: `expected endstream`, then
    ///   recovery, 8.8 MB peak RSS. A file-backed source refuses the larger
    ///   offset outright — qpdf: `seek to …, offset 9223372036854775000 (1):
    ///   Invalid argument`; Rust `File::seek(SeekFrom::Current(..))`: the same
    ///   `EINVAL`, both from `lseek` rejecting a result past the filesystem's
    ///   maximum, which is not an integer overflow;
    /// - `i64::MAX` added to any non-zero position *does* overflow
    ///   `qpdf_offset_t`, which is what `QIntC::range_check` refuses and what
    ///   [`super::ResolverCore::seek_relative`] refuses in the same words.
    ///   This one is not source-dependent: `Cursor::seek` would accept it
    ///   (measured: `Ok(9223372036854776043)` from position 236), so without
    ///   that check flpdf would silently file it as `expected endstream` — on
    ///   qpdf's recovery fork rather than on its resolve-to-null one.
    #[test]
    fn an_absurd_declared_length_is_diagnosed_without_allocating_it() {
        with_second_object_strict(
            b"2 0 obj\n<< /Length 9223372036854775000 >>\nstream\nabc\nendstream\nendobj\n",
            |handle, outcome, warnings| {
                outcome.expect("qpdf catches a missing endstream at the distant offset");
                assert!(handle.is_null());
                assert!(warnings
                    .iter()
                    .any(|warning| warning.contains("expected endstream")));
            },
        );

        with_second_object_strict(
            b"2 0 obj\n<< /Length 9223372036854775807 >>\nstream\nabc\nendstream\nendobj\n",
            |handle, outcome, warnings| {
                outcome.expect("qpdf catches the seek overflow");
                assert!(handle.is_null());
                assert!(warnings
                    .iter()
                    .any(|warning| warning.contains("would cause an integer overflow")));
            },
        );
    }

    /// A stream not followed by `endobj` warns, matching the direct case.
    #[test]
    fn a_stream_not_followed_by_endobj_warns() {
        with_second_object(
            b"2 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nenddobj\n",
            |handle, outcome, warnings| {
                outcome.expect("qpdf warns here rather than failing");
                assert_eq!(
                    handle
                        .get_raw_stream_data()
                        .expect("read raw stream data")
                        .as_slice(),
                    &b"abc"[..]
                );
                assert_eq!(warnings, ["(object 2 0, offset 90): expected endobj"]);
            },
        );
    }

    /// Every branch of qpdf's `validateStreamLineEnd`
    /// (`libqpdf/QPDF.cc:1400-1448`), including the exact warning texts.
    ///
    /// The payload assertion is the point: each branch decides where the
    /// stream's *data* starts, so getting one wrong shifts every following
    /// byte rather than merely mislabelling the warning. The last case's
    /// separator is empty because that branch is reached only when the byte
    /// after `stream` is neither whitespace nor a newline — which means it
    /// must be a delimiter, or the tokenizer would have read one longer word
    /// instead of `stream`. `(` is that delimiter, and it is therefore part
    /// of the payload rather than of the framing.
    #[test]
    fn the_stream_line_end_check_covers_qpdfs_four_branches() {
        for (separator, payload, expected_warnings) in [
            (&b"\n"[..], &b"abc"[..], &[][..]),
            (b"\r\n", b"abc", &[]),
            (
                b"\r",
                b"abc",
                &["stream keyword followed by carriage return only"][..],
            ),
            (
                b" \n",
                b"abc",
                &["stream keyword followed by extraneous whitespace"][..],
            ),
            (
                b"",
                b"(abc)",
                &["stream keyword not followed by proper line terminator"][..],
            ),
        ] {
            let mut body = format!("2 0 obj\n<< /Length {} >>\nstream", payload.len()).into_bytes();
            body.extend_from_slice(separator);
            body.extend_from_slice(payload);
            body.extend_from_slice(b"\nendstream\nendobj\n");

            with_second_object(&body, |handle, outcome, warnings| {
                outcome.expect("every line-ending branch still resolves the stream");
                assert_eq!(
                    handle
                        .get_raw_stream_data()
                        .expect("read raw stream data")
                        .as_slice(),
                    payload,
                    "separator {separator:?} moved the payload"
                );
                assert_eq!(warnings, expected_warnings, "separator {separator:?}");
            });
        }
    }

    /// An input source that returns short reads and an `Interrupted` error
    /// still yields the same object.
    ///
    /// `Read::read` is allowed to do both, and qpdf's `InputSource::read`
    /// contract hides them (`FileInputSource::read` loops over `fread`), so
    /// [`super::ResolverCore::read`] has to as well. Without the loop a short
    /// read would silently truncate an object; without the `Interrupted` arm
    /// it would fail outright.
    #[test]
    fn a_reluctant_input_source_still_resolves_the_same_object() {
        struct Reluctant {
            inner: std::io::Cursor<Vec<u8>>,
            calls: usize,
        }

        impl std::io::Read for Reluctant {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.calls += 1;
                // Every third call, not just the first: the first reads belong
                // to `Pdf::open`'s xref load, so interrupting only once would
                // never reach `ResolverCore::read` at all.
                if self.calls.is_multiple_of(3) {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                let len = buf.len().min(7);
                self.inner.read(&mut buf[..len])
            }
        }

        impl std::io::Seek for Reluctant {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                self.inner.seek(position)
            }
        }

        let mut pdf = Pdf::open(Reluctant {
            inner: std::io::Cursor::new(indirect_length_pdf_bytes()),
            calls: 0,
        })
        .expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(4, 0));

        pdf.resolve(&handle).expect("resolve");

        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("read raw stream data")
                .as_slice(),
            STREAM_PAYLOAD
        );
    }

    /// A stream whose `stream` keyword is the last thing in the input.
    ///
    /// qpdf returns from `validateStreamLineEnd` on a premature EOF because
    /// "a premature EOF here will result in some other problem that will get
    /// reported at another time" (`libqpdf/QPDF.cc:1414-1415`) — which is
    /// exactly what happens: the seek past the declared length lands beyond
    /// the input, and the token read there finds EOF instead of `endstream`.
    ///
    /// The object is appended past `%%EOF` and given an xref entry by hand,
    /// because a well-formed document always has its trailer after every
    /// object and so can never put one at the end of the input.
    #[test]
    fn a_stream_keyword_at_end_of_input_resolves_to_null_with_a_warning() {
        let mut bytes = pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec()]);
        let truncated_at = bytes.len() as u64;
        bytes.extend_from_slice(b"9 0 obj\n<< /Length 1 >>\nstream");

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(9, 0),
            crate::XrefEntry::Uncompressed {
                offset: truncated_at,
            },
        );
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(9, 0));

        pdf.resolve(&handle)
            .expect("qpdf catches the missing endstream framing error");
        assert!(handle.is_null());
        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert!(messages
            .iter()
            .any(|message| message.contains("expected endstream")));
    }

    /// A stream whose `endstream` is the last thing in the input resolves to
    /// null, warning about the `endobj` that never arrives and the subsequent
    /// EOF while recording the cache extent.
    ///
    /// The other half of [`super::ResolverHandle::read_token_from_input`]'s
    /// `allow_eof`, and the half the `endstream` fixtures cannot reach —
    /// they return at the check before it. qpdf takes the same two steps:
    /// `readToken` gives back `tt_eof` rather than a bad token
    /// (`libqpdf/QPDF.cc:208`), so `readObject`'s trailing check reports the
    /// keyword it wanted and returns the object anyway (`:1352-1355`) instead
    /// of failing on the EOF itself.
    ///
    /// **That describes `readObject`, not qpdf's net answer, and an earlier
    /// revision of this comment stopped one step too early.** Because the
    /// object is the last thing in the file, `readObjectAtOffset` goes on to
    /// throw `EOF after endobj` (`:1660`) while recording `end_after_space`,
    /// `QPDF::resolve` catches it (`:1737-1738`), and the object resolves to
    /// null (`:1745-1748`) — observed on qpdf 11.9.0 over this exact fixture
    /// shape: two warnings, `null`, exit 3. flpdf records the same
    /// `end_before_space`/`end_after_space` extent and applies the same
    /// structural-error catch, so this stream case now pins that net result
    /// alongside the direct-path test above.
    ///
    /// Appended past `%%EOF` with a hand-written xref entry for the same
    /// reason as the fixture above: a well-formed document always has a
    /// trailer after its objects.
    #[test]
    fn a_stream_ending_the_input_after_endstream_warns_and_resolves_to_null() {
        let mut bytes = pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec()]);
        let appended_at = bytes.len() as u64;
        bytes.extend_from_slice(b"9 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream");

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(9, 0),
            crate::XrefEntry::Uncompressed {
                offset: appended_at,
            },
        );
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(9, 0));

        pdf.resolve(&handle)
            .expect("qpdf catches EOF while recording the cache extent");
        assert!(handle.is_null());
        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert!(messages
            .iter()
            .any(|message| message == "(object 9 0, offset 191): expected endobj"));
        assert!(messages
            .iter()
            .any(|message| message.contains("EOF after endobj")));
    }

    /// An input source that starts failing part-way through a resolution
    /// propagates the failure instead of truncating the object.
    #[test]
    fn an_input_source_that_fails_mid_resolution_propagates_the_error() {
        struct Breakable {
            inner: std::io::Cursor<Vec<u8>>,
            broken: bool,
        }

        impl std::io::Read for Breakable {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.broken {
                    return Err(std::io::Error::other("input source went away"));
                }
                self.inner.read(buf)
            }
        }

        impl std::io::Seek for Breakable {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                self.inner.seek(position)
            }
        }

        let mut pdf = Pdf::open(Breakable {
            inner: std::io::Cursor::new(minimal_pdf_bytes()),
            broken: false,
        })
        .expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolver.with_reader_mut(|reader| reader.broken = true);

        let error = pdf
            .resolve(&handle)
            .expect_err("a dead input source cannot resolve anything");

        assert!(
            matches!(&error, Error::Io(_)),
            "the I/O failure must reach the caller unchanged, got {error:?}"
        );
        assert!(
            pdf.resolver.core.borrow().resolving.is_empty(),
            "and must not leave the reference marked in progress"
        );
    }

    /// A named file source uses qpdf's `FileInputSource::read` exception shape
    /// for a lazy read failure, including the source name and requested length;
    /// the platform I/O message is intentionally omitted.
    #[test]
    fn a_described_input_source_formats_mid_resolution_read_failures_like_qpdf() {
        struct Breakable {
            inner: std::io::Cursor<Vec<u8>>,
            broken: bool,
        }

        impl std::io::Read for Breakable {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.broken {
                    return Err(std::io::Error::from(std::io::ErrorKind::IsADirectory));
                }
                self.inner.read(buf)
            }
        }

        impl std::io::Seek for Breakable {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                self.inner.seek(position)
            }
        }

        let options = crate::PdfOpenOptions {
            description: b"input.pdf".to_vec(),
            ..Default::default()
        };
        let mut pdf = Pdf::open_with_options(
            Breakable {
                inner: std::io::Cursor::new(minimal_pdf_bytes()),
                broken: false,
            },
            options,
        )
        .expect("open");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolver.with_reader_mut(|reader| reader.broken = true);

        let error = pdf
            .resolve(&handle)
            .expect_err("a named source read failure must be reported");
        let Error::SystemBytes(message) = error else {
            panic!("expected qpdf-shaped read failure, got {error:?}"); // cov:ignore: the assertion covers the defensive variant split
        };
        let rendered = String::from_utf8_lossy(&message);
        assert!(rendered.starts_with("input.pdf"), "{rendered}");
        assert!(rendered.contains(": read "), "{rendered}");
        assert!(rendered.ends_with(" bytes"), "{rendered}");
    }

    /// A described source that reaches EOF but cannot perform qpdf's
    /// normalization seek must preserve that seek failure instead of
    /// reporting a failed read.
    #[test]
    fn a_described_input_source_preserves_eof_normalization_seek_failures() {
        struct EofSeekFails;

        impl std::io::Read for EofSeekFails {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }

        impl std::io::Seek for EofSeekFails {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                match position {
                    std::io::SeekFrom::Current(0) => Ok(0),
                    std::io::SeekFrom::End(0) => {
                        Err(std::io::Error::other("EOF normalization seek failed"))
                    }
                    _ => panic!("unexpected seek during the direct read probe"), // cov:ignore: the direct probe only issues Current(0) and End(0)
                }
            }
        }

        let resolver = ResolverHandle::new_shared(
            EofSeekFails,
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            false,
            Diagnostics::default(),
            ResolverWarningOptions::new(crate::QPDFLogger::create(), true, b"input.pdf".to_vec()),
            0,
        );
        let error = resolver
            .core
            .borrow_mut()
            .read(&mut [0; 1])
            .expect_err("the EOF normalization seek failure must propagate");

        assert!(
            matches!(&error, Error::Io(error) if error.to_string() == "EOF normalization seek failed"),
            "the seek failure must not be rewritten as a read failure, got {error:?}"
        );
    }

    /// Two nested [`super::ResolverHandle::read_stream`] frames each keep their
    /// own saved offset — a single shared save/restore slot would give the
    /// outer stream the inner one's position.
    ///
    /// **The nesting is only reachable on an error path, and that is a fact
    /// about the format rather than about this fixture.** Entering
    /// `read_stream` twice requires the `/Length` reference to resolve to a
    /// stream, and a stream is not an integer — so qpdf's own
    /// `length_obj.isInteger()` test (`libqpdf/QPDF.cc:1370`) rejects the outer
    /// stream whenever the inner frame was entered at all. The inner
    /// resolution still completes first, which is what makes it observable:
    /// object 3's payload is asserted directly, and it can only be right if
    /// its own `stream_offset` was saved and restored while object 2's was
    /// also live.
    ///
    /// The two payloads differ in length and content so a mix-up cannot
    /// coincide.
    #[test]
    fn nested_stream_length_resolutions_each_restore_their_own_offset() {
        let outer: &[u8] = b"OUTER-PAYLOAD";
        let inner: &[u8] = b"IN";

        let mut second = b"2 0 obj\n<< /Length 3 0 R >>\nstream\n".to_vec();
        second.extend_from_slice(outer);
        second.extend_from_slice(b"\nendstream\nendobj\n");

        let mut third = b"3 0 obj\n<< /Length 4 0 R >>\nstream\n".to_vec();
        third.extend_from_slice(inner);
        third.extend_from_slice(b"\nendstream\nendobj\n");

        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            second,
            third,
            format!("4 0 obj\n{}\nendobj\n", inner.len()).into_bytes(),
        ]);

        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: false,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open strict fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));

        pdf.resolve(&handle)
            .expect("qpdf catches the unusable outer /Length");
        assert!(handle.is_null());

        assert_eq!(
            pdf.get_object_handle(ObjectRef::new(3, 0))
                .get_raw_stream_data()
                .expect("read inner raw stream data")
                .as_slice(),
            inner,
            "the inner stream, resolved while the outer frame was live, must \
             have read from its own saved offset"
        );
        assert!(
            pdf.resolver.core.borrow().resolving.is_empty(),
            "every mark must be gone once the outermost resolution returns"
        );
    }

    /// A nested handle such as `/AP /N 5 0 R` resolves through the owning
    /// document.
    ///
    /// The mechanism this exercises — [`ChildHandles::indirect_handle`]
    /// minting a canonical handle during a parse — is the same one
    /// [`a_nested_reference_resolves_to_the_documents_canonical_handle`]
    /// already pins; what is new here is the *container shape*. That
    /// sibling's two nested refs are `/Pages`, a dictionary value that *is*
    /// the reference, and `/Kids`' entry, an array item. `/AP` here is a
    /// direct sub-dictionary — never itself dereferenced — and `/N` is
    /// nested inside *that*: a dictionary nested inside a dictionary, a
    /// shape no other fixture in this file builds. (Parser nesting depth is
    /// not what distinguishes it — `/Kids[0]` and `/AP /N` both land at
    /// parser depth 3 — it is genuinely the container kind.)
    ///
    /// It is also the only fixture whose *test body* dereferences a handle
    /// obtained purely by navigating an already-resolved value's own
    /// dictionary rather than one re-fetched through
    /// [`Pdf::get_object_handle`]. Production code already does this same
    /// thing — [`ResolverHandle::read_stream`] dereferences the `/Length`
    /// handle it just pulled out of the freshly-parsed stream dictionary the
    /// same way — which is exactly why the mutation below also reddens the
    /// two fixtures that exercise indirect `/Length`: their nested handle is
    /// navigated, not re-fetched, too.
    ///
    /// **This mutation reddens it, and its siblings too, not just it.**
    /// Changing [`ChildHandles::indirect_handle`] to mint an unattached
    /// handle — `ObjectHandle::new_indirect_unresolved(object_ref,
    /// NO_PARSED_OFFSET)` in place of `self.resolver.get_object_handle(...)`
    /// — makes this test's `n.try_dereference()` return `Error::Internal`
    /// ("... belongs to a dropped PDF") instead of resolving, which is
    /// exactly the failure the acceptance criterion rules out. The same
    /// change also reddens
    /// `a_nested_reference_resolves_to_the_documents_canonical_handle` (the
    /// minted `/Pages` child is no longer `is_same_object_as` the handle
    /// [`Pdf::get_object_handle`] hands back) and every other fixture that
    /// dereferences a handle reached through a nested `N G R` rather than a
    /// fresh top-level fetch — because minting is one function shared by
    /// every nesting depth and container shape, not a distinct routine this
    /// fixture alone calls into. That sharing is why one fixture at each
    /// distinct *shape* earns its place rather than one fixture proving the
    /// whole mechanism: a bug scoped to one shape (say, a dictionary nested
    /// inside a dictionary, as `/AP` is) would not necessarily show up in a
    /// fixture built only from top-level refs and arrays.
    #[test]
    fn a_nested_ap_n_reference_resolves_through_the_owning_document() {
        let appearance_payload: &[u8] = b"q 1 0 0 1 0 0 cm Q";
        let mut appearance_stream = format!(
            "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Length {} >>\nstream\n",
            appearance_payload.len()
        )
        .into_bytes();
        appearance_stream.extend_from_slice(appearance_payload);
        appearance_stream.extend_from_slice(b"\nendstream\nendobj\n");

        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Annots [4 0 R] >>\nendobj\n".to_vec(),
            b"4 0 obj\n<< /Type /Annot /Subtype /Widget /Rect [0 0 100 100] \
              /AP << /N 5 0 R >> >>\nendobj\n"
                .to_vec(),
            appearance_stream,
        ]);

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let annotation: ObjectHandle = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve(&annotation)
            .expect("the annotation dictionary resolves");

        let ap = annotation
            .as_dictionary()
            .expect("the annotation is a dictionary")
            .get(b"/AP".as_slice())
            .expect("the annotation has /AP")
            .clone();
        // Pins the fixture's own shape, not resolver behavior: confirms /AP
        // really did parse as an inline sub-dictionary rather than a
        // reference, which is the precondition the rest of this test needs.
        assert!(
            ap.is_direct(),
            "/AP must be direct in this fixture for the dict-inside-dict shape \
             below to be the one under test"
        );

        let n: ObjectHandle = ap
            .as_dictionary()
            .expect("/AP is a dictionary")
            .get(b"/N".as_slice())
            .expect("/AP has /N")
            .clone();
        assert!(
            n.is_indirect() && !n.is_resolved(),
            "the annotation's own parse must have minted /N as an unresolved \
             indirect handle, not resolved it eagerly or copied its value"
        );

        pdf.resolve(&n).expect(
            "a nested handle reached only by navigating /AP /N, never re-fetched \
             through `Pdf::get_object_handle`, must still resolve through the \
             owning document",
        );

        assert_eq!(
            n.get_raw_stream_data()
                .expect("read appearance raw stream data")
                .as_slice(),
            appearance_payload,
            "the resolved appearance stream must carry its own payload"
        );
        assert!(
            n.is_same_object_as(&pdf.get_object_handle(ObjectRef::new(5, 0))),
            "the nested handle must be the document's one canonical handle for object 5"
        );
    }

    /// `links` streams, each declaring its `/Length` as a reference to the
    /// next, ending in a plain integer — the shape that makes
    /// [`super::ResolverHandle::resolve_indirect`] re-enter itself once per
    /// object without ever repeating a reference.
    ///
    /// Generated rather than committed: 4000 links is 314,083 bytes, two
    /// orders of magnitude past anything else in `tests/fixtures`.
    fn chained_indirect_length_pdf_bytes(links: u32) -> Vec<u8> {
        let mut pdf = Vec::from(*b"%PDF-1.4\n");
        let mut offsets = Vec::new();
        for number in 1..=links {
            offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(
                format!(
                    "{number} 0 obj\n<< /Length {} 0 R >>\nstream\n\nendstream\nendobj\n",
                    number + 1
                )
                .as_bytes(),
            );
        }
        // The chain's foot: a direct integer, so the deepest frame returns a
        // usable length rather than bottoming out on the input's end.
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(format!("{} 0 obj\n0\nendobj\n", links + 1).as_bytes());
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Type /Catalog /Pages {} 0 R >>\nendobj\n",
                links + 2,
                links + 3
            )
            .as_bytes(),
        );
        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
                links + 3
            )
            .as_bytes(),
        );

        let size = links + 4;
        let xref_start = pdf.len() as u64;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for offset in &offsets {
            xref.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
                links + 2
            )
            .as_bytes(),
        );
        pdf
    }

    /// A 4000-link `/Length` chain resolves to a diagnosis instead of
    /// aborting the process, on a thread far too small to hold 4000 frames.
    ///
    /// **The depth this pins is qpdf's, not an arbitrary one.** qpdf 11.9.0
    /// run against this exact generator's output survives 4000 links —
    /// `qpdf --show-object=1`, exit 3, recovery warnings — and segfaults
    /// (exit 139) at 20,000 and at 100,000, because `QPDF::resolve`'s
    /// `m->resolving` check (`libqpdf/QPDF.cc:1706-1712`) is a
    /// reference-repeat test with no depth counter. A depth limit in flpdf
    /// would therefore have to refuse documents qpdf reads, which is why
    /// [`super::ResolverHandle::resolve_indirect`] grows the stack instead.
    ///
    /// **The small stack is the point, not a convenience.** Measured before
    /// the `stacker::maybe_grow` wrap, on libtest's own default thread: this
    /// resolver aborted the whole test binary — `has overflowed its stack /
    /// fatal runtime error: stack overflow, aborting`, SIGABRT — at 300 links
    /// in a debug build and 2000 in a release build, both far under qpdf's
    /// 4000. Pinning it on a 256 KiB thread makes the assertion independent of
    /// whichever default a runner happens to give, and reproduces the
    /// situation `lift_bounded`'s own wrap exists for: a caller that does not
    /// own the thread it is called on. Unix pins 256 KiB; Windows uses a
    /// larger initial stack because stacker's fiber backend has larger debug
    /// frames before it can switch to a grown fiber.
    ///
    /// The expected outcome is a *diagnosis*: link 1's `/Length` resolves to
    /// link 2, which is a stream rather than an integer. qpdf reaches the same
    /// judgement (`/Length key in stream dictionary is not an integer`,
    /// `libqpdf/QPDF.cc:1379`) and then recovers the length. This test uses
    /// explicit `repair = false`, so it pins the resolver's warning/null
    /// fallback rather than the recovery arm.
    #[test]
    fn a_long_chain_of_indirect_lengths_grows_the_stack_instead_of_aborting() {
        // Built inside the closure rather than moved in: `Pdf` and
        // `ObjectHandle` are not `Send`, the same reason `reader.rs`'s
        // `trailer_key_handle_is_null_when_the_keys_own_value_exceeds_the_parse_depth_bound`
        // builds its tree in the spawned thread.
        #[cfg(windows)]
        let stack_size = 32 * 1024 * 1024;
        #[cfg(not(windows))]
        let stack_size = 256 * 1024;
        std::thread::Builder::new()
            .stack_size(stack_size)
            .spawn(|| {
                let bytes = chained_indirect_length_pdf_bytes(4000);
                // This test covers resolution stack growth, not the separate
                // recursive teardown behavior is separate. On Windows,
                // dropping the deep graph after the assertion can overflow
                // before the spawned thread reports the result under test.
                let mut pdf = std::mem::ManuallyDrop::new(
                    Pdf::open_mem_owned_with_options(
                        bytes,
                        crate::PdfOpenOptions {
                            repair: false,
                            ..crate::PdfOpenOptions::default()
                        },
                    )
                    .expect("open strict fixture"),
                );
                let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));

                pdf.resolve(&handle)
                    .expect("qpdf catches the unusable chained /Length");
                assert!(handle.is_null());
                assert!(pdf
                    .repair_diagnostics()
                    .entries()
                    .iter()
                    .any(|diagnostic| diagnostic
                        .message
                        .contains("/Length key in stream dictionary is not an integer")));
            })
            .expect("spawn")
            .join()
            .expect("a 4000-link chain must not overflow a small stack");
    }

    fn synthetic_mismatch_pdf(repair_target_present: bool) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let obj2_offset = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\ntrue\nendobj\n");
        let _obj1_offset = pdf.len();
        if repair_target_present {
            pdf.extend_from_slice(b"1 0 obj\n(recovered)\nendobj\n");
        } else {
            pdf.extend_from_slice(b"3 0 obj\n42\nendobj\n");
        }
        let xref_offset = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 3\n0000000000 65535 f \n{obj2_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 2 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn synthetic_malformed_recovery_mismatch_pdf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let obj2_offset = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\ntrue\nendobj\n");
        pdf.extend_from_slice(b"1 0 obj\n<< /Length /Broken >>\nstream\nabc\nendstream\nendobj\n");
        let xref_offset = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 3\n0000000000 65535 f \n{obj2_offset:010} 00000 n \n{obj2_offset:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 2 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn synthetic_mismatch_discovers_unindexed_object_pdf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let object_two_offset = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\ntrue\nendobj\n");
        pdf.extend_from_slice(b"1 0 obj\n(recovered)\nendobj\n");
        pdf.extend_from_slice(b"3 0 obj\n99\nendobj\n");
        let xref_offset = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 3\n0000000000 65535 f \n{object_two_offset:010} 00000 n \n{object_two_offset:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 2 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn synthetic_mismatch_discovers_loaded_tombstone_pdf() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let object_two_offset = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\ntrue\nendobj\n");
        pdf.extend_from_slice(b"1 0 obj\n(recovered)\nendobj\n");
        pdf.extend_from_slice(b"3 0 obj\n99\nendobj\n");
        let xref_offset = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{object_two_offset:010} 00000 n \n{object_two_offset:010} 00000 n \n0000000000 00001 f \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 2 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// A recovered stream whose original xref entries both point into the
    /// first object's dictionary.  The stale second offset is intentionally
    /// between the real object boundaries so it can truncate a legacy read
    /// window after canonical recovery.
    fn recovered_stream_with_stale_boundary() -> (Vec<u8>, u64, u64) {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let object_one_offset = pdf.len() as u64;
        let object_one = b"1 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n";
        let false_offset = object_one_offset + b"1 0 obj\n<< /Length".len() as u64;
        pdf.extend_from_slice(object_one);
        let object_two_offset = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n22\nendobj\n");
        let xref_offset = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "xref\n0 3\n0000000000 65535 f \n{false_offset:010} 00000 n \n{false_offset:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        (pdf, object_one_offset, object_two_offset)
    }

    fn malformed_recovery_pdf() -> Vec<u8> {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let object_offset = pdf.len() as u64;
        // The initial xref points into the object header, forcing recovery;
        // the recovered object then has an unusable stream length, which the
        // canonical qpdf stream path recovers after the xref retry.
        pdf.extend_from_slice(b"1 0 obj\n<< /Length /Broken >>\nstream\nabc\nendstream\nendobj\n");
        let false_offset = object_offset + 4;
        let xref_offset = pdf.len() as u64;
        pdf.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{false_offset:010} 00000 n \n").as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn recovered_objstm_member_pdf() -> Vec<u8> {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let catalog_offset = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

        let object_stream_offset = pdf.len() as u64;
        let object_stream_data = b"7 0 << /Value 9 >>";
        pdf.extend_from_slice(
            format!(
                "5 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
                object_stream_data.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(object_stream_data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_offset = pdf.len() as u64;
        let mut xref = String::from("xref\n0 8\n0000000000 65535 f \n");
        xref.push_str(&format!("{catalog_offset:010} 00000 n \n"));
        for _ in 2..=4 {
            xref.push_str("0000000000 00000 f \n");
        }
        xref.push_str(&format!("{object_stream_offset:010} 00000 n \n"));
        xref.push_str("0000000000 00000 f \n");
        // The damaged table advertises object 7 as uncompressed at the
        // object-stream offset; recovery replaces it with the validated type-2
        // mapping discovered from the stream contents.
        xref.push_str(&format!("{object_stream_offset:010} 00000 n \n"));
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn reconstruct_retry_on_header_mismatch_with_recovery_enabled() {
        let bytes = synthetic_mismatch_pdf(true);
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open");

        assert!(!pdf.reconstructed_xref());

        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&handle).expect("resolved after recovery");
        assert_eq!(
            handle.unparse_resolved(),
            b"(recovered)",
            "object 1 0 must resolve to the reconstructed value"
        );

        assert!(pdf.reconstructed_xref());

        let warnings: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|d| d.message.clone())
            .collect();
        assert!(
            warnings.iter().any(|w| w.contains("file is damaged")),
            "diagnostics must contain 'file is damaged': {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("expected 1 0 obj")),
            "diagnostics must contain triggering error 'expected 1 0 obj': {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("Attempting to reconstruct cross-reference table")),
            "diagnostics must contain 'Attempting to reconstruct cross-reference table': {warnings:?}"
        );
    }

    #[test]
    fn reconstruction_reregisters_privately_removed_unindexed_object_like_qpdf() {
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(
            synthetic_mismatch_discovers_unindexed_object_pdf(),
            options,
        )
        .expect("open recovery fixture");
        let removed_ref = ObjectRef::new(3, 0);
        pdf.remove_object_handle(removed_ref)
            .expect("remove the unindexed object before recovery");

        // Resolving object 1 forces xref reconstruction. QPDF::removeObject
        // erases only the exact xref/cache state (QPDF.cc:1996-2006); it does
        // not add this object number to reconstruction's deleted_objects set.
        // Reconstruction scans the stale body and registers it
        // (QPDF.cc:516-575,1194-1210). This source-derived private-method
        // contract is distinct from the public probe's removal_proxy, which
        // observes replaceObject(..., newNull()).
        let recovery_trigger: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&recovery_trigger)
            .expect("the damaged header must recover object 1");

        assert!(pdf.reconstructed_xref());
        assert!(pdf.get_xref_table().contains_key(&removed_ref));
        let recovered: ObjectHandle = pdf.get_object_handle(removed_ref);
        pdf.resolve(&recovered)
            .expect("reconstruction must mint a canonical handle");
        assert_eq!(recovered.as_integer(), Some(99));
        assert!(pdf.resolver.registered_handle(removed_ref).is_some());
        assert!(pdf
            .get_all_objects()
            .expect("enumerate the recovered cache")
            .iter()
            .any(|handle| handle.object_ref() == Some(removed_ref)));
    }

    fn assert_generation_replacement_matches_qpdf_tombstone_lifetime(
        mut replace: impl FnMut(&mut Pdf<std::io::Cursor<Vec<u8>>>, ObjectRef, i64),
    ) {
        let mut pdf = Pdf::open_mem_owned_with_options(
            synthetic_mismatch_discovers_unindexed_object_pdf(),
            crate::PdfOpenOptions {
                repair: true,
                ..Default::default()
            },
        )
        .expect("open recovery fixture");
        let source_ref = ObjectRef::new(3, 0);
        let replacement_ref = ObjectRef::new(3, 1);

        pdf.remove_object_handle(source_ref)
            .expect("remove source object before replacement");
        replace(&mut pdf, source_ref, 70);
        assert_eq!(
            pdf.get_object_handle(source_ref).as_integer(),
            Some(70),
            "same-generation replacement must remain visible before recovery"
        );
        replace(&mut pdf, replacement_ref, 71);

        let recovery_trigger: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&recovery_trigger)
            .expect("damaged header must trigger xref reconstruction");

        assert!(pdf.reconstructed_xref());
        let xref = pdf.get_xref_table();
        assert!(
            xref.contains_key(&source_ref),
            "physical recovery must expose the source generation in getXRefTable"
        );
        assert!(
            !xref.contains_key(&replacement_ref),
            "a cache-only replacement generation must stay out of getXRefTable"
        );

        let all_objects = pdf.get_all_objects().expect("enumerate recovered objects");
        for object_ref in [source_ref, replacement_ref] {
            assert!(
                all_objects
                    .iter()
                    .any(|handle| handle.object_ref() == Some(object_ref)),
                "get_all_objects must retain {object_ref:?} after recovery"
            );
        }

        let recovered_source: ObjectHandle = pdf.get_object_handle(source_ref);
        pdf.resolve(&recovered_source)
            .expect("same-generation replacement must keep a canonical handle");
        assert_eq!(
            recovered_source.as_integer(),
            Some(70),
            "qpdf keeps the same-generation replacement cached across recovery"
        );
        let replacement: ObjectHandle = pdf.get_object_handle(replacement_ref);
        pdf.resolve(&replacement)
            .expect("different-generation replacement must stay initialized");
        assert_eq!(replacement.as_integer(), Some(71));
        assert!(pdf.resolver.registered_handle(source_ref).is_some());
        assert!(pdf.resolver.registered_handle(replacement_ref).is_some());
    }

    #[test]
    fn replace_object_generation_replacement_matches_qpdf_tombstone_lifetime() {
        assert_generation_replacement_matches_qpdf_tombstone_lifetime(|pdf, object_ref, value| {
            pdf.replace_object(object_ref, ObjectHandle::integer(value))
                .expect("replace canonical object");
        });
    }

    #[test]
    fn reconstruction_discards_loaded_free_object_tombstones() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            synthetic_mismatch_discovers_loaded_tombstone_pdf(),
            crate::PdfOpenOptions {
                repair: true,
                ..Default::default()
            },
        )
        .expect("open recovery fixture");
        let removed_ref = ObjectRef::new(3, 0);

        let recovery_trigger: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&recovery_trigger)
            .expect("the damaged header must recover object 1");

        assert!(pdf.reconstructed_xref());
        assert!(pdf.resolver.xref_entry(removed_ref).is_some());
        let recovered: ObjectHandle = pdf.get_object_handle(removed_ref);
        pdf.resolve(&recovered)
            .expect("reconstruction must re-register the stale body after xref loading clears it");
        assert_eq!(recovered.as_integer(), Some(99));
        assert!(pdf
            .get_all_objects()
            .expect("enumerate the recovered cache")
            .iter()
            .any(|handle| handle.object_ref() == Some(removed_ref)));
    }

    #[test]
    fn public_resolve_retries_a_recovered_header_mismatch() {
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(synthetic_mismatch_pdf(true), options).expect("open");

        let recovered: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&recovered)
            .expect("public resolve must use the reconstructed xref");
        assert_eq!(
            recovered.as_string(),
            Some(b"recovered".to_vec()),
            "canonical resolver must not turn a recoverable object into null"
        );
        assert!(pdf.reconstructed_xref());

        let mut second_pdf = Pdf::open_mem_owned_with_options(
            synthetic_mismatch_pdf(true),
            crate::PdfOpenOptions {
                repair: true,
                ..Default::default()
            },
        )
        .expect("open second recovery fixture");
        let second_recovered: ObjectHandle = second_pdf.get_object_handle(ObjectRef::new(1, 0));
        second_pdf
            .resolve(&second_recovered)
            .expect("canonical resolver must use the reconstructed xref");
        assert_eq!(second_recovered.as_string(), Some(b"recovered".to_vec()));
    }

    #[test]
    fn public_resolve_preserves_absent_recovery_as_null() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            synthetic_mismatch_pdf(false),
            crate::PdfOpenOptions {
                repair: true,
                ..Default::default()
            },
        )
        .expect("open absent-recovery fixture");
        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));

        pdf.resolve(&handle)
            .expect("absent recovered object must resolve to null");
        assert!(pdf.reconstructed_xref());
        assert!(handle.is_resolved());
        assert!(handle.is_null());
        assert_eq!(handle.get_parsed_offset(), -1);
    }

    #[test]
    fn public_resolve_returns_null_for_unindexed_objstm_member() {
        let object_ref = ObjectRef::new(7, 0);
        let mut pdf = Pdf::open_mem_owned_with_options(
            recovered_objstm_member_pdf(),
            crate::PdfOpenOptions {
                repair: true,
                ..Default::default()
            },
        )
        .expect("open");

        let handle: ObjectHandle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)
            .expect("an unindexed packed member must resolve to null");
        assert!(handle.is_null());
        assert!(pdf.reconstructed_xref());
    }

    #[test]
    fn public_resolve_recovers_a_malformed_stream_after_xref_reconstruction() {
        let bytes = synthetic_malformed_recovery_mismatch_pdf();
        let stream_offset = bytes
            .windows(b"stream\n".len())
            .position(|window| window == b"stream\n")
            .expect("stream keyword")
            + b"stream\n".len();
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            crate::PdfOpenOptions {
                repair: true,
                ..Default::default()
            },
        )
        .expect("open malformed-recovery fixture");

        let stream: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&stream)
            .expect("qpdf stream recovery must run after xref reconstruction");
        assert!(stream.as_stream_dict().is_some(), "recovered stream");
        assert_eq!(
            stream
                .get_raw_stream_data()
                .expect("recovered stream data")
                .as_slice(),
            b"abc\n"
        );
        assert!(pdf.repair_diagnostics().entries().iter().any(|entry| {
            entry.message
                == format!("(object 1 0, offset {stream_offset}): recovered stream length: 4")
        }));
    }

    #[test]
    fn mismatch_without_recovery_warns_and_resolves_to_null() {
        let bytes = synthetic_mismatch_pdf(true);
        let options = crate::PdfOpenOptions {
            repair: false,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open");

        assert!(!pdf.reconstructed_xref());

        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&handle)
            .expect("qpdf catches a type-1 header mismatch");
        assert!(handle.is_null());
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|entry| entry.message.contains("expected 1 0 obj")));
        assert!(!pdf.reconstructed_xref());
    }

    #[test]
    fn absent_after_rebuild_warns_and_resolves_to_null() {
        let bytes = synthetic_mismatch_pdf(false);
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open");

        assert!(!pdf.reconstructed_xref());

        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&handle)
            .expect("absent post-rebuild resolves to null without panicking");
        assert!(handle.is_null(), "absent post-rebuild must resolve to null");

        assert!(pdf.reconstructed_xref());

        let warnings: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|d| d.message.clone())
            .collect();
        assert!(
            warnings.iter().any(|w| w
                .contains("object 1 0 not found in file after regenerating cross reference table")),
            "diagnostics must contain absent-after-rebuild warning: {warnings:?}"
        );
    }

    #[test]
    fn absent_from_xref_table_resolves_to_null_without_reconstruction() {
        let bytes = synthetic_mismatch_pdf(true);
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open");

        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(99, 0));
        pdf.resolve(&handle).expect("absent entry resolves to null");
        assert!(handle.is_null(), "absent entry must resolve to null");
        assert!(
            !pdf.reconstructed_xref(),
            "absent entry must not trigger xref reconstruction"
        );
    }

    #[test]
    fn zero_offset_xref_entry_warns_and_resolves_to_null_without_reconstruction() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        pdf.resolver
            .insert_xref_entry(object_ref, XrefEntry::Uncompressed { offset: 0 });

        let handle: ObjectHandle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)
            .expect("qpdf treats an offset-zero object as null");

        assert!(handle.is_null(), "offset zero must resolve to null");
        assert!(
            !pdf.reconstructed_xref(),
            "offset zero must not enter xref reconstruction"
        );
        assert!(
            pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
                diagnostic.offset == Some(0) && diagnostic.message == "object has offset 0"
            }),
            "offset-zero resolution must emit qpdf's warning"
        );
    }

    #[test]
    fn second_reconstruction_attempt_rethrows_error_to_prevent_infinite_loop() {
        let bytes = synthetic_mismatch_pdf(false);
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open");

        // First resolution triggers reconstruction and sets reconstructed_xref = true
        let recovery_trigger: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        let _ = pdf.resolve(&recovery_trigger);
        assert!(pdf.reconstructed_xref());

        // Simulate a second recovery trigger by invoking reconstruct_xref_and_retry directly
        let err = Error::parse(10, "expected 2 0 obj");
        let result = pdf
            .resolver
            .reconstruct_xref_and_retry(err, ObjectRef::new(2, 0)); // cov:ignore: executed reconstruction retry continuation has no LLVM counter.

        assert!(
            matches!(&result, Err(Error::Parse { message, .. }) if message == "expected 2 0 obj"),
            "second reconstruction attempt must re-throw original error: {result:?}"
        );
    }

    #[test]
    fn reconstruct_xref_and_retry_with_non_parse_trigger_error() {
        let bytes = synthetic_mismatch_pdf(true);
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open");

        let err = Error::file_io(
            "read",
            "dummy.pdf",
            std::io::Error::other("custom io error"),
        );
        // qpdf only triggers reconstruction on QPDFExc (parse errors).
        // A non-parse trigger propagates unchanged without touching the guard.
        let res = pdf
            .resolver
            .reconstruct_xref_and_retry(err, ObjectRef::new(1, 0));
        assert!(
            matches!(res, Err(Error::FileIo { .. })),
            "non-parse error must propagate unchanged: {res:?}"
        );
    }

    #[test]
    fn reconstruct_xref_and_retry_when_read_object_at_offset_fails() {
        // A PDF where reconstruction finds the object in the xref scan but
        // the recovered object body has an unusable stream length. The retry
        // disables another xref reconstruction, while qpdf's readStream still
        // performs its own stream-length recovery.
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(malformed_recovery_pdf(), options)
            .expect("open malformed-object fixture");

        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&handle)
            .expect("the reconstructed stream must recover its length");
        assert_eq!(
            handle
                .get_raw_stream_data()
                .expect("recovered source stream")
                .as_ref(),
            b"abc\n"
        );
    }

    #[test]
    fn reconstruction_keeps_canonical_resolution_on_the_rebuilt_xref() {
        let (bytes, _object_one_offset, _object_two_offset) =
            recovered_stream_with_stale_boundary();
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open fixture");
        let object_one: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&object_one)
            .expect("canonical resolution must recover object 1");

        let object_two: ObjectHandle = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve(&object_two)
            .expect("canonical cache must use the rebuilt offset");
        assert_eq!(object_two.as_integer(), Some(22));
        assert!(pdf.reconstructed_xref());
    }

    #[test]
    fn reconstruction_reconciles_compressed_member_provenance() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let changed_type = ObjectRef::new(1, 0);
        let changed_parent = ObjectRef::new(2, 0);
        let unchanged = ObjectRef::new(3, 0);

        pdf.compressed_member_parents.insert(
            changed_type,
            crate::pdf::CompressedMemberProvenance {
                source_stream: 9,
                source_index: 1,
            },
        );
        pdf.compressed_member_parents.insert(
            changed_parent,
            crate::pdf::CompressedMemberProvenance {
                source_stream: 9,
                source_index: 1,
            },
        );
        pdf.compressed_member_parents.insert(
            unchanged,
            crate::pdf::CompressedMemberProvenance {
                source_stream: 9,
                source_index: 3,
            },
        );
        pdf.resolver
            .insert_xref_entry(changed_type, XrefEntry::Uncompressed { offset: 10 });
        pdf.resolver.insert_xref_entry(
            changed_parent,
            XrefEntry::Compressed {
                stream: 9,
                index: 2,
            },
        );
        pdf.resolver.insert_xref_entry(
            unchanged,
            XrefEntry::Compressed {
                stream: 9,
                index: 3,
            },
        );
        pdf.resolver.core.borrow_mut().reconstructed_xref = true;

        pdf.synchronize_cache_with_resolver_xref();

        assert!(!pdf.compressed_member_parents.contains_key(&changed_type));
        assert!(!pdf.compressed_member_parents.contains_key(&changed_parent));
        assert_eq!(
            pdf.compressed_member_parents
                .get(&unchanged)
                .map(|provenance| (provenance.source_stream, provenance.source_index)),
            Some((9, 3))
        );
    }

    #[test]
    fn reconstruction_synchronizes_before_replace_object() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        pdf.cache = crate::cache::test_support::stale_deleted_entry(object_ref);
        pdf.resolver
            .insert_xref_entry(object_ref, XrefEntry::Uncompressed { offset: 10 });
        pdf.resolver.core.borrow_mut().reconstructed_xref = true;

        pdf.replace_object(object_ref, ObjectHandle::null())
            .expect("replace the reconstructed object with null");

        assert!(matches!(
            pdf.cache.entry(object_ref),
            Some(crate::cache::CacheEntry::Unresolved { .. })
        ));
        assert!(
            pdf.dirty_object_refs.contains(&object_ref),
            "replace_object must record the mutation after reconstruction refreshes a stale entry"
        );
    }

    #[test]
    fn full_rewrite_synchronizes_recovered_compressed_parent_provenance() {
        let object_ref = ObjectRef::new(7, 0);
        let stream_ref = ObjectRef::new(5, 0);
        let mut pdf = Pdf::open_mem_owned(recovered_objstm_member_pdf()).expect("open fixture");
        let stream_offset = match pdf.resolver.xref_entry(stream_ref) {
            Some(XrefEntry::Uncompressed { offset }) => offset,
            other => panic!("object stream must have a type-1 xref entry, got {other:?}"), // cov:ignore: fixture invariant is asserted by this test
        };
        let root_ref = pdf.root_ref().expect("catalog ref");
        pdf.resolver.insert_xref_entry(
            object_ref,
            XrefEntry::Compressed {
                stream: stream_ref.number,
                index: 0,
            },
        );
        pdf.cache.set_compressed(object_ref, stream_ref.number, 0);
        let recovered = pdf.get_object_handle(object_ref);
        pdf.resolve(&recovered)
            .expect("resolve compressed member canonically");
        // The canonical resolver carries this relationship in the type-2 xref
        // entry. Seed the legacy compatibility projection explicitly so this
        // test can continue to exercise its later reconstruction cleanup
        // without re-entering the legacy `set_object` route.
        pdf.compressed_member_parents.insert(
            object_ref,
            crate::pdf::CompressedMemberProvenance {
                source_stream: stream_ref.number,
                source_index: 0,
            },
        );
        let root = pdf.get_object_handle(root_ref);
        pdf.resolve(&root).expect("catalog");
        root.replace_key(b"/Recovered", recovered).unwrap();
        pdf.mark_object_handle_dirty(&root).unwrap();

        // This is the state immediately after editing a member that was
        // originally in an object stream: the canonical replacement records
        // the dirty value and its old compressed-member provenance.
        pdf.replace_object(object_ref, ObjectHandle::integer(42))
            .unwrap();

        // A later canonical recovery discovers that the same object is a
        // standalone type-1 object. The writer must observe this live xref
        // before classifying the dirty ref.
        pdf.resolver.insert_xref_entry(
            object_ref,
            XrefEntry::Uncompressed {
                offset: stream_offset,
            },
        );
        pdf.resolver.core.borrow_mut().reconstructed_xref = true;

        let mut writer = crate::PdfWriter::new(&mut pdf);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("full rewrite");
        let emitted_ref = writer
            .get_renumbered_obj_gen(object_ref)
            .expect("query recovered object mapping")
            .expect("reachable recovered object must be emitted");
        let output = writer.get_buffer().expect("take full-rewrite output");

        let mut reopened = Pdf::open_mem_owned(output).expect("reopen full-rewrite output");
        let emitted: ObjectHandle = reopened.get_object_handle(emitted_ref);
        reopened
            .resolve(&emitted)
            .expect("resolve rewritten object");
        assert_eq!(
            emitted.as_integer(),
            Some(42),
            "a recovered standalone object must be emitted outside the obsolete ObjStm"
        );
    }

    #[test]
    fn handle_recovery_updates_public_object_enumeration() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            synthetic_mismatch_discovers_unindexed_object_pdf(),
            crate::PdfOpenOptions {
                repair: true,
                ..Default::default()
            },
        )
        .expect("open enumeration recovery fixture");
        let recovered_ref = ObjectRef::new(1, 0);
        let discovered_ref = ObjectRef::new(3, 0);

        assert!(!pdf.object_refs().contains(&discovered_ref));
        assert!(!pdf.live_object_refs().contains(&discovered_ref));

        let recovered: ObjectHandle = pdf.get_object_handle(recovered_ref);
        pdf.resolve(&recovered)
            .expect("handle resolution must reconstruct the xref");

        assert!(pdf.reconstructed_xref());
        assert!(
            pdf.object_refs().contains(&discovered_ref),
            "object_refs must include objects discovered by handle-driven recovery"
        );
        assert!(
            pdf.live_object_refs().contains(&discovered_ref),
            "live_object_refs must follow the reconstructed live xref"
        );
    }

    #[test]
    fn reconstruction_returns_null_for_unindexed_objstm_member() {
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(recovered_objstm_member_pdf(), options)
            .expect("open object-stream recovery fixture");

        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(7, 0));
        pdf.resolve(&handle)
            .expect("an unindexed packed member must resolve to null");
        assert_eq!(
            handle.unparse_resolved(),
            b"null",
            "reconstruction must not manufacture a type-2 entry for the packed member"
        );
        assert!(pdf.reconstructed_xref());
    }

    #[test]
    fn reconstruction_warns_and_resolves_to_null_when_the_detected_header_disappears() {
        let prefix = b"prefix\n";
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&synthetic_mismatch_pdf(true));
        let options = crate::PdfOpenOptions {
            repair: true,
            ..Default::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).expect("open prefixed PDF");
        assert_eq!(pdf.resolver.header_offset(), prefix.len());

        pdf.resolver.with_reader_mut(|reader| {
            reader.get_mut().truncate(prefix.len() - 1);
        });

        let handle: ObjectHandle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&handle)
            .expect("qpdf catches the reconstruction parse error");
        assert!(handle.is_null());
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|entry| entry.message.contains("input ended before")));
    }

    #[test]
    fn get_object_count_prepares_every_effective_xref_object_and_is_idempotent() {
        let mut pdf = Pdf::open(CountingReader::new(minimal_pdf_bytes())).expect("open");
        let reads_before = pdf.resolver.with_reader_mut(|reader| reader.reads);

        assert_eq!(
            pdf.resolver.max_object_number(),
            Some(1),
            "the canonical trailer rebind registers /Root during open"
        );

        let first = pdf.get_object_count().expect("prepare object cache");
        assert_eq!(first, 3);
        for object_ref in [
            ObjectRef::new(1, 0),
            ObjectRef::new(2, 0),
            ObjectRef::new(3, 0),
        ] {
            let handle = pdf
                .resolver
                .registered_handle(object_ref)
                .expect("every live xref entry must be registered");
            assert!(handle.is_resolved(), "{object_ref:?} must be resolved");
        }
        assert!(
            pdf.resolver
                .registered_handle(ObjectRef::new(0, u16::MAX))
                .is_none(),
            "free xref entries must not become canonical objects"
        );

        let reads_after_first = pdf.resolver.with_reader_mut(|reader| reader.reads);
        let second = pdf.get_object_count().expect("idempotent preparation");
        assert_eq!(second, first);
        assert_eq!(
            pdf.resolver.with_reader_mut(|reader| reader.reads),
            reads_after_first,
            "a fixed preparation must not resolve the xref table again"
        );
        assert!(reads_after_first > reads_before);
    }

    #[test]
    fn get_object_count_returns_zero_for_an_empty_canonical_cache() {
        let resolver = bare_resolver();

        assert_eq!(resolver.get_object_count().expect("empty cache"), 0);
        assert_eq!(
            resolver
                .get_object_count()
                .expect("fixed empty cache remains empty"),
            0
        );
    }

    #[test]
    fn get_object_count_rescans_after_xref_reconstruction() {
        let mut pdf = Pdf::open_mem_owned_with_options(
            synthetic_mismatch_discovers_unindexed_object_pdf(),
            crate::PdfOpenOptions {
                repair: true,
                ..Default::default()
            },
        )
        .expect("open recovery fixture");

        assert_eq!(pdf.get_object_count().expect("prepare after recovery"), 3);
        assert!(pdf.reconstructed_xref());
        for object_ref in [
            ObjectRef::new(1, 0),
            ObjectRef::new(2, 0),
            ObjectRef::new(3, 0),
        ] {
            let handle = pdf
                .resolver
                .registered_handle(object_ref)
                .expect("reconstructed xref entry must be registered");
            assert!(handle.is_resolved(), "{object_ref:?} must be resolved");
        }
    }

    #[test]
    fn get_object_count_keeps_parser_discovered_dangling_reference_in_canonical_cache() {
        let mut pdf = Pdf::open_mem_owned(dangling_reference_pdf_bytes()).expect("open");

        assert_eq!(
            pdf.get_object_count().expect("prepare dangling references"),
            99
        );
        let dangling = pdf
            .resolver
            .registered_handle(ObjectRef::new(99, 0))
            .expect("the parser must register a valid dangling reference");
        assert!(!dangling.is_resolved());
    }

    #[test]
    fn get_object_count_keeps_a_referenced_free_objgen_as_dangling_only() {
        let mut pdf = Pdf::open_mem_owned(free_reference_pdf_bytes()).expect("open");

        assert_eq!(pdf.get_object_count().expect("prepare free reference"), 4);
        let free = pdf
            .resolver
            .registered_handle(ObjectRef::new(4, 1))
            .expect("the parser must retain a referenced free ObjGen");
        assert!(!free.is_resolved());
        assert!(
            pdf.resolver
                .registered_handle(ObjectRef::new(0, u16::MAX))
                .is_none(),
            "the xref free head must not become a canonical object"
        );
    }

    #[test]
    fn next_obj_gen_uses_a_parser_discovered_dangling_reference() {
        let pdf = Pdf::open_mem_owned(dangling_reference_pdf_bytes()).expect("open");

        assert_eq!(
            pdf.resolver.next_obj_gen().expect("next dangling ObjGen"),
            ObjectRef::new(100, 0)
        );
    }

    #[test]
    fn next_obj_gen_excludes_unreferenced_high_free_xref_entries() {
        let pdf = Pdf::open_mem_owned(unreferenced_high_free_pdf_bytes()).expect("open");

        assert_eq!(
            pdf.resolver
                .next_obj_gen()
                .expect("next ObjGen after high free entry"),
            ObjectRef::new(4, 0)
        );
    }

    #[test]
    fn get_object_count_prepares_the_pinned_xref_stream_and_objstm_fixture() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/three-page-objstm.pdf"
        );
        let mut pdf = Pdf::open_mem_owned(std::fs::read(fixture).expect("read ObjStm fixture"))
            .expect("open ObjStm fixture");

        // qpdf 11.9.0 --show-xref reports 1/0 through 13/0 for this fixture.
        assert_eq!(pdf.get_object_count().expect("prepare xref stream"), 13);
        for object_ref in pdf.resolver.xref_refs() {
            assert!(
                pdf.resolver
                    .registered_handle(object_ref)
                    .is_some_and(|handle| handle.is_resolved()),
                "effective xref object {object_ref:?} must be resolved"
            );
        }
    }

    #[test]
    fn get_object_count_keeps_an_objstm_decode_failure_on_qpdfs_null_path() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let member_ref = ObjectRef::new(7, 0);
        pdf.resolver.insert_xref_entry(
            member_ref,
            XrefEntry::Compressed {
                stream: 9,
                index: 0,
            },
        );

        assert_eq!(
            pdf.get_object_count().expect("prepare malformed ObjStm"),
            9,
            "the missing ObjStm parent is itself a canonical dangling cache entry"
        );
        assert!(pdf
            .resolver
            .registered_handle(member_ref)
            .is_some_and(|handle| handle.is_null()));
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic
                .message
                .contains("supposed object stream 9 is not a stream")));
    }
}
