//! The canonical document resolver: the state `QPDF::resolve` reaches for, and
//! the borrow seam that lets it be reached from an [`ObjectHandle`] alone.
//!
//! qpdf correspondence: `QPDF::resolve` (`libqpdf/QPDF.cc:1700-1753`) and the
//! `QPDF::Members` fields it touches.
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
//! `libqpdf/QPDF.cc:1360-1398`), so a borrow held across a nested resolve is a
//! `RefCell` double-borrow panic in production.
//!
//! **The qualifier is load-bearing and the hazard is real.**
//! [`ResolverCore::read_window`] holds `borrow_mut()` across
//! `R::seek`/`R::read`, which is caller-supplied code. `R` is arbitrary and
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
//! This discipline is currently *stated and readable, not pinned by a test*.
//! [`ResolverHandle::resolve_indirect`] now takes borrows of its own — three
//! of them, all short: the two [`ResolveMark`] takes (one inside `begin`, one
//! inside `drop`) and the one [`ResolverHandle::push_warning`] takes on the
//! loop branch. Each is confined to a single statement, so nothing yet holds a
//! borrow across the body a nested resolution would run in.
//!
//! The warning sink is the newest way to get this wrong, and the way that will
//! bite first: Task 4 warns on a damaged object *while* it has the xref entry
//! in hand, so the push has to happen after that borrow is released, not
//! inside it. The other borrows in play are the ones `Pdf`'s legacy read
//! helpers take, and those are exercised, including through the re-entrant
//! indirect-`/Length` path (`reader.rs`'s
//! `qpdf_object_read_uses_bounded_fallback_and_preserves_strict_errors`). The
//! regression that makes a borrow spanning a nested resolve fail loudly
//! arrives with the resolver's own re-entrancy test.

use crate::object_handle::{DocumentResolver, NO_PARSED_OFFSET};
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
/// **One member of that restriction is deliberately missing.**
///
/// *The string decrypter's encryption parameters* (qpdf `m->encp`). qpdf
/// builds a `StringDecrypter` per object inside `readObjectAtOffset`'s parse
/// and passes it to `QPDFParser` only when the document is encrypted —
/// `StringDecrypter* decrypter_ptr = m->encp->encrypted ? &decrypter : nullptr`
/// (`libqpdf/QPDF.cc:1337-1339`). Encrypted documents are out of scope for
/// this slice, so there is nothing here for `encp` to feed; the field arrives
/// with the code that decrypts strings during resolution. `Pdf::encryption`
/// holds flpdf's equivalent state meanwhile. Note this is *string*
/// decryption only — stream decryption is not part of the resolver at all
/// (qpdf decrypts streams at pipe time, `decryptStream`, `QPDF.cc:2491`).
pub(crate) struct ResolverCore<R: Read + Seek + 'static> {
    /// qpdf `m->file` (`QPDF.hh:1456`).
    reader: R,
    /// Also qpdf `m->file`: when repair finds a valid header after leading
    /// material, qpdf does not keep the offset beside the input source, it
    /// *wraps* the source so the shift is invisible to every later read —
    /// `m->file = std::shared_ptr<InputSource>(new OffsetInputSource(m->file,
    /// global_offset))` (`libqpdf/QPDF.cc:406`). Keeping the shift beside
    /// `reader` and applying it in [`Self::read_window`] puts it under the
    /// same single owner, without a second input-source type.
    ///
    /// It is *not* equivalent, and the difference is visible right here:
    /// wrapping makes the shift unskippable, so qpdf has no way to read from
    /// physical zero through `m->file` at all, whereas
    /// [`Self::read_physical_input`] does exactly that. That method is a
    /// legacy tenant (see its own note), not a port of anything qpdf does.
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
    /// already held. That is a real constraint on Task 4's read-and-parse
    /// phase, which warns on a damaged object: the warning must be pushed
    /// outside the borrow that read the xref entry, not inside it.
    repair_diagnostics: Diagnostics,
}

impl<R: Read + Seek> ResolverCore<R> {
    /// Read `[offset, next)` — or `[offset, EOF)` when `next` is `None` — from
    /// the input source, in qpdf-logical coordinates.
    ///
    /// `qpdf-legacy-tenant(flpdf-25kg.3.5)`: **this is not a port of anything
    /// in qpdf, and the resolver never calls it.** Its only callers are
    /// `Pdf`'s legacy read helpers, which live on the other side of the
    /// cutover; it sits here solely because the input source it reads now has
    /// a single owner. Delete it once those callers are gone.
    ///
    /// Do not build on its shape. Reading a bounded window into an owned
    /// `Vec` is an flpdf-ism: qpdf streams from `m->file` and brackets the one
    /// re-entrant seam by saving and restoring the position
    /// (`QPDF::readStream`, `libqpdf/QPDF.cc:1360-1398`). The design of record
    /// names generalising this owned-window shape as a wrong turn that would
    /// entrench a divergence, so `readObjectAtOffset`/`readStream` port that
    /// save/restore seam rather than reusing this.
    fn read_window(&mut self, offset: u64, next: Option<u64>) -> Result<Vec<u8>> {
        let physical = (self.header_offset as u64).saturating_add(offset);
        self.reader.seek(SeekFrom::Start(physical))?;
        let mut bytes = Vec::new();
        match next {
            Some(next) => {
                self.reader
                    .by_ref()
                    .take(next.saturating_sub(offset))
                    .read_to_end(&mut bytes)?;
            }
            None => {
                self.reader.read_to_end(&mut bytes)?;
            }
        }
        Ok(bytes)
    }

    /// Read the entire input from *physical* offset zero, header shift
    /// included. Callers that want qpdf-logical coordinates use
    /// [`Self::read_window`].
    ///
    /// `qpdf-legacy-tenant(flpdf-25kg.3.5)`: same status as
    /// [`Self::read_window`] — no qpdf counterpart, no resolver caller, here
    /// only because it needs the input source. qpdf could not express it
    /// through `m->file` even if it wanted to, since `OffsetInputSource`
    /// (`libqpdf/QPDF.cc:406`) makes the header shift unskippable. Its one
    /// caller is `Pdf::source_bytes`, used by the writer to copy the original
    /// file verbatim.
    fn read_physical_input(&mut self) -> Result<Vec<u8>> {
        self.reader.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        self.reader.read_to_end(&mut bytes)?;
        Ok(bytes)
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

    /// See [`ResolverCore::read_window`].
    pub(crate) fn read_window(&self, offset: u64, next: Option<u64>) -> Result<Vec<u8>> {
        self.core.borrow_mut().read_window(offset, next)
    }

    /// See [`ResolverCore::read_physical_input`].
    pub(crate) fn read_physical_input(&self) -> Result<Vec<u8>> {
        self.core.borrow_mut().read_physical_input()
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
    /// *The canonical cache* is not written, because
    /// [`ResolverCore::object_cache`] has no writer and no reader anywhere in
    /// this slice; Task 4 introduces both together. Nothing observable turns
    /// on it meanwhile: `Pdf::get_object_handle` vends one registry entry per
    /// [`ObjectRef`] and re-hands that same handle, so the null this installs
    /// is what the next lookup sees, and [`ObjectHandle::try_dereference`]
    /// short-circuits on it without consulting the resolver again.
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
    /// rather than qpdf's `getLastOffset()`: the resolver tracks no input
    /// position in this slice, and a fabricated one would be worse than an
    /// absent one.
    ///
    /// Not ported for a different reason: qpdf's `isUnresolved(og)` early
    /// return (`:1702-1704`) is phase 1 of Task 4's three-phase split.
    /// [`ObjectHandle::try_dereference`] happens to make the same test before
    /// calling in, so it is not reachable through that path today, but this
    /// method does not make it itself.
    fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
        // Bound to a named local, not to `_`: the mark must live until this
        // method returns or unwinds, and `let Some(_) = ..` would drop it at
        // the end of this statement.
        let Some(_mark) = ResolveMark::begin(&self.core, object_ref) else {
            // qpdf's order: warn, then cache null (`libqpdf/QPDF.cc:1710-1711`).
            // Neither call may hold a borrow across the other — `push_warning`
            // takes its own `borrow_mut`.
            self.push_warning(format!(
                "loop detected resolving object {} {}",
                object_ref.number, object_ref.generation
            ));
            handle.set_missing();
            return Ok(());
        };

        // `handle` is also the slot a later slice writes the parsed value into.
        Err(Error::Unsupported(format!(
            "canonical resolver cannot yet resolve object {} {}: no source class is implemented",
            object_ref.number, object_ref.generation
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::ResolveMark;
    use crate::object_handle::NO_PARSED_OFFSET;
    use crate::{Error, ObjectRef, Pdf, Severity};
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

    /// The attach itself: a handle vended by a live document must reach that
    /// document's resolver.
    ///
    /// The two failure modes are different errors and this test tells them
    /// apart deliberately. `Error::Internal("... belongs to a dropped PDF")`
    /// is `try_dereference` failing to upgrade its `Weak` — no resolver was
    /// ever attached, which is what happened before this slice.
    /// `Error::Unsupported` is the resolver being reached and declining the
    /// class, which is the whole of what this slice promises. Asserting only
    /// `is_err()` would pass in both cases and prove nothing.
    #[test]
    fn a_vended_handle_reaches_its_documents_resolver_rather_than_reporting_a_dropped_pdf() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

        let error = handle
            .try_dereference()
            .expect_err("no source class resolves in this slice");

        match &error {
            Error::Unsupported(message) => {
                assert!(
                    message.contains("object 1 0"),
                    "the rejection should name the reference it declined, got {message:?}"
                );
            }
            Error::Internal(message) if message.contains("belongs to a dropped PDF") => {
                panic!(
                    "`get_object_handle` vended a handle with no resolver attached: {message:?}"
                );
            }
            other => panic!("unexpected error from an attached resolver: {other:?}"),
        }
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
    /// Every non-loop reference errors in this slice, so the error exit is the
    /// only exit `resolve_indirect` currently has — which makes this the test
    /// that a matched insert/remove pair placed only on the success path would
    /// fail. Left behind, the mark would make the very next attempt at the
    /// same reference report a phantom loop and resolve it to null.
    #[test]
    fn a_resolution_that_returns_an_error_leaves_no_in_progress_mark() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);

        let error = handle
            .try_dereference()
            .expect_err("no source class resolves in this slice");
        assert!(
            matches!(error, Error::Unsupported(_)),
            "the class rejection, not a loop: {error:?}"
        );

        assert!(
            pdf.resolver.core.borrow().resolving.is_empty(),
            "a failed resolution must not leave its reference marked in progress"
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
}
