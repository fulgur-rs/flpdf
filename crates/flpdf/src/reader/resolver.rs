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
//! within a single expression, and calls nothing that could re-enter
//! resolution while that borrow is live. That is not stylistic: resolution is
//! re-entrant (qpdf's own `/Length`-as-indirect-reference case,
//! `QPDF::readStream`, `libqpdf/QPDF.cc:1360-1398`), so a borrow held across
//! a nested resolve is a `RefCell` double-borrow panic in production. Reads
//! return owned `Vec<u8>` for exactly this reason — a caller cannot hold a
//! borrow it never receives.
//!
//! [`ResolverHandle::with_reader_mut`] is the single exception: it hands out
//! `&mut R` while the borrow is held, which is why it is `#[cfg(test)]` and
//! carries its own warning.
//!
//! This discipline is currently *stated and readable, not pinned by a test*.
//! [`ResolverHandle::resolve_indirect`] takes no borrow at all yet, so the
//! only borrows in play are the ones `Pdf`'s legacy read helpers take — and
//! those are exercised, including through the re-entrant indirect-`/Length`
//! path (`reader.rs`'s
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
/// *The warning sink.* `QPDF::resolve` warns (`m->warnings`, `QPDF.hh:1475`)
/// on a resolution loop and on a damaged object, so a complete resolver owns
/// it. flpdf's sink is `Pdf::repair_diagnostics`, and
/// `Pdf::repair_diagnostics()` hands out a `&Diagnostics` that cannot be
/// returned from behind a `RefCell`. Nothing here warns yet, so moving it now
/// would change a public signature for no exercised behaviour; it moves with
/// the first code that writes a warning.
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
    /// global_offset))` (`libqpdf/QPDF.cc:406`). Storing the shift next to
    /// `reader` and applying it in [`Self::read_window`] is the same
    /// single-owner arrangement without a second input-source type.
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
    /// qpdf `m->resolving` (`QPDF.hh:1468`), the set `QPDF::resolve` tests to
    /// detect "an object references itself directly or indirectly in some key
    /// that has to be resolved during object parsing, such as stream length"
    /// (`libqpdf/QPDF.cc:1706-1708`).
    #[allow(dead_code)] // the in-progress guard is the next task
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

impl<R: Read + Seek + 'static> ResolverCore<R> {
    /// Read `[offset, next)` — or `[offset, EOF)` when `next` is `None` — from
    /// the input source, in qpdf-logical coordinates.
    ///
    /// Returns owned bytes rather than lending the reader out, so no caller
    /// can hold this borrow across a parse that re-enters resolution.
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
    fn read_physical_input(&mut self) -> Result<Vec<u8>> {
        self.reader.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        self.reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

/// The `Rc`-shared, interior-mutable owner of [`ResolverCore`], and the
/// [`DocumentResolver`] a document's handles hold a `Weak` to.
pub(crate) struct ResolverHandle<R: Read + Seek + 'static> {
    core: RefCell<ResolverCore<R>>,
}

impl<R: Read + Seek + 'static> ResolverHandle<R> {
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
    pub(crate) fn xref_entries(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        self.core.borrow().source_xref_entries.clone()
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

impl<R: Read + Seek + 'static> DocumentResolver for ResolverHandle<R> {
    /// Resolve `object_ref`'s slot in place.
    ///
    /// No source class is implemented yet, so every reference is rejected
    /// with [`Error::Unsupported`]. This is deliberately *not* a fallback
    /// into `Pdf`'s legacy `resolve_object_handle`/`resolve_borrowed` route:
    /// that bridge is what `flpdf-25kg.3.5`'s acceptance criteria forbid, so
    /// an unhandled class errors and gains real support in a later slice.
    ///
    /// Reaching this at all is what distinguishes an attached handle from a
    /// detached one — [`ObjectHandle::try_dereference`] reports
    /// `"belongs to a dropped PDF"` when it cannot upgrade its `Weak`, which
    /// is a different failure from this one.
    fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
        // `handle` is the slot a later slice writes the parsed value into.
        let _ = handle;
        Err(Error::Unsupported(format!(
            "canonical resolver cannot yet resolve object {} {}: no source class is implemented",
            object_ref.number, object_ref.generation
        )))
    }
}

#[cfg(test)]
mod tests {
    use crate::{Error, ObjectRef, Pdf};

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

    /// The resolver cannot outlive the document that owns it: the only strong
    /// reference lives in `Pdf`, and every handle holds a `Weak`. Dropping the
    /// document therefore drops the input source too, rather than leaving it
    /// alive behind a handle someone kept.
    #[test]
    fn a_surviving_handle_does_not_keep_its_documents_resolver_alive() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));
        assert!(
            pdf.resolver_is_uniquely_owned(),
            "the document must hold the only strong reference to its resolver"
        );
        drop(handle);
    }
}
