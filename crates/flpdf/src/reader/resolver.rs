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
//! [`ResolverHandle::resolve_indirect`] now takes borrows of its own, but only
//! the two [`ResolveMark`] takes — one inside `begin`, one inside `drop`, each
//! confined to a single statement — so nothing yet holds a borrow across the
//! body a nested resolution would run in. The other borrows in play are the
//! ones `Pdf`'s legacy read helpers take, and those are exercised, including
//! through the re-entrant indirect-`/Length` path (`reader.rs`'s
//! `qpdf_object_read_uses_bounded_fallback_and_preserves_strict_errors`). The
//! regression that makes a borrow spanning a nested resolve fail loudly
//! arrives with the resolver's own re-entrancy test.

use crate::object_handle::DocumentResolver;
use crate::{Error, ObjectHandle, ObjectRef, Result, XrefEntry};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};

/// The state `QPDF::resolve` and the functions it calls operate on.
///
/// The field list is taken from qpdf's `QPDF::Members`
/// (`include/qpdf/QPDF.hh`) restricted to what `QPDF::resolve`,
/// `QPDF::readObjectAtOffset`, and `QPDF::resolveObjectsInStream` touch.
/// Everything else a document owns — `version`, `trailer`, `startxref`,
/// `foreign_object_maps`, dirty tracking, and every legacy field already
/// carrying a `qpdf-cutover-delete` marker — stays on `Pdf`.
///
/// **Two members of that restriction are deliberately missing.**
///
/// *The warning sink.* `QPDF::resolve` warns (`m->warnings`,
/// `include/qpdf/QPDF.hh:1475`) on a resolution loop and on a damaged object,
/// so a complete resolver owns it. flpdf's sink is `Pdf::repair_diagnostics`,
/// and `Pdf::repair_diagnostics()` hands out a `&Diagnostics` that cannot be
/// returned from behind a `RefCell`. It is still absent, and that is now a
/// live divergence rather than a deferral of dead code: the loop branch of
/// [`ResolverHandle::resolve_indirect`] is a place qpdf warns and flpdf does
/// not. That method's own doc carries the reasoning and the cost of moving
/// the sink.
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
    /// qpdf `m->obj_cache` (`QPDF.hh:1467`).
    ///
    /// Empty for the whole of this slice: nothing populates it until
    /// uncompressed resolution lands, so it is not yet a second view of
    /// `Pdf::handle_registry`.
    ///
    /// **Teardown hazard for whoever populates it.** `Pdf::drop` breaks the
    /// resolved-handle reference cycle by walking `Pdf::handle_registry` and
    /// disconnecting each entry. It does not walk this map. A handle
    /// installed here but not also in `handle_registry` would therefore keep
    /// its cycle — and every stream buffer reachable from it — alive past the
    /// document. Either install into both, or move the teardown walk here.
    #[allow(dead_code)] // populated when uncompressed resolution lands
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
}

impl<R: Read + Seek> ResolverHandle<R> {
    pub(crate) fn new(
        reader: R,
        header_offset: usize,
        source_xref_entries: BTreeMap<ObjectRef, XrefEntry>,
        attempt_recovery: bool,
    ) -> Self {
        Self {
            core: RefCell::new(ResolverCore {
                reader,
                header_offset,
                source_xref_entries,
                object_cache: BTreeMap::new(),
                resolving: BTreeSet::new(),
                resolved_object_streams: BTreeSet::new(),
                attempt_recovery,
            }),
        }
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
    /// # The loop branch, and what it does not yet do
    ///
    /// qpdf's loop branch (`libqpdf/QPDF.cc:1706-1712`) does three things:
    /// warns `damagedPDF("", "loop detected resolving object " +
    /// og.unparse(' '))`, calls `updateCache(og, QPDF_Null::create(), -1, -1)`,
    /// and returns without throwing. Two of the three are ported.
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
    /// *The canonical cache* is not written, because
    /// [`ResolverCore::object_cache`] has no writer and no reader anywhere in
    /// this slice; Task 4 introduces both together. Nothing observable turns
    /// on it meanwhile: `Pdf::get_object_handle` vends one registry entry per
    /// [`ObjectRef`] and re-hands that same handle, so the null this installs
    /// is what the next lookup sees, and [`ObjectHandle::try_dereference`]
    /// short-circuits on it without consulting the resolver again.
    ///
    /// *The warning is not emitted at all.* flpdf's counterpart of
    /// `m->warnings` (`include/qpdf/QPDF.hh:1475`) is `Pdf::repair_diagnostics`,
    /// which [`ResolverCore`]'s own doc records as deliberately absent from
    /// the resolver, and this slice does not move it: `Pdf::repair_diagnostics`
    /// returns `&Diagnostics`, a reference that cannot come out of a `RefCell`,
    /// so the sink can only move by changing that signature across 90 call
    /// sites in three crates. The mechanical replacement does not work either:
    /// returning `Ref<'_, Diagnostics>` makes every
    /// `let entries = pdf.repair_diagnostics().entries();` binding a rustc
    /// E0716 — the `Ref` is a temporary freed at the end of that statement
    /// while the slice still borrows it — and seven such bindings exist,
    /// including `flpdf-qtest-tools/src/driver/mod.rs:323`. A second,
    /// resolver-local sink was rejected as the larger divergence: qpdf has
    /// exactly one warning list, and
    /// splitting flpdf's would have to be undone at the same cost later. So
    /// **a resolution loop is silent here where qpdf warns**, which is a
    /// divergence and not a slice of parity. The plan of record assigns
    /// warnings to Task 4, whose step 3 lists them among what it must
    /// preserve.
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
    use crate::{Error, ObjectRef, Pdf};
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
