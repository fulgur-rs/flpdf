//! qpdf correspondence: QPDF.cc document-construction entry points (`emptyPDF()`, `processFile()`, and `processMemoryFile()`) and their shared construction orchestration.
//!
//! Rust splits QPDF.cc construction into `engine.rs` while retaining the single `Pdf<R>` type.

use crate::cache::ObjectCache;
// Used by the public factory API's intra-doc links.
#[allow(unused_imports)]
use crate::error::EncryptedError;
use crate::reader::resolver::{ResolverHandle, ResolverWarningOptions};
use crate::reader::PdfOpenOptions;
use crate::xref::{load_xref_state_with_options, XrefLoadOptions};
#[allow(unused_imports)]
use crate::{Error, ObjectHandle};
use crate::{Pdf, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_PDF_ID: AtomicU64 = AtomicU64::new(1);
// Upper bound on read-to-end fallbacks during object resolution (see
// `resolution_fallbacks_remaining`). Each fallback may scan to EOF, so the total
// fallback work is bounded by this many file scans — O(file size), not the
// quadratic cost an unbounded read-to-end per object would incur. 64 tolerates a
// handful of corrupt/overlapping offsets in an otherwise valid file while still
// defeating a flood of objects whose bodies run to EOF.
const MAX_RESOLUTION_FALLBACKS: u32 = 64;

impl<R: Read + Seek> Pdf<R> {
    /// Open a document with qpdf's default recovery policy.
    ///
    /// qpdf enables recovery by default. Use [`Pdf::open_with_options`] with
    /// `repair: false` for the explicit strict/suppressed-recovery route.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open_with_options`] (called with qpdf's
    /// default recovery options): [`Error::Io`] / [`Error::Parse`] /
    /// [`Error::Missing`] from loading the cross-reference and trailer, and
    /// [`Error::Encrypted`] when the document is encrypted and cannot be
    /// authenticated.
    pub fn open(reader: R) -> Result<Self> {
        Self::open_with_options(reader, PdfOpenOptions::default())
    }

    /// Open a document with qpdf-style xref/trailer recovery explicitly enabled.
    ///
    /// This is equivalent to [`Pdf::open`] because qpdf enables recovery by
    /// default. Diagnostics from the recovery pass are stored on the handle and
    /// exposed via [`Pdf::repair_diagnostics`].
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open_with_options`] (called with `repair`
    /// enabled); see that method for the full error set.
    pub fn open_with_repair(reader: R) -> Result<Self> {
        Self::open_with_options(
            reader,
            PdfOpenOptions {
                repair: true,
                ..PdfOpenOptions::default()
            },
        )
    }

    /// Alias for [`Pdf::open_with_repair`].
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open_with_repair`].
    pub fn open_best_effort(reader: R) -> Result<Self> {
        Self::open_with_repair(reader)
    }

    /// Open a document with explicit repair and password options.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] / [`Error::Parse`] / [`Error::Missing`] when loading the
    ///   cross-reference table and trailer fails (e.g. an unreadable stream, a
    ///   malformed xref, or a cross-reference stream missing its `/Size` or `/W`
    ///   entry). With `options.repair` set, the qpdf-style recovery pass runs
    ///   first and only its residual failures surface.
    /// - [`Error::Unsupported`] when a cross-reference stream uses an unsupported
    ///   entry type or `/W` field-width layout.
    /// - [`Error::Encrypted`] when the document carries an `/Encrypt` dictionary
    ///   that cannot be authenticated or processed: a wrong password
    ///   ([`EncryptedError::BadPassword`]), an unsupported filter or revision
    ///   ([`EncryptedError::UnsupportedHandler`]), a structurally invalid
    ///   `/Encrypt` dictionary ([`EncryptedError::Malformed`]). Weak
    ///   encryption is readable; qpdf's `--allow-weak-crypto` is a write-only
    ///   policy.
    pub fn open_with_options(reader: R, options: PdfOpenOptions) -> Result<Self> {
        Self::open_with_repair_mode(reader, options, false)
    }

    /// Open a document for qpdf's read-only encryption inspection path.
    ///
    /// Unlike [`Pdf::open_with_options`], this retains the parsed encryption
    /// parameters and returns a document when password authentication fails
    /// with [`EncryptedError::BadPassword`]. The returned document is only
    /// suitable for encryption inspection; its authenticated decryption state
    /// remains absent.
    pub fn open_for_encryption_inspection(reader: R, options: PdfOpenOptions) -> Result<Self> {
        Self::open_with_repair_mode(reader, options, true)
    }

    fn open_with_repair_mode(
        mut reader: R,
        options: PdfOpenOptions,
        allow_bad_password: bool,
    ) -> Result<Self> {
        let warning_options = ResolverWarningOptions::new(
            options
                .logger
                .clone()
                .unwrap_or_else(crate::QPDFLogger::default_logger),
            options.suppress_warnings,
            options.description.clone(),
        );
        let loaded_state = match load_xref_state_with_options(
            &mut reader,
            XrefLoadOptions {
                allow_repair: options.repair,
                ignore_xref_streams: options.ignore_xref_streams,
            },
        ) {
            Ok(state) => state,
            Err(error) => {
                if let Some((_, diagnostics)) = error.open_failure() {
                    warning_options.replay_warnings(diagnostics)?;
                }
                return Err(error);
            }
        };
        let parsed_xref_streams = loaded_state.parsed_xref_streams;
        let trailer_references = loaded_state.trailer_references;
        let header_offset = loaded_state.header_offset;
        let already_reconstructed = loaded_state.already_reconstructed;
        let first_xref_item_offset = loaded_state.first_xref_item_offset;
        let loaded = loaded_state.loaded;
        let source_xref_entries = loaded.entries.clone();
        let mut sorted_object_offsets: Vec<u64> = loaded
            .entries
            .values()
            .filter_map(|offset| match offset {
                crate::XrefEntry::Uncompressed { offset } => Some(*offset),
                _ => None,
            })
            .collect();
        sorted_object_offsets.sort_unstable();
        sorted_object_offsets.dedup();
        let cache = ObjectCache::from_offsets(&loaded.entries);
        // Hoisted out of the struct literal because the resolver needs the
        // same id: it stamps `pdf_unique_id` onto every canonical handle it
        // mints, which `ObjectHandle::belongs_to_pdf` answers on.
        let unique_id = NEXT_PDF_ID.fetch_add(1, Ordering::Relaxed);
        let initial_diagnostics = loaded.repair_diagnostics.clone();
        let resolver = ResolverHandle::new_shared(
            reader,
            header_offset,
            source_xref_entries,
            options.repair,
            already_reconstructed,
            loaded.repair_diagnostics,
            warning_options,
            unique_id,
        );
        resolver.replay_warnings(&initial_diagnostics)?;
        // `Pdf::encryption` is the same `Rc<RefCell<..>>` allocation as
        // `ResolverCore::encryption_parameters` (qpdf's `m->encp`), not a
        // separate copy kept in sync.
        let encryption = resolver.encryption_parameters();
        let mut pdf = Self {
            unique_id,
            resolver,
            version: loaded.version,
            trailer: loaded.trailer,
            last_xref_form: loaded.last_xref_form,
            first_xref_item_offset,
            cache,
            foreign_object_maps: BTreeMap::new(),
            foreign_object_visiting: BTreeMap::new(),
            acroform_cache: Rc::new(RefCell::new(None)),
            trailer_handle_memo: None,
            legacy_materialized_memo: BTreeMap::new(),
            legacy_materialized_replacement_refs: BTreeSet::new(),
            compressed_member_parents: BTreeMap::new(),
            sorted_object_offsets,
            legacy_resolution_state_synced: already_reconstructed,
            resolution_fallbacks_remaining: MAX_RESOLUTION_FALLBACKS,
            dirty_object_refs: BTreeSet::new(),
            handle_mutated_object_refs: BTreeSet::new(),
            recovered_stream_eols: BTreeMap::new(),
            transformed_stream_refs: BTreeSet::new(),
            qpdf_dangling_refs: BTreeSet::new(),
            qpdf_trailer_references: trailer_references,
            qpdf_parsed_xref_stream_refs: BTreeSet::new(),
            qpdf_removed_refs: BTreeSet::new(),
            ever_called_get_all_pages: false,
            page_list_cache: None,
            encryption,
            encryption_inspection: Rc::new(RefCell::new(None)),
        };
        pdf.install_parsed_xref_stream_handles(parsed_xref_streams)?;
        if let Err(error) = pdf.initialize_encryption_inspection() {
            // Same diagnostic-wrapping boundary as the authentication
            // failure below: xref recovery may have already recorded
            // repair warnings, and a consumer like `run_check` (which
            // suppresses live open-time warnings and replays them via
            // `Error::OpenFailure`/`report_open_failure`) must not lose
            // them just because a malformed `/Encrypt` entry fails this
            // password-independent parse before authentication even runs.
            let diagnostics = pdf.repair_diagnostics();
            return Err(Error::with_open_diagnostics(error, diagnostics));
        }
        if let Err(error) = pdf.authenticate_if_encrypted(&options) {
            // qpdf reconstructs and records warnings before
            // `initializeEncryption` (`libqpdf/QPDF.cc:450-471`) and raises
            // authentication errors afterward (`libqpdf/QPDF_encryption.cc:929`).
            // Preserve that warning stream alongside the terminal
            // password/encryption error so file-backed helpers can emit it
            // before the final diagnostic.
            if allow_bad_password && matches!(error, Error::Encrypted(EncryptedError::BadPassword))
            {
                return Ok(pdf);
            }
            let diagnostics = pdf.repair_diagnostics();
            return Err(Error::with_open_diagnostics(error, diagnostics));
        }
        Ok(pdf)
    }
}

impl Pdf<Cursor<Arc<[u8]>>> {
    /// Open a PDF document from a shared, reference-counted byte buffer.
    ///
    /// **This call copies nothing.** The document and the caller share one
    /// allocation; clone the `Arc` to keep reading the same bytes elsewhere,
    /// and it is freed once both are done with it. (Producing the `Arc<[u8]>`
    /// in the first place may copy — `Vec<u8> -> Arc<[u8]>` reallocates so the
    /// refcount can sit beside the data — but that happens once, in the
    /// caller's own code, not per open.)
    ///
    /// ```no_run
    /// use std::sync::Arc;
    /// use flpdf::Pdf;
    ///
    /// let bytes: Arc<[u8]> = std::fs::read("input.pdf")?.into();
    /// let kept = Arc::clone(&bytes);
    /// let mut pdf = Pdf::open_mem(bytes)?;
    /// // `kept` and the document are the same bytes, not two copies.
    /// println!("version {} over {} shared bytes", pdf.version(), kept.len());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Why `Arc<[u8]>` and not `&[u8]`
    ///
    /// This took `&[u8]` and returned `Pdf<Cursor<&'a [u8]>>`, borrowing
    /// without copying. That type is no longer well-formed: `Pdf<R>` requires
    /// `R: 'static` (see the bound on [`Pdf`] for why), so the input must be
    /// owned rather than borrowed.
    ///
    /// Copying the slice internally would have kept the old signature, and it
    /// is the wrong trade. qpdf's own in-memory entry point does not copy:
    /// `QPDF::processMemoryFile` (`libqpdf/QPDF.cc:259-268`) wraps the
    /// caller's pointer in a `BufferInputSource` over
    /// `Buffer(unsigned char*, size_t)`, whose contract is "memory is owned by
    /// the caller and will not be freed when the Buffer is destroyed"
    /// (`include/qpdf/Buffer.hh:42-45`). Shared ownership is the safe-Rust
    /// analogue of that contract — no copy on either side — and a caller who
    /// holds only a slice writes `Arc::from(slice)` itself, so the copy is
    /// visible at the call site rather than hidden in the library.
    ///
    /// `Arc` rather than `Rc` because the buffer, unlike the document, can then
    /// be shared across threads that each open their own `Pdf`. The `Arc` buys
    /// sharing of the *input*, not of the document — the two doctests below
    /// pin both halves of that, so neither can go stale silently.
    ///
    /// One buffer, cloned across threads, each clone opening its own document:
    ///
    /// ```
    /// use std::sync::Arc;
    /// use flpdf::Pdf;
    ///
    /// let bytes: Arc<[u8]> = Arc::from(&b"%PDF-1.4\n"[..]);
    /// let workers: Vec<_> = (0..2)
    ///     .map(|_| {
    ///         let shared = Arc::clone(&bytes);
    ///         std::thread::spawn(move || Pdf::open_mem(shared).is_ok())
    ///     })
    ///     .collect();
    /// for worker in workers {
    ///     worker.join().unwrap();
    /// }
    /// ```
    ///
    /// The document itself, by contrast, is not `Send`, and has not been since
    /// long before the resolver existed: `handle_registry` holds
    /// [`ObjectHandle`]s whose identity is `Rc<RefCell<..>>`, as that field's
    /// own comment records. The resolver's `Rc` is a second reason, not the
    /// reason — compiling the snippet below standalone reports both, and
    /// `Rc<RefCell<DirectSlot>>` is the one that predates this work.
    /// `compile_fail` passes on *any* error,
    /// so this one is only meaningful next to the example above: that one
    /// builds the same `Arc<[u8]>` and calls the same `open_mem`, and it runs.
    /// The bound `require_send` adds is therefore the only thing left to
    /// reject:
    ///
    /// ```compile_fail
    /// use std::sync::Arc;
    /// use flpdf::Pdf;
    ///
    /// fn require_send<T: Send>(_: T) {}
    /// let bytes: Arc<[u8]> = Arc::from(&b"%PDF-1.4\n"[..]);
    /// require_send(Pdf::open_mem(bytes));
    /// ```
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open`]; see that method for the full error set.
    pub fn open_mem(bytes: Arc<[u8]>) -> crate::Result<Self> {
        Self::open(Cursor::new(bytes))
    }

    /// Open a PDF document from a shared byte buffer with explicit open options.
    ///
    /// Like [`Pdf::open_mem`] but accepts a [`PdfOpenOptions`] struct for repair and
    /// password configuration, mirroring [`Pdf::open_with_options`]. Shares
    /// `bytes` without copying, on the same terms.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open_with_options`]; see that method for the
    /// full error set.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::sync::Arc;
    /// use flpdf::{Pdf, PdfOpenOptions};
    ///
    /// let bytes: Arc<[u8]> = std::fs::read("input.pdf")?.into();
    /// let opts = PdfOpenOptions { repair: true, ..PdfOpenOptions::default() };
    /// let mut pdf = Pdf::open_mem_with_options(bytes, opts)?;
    /// println!("version {}", pdf.version());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_mem_with_options(bytes: Arc<[u8]>, options: PdfOpenOptions) -> crate::Result<Self> {
        Self::open_with_options(Cursor::new(bytes), options)
    }
}

impl Pdf<Cursor<Vec<u8>>> {
    /// Open a PDF document from an owned byte vector without wrapping it in a `Cursor` manually.
    ///
    /// The sole-ownership counterpart to [`Pdf::open_mem`]: the handle takes the
    /// `Vec` outright rather than sharing an `Arc`. Neither copies; both are
    /// `'static` and can be freely moved and stored in data structures.
    ///
    /// This is the preferred form for in-memory PDF handling in most contexts (e.g. WASM,
    /// test helpers, fulgur's document pipeline).
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open`]; see that method for the full error set.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::Pdf;
    ///
    /// let bytes: Vec<u8> = std::fs::read("input.pdf")?;
    /// let mut pdf = Pdf::open_mem_owned(bytes)?;
    /// println!("version {}", pdf.version());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_mem_owned(bytes: Vec<u8>) -> crate::Result<Self> {
        Self::open(Cursor::new(bytes))
    }

    /// Open a PDF document from an owned byte vector with explicit open options.
    ///
    /// Like [`Pdf::open_mem_owned`] but accepts a [`PdfOpenOptions`] struct for repair
    /// and password configuration, mirroring [`Pdf::open_with_options`].
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open_with_options`]; see that method for the
    /// full error set.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use flpdf::{Pdf, PdfOpenOptions};
    ///
    /// let bytes: Vec<u8> = std::fs::read("input.pdf")?;
    /// let opts = PdfOpenOptions { repair: true, ..PdfOpenOptions::default() };
    /// let mut pdf = Pdf::open_mem_owned_with_options(bytes, opts)?;
    /// println!("version {}", pdf.version());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_mem_owned_with_options(
        bytes: Vec<u8>,
        options: PdfOpenOptions,
    ) -> crate::Result<Self> {
        Self::open_with_options(Cursor::new(bytes), options)
    }
}

// Mirrors qpdf's `EMPTY_PDF` (`libqpdf/QPDF.cc:34-51`) byte for byte: PDF
// 1.3, a Catalog (object 1) pointing at an empty Pages tree (object 2), and
// a classic xref table whose offsets match this exact literal.
const EMPTY_PDF_BYTES: &[u8] = concat!(
    "%PDF-1.3\n",
    "1 0 obj\n",
    "<< /Type /Catalog /Pages 2 0 R >>\n",
    "endobj\n",
    "2 0 obj\n",
    "<< /Type /Pages /Kids [] /Count 0 >>\n",
    "endobj\n",
    "xref\n",
    "0 3\n",
    "0000000000 65535 f \n",
    "0000000009 00000 n \n",
    "0000000058 00000 n \n",
    "trailer << /Size 3 /Root 1 0 R >>\n",
    "startxref\n",
    "110\n",
    "%%EOF\n",
)
.as_bytes();

impl Pdf<Cursor<Vec<u8>>> {
    // CLAUDE.md deviation class (B): qpdf's `QPDF::emptyPDF()` is a `void`
    // method that lazily initializes an already-constructed `QPDF` (the
    // C++ type has a default-constructed, not-yet-loaded state). flpdf's
    // `Pdf` has no such state; every `open*` returns a ready-to-use
    // `Result<Self>`. `empty()` is the static-factory counterpart of that
    // mutator — same fixed bytes, same `processMemoryFile`-equivalent
    // parse path (`open_mem_owned`), only the "already-constructed
    // instance to mutate" scaffolding is replaced by a factory return.
    // Recorded in docs/qpdf-correspondence.md (QPDFPageDocumentHelper.cc
    // row, §7).
    /// Open a canonical minimal PDF: a `Catalog` (object 1) pointing at an
    /// empty `Pages` tree (object 2, zero pages), read through the normal
    /// parser and object cache like any other document.
    ///
    /// Mirrors qpdf's `QPDF::emptyPDF()` (`libqpdf/QPDF.cc:34-51,290-293`):
    /// the same fixed bytes, opened the same way [`Pdf::open_mem_owned`]
    /// opens any in-memory PDF. Objects can be added and the trailer or
    /// catalog mutated exactly as with any other opened document.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open_mem_owned`]; the fixed bytes
    /// are well-formed, so in practice this only surfaces allocator or
    /// similar infrastructure failures.
    ///
    /// # Examples
    ///
    /// ```
    /// use flpdf::Pdf;
    ///
    /// let pdf = Pdf::empty()?;
    /// assert_eq!(pdf.version(), "1.3");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn empty() -> crate::Result<Self> {
        Self::open_mem_owned(EMPTY_PDF_BYTES.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Object, ObjectRef};

    /// A classic-xref PDF whose trailer `/Encrypt` dictionary is missing
    /// `/V` (so `initialize_encryption_inspection` fails before
    /// authentication ever runs), and whose xref table header is corrupted
    /// (so reconstruction records a repair warning before that failure).
    fn damaged_xref_with_malformed_encrypt_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        // Missing /V (and /R): `required_version` fails first, inside
        // `parse_inspection_state`, before `/Filter` is even checked.
        pdf.extend_from_slice(b"4 0 obj\n<< /Filter /Standard >>\nendobj\n");

        let size = offs.len() + 1;
        let xref_start = pdf.len() as u64;
        // Corrupt the object-count digit ("4" -> "X"), matching the
        // established damaged-xref-header technique used elsewhere in this
        // crate's tests: the classic-xref parser fails to read this header
        // and falls back to line-scan reconstruction, recording a repair
        // warning before the trailer (and its /Encrypt entry) is ever read.
        let mut xref = String::from("xref\n0 X\n0000000000 65535 f \n");
        for off in &offs {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size {size} /Root 1 0 R /Encrypt 4 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn open_preserves_repair_warnings_when_encryption_inspection_initialization_fails() {
        let bytes = damaged_xref_with_malformed_encrypt_bytes();
        let error = Pdf::open_mem_owned(bytes)
            .err()
            .expect("a malformed /Encrypt dictionary must fail the open");
        let (source, diagnostics) = error.open_failure().unwrap_or_else(|| {
            // cov:ignore-start: diagnostic panic reachable only if this
            // regression test itself starts failing in an unexpected shape;
            // the passing-suite path always takes `Some(..)` here.
            panic!(
                "the terminal error must be wrapped with the repair diagnostics \
                 accumulated during xref reconstruction, not returned bare: {error}"
            )
            // cov:ignore-end
        });
        assert!(
            !diagnostics.entries().is_empty(),
            "xref reconstruction must have recorded at least one repair warning"
        );
        assert!(
            matches!(source, Error::Missing(_) | Error::Encrypted(_)),
            "the wrapped terminal error must still be the /Encrypt parse failure: {source:?}"
        );
    }

    #[test]
    fn empty_returns_canonical_minimal_document() {
        let mut pdf = Pdf::empty().expect("Pdf::empty must succeed");
        assert_eq!(pdf.version(), "1.3");

        let root_ref = pdf.root_ref().expect("root ref");
        assert_eq!(root_ref, ObjectRef::new(1, 0));
        let catalog = pdf.resolve_object(root_ref).unwrap();
        let catalog_dict = catalog.as_dict().unwrap();
        assert_eq!(
            catalog_dict.get("Type").unwrap().as_name(),
            Some(&b"Catalog"[..])
        );
        let pages_ref = catalog_dict
            .get("Pages")
            .and_then(Object::as_ref_id)
            .expect("/Pages must be a reference");
        assert_eq!(pages_ref, ObjectRef::new(2, 0));

        let pages = pdf.resolve_object(pages_ref).unwrap();
        let pages_dict = pages.as_dict().unwrap();
        assert_eq!(
            pages_dict.get("Type").unwrap().as_name(),
            Some(&b"Pages"[..])
        );
        assert_eq!(pages_dict.get("Kids"), Some(&Object::Array(vec![])));
        assert_eq!(pages_dict.get("Count"), Some(&Object::Integer(0)));

        assert_eq!(
            pdf.trailer_dictionary().get("Size"),
            Some(&Object::Integer(3))
        );
        assert_eq!(
            pdf.trailer_dictionary().get("Root"),
            Some(&Object::Reference(root_ref))
        );
    }

    #[test]
    fn empty_generations_are_independent_with_stable_root_and_pages_identity() {
        let mut a = Pdf::empty().unwrap();
        let mut b = Pdf::empty().unwrap();

        let root_ref = ObjectRef::new(1, 0);
        let pages_ref = ObjectRef::new(2, 0);
        assert_eq!(a.root_ref(), Some(root_ref));
        assert_eq!(b.root_ref(), Some(root_ref));

        // Mutating `a`'s Pages dict must not leak into `b`: each call to
        // `Pdf::empty()` owns its own bytes and cache.
        let mut pages_dict = a
            .resolve_object(pages_ref)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        pages_dict.insert("Count", Object::Integer(7));
        a.set_object(pages_ref, Object::Dictionary(pages_dict));

        assert_eq!(
            a.resolve_object(pages_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Count"),
            Some(&Object::Integer(7))
        );
        assert_eq!(
            b.resolve_object(pages_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Count"),
            Some(&Object::Integer(0))
        );
    }
}
