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
//! diagnostics to [`ResolverHandle::read_object_at_offset`] after it has
//! released its input adapter, so each source token contributes at most one
//! document warning.

use crate::object_handle::{DocumentResolver, ObjectValue, NO_PARSED_OFFSET};
use crate::parser::{
    parse_live_file_object_with_decrypter, LiveInput, LiveTokenSource, StringDecrypter,
};
use crate::pipeline::Pipeline;
use crate::tokenizer::{Token, Tokenizer};
use crate::{Diagnostic, Diagnostics, Error, ObjectHandle, ObjectRef, Result, XrefEntry};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::rc::{Rc, Weak};

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
/// time (`decryptStream`, `QPDF.cc:2491`); wiring the string decrypter in is
/// still flpdf-25kg.3.5 AC2. See [`ResolverCore::encryption_parameters`].
pub(crate) struct ResolverCore<R: Read + Seek + 'static> {
    /// qpdf `m->file` (`QPDF.hh:1456`).
    reader: R,
    /// Also qpdf `m->file`: when repair finds a valid header after leading
    /// material, qpdf does not keep the offset beside the input source, it
    /// *wraps* the source so the shift is invisible to every later read —
    /// `m->file = std::shared_ptr<InputSource>(new OffsetInputSource(m->file,
    /// global_offset))` (`libqpdf/QPDF.cc:406`). Keeping the shift beside
    /// `reader` and applying it in [`Self::seek`] and [`Self::tell`] puts it
    /// under the same single owner, without a second input-source type.
    ///
    /// It is *not* equivalent, and the difference is what
    /// [`Self::rewind_underlying_source`] exists for: wrapping makes the shift
    /// unskippable, so qpdf reaches the bytes before it through the wrapper's
    /// `proxied` member rather than through `m->file`.
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
    #[allow(dead_code)] // object streams are a later slice
    resolved_object_streams: BTreeSet<u32>,
    /// qpdf `m->attempt_recovery` (`QPDF.hh:1461`).
    ///
    /// Same on/off flag, opposite default: qpdf initialises it to `true` and
    /// `QPDF::setAttemptRecovery(false)` (`QPDF.hh:234`, `QPDF.cc:334`) opts
    /// out, whereas flpdf's
    /// [`crate::PdfOpenOptions::repair`] defaults to `false` and
    /// [`crate::Pdf::open_with_repair`] opts in. This records the permission
    /// the document was opened with, not whether recovery actually ran —
    /// qpdf's own flag is likewise setter-controlled, and it tracks a
    /// reconstruct that happened in a separate member
    /// (`m->reconstructed_xref`, `QPDF.hh:1480`) that flpdf does not port
    /// here.
    #[allow(dead_code)] // read once resolution can fail and recover
    attempt_recovery: bool,
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
    /// [`ResolverHandle::read_object_at_offset`] in between input operations;
    /// each of those takes and drops its own borrow, so nothing is held when
    /// the push happens.
    repair_diagnostics: Diagnostics,
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
    encryption_parameters: Rc<RefCell<Option<crate::reader::EncryptionState>>>,
    /// qpdf `InputSource::last_offset` (`include/qpdf/InputSource.hh:88`),
    /// which `getLastOffset()` reports and which every warning raised from a
    /// failed pipe is attributed to (`libqpdf/QPDF.cc:2513,2525`).
    ///
    /// Only a *read* updates it: both input sources set it to the position
    /// they are about to read from, before the read can fail
    /// (`BufferInputSource.cc:128`, `FileInputSource.cc:118-119`), and
    /// **`seek` never touches it**. Reporting the requested seek target
    /// instead would attribute a rejected seek to a byte the reader never
    /// reached.
    last_offset: u64,
}

impl<R: Read + Seek> ResolverCore<R> {
    /// Position the input source at qpdf-logical `offset`.
    ///
    /// qpdf `m->file->seek(offset, SEEK_SET)`. The header shift is applied
    /// here for the same reason `OffsetInputSource` applies it inside
    /// `m->file` (`libqpdf/QPDF.cc:406`): every caller above this line works
    /// in qpdf-logical coordinates and never sees the physical position.
    fn seek(&mut self, offset: u64) -> Result<()> {
        let physical = (self.header_offset as u64).saturating_add(offset);
        self.reader.seek(SeekFrom::Start(physical))?;
        Ok(())
    }

    /// The input source's current qpdf-logical position.
    ///
    /// qpdf `m->file->tell()`. This is the live position `QPDF::readStream`
    /// saves before resolving `/Length` and restores afterwards
    /// (`libqpdf/QPDF.cc:1367-1384`) — not a value recomputed from an
    /// argument, which is precisely why the restore is load-bearing.
    fn tell(&mut self) -> Result<u64> {
        Ok(self
            .reader
            .stream_position()?
            .saturating_sub(self.header_offset as u64))
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
        // qpdf `std::numeric_limits<qpdf_offset_t>::max()`.
        const MAX_OFFSET: u64 = i64::MAX as u64;

        let position = self.reader.stream_position()?;
        if delta > MAX_OFFSET.saturating_sub(position) {
            return Err(Error::parse(
                position as usize,
                format!("adding {delta} to {position} would cause an integer overflow"),
            ));
        }
        // Exact: the check above bounds `delta` by `i64::MAX`.
        self.reader.seek(SeekFrom::Current(delta as i64))?;
        Ok(())
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
        // Both of qpdf's input sources record where the read starts before it
        // can fail, so a failing read is still attributed to the byte it
        // reached for.
        self.last_offset = self.tell().unwrap_or(self.last_offset);
        let mut filled = 0;
        while filled < buf.len() {
            match self.reader.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(filled)
    }

    /// Position the *unwrapped* input source at its own offset zero, before
    /// the header shift.
    ///
    /// **The one method here with no `m->file` counterpart, and it is
    /// deliberately a primitive rather than a helper.** qpdf's wrapped source
    /// cannot express this: `OffsetInputSource::rewind` is
    /// `seek(0, SEEK_SET)` (`libqpdf/OffsetInputSource.cc:55-59`), which lands
    /// on *logical* zero, and every other method adds `global_offset` too. But
    /// the unwrapped source is not hidden from qpdf either — the wrapper holds
    /// it as `std::shared_ptr<InputSource> proxied`
    /// (`libqpdf/qpdf/OffsetInputSource.hh:24`). This is flpdf reaching that
    /// same member; `header_offset` is what stands in for the wrapper, so the
    /// member has to become a method.
    ///
    /// qpdf never reads `proxied` directly. flpdf must, for
    /// `ResolverHandle::read_physical_input` — see there.
    fn rewind_underlying_source(&mut self) -> Result<()> {
        self.reader.seek(SeekFrom::Start(0))?;
        Ok(())
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
    /// The owning document's [`crate::Pdf`] identity, stamped onto every
    /// handle this minted — `ObjectHandle`'s `pdf_unique_id`, whose own doc
    /// traces it to qpdf's `QPDF::getUniqueId`
    /// (`include/qpdf/QPDF.hh:283`, `libqpdf/QPDF.cc:2294-2296`).
    ///
    /// Duplicated from `Pdf::unique_id` rather than reached through it: the
    /// resolver has no `&Pdf`, and the value is assigned once before either
    /// is constructed, so the two copies cannot drift.
    pdf_unique_id: u64,
}

/// The `QPDF::StringDecrypter` qpdf binds to one indirect object immediately
/// before `QPDFParser::parse` (`libqpdf/QPDF.cc:1331-1340`).
struct ResolverStringDecrypter<'resolver, R: Read + Seek + 'static> {
    object_ref: ObjectRef,
    encryption_parameters: Rc<RefCell<Option<crate::reader::EncryptionState>>>,
    resolver: &'resolver ResolverHandle<R>,
}

impl<R: Read + Seek + 'static> StringDecrypter for ResolverStringDecrypter<'_, R> {
    fn decrypt_string(&mut self, bytes: &mut Vec<u8>) -> Result<()> {
        let warn_unknown_string = {
            let mut encryption_parameters = self.encryption_parameters.borrow_mut();
            let encryption = encryption_parameters.as_mut().ok_or_else(|| {
                Error::Internal("string decrypter invoked without encryption parameters".into())
            })?;
            encryption.decrypt_object_string(self.object_ref, bytes)?
        };

        if warn_unknown_string {
            self.resolver.push_warning(
                "unknown encryption filter for strings (check /StrF in /Encrypt dictionary); \
                 strings may be decrypted improperly",
            );
        }
        Ok(())
    }
}

impl<R: Read + Seek> ResolverHandle<R> {
    /// Build the resolver already inside its `Rc`.
    ///
    /// `Rc::new_cyclic` rather than `Rc::new` because [`Self::self_weak`]
    /// has to point at this very allocation; there is no way to add it
    /// afterwards without making the field mutable.
    pub(crate) fn new_shared(
        reader: R,
        header_offset: usize,
        source_xref_entries: BTreeMap<ObjectRef, XrefEntry>,
        attempt_recovery: bool,
        repair_diagnostics: Diagnostics,
        pdf_unique_id: u64,
    ) -> Rc<Self> {
        Rc::new_cyclic(|self_weak| Self {
            core: RefCell::new(ResolverCore {
                reader,
                header_offset,
                source_xref_entries,
                object_cache: BTreeMap::new(),
                resolving: BTreeSet::new(),
                resolved_object_streams: BTreeSet::new(),
                attempt_recovery,
                repair_diagnostics,
                encryption_parameters: Rc::new(RefCell::new(None)),
                last_offset: 0,
            }),
            self_weak: self_weak.clone(),
            pdf_unique_id,
        })
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
                    self.pdf_unique_id,
                    resolver,
                )
            })
            .clone()
    }

    /// The canonical handle for `object_ref` **if one has already been
    /// minted**, without minting one.
    ///
    /// The read-only counterpart of [`Self::get_object_handle`], for the
    /// `&self` callers that ask whether a reference has a handle at all.
    pub(crate) fn registered_handle(&self, object_ref: ObjectRef) -> Option<ObjectHandle> {
        self.core.borrow().object_cache.get(&object_ref).cloned()
    }

    /// Every canonical handle minted so far, in [`ObjectRef`] order.
    ///
    /// qpdf `QPDF::getAllObjects` walks `m->obj_cache` the same way
    /// (`libqpdf/QPDF.cc:1285-1295`).
    pub(crate) fn all_object_handles(&self) -> Vec<ObjectHandle> {
        self.core.borrow().object_cache.values().cloned().collect()
    }

    /// The refs whose canonical handle has already left `NotYetResolved`.
    ///
    /// Collected under one short borrow, and deliberately *not* a
    /// predicate-taking variant: [`ObjectHandle::is_resolved`] cannot
    /// re-enter resolution, whereas a caller-supplied predicate could.
    pub(crate) fn resolved_object_refs(&self) -> Vec<ObjectRef> {
        self.core
            .borrow()
            .object_cache
            .iter()
            .filter(|(_, handle)| handle.is_resolved())
            .map(|(object_ref, _)| *object_ref)
            .collect()
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

    /// Whether a canonical handle occupies `number` at any generation.
    pub(crate) fn holds_object_number(&self, number: u32) -> bool {
        self.core
            .borrow()
            .object_cache
            .range(ObjectRef::new(number, 0)..=ObjectRef::new(number, u16::MAX))
            .next()
            .is_some()
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
    /// first half is here. flpdf accumulates and lets the front end decide;
    /// `flpdf-cli` walks the collection and writes to stderr itself
    /// (`crates/flpdf-cli/src/main.rs:5342`), which is why flpdf has no
    /// `suppress_warnings` counterpart to port.
    ///
    /// Takes `&self`, which is the whole reason the sink moved here: the
    /// resolver reaches its document through a `Weak` and never holds a
    /// `&mut Pdf`. `Pdf::push_warning` is the same door for callers that do.
    ///
    /// Borrow discipline: the `borrow_mut()` is taken and dropped inside this
    /// expression, so it composes with a nested resolution — but it must not
    /// be called while a borrow of the core is already held.
    pub(crate) fn push_warning(&self, message: impl Into<String>) {
        self.core
            .borrow_mut()
            .repair_diagnostics
            .push(Diagnostic::warning(message, None));
    }

    /// [`Self::push_warning`] with the offset qpdf attributes the warning to.
    ///
    /// qpdf carries the position inside the exception it throws — every
    /// `damagedPDF(file, object, offset, message)` overload takes one
    /// (`include/qpdf/QPDF.hh:1044-1050`) — where flpdf keeps it in
    /// [`Diagnostic::offset`] beside the text.
    ///
    /// Same borrow discipline as [`Self::push_warning`].
    /// qpdf `InputSource::getLastOffset` (`include/qpdf/InputSource.hh:55`).
    ///
    /// Borrow discipline: taken and dropped inside this expression.
    fn last_offset(&self) -> u64 {
        self.core.borrow().last_offset
    }

    pub(crate) fn push_warning_at(&self, offset: u64, message: impl Into<String>) {
        self.core
            .borrow_mut()
            .repair_diagnostics
            .push(Diagnostic::warning(message, Some(offset)));
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

    /// The offset repair chose as qpdf-logical zero. See
    /// [`ResolverCore::header_offset`].
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
    /// `pub(in crate::reader)` rather than the plan's `pub(crate)`: the
    /// return type names `EncryptionState`, which is private to `reader.rs`
    /// (`pub(self)`), so a `pub(crate)` signature trips rustc's
    /// `private_interfaces` lint (denied under this workspace's
    /// `-D warnings` clippy gate) — a real signature is at most as visible
    /// as the least visible type it names. Narrowing the *accessor* to match
    /// is the fix that stays inside `resolver.rs`; widening `EncryptionState`
    /// itself would touch `reader.rs`, out of scope for this task. Every
    /// caller identified so far (`Pdf` and flpdf-25kg.3.10's pipe-time read
    /// primitive) lives under `crate::reader`, so this is not yet known to be
    /// too narrow — but if a later consumer lives outside `crate::reader`,
    /// this will need widening together with `EncryptionState` itself.
    pub(in crate::reader) fn encryption_parameters(
        &self,
    ) -> Rc<RefCell<Option<crate::reader::EncryptionState>>> {
        self.core.borrow().encryption_parameters.clone()
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

    /// A snapshot of the whole cross-reference table.
    ///
    /// Prefer [`Self::xref_refs_matching`] when only a selection of refs is
    /// wanted: this clones every entry, and moving the table behind a
    /// `RefCell` would otherwise make that the only option for all ten
    /// existing consumers.
    pub(crate) fn xref_entries(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        self.core.borrow().source_xref_entries.clone()
    }

    /// The refs the source cross-reference table declares whose entry
    /// satisfies `keep`, collected under a single short borrow.
    ///
    /// The escape hatch from [`Self::xref_entries`]: a caller that wants a
    /// filtered list of refs gets one without cloning the table to filter it
    /// and drop it again.
    ///
    /// `keep` runs while the borrow is held, so it inherits the module-level
    /// hazard: a predicate that resolved something would re-enter. Every
    /// in-tree predicate is a `matches!` over the entry it is handed.
    pub(crate) fn xref_refs_matching(&self, keep: impl Fn(&XrefEntry) -> bool) -> Vec<ObjectRef> {
        self.core
            .borrow()
            .source_xref_entries
            .iter()
            .filter(|(_, entry)| keep(entry))
            .map(|(object_ref, _)| *object_ref)
            .collect()
    }

    /// Read `[offset, next)` — or `[offset, EOF)` when `next` is `None` — in
    /// qpdf-logical coordinates, into an owned buffer.
    ///
    /// `qpdf-legacy-tenant(flpdf-25kg.3.5)`: **not a port of anything in
    /// qpdf, and the resolver never calls it.** Its only callers are `Pdf`'s
    /// legacy read helpers, on the other side of the cutover. Delete it once
    /// they are gone.
    ///
    /// It lives on [`ResolverHandle`] rather than on [`ResolverCore`] on
    /// purpose. `ResolverCore`'s method surface is meant to be checkable
    /// against qpdf line by line — it is `m->file`'s operations and nothing
    /// else — and a bounded owned-window read is exactly the flpdf-ism that
    /// surface must stay free of. Hosting it one level out keeps it built
    /// *on* the primitives instead of beside them.
    ///
    /// Do not build on its shape. qpdf streams from `m->file` and brackets the
    /// one re-entrant seam by saving and restoring the position
    /// (`QPDF::readStream`, `libqpdf/QPDF.cc:1360-1398`). The design of record
    /// names generalising this owned-window shape as a wrong turn that would
    /// entrench a divergence, so `readObjectAtOffset`/`readStream` port that
    /// save/restore seam rather than reusing this.
    pub(crate) fn read_window(&self, offset: u64, next: Option<u64>) -> Result<Vec<u8>> {
        self.seek(offset)?;
        self.read_to_owned(next.map(|next| next.saturating_sub(offset)))
    }

    /// Read the entire input from the *unwrapped* source's offset zero, header
    /// shift included. Callers that want qpdf-logical coordinates use
    /// [`Self::read_window`].
    ///
    /// `qpdf-legacy-tenant(flpdf-25kg.3.5)`: same status and same reason for
    /// living here as [`Self::read_window`]. Its one caller is
    /// `Pdf::source_bytes`, which the writer uses to copy the original file
    /// verbatim — which is why the bytes before the header shift have to be
    /// included, and why [`ResolverCore::rewind_underlying_source`] exists.
    pub(crate) fn read_physical_input(&self) -> Result<Vec<u8>> {
        self.core.borrow_mut().rewind_underlying_source()?;
        self.read_to_owned(None)
    }

    /// Collect `limit` bytes — or everything left when `limit` is `None` —
    /// from the current position into an owned buffer.
    ///
    /// Grows as it goes rather than pre-allocating `limit`, and every caller
    /// it has wants that property, not merely the two legacy tenants below it:
    /// [`Self::read_window`]'s bound comes from the *next* cross-reference
    /// offset, which a corrupt table can make arbitrarily large on a small
    /// file, and [`Self::read_stream`]'s is a declared `/Length` — a value the
    /// input asserts about itself. `std::io::Read::take(n).read_to_end(..)`,
    /// which this replaces, had the same property.
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
    /// Returns whether the bytes were delivered, mirroring qpdf's `bool`:
    /// a damaged source is a warning and a `false`, not an error for the
    /// caller to propagate.
    ///
    /// qpdf allocates `length` and reads it in one operation (`:2497-2501`).
    /// That is deliberate here even though parsing deliberately does *not*
    /// pre-allocate a declared `/Length`: by pipe time the offset and length
    /// have already been validated by the framing scan.
    ///
    /// **The decryption stage is not inserted yet.** qpdf prepends one when
    /// the document is encrypted (`:2490-2492`, `QPDF::decryptStream`); until
    /// that lands, an encrypted document pipes ciphertext. No test asserts
    /// that as correct.
    //
    // The parameter list is qpdf's (`QPDF.cc:2542-2550` passes seven beyond
    // the receiver); bundling them would be a shape this port does not have a
    // counterpart for. Not wired to a production caller until
    // `QPDF_Stream::pipeStreamData`'s source dispatch lands, the same
    // not-yet-wired state the other ported primitives carry.
    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn pipe_stream_data(
        &self,
        object_ref: ObjectRef,
        offset: i64,
        length: usize,
        _stream_dict: &ObjectHandle,
        pipeline: &mut dyn Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> bool {
        // qpdf's shape is one `try` whose every escape lands in a `catch`,
        // followed by a tail common to all of them (`libqpdf/QPDF.cc:2494-2538`).
        // `attempted_finish` is what the tail consults, and it is set
        // immediately before `finish()` so that the tail finishes exactly the
        // pipelines that never got the call.
        let mut attempted_finish = false;
        let Some(failure) =
            self.attempt_pipe_stream_data(offset, length, pipeline, &mut attempted_finish)
        else {
            return true;
        };

        if !suppress_warnings {
            match failure {
                // qpdf `:2498-2500` throws `damagedPDF(file, "", offset +
                // read, ...)`, which its `catch (QPDFExc&)` arm reports
                // (`:2505-2509`). The position is where the read stopped, not
                // where it began.
                PipeFailure::ShortRead { at } => {
                    self.push_warning_at(at, "unexpected EOF reading stream data");
                }
                // qpdf `:2510-2530`.
                PipeFailure::Decoding { at, ref detail } => {
                    let og = format!("{} {}", object_ref.number, object_ref.generation);
                    self.push_warning_at(
                        at,
                        format!("error decoding stream data for object {og}: {detail}"),
                    );
                    if will_retry {
                        self.push_warning_at(
                            at,
                            "stream will be re-processed without filtering to avoid data loss",
                        );
                    }
                }
            }
        }

        // qpdf `:2531-2537`, reached from either arm. Its own failure is
        // swallowed.
        if !attempted_finish {
            let _ = pipeline.finish();
        }
        false
    }

    /// qpdf's `try` block (`libqpdf/QPDF.cc:2495-2504`). `None` is its
    /// `return true`; anything else is what it would have thrown.
    fn attempt_pipe_stream_data(
        &self,
        offset: i64,
        length: usize,
        pipeline: &mut dyn Pipeline,
        attempted_finish: &mut bool,
    ) -> Option<PipeFailure> {
        let start = match u64::try_from(offset) {
            Ok(start) => start,
            // qpdf's input source throws `std::logic_error("INTERNAL ERROR:
            // BufferInputSource offset < 0")` here
            // (`libqpdf/BufferInputSource.cc:119-121`).
            Err(_) => {
                return Some(PipeFailure::Decoding {
                    at: self.last_offset(),
                    detail: format!("stream offset {offset} is negative"),
                })
            }
        };

        // qpdf `:2496-2497`: the seek comes *before* the allocation, so an
        // offset the source rejects is diagnosed without first trusting the
        // declared length for a buffer.
        if let Err(error) = self.seek(start) {
            // `getLastOffset()` is where the last *read* happened; a seek
            // never sets it, so a rejected seek is not attributed to the byte
            // it was asked for.
            return Some(PipeFailure::Decoding {
                at: self.last_offset(),
                detail: error.to_string(),
            });
        }

        // qpdf `:2497` allocates with `make_unique<char[]>`, whose
        // `std::bad_alloc` its own `catch (std::exception&)` arm reports
        // (`:2510-2520`). An infallible `vec![0u8; length]` would abort the
        // process instead, so a hostile declared length has to go through a
        // fallible reservation.
        let mut buf: Vec<u8> = Vec::new();
        if let Err(error) = buf.try_reserve_exact(length) {
            return Some(PipeFailure::Decoding {
                at: self.last_offset(),
                detail: format!("cannot allocate {length} bytes of stream data: {error}"),
            });
        }
        buf.resize(length, 0);

        // qpdf `:2498-2500`.
        match self.read(&mut buf) {
            Ok(read) if read == length => {}
            Ok(read) => {
                return Some(PipeFailure::ShortRead {
                    at: start.saturating_add(read as u64),
                })
            }
            Err(error) => {
                return Some(PipeFailure::Decoding {
                    at: self.last_offset(),
                    detail: error.to_string(),
                })
            }
        }

        // qpdf `:2501-2504`.
        if let Err(error) = pipeline.write(&buf) {
            return Some(PipeFailure::Decoding {
                at: self.last_offset(),
                detail: error.to_string(),
            });
        }
        *attempted_finish = true;
        if let Err(error) = pipeline.finish() {
            return Some(PipeFailure::Decoding {
                at: self.last_offset(),
                detail: error.to_string(),
            });
        }
        None
    }

    /// Test-only mutable access to the input source itself, for fixtures that
    /// arm a reader to start failing partway through a resolution.
    ///
    /// `f` must not resolve anything: it runs while the core's borrow is held,
    /// so a nested resolve would double-borrow. Every in-tree caller only
    /// flips a field on a fault-injecting cursor.
    #[cfg(test)]
    pub(crate) fn with_reader_mut<T>(&self, f: impl FnOnce(&mut R) -> T) -> T {
        f(&mut self.core.borrow_mut().reader)
    }

    /// Test-only: install a cross-reference entry the source did not declare,
    /// for fixtures that drive resolution of a hand-built object.
    #[cfg(test)]
    pub(crate) fn insert_xref_entry(&self, object_ref: ObjectRef, entry: XrefEntry) {
        self.core
            .borrow_mut()
            .source_xref_entries
            .insert(object_ref, entry);
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

    /// Append the next chunk of input to `bytes`, reporting whether anything
    /// was left to append.
    ///
    /// The position advances by exactly what was appended, so `bytes` always
    /// mirrors `[scan start, current position)`.
    ///
    /// This belongs only to legacy token consumers that still use
    /// [`Self::scan_forward`]. File-object parsing has moved to the live
    /// InputSource adapter and does not accumulate or retry its prefix.
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
    /// This remains a legacy token helper. `QPDFTokenizer` and `QPDFParser`
    /// consume `m->file` a character at a time
    /// (`QPDFTokenizer::presentCharacter`, and `unreadCh` to give back the
    /// one character of overshoot — `libqpdf/QPDF.cc:1656` uses
    /// `seek(-1, SEEK_CUR)` for the same purpose), so qpdf never has to know
    /// in advance how far an object reaches. The file-object path no longer
    /// uses this helper: it advances through `LiveTokenSource` directly.
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
    /// One divergence stays: qpdf passes `allow_bad = true`
    /// (`QPDF::readToken`, `:1536-1539`), so a *malformed* token is returned
    /// as `tt_bad` and likewise reported as the missing keyword, whereas
    /// `read_token(false, ..)` below turns it into an error. Widening that
    /// changes every caller's error surface, so it is recorded rather than
    /// taken here.
    ///
    /// This legacy helper still reports a slice-relative error for malformed
    /// stream framing. File-object diagnostics no longer pass through it and
    /// retain their absolute input offsets.
    fn read_token_from_input(&self) -> Result<Token> {
        self.scan_forward(|bytes| {
            let mut tokenizer = Tokenizer::new(bytes);
            tokenizer.allow_eof();
            let token = tokenizer.read_token(false, 0)?;
            Ok((token, tokenizer.position()))
        })
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

    /// qpdf `QPDF::readObjectAtOffset` (`libqpdf/QPDF.cc:1541-1697`),
    /// restricted to the call `QPDF::resolve` makes for a type-1 entry.
    ///
    /// Returns the value and the parsed offset to record on the slot, in
    /// qpdf-logical coordinates.
    ///
    /// Four of qpdf's behaviours here are **not** ported, each because it
    /// belongs to a class this slice excludes:
    ///
    /// - the `offset == 0` special case (`:1571-1575`), which warns and
    ///   returns null — a damaged-xref case;
    /// - the `try_recovery` catch (`:1613-1637`), which reconstructs the xref
    ///   table and retries — gated on `m->attempt_recovery`, and recovery is
    ///   out of scope;
    /// - the object-id **mismatch** outcome (`:1600-1608` together with
    ///   `:1641-1693`). qpdf, with recovery off, warns and then reads the
    ///   object anyway — but it caches what it read under the id the *file*
    ///   carries, not the one the xref table asked for, so back in
    ///   `QPDF::resolve` the requested reference is still unresolved and falls
    ///   through to `updateCache(og, QPDF_Null::create(), -1, -1)` (`:1745`).
    ///   Reproducing that means porting the resolve-to-null fallback, which
    ///   arrives with the recovery work; until then a mismatch is an error, so
    ///   nothing silently diverges;
    /// - `end_before_space`/`end_after_space` (`:1649-1663`), the two extra
    ///   positions `updateCache` stores for linearization-hint validation.
    ///   flpdf's `ObjCache` counterpart is a bare `ObjectHandle`, which has
    ///   nowhere to put them; the position is still left after `endobj` so a
    ///   later slice can take them without changing this shape. The
    ///   *whitespace skip* that computes `end_after_space` is skipped with
    ///   them, and it is not inert: it throws
    ///   `damagedPDF(tell(), "EOF after endobj")` (`:1660`) when the object is
    ///   the last thing in the file, which `QPDF::resolve` catches
    ///   (`:1737-1738`) and turns into a resolve-to-null (`:1745-1748`). Every
    ///   flpdf fixture whose object ends at EOF therefore resolves where qpdf
    ///   returns null — see
    ///   `a_direct_value_ending_the_input_warns_and_is_still_resolved`.
    ///
    /// **Positions reported from here are the file's.** The live parser reads
    /// the resolver-owned input directly, as qpdf does; `QPDFParser::warn`
    /// takes its position from `input->getLastOffset()`
    /// (`libqpdf/QPDFParser.cc:516-519`).
    /// `a_recovered_malformed_body_reports_its_warning_at_the_file_offset`
    /// pins the absolute diagnostic. The mismatch error is likewise anchored
    /// directly at `offset`.
    fn read_object_at_offset(
        &self,
        offset: u64,
        expected: ObjectRef,
    ) -> Result<(ObjectValue, i64)> {
        self.seek(offset)?;
        let (found, parsed, trailing) = {
            let mut input = self.live_input();
            let mut tokenizer = LiveTokenSource::new(&mut input);
            let number = read_live_header_integer(tokenizer.next_token()?)?;
            let generation = read_live_header_integer(tokenizer.next_token()?)?;
            let obj = tokenizer.next_token()?;
            if !obj.is_word_value(b"obj") {
                return Err(Error::parse(obj.start, "expected obj"));
            }
            drop(tokenizer);

            let found = u32::try_from(number)
                .ok()
                .zip(u16::try_from(generation).ok())
                .map(|(number, generation)| ObjectRef::new(number, generation));
            let mut minter = ChildHandles { resolver: self };
            let encryption_parameters = self.encryption_parameters();
            let has_encryption = encryption_parameters.borrow().is_some();
            let mut decrypter = if has_encryption {
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
            )?;
            let trailing = if parsed.empty.is_none() {
                let mut trailing_tokens = LiveTokenSource::new(&mut input);
                let trailing = trailing_tokens.next_token()?;
                drop(trailing_tokens);
                Some(trailing)
            } else {
                None
            };
            input.finish()?;
            (found, parsed, trailing)
        };

        if found.is_some_and(|object_ref| object_ref.number == 0) {
            return Err(Error::parse(offset as usize, "object with ID 0"));
        }

        if found != Some(expected) {
            return Err(Error::parse(
                offset as usize,
                format!("expected {} {} obj", expected.number, expected.generation),
            ));
        }

        for warning in parsed.diagnostics {
            self.push_warning_at(warning.relative_offset as u64, warning.message);
        }

        if let Some(empty_offset) = parsed.empty {
            self.push_warning_at(empty_offset, "empty object treated as null");
            let (value, parsed_offset) = parsed
                .value
                .into_direct_value()
                .expect("live file parser's recovered empty object is always a direct null");
            debug_assert_eq!(parsed_offset, parsed.parsed_offset);
            return Ok((value, parsed_offset));
        }

        let (value, parsed_offset) = parsed.value.into_direct_value().expect(
            "live file parser's top-level bare-reference recovery always returns a direct value",
        );
        debug_assert_eq!(parsed_offset, parsed.parsed_offset);
        let trailing = trailing.expect("non-empty parse must have a framing token");
        if trailing.is_word_value(b"stream") {
            self.read_stream(value, parsed_offset)
        } else {
            if !trailing.is_word_value(b"endobj") {
                self.push_warning("expected endobj");
            }
            Ok((value, parsed_offset))
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
    /// input somewhere else entirely, and return. The *first* [`Self::seek`]
    /// back to `stream_offset` is what undoes that movement — delete it and a
    /// stream whose `/Length` is an indirect reference looks for `endstream`
    /// wherever the nested resolution happened to finish. The second one, in
    /// front of the payload read, has nothing to do with re-entrancy: it
    /// rewinds from where the `endstream` check left the position.
    ///
    /// **The two are separately pinned, and the mutations were run rather than
    /// reasoned.** Deleting the first reddens exactly the fixtures built on
    /// `indirect_length_pdf_bytes` — the ones with something to restore from —
    /// and nothing else; deleting the second reddens every fixture that
    /// asserts a payload, direct `/Length` included, while leaving the
    /// resolution-order assertions that carry no payload alone. Counts are
    /// left out on purpose: both sets grow with the fixtures, and this file
    /// has already carried one stale number.
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
    /// flpdf's [`ObjectValue::Stream`] owns its bytes, so the payload read has
    /// to happen somewhere, and it happens **after** the `endstream` check
    /// rather than before it. That ordering is the whole defence: reaching a
    /// non-EOF `endstream` token at `stream_offset + length` proves the input
    /// holds at least that many bytes, so the buffer that follows is bounded
    /// by the file rather than by the declaration. Reading first — as this did
    /// until a review of PR #630 — turns `<< /Length 9223372036854775000 >>`
    /// over a three-byte payload into `vec![0u8; 9223372036854775000]`, which
    /// is not an error but an allocator abort: `memory allocation of
    /// 9223372036854775000 bytes failed`, `SIGABRT`, process gone.
    /// `an_absurd_declared_length_is_diagnosed_without_allocating_it` pins it.
    ///
    /// The same ordering is why there is no short-read check on the payload:
    /// once the token check has passed it cannot come up short, and qpdf's own
    /// short-read arm is at pipe time, where the length has already been
    /// through this check or through `recoverStreamLength`.
    ///
    /// # Two divergences from qpdf's outcome, both pre-existing
    ///
    /// Every failure below is returned as an error. qpdf reaches neither
    /// outcome that way, and both gaps belong to slices this one excludes:
    ///
    /// - a `QPDFExc` — `expected endstream` is one — is caught at
    ///   `:1390-1397` and, when `m->attempt_recovery` is set (its default),
    ///   warned about and passed to `recoverStreamLength`. Observed on qpdf
    ///   11.9.0 for a stream whose `/Length` is 100000 over a 420-byte file:
    ///   `expected endstream`, then `attempting to recover stream length`,
    ///   then `recovered stream length: 4`. Recovery is out of this slice, so
    ///   flpdf stops at the first of those three;
    /// - anything that is *not* a `QPDFExc` — [`ResolverCore::seek_relative`]'s
    ///   overflow refusal, and the `lseek` failure a file-backed source raises
    ///   for an offset past the filesystem's maximum — passes that catch and
    ///   is warned about by `QPDF::resolve` (`:1739-1741`), leaving the object
    ///   to resolve to null (`:1745-1748`). Observed: `object 4/0: error
    ///   reading object: seek to …, offset 9223372036854775000 (1): Invalid
    ///   argument`, exit 3, 8.9 MB peak RSS. The resolve-to-null fallback is
    ///   not ported either — see [`Self::read_object_at_offset`].
    fn read_stream(&self, dict: ObjectValue, dict_offset: i64) -> Result<(ObjectValue, i64)> {
        self.validate_stream_line_end()?;

        // qpdf `:1365-1367`: "Must get offset before accessing any additional
        // objects since resolving a previously unresolved indirect object
        // will change file position."
        let stream_offset = self.tell()?;

        let length = Self::stream_length(&dict)?;
        let span = length as u64;

        // qpdf `:1383-1385`: "Seek in two steps to avoid potential integer
        // overflow", `m->file->seek(stream_offset, SEEK_SET)` then
        // `m->file->seek(toO(length), SEEK_CUR)`. Nothing is read and nothing
        // is allocated yet — the declared length has only moved the position.
        self.seek(stream_offset)?;
        self.seek_relative(span)?;

        // qpdf `:1386-1389`.
        if !self.read_token_from_input()?.is_word_value(b"endstream") {
            return Err(Error::parse(stream_offset as usize, "expected endstream"));
        }
        let after_endstream = self.tell()?;

        // No qpdf counterpart: `QPDF_Stream` keeps `stream_offset` and
        // `length` and reads the payload at pipe time, while
        // `ObjectValue::Stream` carries the bytes themselves (shared, but
        // still read here rather than at pipe time). `read_to_owned` grows to
        // what the input actually yields rather than pre-allocating `length`,
        // so even this now-bounded read never trusts the declaration.
        self.seek(stream_offset)?;
        let data = self.read_to_owned(Some(span))?;

        self.seek(after_endstream)?;
        // qpdf `:1350-1354`: `readObject` reads one more token after
        // `readStream` returns and warns if it is not `endobj`.
        if !self.read_token_from_input()?.is_word_value(b"endobj") {
            self.push_warning("expected endobj");
        }

        let dict = ObjectHandle::from_value(dict);
        dict.set_parsed_offset_if_unset(dict_offset);
        Ok((
            ObjectValue::Stream {
                stream_dict: dict,
                stream_data: Rc::new(data),
            },
            i64::try_from(stream_offset).unwrap_or(i64::MAX),
        ))
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
                        self.push_warning("stream keyword followed by carriage return only");
                    }
                }
                return Ok(());
            }
            if !crate::tokenizer::is_ws(byte) {
                self.unread_byte()?;
                self.push_warning("stream keyword not followed by proper line terminator");
                return Ok(());
            }
            self.push_warning("stream keyword followed by extraneous whitespace");
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
    /// **The `0` offsets below are placeholders, and the right value is
    /// recorded rather than taken.** Nothing rebases these because the only caller,
    /// [`Self::read_stream`], is not given the object's start. qpdf reports
    /// them at `readObject`'s own `offset` — `m->file->tell()` taken at
    /// `:1334`, immediately after the `obj` keyword and *before* any
    /// whitespace is skipped — passed to `damagedPDF(offset, …)` at `:1376`
    /// and `:1379`. Observed on qpdf 11.9.0 over a chained-`/Length` fixture:
    /// `/Length key in stream dictionary is not an integer` at 233690 against
    /// `attempting to recover stream length` at 233721, 31 bytes apart, which
    /// is exactly `\n<< /Length 4002 0 R >>\nstream\n`. flpdf's nearest
    /// quantity is the live parser's post-header position, which is taken
    /// after the header delimiter. Closing the gap means carrying the object
    /// header offset through `read_stream`, and is left to the slice that
    /// ports `end_before_space`.
    fn stream_length(dict: &ObjectValue) -> Result<usize> {
        let ObjectValue::Dictionary(entries) = dict else {
            return Err(Error::parse(
                0,
                "stream keyword follows an object that is not a dictionary",
            ));
        };
        let length = entries.get(b"Length".as_slice());
        if let Some(length) = length {
            length.try_dereference()?;
        }
        // qpdf tests `isNull()` before `isInteger()` and reports the two
        // separately (`:1373-1380`); an absent key reads as null there, so
        // both routes land on the same message here.
        match length.map(ObjectHandle::as_integer) {
            Some(Some(value)) => usize::try_from(value)
                .map_err(|_| Error::parse(0, "/Length key in stream dictionary is out of range")),
            Some(None) if length.is_some_and(|length| !length.is_null()) => Err(Error::parse(
                0,
                "/Length key in stream dictionary is not an integer",
            )),
            _ => Err(Error::parse(0, "stream dictionary lacks /Length key")),
        }
    }
}

/// Bytes pulled from the input source per [`ResolverHandle::refill`].
const INPUT_CHUNK: usize = 4096;

/// What qpdf's `pipeStreamData` `try` block would have thrown
/// (`libqpdf/QPDF.cc:2495-2504`), carrying the position each exception is
/// attributed to. The two variants are qpdf's two `catch` arms.
enum PipeFailure {
    /// `damagedPDF(file, "", offset + read, "unexpected EOF reading stream
    /// data")` (`:2498-2500`), caught as a `QPDFExc` (`:2505-2509`).
    ShortRead { at: u64 },
    /// Anything else (`:2510-2530`).
    Decoding { at: u64, detail: String },
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
}

impl<R: Read + Seek> crate::parser::HandleResolver for ChildHandles<'_, R> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.resolver.get_object_handle(object_ref)
    }
}

impl<R: Read + Seek> DocumentResolver for ResolverHandle<R> {
    /// Resolve `object_ref`'s slot in place.
    ///
    /// Two of `QPDF::resolve`'s branches (`libqpdf/QPDF.cc:1700-1753`) exist
    /// so far. The resolution loop is handled; no source class is implemented
    /// yet, so every other reference is rejected with [`Error::Unsupported`].
    /// That rejection is deliberately *not* a fallback into `Pdf`'s legacy
    /// `resolve_object_handle`/`resolve_borrowed` route: that bridge is what
    /// `flpdf-25kg.3.5`'s acceptance criteria forbid, so an unhandled class
    /// errors and gains real support in a later slice.
    ///
    /// Reaching this at all is what distinguishes an attached handle from a
    /// detached one — [`ObjectHandle::try_dereference`] reports
    /// `"belongs to a dropped PDF"` when it cannot upgrade its `Weak`, which
    /// is a different failure from this one.
    ///
    /// # The loop branch
    ///
    /// qpdf's loop branch (`libqpdf/QPDF.cc:1706-1712`) does three things:
    /// warns `damagedPDF("", "loop detected resolving object " +
    /// og.unparse(' '))`, calls `updateCache(og, QPDF_Null::create(), -1, -1)`,
    /// and returns without throwing. All three are ported; the notes below are
    /// on *how*, and on the one qpdf side effect that stays out.
    ///
    /// *Null at offset -1* is [`ObjectHandle::set_missing`], not
    /// `set_resolved(ObjectValue::Null)`. qpdf draws no distinction to port
    /// here — a loop and an object absent from the xref table both end at the
    /// same `updateCache(og, QPDF_Null::create(), -1, -1)` call (`:1711` and
    /// `:1748`) — so the loop takes whichever route flpdf's absent-reference
    /// case already takes, which is `set_missing` (`reader.rs`'s
    /// `resolve_object_handle`). It is also the one that clears the parsed
    /// offset, matching qpdf's `-1` argument; `set_resolved` leaves any
    /// recorded offset in place. The design's Parsed-Offset Contract names
    /// "cyclic" in the set `set_missing`'s own doc quotes.
    ///
    /// The two routes really are different internal states, so the choice was
    /// checked rather than assumed: `IndirectState::Missing` presents as null
    /// through `with_value` but hands out no `&mut` through `with_value_mut`,
    /// where `Resolved(ObjectValue::Null)` hands out one. Nothing can observe
    /// that today — all six `with_value_mut` callers (`replace_key`,
    /// `remove_key`, `replace_array_item`, `replace_array_items`,
    /// `replace_stream_data`, `append_array_item`) match on a container
    /// variant that `Null` is not, so both routes are the same no-op. Whoever
    /// gives a null value a mutable meaning should re-check this against qpdf,
    /// where the loop's cache entry is a live `QPDF_Null` in `m->obj_cache`.
    ///
    /// *The canonical cache* needs no separate write. The `handle` this was
    /// called with is already the [`ResolverCore::object_cache`] entry for
    /// `object_ref` — [`ObjectHandle::try_dereference`] can only reach here
    /// through a handle carrying this resolver's `Weak`, and
    /// [`Self::get_object_handle`] is the only thing that mints one — so
    /// `set_missing` writes straight through the cached slot, which is what
    /// qpdf's `updateCache` achieves with `cache.object->assign(...)`
    /// (`libqpdf/QPDF.cc:1849-1853`).
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
    /// [`Self::read_object_at_offset`] is that call; the value it returns is
    /// written into `handle`'s slot, which *is* the
    /// [`ResolverCore::object_cache`] entry (see the loop branch above).
    ///
    /// **The three phases, and where each borrow of [`ResolverCore`] lives.**
    ///
    /// 1. *Short borrows.* [`ResolveMark::begin`] takes one to test and set
    ///    `resolving`; [`Self::xref_entry`] takes one to read the entry. Both
    ///    end inside their own call.
    /// 2. *No borrow at all.* `read_object_at_offset` runs here. It reads and
    ///    parses through [`Self::scan_forward`], every step of which takes and
    ///    drops its own borrow, so the `/Length` dereference inside
    ///    [`Self::read_stream`] is free to re-enter this very method.
    /// 3. *Short borrows.* `set_resolved`/`set_parsed_offset_if_unset` touch
    ///    only the handle's own cell — which is the canonical cache entry, so
    ///    they are the `updateCache` equivalent — and the mark's `drop` takes
    ///    the last borrow of the core.
    ///
    /// **What is not ported, and why it is not a fallback.** A `Compressed`
    /// (type-2) entry, a `Free` entry and an entry absent from the table all
    /// return [`Error::Unsupported`]. qpdf resolves the first through
    /// `resolveObjectsInStream` and the other two to null (`:1745-1749`);
    /// both are later slices. Erroring is deliberately *not* a delegation to
    /// `Pdf`'s legacy `resolve_object_handle`/`resolve_borrowed` route —
    /// that bridge is what `flpdf-25kg.3.5`'s acceptance criteria forbid.
    ///
    /// A read or parse failure likewise propagates instead of taking qpdf's
    /// `catch (QPDFExc& e) { warn(e); }` (`:1740-1743`) followed by the
    /// resolve-to-null fallback (`:1745-1749`). Reproducing `warn(e)` means
    /// reproducing `QPDFExc`'s rendered text for whatever failed, which flpdf
    /// has no counterpart for; a fabricated message would be a worse ledger
    /// entry than a propagated error. The damaged-object route arrives with
    /// the recovery work `attempt_recovery` gates.
    ///
    /// # Why the body is wrapped in `stacker::maybe_grow`
    ///
    /// **This method is re-entrant, and nothing bounds how deep.** A stream
    /// whose `/Length` is an indirect reference to another stream whose
    /// `/Length` is an indirect reference to another … recurses
    /// `resolve_indirect` → [`Self::read_object_at_offset`] →
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
    /// `parser.rs`'s recursive-descent hub (`Parser::object`) and
    /// [`super::Pdf::lift_bounded`] already use: grow the stack rather than
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
        stacker::maybe_grow(
            super::READER_STACK_RED_ZONE,
            super::READER_STACK_GROWTH_SIZE,
            || {
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
                    ));
                    handle.set_missing();
                    return Ok(());
                };
                let entry = self.xref_entry(object_ref);

                // ---- phase 2: no borrow is held across this ----
                let Some(XrefEntry::Uncompressed { offset }) = entry else {
                    return Err(Error::Unsupported(format!(
                        "canonical resolver cannot yet resolve object {} {}: \
                         only uncompressed cross-reference entries are implemented",
                        object_ref.number, object_ref.generation
                    )));
                };
                let (value, parsed_offset) = self.read_object_at_offset(offset, object_ref)?;

                // ---- phase 3: short borrows, then the mark drops ----
                handle.set_resolved(value);
                handle.set_parsed_offset_if_unset(parsed_offset);
                Ok(())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ResolveMark;
    use super::ResolverHandle;
    use crate::object_handle::NO_PARSED_OFFSET;
    use crate::{Diagnostics, Error, ObjectRef, Pdf, Severity, XrefEntry};
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Cursor;
    use std::process::Command;
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

    /// A resolver built directly, bypassing `Pdf::open` entirely — the state
    /// qpdf's `Members::encp(new EncryptionParameters)` is in immediately
    /// after `QPDF` construction, before `initializeEncryption()` has run.
    fn bare_resolver() -> std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>> {
        ResolverHandle::new_shared(
            Cursor::new(minimal_pdf_bytes()),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            Diagnostics::default(),
            0,
        )
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

    /// A resolver over `bytes`, so a test can hand `pipe_stream_data` an
    /// offset and length directly instead of parsing a document to reach one.
    fn resolver_over(bytes: Vec<u8>) -> std::rc::Rc<ResolverHandle<Cursor<Vec<u8>>>> {
        ResolverHandle::new_shared(
            Cursor::new(bytes),
            0,
            BTreeMap::<ObjectRef, XrefEntry>::new(),
            false,
            Diagnostics::default(),
            0,
        )
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

        let ok = resolver.pipe_stream_data(
            ObjectRef::new(4, 0),
            offset,
            b"payload bytes".len(),
            &dict,
            &mut sink,
            false,
            false,
        );

        assert!(ok, "an in-bounds read succeeds");
        assert_eq!(sink.take_buffer().expect("buffer"), b"payload bytes");
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

        let ok = resolver.pipe_stream_data(
            ObjectRef::new(4, 0),
            offset,
            available + 100,
            &dict,
            &mut sink,
            false,
            false,
        );

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

        let ok = resolver.pipe_stream_data(
            ObjectRef::new(4, 0),
            9,
            1_000,
            &dict,
            &mut sink,
            true,
            false,
        );

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

        let ok =
            resolver.pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, false, false);

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

        let ok =
            resolver.pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, false, true);

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

        let ok =
            resolver.pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, true, false);

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

        let ok =
            resolver.pipe_stream_data(ObjectRef::new(4, 0), 9, 7, &dict, &mut sink, true, false);

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

        let ok = resolver.pipe_stream_data(
            ObjectRef::new(4, 0),
            past_end,
            7,
            &dict,
            &mut sink,
            false,
            false,
        );

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

        let ok =
            resolver.pipe_stream_data(ObjectRef::new(4, 0), -1, 7, &dict, &mut sink, false, false);

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

        let ok =
            resolver.pipe_stream_data(ObjectRef::new(4, 0), 9, 0, &dict, &mut sink, false, false);

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
                Diagnostics::default(),
                0,
            );
            let dict = crate::ObjectHandle::dictionary(vec![]);
            let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);

            let ok = resolver.pipe_stream_data(
                ObjectRef::new(4, 0),
                9,
                7,
                &dict,
                &mut sink,
                false,
                false,
            );

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

            let ok = resolver.pipe_stream_data(
                ObjectRef::new(4, 0),
                offset,
                length,
                &dict,
                &mut sink,
                true,
                false,
            );

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
            Diagnostics::default(),
            0,
        );
        let dict = crate::ObjectHandle::dictionary(vec![]);

        // One good pipe, so the last read is at offset 9.
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
        assert!(resolver.pipe_stream_data(
            ObjectRef::new(4, 0),
            9,
            7,
            &dict,
            &mut sink,
            false,
            false
        ));

        resolver.with_reader_mut(|reader| reader.fail_seeks = true);
        let mut sink = crate::pipeline::buffer::Buffer::new("stream data", None);
        assert!(!resolver.pipe_stream_data(
            ObjectRef::new(5, 0),
            1_000,
            7,
            &dict,
            &mut sink,
            false,
            false
        ));

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

        let ok = resolver.pipe_stream_data(
            ObjectRef::new(4, 0),
            9,
            usize::MAX,
            &dict,
            &mut sink,
            false,
            false,
        );

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
            allow_weak_crypto: true,
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
            crate::reader::EncryptionMode::Identity
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
        assert_eq!(encryption.cf_stream, crate::reader::EncryptionMode::Aes128);
        assert_eq!(encryption.file_key.len(), 16);
    }

    // This catches a production regression where canonical resolver parsing
    // exposes ciphertext because it has no object-bound StringDecrypter.
    // Removing the decrypter passed to `parse_live_file_object` makes either
    // the top-level or nested string assertion fail.
    #[test]
    fn canonical_resolver_decrypts_strings_at_parse_time() {
        use crate::encrypt_setup::EncryptParams;
        use crate::writer::{write_pdf_with_options, CompressStreams, WriteOptions};

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
        write_pdf_with_options(
            &mut plaintext,
            &mut encrypted,
            &WriteOptions {
                full_rewrite: true,
                compress_streams: CompressStreams::No,
                encrypt: Some(EncryptParams::v4_aes128(
                    b"user-pw".to_vec(),
                    b"owner-pw".to_vec(),
                )),
                ..WriteOptions::default()
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
        let info_ref = match rt.trailer().get("Info") {
            Some(crate::Object::Reference(object_ref)) => *object_ref,
            other => panic!("trailer /Info must be a reference, got {other:?}"),
        };

        let info = rt.get_object_handle(info_ref);
        info.try_dereference()
            .expect("canonical resolver must resolve /Info");
        let values = info.as_dictionary().expect("/Info must be a dictionary");
        assert_eq!(
            values
                .get(b"Title".as_slice())
                .and_then(crate::ObjectHandle::as_string),
            Some(b"TopSecretTitle".to_vec())
        );
        assert_eq!(
            values
                .get(b"Metadata".as_slice())
                .and_then(crate::ObjectHandle::as_dictionary)
                .and_then(|metadata| metadata.get(b"Label".as_slice()).cloned())
                .and_then(|label| label.as_string()),
            Some(b"NestedSecret".to_vec())
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
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

        handle.try_dereference().expect(
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
                .get(b"Type".as_slice())
                .and_then(crate::ObjectHandle::as_name),
            Some(b"Catalog".to_vec())
        );
    }

    /// Attaching a resolver must not disturb `Pdf::drop`'s teardown.
    ///
    /// A handle that outlives its document reads as null and does *not*
    /// error, because `Pdf::drop` disconnects every registry entry into
    /// `IndirectState::Destroyed` before the `Rc<ResolverHandle>` drops, and
    /// `try_dereference` short-circuits on any non-`NotYetResolved` state
    /// without consulting the resolver. That "reads as null" outcome is
    /// `flpdf-nrp3`'s recorded divergence from qpdf; this slice must leave it
    /// exactly as it found it, so this test pins it rather than endorsing it.
    #[test]
    fn a_handle_outliving_its_document_still_reads_as_null_instead_of_erroring() {
        let handle = {
            let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
            pdf.get_object_handle(ObjectRef::new(1, 0))
        };

        handle
            .try_dereference()
            .expect("a destroyed slot is terminal, not an error");
        assert!(handle.is_null());
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
        assert!(handle.is_indirect());
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
    ///    pre-set below only so that assertion can tell the two null routes
    ///    apart: a freshly vended handle already reports `NO_PARSED_OFFSET`,
    ///    so `set_resolved(ObjectValue::Null)` — which leaves the offset
    ///    untouched — would otherwise satisfy it by accident;
    /// 3. the mark is *still* held after the inner call returns. qpdf reaches
    ///    its `return` at `:1712` before constructing `ResolveRecorder rr` at
    ///    `:1714`, so the inner call never owns a recorder and never erases
    ///    the outer's entry. A guard that inserted unconditionally and erased
    ///    on drop would clear the outer resolution's mark from underneath it.
    #[test]
    fn a_reference_already_being_resolved_takes_the_loop_branch_and_leaves_the_outer_mark() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        handle.set_parsed_offset_if_unset(100);

        let outer = ResolveMark::begin(&pdf.resolver.core, object_ref)
            .expect("the first mark for a reference must be recorded, not reported as a loop");

        handle
            .try_dereference()
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
    /// Driven through an object-stream (type 2) entry, which is the class this
    /// slice does not implement. Object 1 really is uncompressed in the
    /// fixture, so the entry is overwritten first — without that this would
    /// resolve successfully and stop testing the error exit at all.
    #[test]
    fn a_resolution_that_returns_an_error_leaves_no_in_progress_mark() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        pdf.resolver.insert_xref_entry(
            object_ref,
            crate::XrefEntry::Compressed {
                stream: 9,
                index: 0,
            },
        );
        let handle = pdf.get_object_handle(object_ref);

        let error = handle
            .try_dereference()
            .expect_err("object streams are not implemented in this slice");
        assert!(
            matches!(error, Error::Unsupported(_)),
            "the class rejection, not a loop: {error:?}"
        );

        assert!(
            pdf.resolver.core.borrow().resolving.is_empty(),
            "a failed resolution must not leave its reference marked in progress"
        );
    }

    /// An unimplemented source class is rejected outright rather than handed
    /// to `Pdf`'s legacy route — the resolver bridge `flpdf-25kg.3.5`'s
    /// acceptance criteria forbid.
    ///
    /// Object 1 resolves perfectly well through `Pdf::resolve_borrowed`, which
    /// is what makes this discriminating: a fallback would succeed here, so
    /// `Unsupported` can only come from the canonical resolver declining.
    #[test]
    fn an_unimplemented_class_is_rejected_instead_of_falling_back_to_the_legacy_route() {
        let object_ref = ObjectRef::new(1, 0);
        // A separate document, so establishing the baseline cannot resolve the
        // handle under test: `resolve_borrowed` writes through to the very slot
        // `try_dereference` would then short-circuit on.
        assert!(
            Pdf::open_mem_owned(minimal_pdf_bytes())
                .expect("open")
                .resolve_borrowed(object_ref)
                .is_ok(),
            "the legacy route can read this object, so a bridge would succeed"
        );

        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.resolver.insert_xref_entry(
            object_ref,
            crate::XrefEntry::Compressed {
                stream: 9,
                index: 0,
            },
        );
        let handle = pdf.get_object_handle(object_ref);

        let error = handle.try_dereference().expect_err("no ObjStm support yet");
        assert!(
            matches!(&error, Error::Unsupported(message) if message.contains("object 1 0")),
            "expected a class rejection naming the reference, got {error:?}"
        );
        assert!(
            !handle.is_resolved(),
            "a rejected class leaves the slot open"
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
        let handle = pdf.get_object_handle(object_ref);
        assert!(
            pdf.repair_diagnostics().entries().is_empty(),
            "this fixture opens without warnings, so the loop is the only source"
        );

        let outer = ResolveMark::begin(&pdf.resolver.core, object_ref).expect("first mark");
        handle.try_dereference().expect("a loop is not an error");
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
        let handle = pdf.get_object_handle(object_ref);

        pdf.push_warning("before the loop");
        let outer = ResolveMark::begin(&pdf.resolver.core, object_ref).expect("first mark");
        handle.try_dereference().expect("a loop is not an error");
        drop(outer);
        pdf.push_warning("after the loop");

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
    }

    impl CountingReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                inner: std::io::Cursor::new(bytes),
                reads: 0,
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

        handle
            .try_dereference()
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
                .get(b"Type".as_slice())
                .and_then(crate::ObjectHandle::as_name),
            Some(b"Catalog".to_vec())
        );

        handle
            .try_dereference()
            .expect("a resolved slot is terminal");

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

        let catalog = pdf.get_object_handle(ObjectRef::new(1, 0));
        catalog.try_dereference().expect("resolve the catalog");
        let minted_child = catalog
            .as_dictionary()
            .expect("the catalog is a dictionary")
            .get(b"Pages".as_slice())
            .expect("the catalog has /Pages")
            .clone();
        assert!(
            minted_child.is_same_object_as(&pdf.get_object_handle(ObjectRef::new(2, 0))),
            "a child minted during the parse must be the canonical handle"
        );

        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        let pages = pdf.get_object_handle(ObjectRef::new(2, 0));
        pages.try_dereference().expect("resolve the page tree");
        let kid = pages
            .as_dictionary()
            .expect("the page tree is a dictionary")
            .get(b"Kids".as_slice())
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

    /// The canonical resolver records the same parsed offset the legacy
    /// `Pdf::resolve_object_handle` route records, for a plain object and for
    /// a stream.
    ///
    /// Compared against the legacy path rather than against a hand-computed
    /// number on purpose: "the exact parsed offset" means the one flpdf
    /// already produces, and a recomputed expectation would just restate this
    /// implementation's own arithmetic. The stream case is the one that can
    /// diverge quietly — a stream's parsed offset is its *data* start, not its
    /// dictionary's, so it depends on `validate_stream_line_end` consuming the
    /// EOL after `stream` exactly as the legacy scanner does.
    #[test]
    fn the_canonical_resolver_records_the_legacy_paths_parsed_offset() {
        for object_ref in [ObjectRef::new(1, 0), ObjectRef::new(4, 0)] {
            let mut legacy = Pdf::open_mem_owned(indirect_length_pdf_bytes()).expect("open");
            let legacy_handle = legacy.get_object_handle(object_ref);
            legacy
                .resolve_object_handle(&legacy_handle)
                .expect("legacy resolution");

            let mut canonical = Pdf::open_mem_owned(indirect_length_pdf_bytes()).expect("open");
            let handle = canonical.get_object_handle(object_ref);
            handle.try_dereference().expect("canonical resolution");

            assert_ne!(
                legacy_handle.get_parsed_offset(),
                NO_PARSED_OFFSET,
                "the legacy baseline must itself have recorded an offset for {object_ref:?}"
            );
            assert_eq!(
                handle.get_parsed_offset(),
                legacy_handle.get_parsed_offset(),
                "parsed offset diverged from the legacy route for {object_ref:?}"
            );
        }
    }

    /// A stream whose `/Length` is an indirect reference resolves, with the
    /// payload read from the position the resolver explicitly restored.
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
    fn a_streams_indirect_length_resolves_mid_parse_and_the_payload_is_read_from_the_restored_position(
    ) {
        let mut pdf = Pdf::open_mem_owned(indirect_length_pdf_bytes()).expect("open");
        let stream = pdf.get_object_handle(ObjectRef::new(4, 0));

        stream
            .try_dereference()
            .expect("a stream with an indirect /Length resolves");

        assert_eq!(
            stream.as_stream_data().as_deref().map(Vec::as_slice),
            Some(STREAM_PAYLOAD),
            "the payload must come from the restored stream offset"
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

    /// A stream whose `/Length` points at the very object being resolved —
    /// qpdf's own example of why the in-progress set exists: "an object
    /// references itself directly or indirectly in some key that has to be
    /// resolved during object parsing, such as stream length"
    /// (`libqpdf/QPDF.cc:1706-1708`).
    ///
    /// **The only fixture where the nested resolution re-enters on the *same*
    /// handle**, which is what makes it more than a restatement of the
    /// sibling-`/Length` seam test above.
    /// [`crate::ObjectHandle::set_missing`] takes `borrow_mut()` on the very
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
    /// **flpdf diverges from qpdf on the outcome, deliberately.** qpdf's inner
    /// call warns and caches null (`:1710-1711`); `readStream` then throws
    /// `damagedPDF(offset, "stream dictionary lacks /Length key")`
    /// (`:1373-1376`); `QPDF::resolve` catches that in
    /// `catch (QPDFExc& e) { warn(e); }` (`:1737-1738`) and warns a *second*
    /// time; and the resolve-to-null fallback at `:1745-1749` then does
    /// nothing, because `isUnresolved(og)` is already false — the inner call
    /// cached the null. So a qpdf caller sees **no error and two warnings**.
    /// flpdf propagates the parse error and raises one, because that `catch`
    /// arm and the fallback behind it are not ported in this slice; see
    /// [`super::ResolverHandle`]'s `resolve_indirect`. The cached state
    /// coincides — null at offset `-1` — but the observable outcome does not,
    /// and the single warning asserted below is flpdf's count, not qpdf's.
    #[test]
    fn a_self_referential_length_takes_the_loop_branch_instead_of_recursing_forever() {
        let bytes = pdf_with_bodies(&[
            b"1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n".to_vec(),
        ]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let object_ref = ObjectRef::new(2, 0);
        let handle = pdf.get_object_handle(object_ref);

        let error = handle
            .try_dereference()
            .expect_err("a self-referential /Length leaves the stream with no usable length");
        assert!(
            matches!(&error, Error::Parse { message, .. }
                if message == "stream dictionary lacks /Length key"),
            "the loop must surface as qpdf's null-/Length message: {error:?}"
        );

        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert_eq!(
            messages,
            ["loop detected resolving object 2 0"],
            "this fixture opens cleanly, so the loop is the only warning source"
        );

        assert!(
            handle.is_null() && handle.is_resolved(),
            "the inner call caches qpdf's loop null through the same slot the \
             outer call was resolving"
        );
        handle
            .try_dereference()
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
            let handle = pdf.get_object_handle(ObjectRef::new(number, 0));
            handle.try_dereference().expect("resolve");
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
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
        handle.try_dereference().expect("resolve");

        assert_eq!(
            handle
                .as_dictionary()
                .expect("dictionary")
                .get(b"Filler".as_slice())
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
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
        // `Pdf::open`'s own xref load has already pulled; only the resolution
        // is under test.
        let before = pdf.resolver.with_reader_mut(|reader| reader.reads);

        handle.try_dereference().expect("resolve");

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
                .get(b"Filler".as_slice())
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
        let handle = pdf.get_object_handle(ObjectRef::new(2, 0));

        handle.try_dereference().expect("qpdf-style recovery");

        let dictionary = handle.as_dictionary().expect("recovered dictionary");
        assert_eq!(
            dictionary
                .get(b"QPDFFake1".as_slice())
                .and_then(crate::ObjectHandle::as_string),
            Some(b"orphan".to_vec())
        );
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec!["expected dictionary key but found non-name object; inserting key /QPDFFake1"]
        );
    }

    #[test]
    fn live_object_header_and_fast_unread_fail_at_qpdfs_boundary() {
        let bad_keyword = resolver_over(b"1 0 nope".to_vec());
        assert!(matches!(
            bad_keyword.read_object_at_offset(0, ObjectRef::new(1, 0)),
            Err(Error::Parse { offset: 4, ref message }) if message == "expected obj"
        ));

        let bad_number = resolver_over(b"/N 0 obj".to_vec());
        assert!(matches!(
            bad_number.read_object_at_offset(0, ObjectRef::new(1, 0)),
            Err(Error::Parse { offset: 0, ref message }) if message == "expected integer"
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
        let handle = pdf.get_object_handle(ObjectRef::new(2, 0));
        handle.try_dereference().expect("flpdf recovery");
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec![expected]
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
        let handle = pdf.get_object_handle(ObjectRef::new(2, 0));

        handle.try_dereference().expect("resolve");

        assert_eq!(
            handle.as_stream_data().as_deref().map(Vec::as_slice),
            Some(STREAM_PAYLOAD),
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
        let handle = pdf.get_object_handle(ObjectRef::new(2, 0));
        let outcome = handle.try_dereference();
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
                ["empty object treated as null"],
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
            assert_eq!(warnings, ["expected endobj"]);
        });
    }

    /// A *direct* value that ends the input warns about the `endobj` that
    /// never arrives and is still resolved — the same outcome the stream path
    /// already reaches.
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
    /// produces the identical warning, which is what makes flpdf's asymmetry
    /// flpdf's own rather than inherited.
    ///
    /// **One divergence follows, and it is shared with the stream path rather
    /// than introduced here.** qpdf does not stop at that warning when the
    /// object is the last thing in the file: `readObjectAtOffset` then skips
    /// trailing whitespace to record `end_after_space` (`:1651-1663`) and
    /// throws `damagedPDF(tell(), "EOF after endobj")` (`:1660`) when the skip
    /// reaches the end, which `QPDF::resolve` catches (`:1737-1738`) and turns
    /// into a resolve-to-null (`:1745-1748`). qpdf's *net* outcome for both
    /// fixtures above is therefore two warnings and `null`, exit 3. flpdf
    /// ports neither `end_after_space` nor the resolve-to-null fallback — see
    /// [`super::ResolverHandle::read_object_at_offset`] — so it stops after
    /// the first warning with the object resolved. Closing that belongs with
    /// the recovery slice.
    #[test]
    fn a_direct_value_ending_the_input_warns_and_is_still_resolved() {
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
        let handle = pdf.get_object_handle(ObjectRef::new(9, 0));

        handle
            .try_dereference()
            .expect("a missing `endobj` warns rather than failing");

        assert_eq!(
            handle
                .as_dictionary()
                .expect("the value is a dictionary")
                .get(b"A".as_slice())
                .and_then(crate::ObjectHandle::as_string)
                .as_deref(),
            Some(&b"bcd"[..]),
            "and the value the parse reached must come back whole"
        );
        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert_eq!(
            messages,
            ["expected endobj"],
            "the EOF must surface as the missing framing keyword, exactly as \
             it does after `endstream`"
        );
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
        let handle = pdf.get_object_handle(ObjectRef::new(2, 0));

        handle.try_dereference().expect("resolve");

        assert_eq!(
            handle
                .as_dictionary()
                .expect("the value is a dictionary")
                .get(b"F".as_slice())
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
    /// **Two things this does not claim.**
    ///
    /// *The header check anchors differently.* qpdf throws
    /// `damagedPDF(offset, "expected n n obj")` (`libqpdf/QPDF.cc:1592-1594`)
    /// with `readObjectAtOffset`'s `offset` argument — the object's own start
    /// — so a `2 0 zzz` header at 256 is reported at 256, where flpdf reports
    /// the offending token at 260 (both observed). Both are absolute after
    /// this change; only the anchor within the object differs, and matching
    /// qpdf's would mean discarding the more precise position rather than
    /// gaining one.
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
        let handle = pdf.get_object_handle(ObjectRef::new(2, 0));

        handle
            .try_dereference()
            .expect("qpdf recovers a stray close parenthesis");

        let dictionary = handle.as_dictionary().expect("recovered dictionary");
        assert!(
            dictionary
                .get(b"QPDFFake1".as_slice())
                .is_some_and(|value| value.as_string().as_deref() == Some(b"x".as_slice())),
            "qpdf retains the orphan word under /QPDFFake1"
        );
        let diagnostics = pdf.repair_diagnostics();
        let warning = diagnostics
            .entries()
            .iter()
            .find(|entry| entry.message == "unexpected )")
            .expect("qpdf tokenizer warning");
        assert_eq!(warning.offset, Some(malformed_at as u64));
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
                    ["name with stray # will not work with PDF >= 1.2"]
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
                        "name with stray # will not work with PDF >= 1.2",
                        "expected endobj",
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
                ["name with stray # will not work with PDF >= 1.2"],
                "one diagnostic per object, not one per scan_forward attempt"
            );
        });
    }

    /// An object whose header carries a different `N G` than the xref table
    /// asked for is an error in this slice, not a silent substitution.
    ///
    /// See [`super::ResolverHandle::read_object_at_offset`] for why qpdf's own
    /// outcome (warn, cache under the found id, resolve the *requested* one to
    /// null) is not reproduced yet.
    #[test]
    fn an_object_whose_header_names_a_different_reference_is_rejected() {
        with_second_object(b"7 0 obj\n42\nendobj\n", |handle, outcome, _| {
            let error = outcome.expect_err("the id mismatch must not pass silently");
            assert!(
                matches!(&error, Error::Parse { message, .. } if message == "expected 2 0 obj"),
                "expected qpdf's own wording, got {error:?}"
            );
            assert!(!handle.is_resolved());
        });
    }

    /// Every way `/Length` can fail to yield a byte count.
    ///
    /// qpdf reports the null and non-integer cases separately
    /// (`libqpdf/QPDF.cc:1370-1377`); an absent key reads as null there, so it
    /// shares the first message.
    #[test]
    fn a_stream_whose_length_is_unusable_is_rejected_with_qpdfs_own_distinction() {
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

            with_second_object(&body, |_, outcome, _| {
                let error = outcome.expect_err("an unusable /Length must not resolve");
                assert!(
                    matches!(&error, Error::Parse { message, .. } if message == expected),
                    "for {dict:?}: expected {expected:?}, got {error:?}"
                );
            });
        }
    }

    /// A `stream` keyword after a non-dictionary value is rejected.
    ///
    /// qpdf cannot reach its own equivalent: `readObject` only calls
    /// `readStream` when `object.isDictionary()` (`libqpdf/QPDF.cc:1350`), so
    /// this guard stands in for a branch qpdf expresses as a condition.
    #[test]
    fn a_stream_keyword_after_a_non_dictionary_is_rejected() {
        with_second_object(
            b"2 0 obj\n42\nstream\nabc\nendstream\nendobj\n",
            |_, outcome, _| {
                let error = outcome.expect_err("only a dictionary can introduce a stream");
                assert!(
                    matches!(&error, Error::Parse { message, .. }
                        if message == "stream keyword follows an object that is not a dictionary"),
                    "got {error:?}"
                );
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
    /// length` / `recovered stream length: 4`, which is the recovery arm
    /// [`super::ResolverHandle::read_stream`] records as not ported. An
    /// earlier revision reported the first case as "stream data ends before
    /// its declared /Length", a message with no counterpart anywhere in
    /// `libqpdf/` — it came from reading the payload before checking the
    /// framing, which is what made the absurd-length allocation reachable.
    #[test]
    fn a_stream_payload_that_does_not_match_its_length_is_rejected() {
        for body in [
            &b"2 0 obj\n<< /Length 100000 >>\nstream\nabc\nendstream\nendobj\n"[..],
            b"2 0 obj\n<< /Length 1 >>\nstream\nabc\nendstream\nendobj\n",
        ] {
            with_second_object(body, |_, outcome, _| {
                assert!(
                    matches!(&outcome.expect_err("no `endstream` is where /Length says"),
                    Error::Parse { message, .. } if message == "expected endstream"),
                    "for {body:?}"
                );
            });
        }
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
        with_second_object(
            b"2 0 obj\n<< /Length 9223372036854775000 >>\nstream\nabc\nendstream\nendobj\n",
            |_, outcome, _| {
                assert!(matches!(&outcome.expect_err("nothing is at that offset"),
                    Error::Parse { message, .. } if message == "expected endstream"));
            },
        );

        with_second_object(
            b"2 0 obj\n<< /Length 9223372036854775807 >>\nstream\nabc\nendstream\nendobj\n",
            |_, outcome, _| {
                let error = outcome.expect_err("that offset cannot be reached at all");
                assert!(
                    matches!(&error, Error::Parse { message, .. }
                        if message.starts_with("adding 9223372036854775807 to ")
                            && message.ends_with(" would cause an integer overflow")),
                    "the overflow refusal must stay distinct from `expected \
                     endstream`, which is qpdf's recovery fork: {error:?}"
                );
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
                    handle.as_stream_data().as_deref().map(Vec::as_slice),
                    Some(&b"abc"[..])
                );
                assert_eq!(warnings, ["expected endobj"]);
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
                    handle.as_stream_data().as_deref().map(Vec::as_slice),
                    Some(payload),
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
        let handle = pdf.get_object_handle(ObjectRef::new(4, 0));

        handle.try_dereference().expect("resolve");

        assert_eq!(
            handle.as_stream_data().as_deref().map(Vec::as_slice),
            Some(STREAM_PAYLOAD)
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
    fn a_stream_keyword_at_end_of_input_ends_the_line_check_without_a_warning() {
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
        let handle = pdf.get_object_handle(ObjectRef::new(9, 0));

        let error = handle
            .try_dereference()
            .expect_err("there is no payload to read");

        assert!(
            matches!(&error, Error::Parse { message, .. }
                if message == "expected endstream"),
            "the EOF must surface as the missing framing keyword, not as a \
             line-ending complaint: {error:?}"
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
            "qpdf returns silently on a premature EOF here"
        );
    }

    /// A stream whose `endstream` is the last thing in the input still
    /// resolves, warning about the `endobj` that never arrives.
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
    /// shape: two warnings, `null`, exit 3. flpdf ports neither of those, so
    /// it stops with the object resolved. The gap is
    /// [`super::ResolverHandle::read_object_at_offset`]'s already-recorded
    /// `end_before_space`/`end_after_space` omission, and it is the same for
    /// the direct path — see
    /// `a_direct_value_ending_the_input_warns_and_is_still_resolved`.
    ///
    /// Appended past `%%EOF` with a hand-written xref entry for the same
    /// reason as the fixture above: a well-formed document always has a
    /// trailer after its objects.
    #[test]
    fn a_stream_ending_the_input_after_endstream_warns_and_still_resolves() {
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
        let handle = pdf.get_object_handle(ObjectRef::new(9, 0));

        handle
            .try_dereference()
            .expect("a missing `endobj` warns rather than failing");

        assert_eq!(
            handle.as_stream_data().as_deref().map(Vec::as_slice),
            Some(&b"abc"[..]),
            "and the payload is still the declared three bytes"
        );
        let messages: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert_eq!(messages, ["expected endobj"]);
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
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolver.with_reader_mut(|reader| reader.broken = true);

        let error = handle
            .try_dereference()
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

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let handle = pdf.get_object_handle(ObjectRef::new(2, 0));

        let error = handle
            .try_dereference()
            .expect_err("a stream is not a usable /Length");
        assert!(
            matches!(&error, Error::Parse { message, .. }
                if message == "/Length key in stream dictionary is not an integer"),
            "the outer stream must fail on the value, not on a mis-read: {error:?}"
        );

        assert_eq!(
            pdf.get_object_handle(ObjectRef::new(3, 0))
                .as_stream_data()
                .as_deref()
                .map(Vec::as_slice),
            Some(inner),
            "the inner stream, resolved while the outer frame was live, must \
             have read from its own saved offset"
        );
        assert!(
            pdf.resolver.core.borrow().resolving.is_empty(),
            "every mark must be gone once the outermost resolution returns"
        );
    }

    /// `flpdf-k8ln`'s acceptance criterion, folded into `flpdf-25kg.3.5`: a
    /// nested handle such as `/AP /N 5 0 R` resolves through the owning
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
        let annotation = pdf.get_object_handle(ObjectRef::new(4, 0));
        annotation
            .try_dereference()
            .expect("the annotation dictionary resolves");

        let ap = annotation
            .as_dictionary()
            .expect("the annotation is a dictionary")
            .get(b"AP".as_slice())
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

        let n = ap
            .as_dictionary()
            .expect("/AP is a dictionary")
            .get(b"N".as_slice())
            .expect("/AP has /N")
            .clone();
        assert!(
            n.is_indirect() && !n.is_resolved(),
            "the annotation's own parse must have minted /N as an unresolved \
             indirect handle, not resolved it eagerly or copied its value"
        );

        n.try_dereference().expect(
            "a nested handle reached only by navigating /AP /N, never re-fetched \
             through `Pdf::get_object_handle`, must still resolve through the \
             owning document",
        );

        assert_eq!(
            n.as_stream_data().as_deref().map(Vec::as_slice),
            Some(appearance_payload),
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
    /// own the thread it is called on.
    ///
    /// The expected outcome is a *diagnosis*: link 1's `/Length` resolves to
    /// link 2, which is a stream rather than an integer. qpdf reaches the same
    /// judgement (`/Length key in stream dictionary is not an integer`,
    /// `libqpdf/QPDF.cc:1379`) and then recovers the length; flpdf stops at
    /// the first of those, the divergence [`super::ResolverHandle::read_stream`]
    /// already records.
    #[test]
    fn a_long_chain_of_indirect_lengths_grows_the_stack_instead_of_aborting() {
        // Built inside the closure rather than moved in: `Pdf` and
        // `ObjectHandle` are not `Send`, the same reason `reader.rs`'s
        // `trailer_key_handle_is_null_when_the_keys_own_value_exceeds_the_parse_depth_bound`
        // builds its tree in the spawned thread.
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let bytes = chained_indirect_length_pdf_bytes(4000);
                let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
                let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

                let error = handle
                    .try_dereference()
                    .expect_err("link 2 is a stream, so link 1's /Length is unusable");

                assert!(
                    matches!(&error, Error::Parse { message, .. }
                        if message == "/Length key in stream dictionary is not an integer"),
                    "the chain must come back diagnosed, not aborted: {error:?}"
                );
            })
            .expect("spawn")
            .join()
            .expect("a 4000-link chain must not overflow a small stack");
    }
}
