//! qpdf correspondence: QPDF.cc object resolution, recovery, diagnostics, and authentication responsibilities.
pub(crate) mod file_object;
pub(crate) mod resolver;

use self::file_object::{
    parse_file_object_handle_syntax, PendingHandleBody, PendingHandleFileObject,
};
use crate::cache::CacheEntry;
use crate::encryption::password::{password_candidates_for_read, PasswordMode};
use crate::encryption::permissions::Permissions;
use crate::encryption::standard::ObjectKeyAlg;
use crate::encryption::CopyEncryptionSource;
use crate::error::EncryptedError;
use crate::object_handle::{ObjectValue, NO_PARSED_OFFSET};
#[cfg(feature = "qtest-driver")]
use crate::parser::parse_qpdf_file_object_handle_with_diagnostics;
use crate::parser::HandleResolver;
use crate::reader::resolver::ResolverHandle;
#[cfg(feature = "qtest-driver")]
use crate::tokenizer::Tokenizer;
use crate::{Diagnostics, Error, ObjectHandle, ObjectRef, Result, XrefEntry, XrefForm};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::pdf::{CompressedMemberProvenance, Pdf};

/// A seekable, owned input source that can cross a qpdf job's document
/// boundary without exposing the concrete reader kind to its consumers.
///
/// qpdf's `QPDF` stores an `InputSource` behind one document type regardless
/// of whether the source came from a file, memory, or a generated JSON seed.
/// This trait is the Rust equivalent for job-owned documents: callers retain
/// lazy reads while `JobDocument` can use one `Pdf` type for every source.
pub trait ReadSeek: Read + Seek {}

impl<T: Read + Seek> ReadSeek for T {}

/// Controller for a file source that can be closed between qpdf page-job
/// operations and reopened at its last logical position.
///
/// This is the Rust equivalent of qpdf's `ClosedFileInputSource::stayOpen`
/// (`libqpdf/ClosedFileInputSource.cc:18-35,97-104`). It is intentionally
/// crate-private: callers select the policy through `QPDFJob`, not by
/// manipulating the underlying reader directly.
#[derive(Clone)]
pub(crate) struct InputSourceControl(Rc<RefCell<ReopenableFileState>>);

impl InputSourceControl {
    pub(crate) fn set_stay_open(&self, value: bool) {
        let mut state = self.0.borrow_mut();
        state.stay_open = value;
        if !value {
            // Every completed read/seek records `position`, so dropping the
            // live reader here preserves the same offset qpdf stores in
            // `ClosedFileInputSource::after` before clearing `fis`.
            state.reader = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn is_closed_for_test(&self) -> bool {
        self.0.borrow().reader.is_none()
    }
}

struct ReopenableFileState {
    path: PathBuf,
    reader: Option<BufReader<File>>,
    position: u64,
    stay_open: bool,
}

/// A file-backed `Read + Seek` source with qpdf's close-and-reopen behavior.
///
/// The source opens eagerly so opening a PDF reports the same initial I/O
/// failure as a normal file reader. Once its controller selects `stay_open =
/// false`, each completed I/O operation drops the `File`; the next operation
/// reopens the path and seeks to the saved logical position. This mirrors
/// qpdf's `ClosedFileInputSource::before`/`after` pair without changing the
/// resolver's generic `ReadSeek` contract.
pub(crate) struct ReopenableFile {
    control: InputSourceControl,
}

impl ReopenableFile {
    pub(crate) fn new(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            control: InputSourceControl(Rc::new(RefCell::new(ReopenableFileState {
                path: path.to_path_buf(),
                reader: Some(BufReader::new(file)),
                position: 0,
                stay_open: true,
            }))),
        })
    }

    pub(crate) fn controller(&self) -> InputSourceControl {
        self.control.clone()
    }

    #[cfg(test)]
    fn is_closed_for_test(&self) -> bool {
        self.control.0.borrow().reader.is_none()
    }
}

impl Read for ReopenableFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut state = self.control.0.borrow_mut();
        if state.reader.is_none() {
            let file = File::open(&state.path)?;
            let mut reader = BufReader::new(file);
            reader.seek(SeekFrom::Start(state.position))?;
            state.reader = Some(reader);
        }
        let bytes_read = match state
            .reader
            .as_mut()
            .expect("reopenable reader is installed")
            .read(buffer)
        {
            Ok(bytes_read) => bytes_read,
            Err(error) => {
                if !state.stay_open {
                    state.reader = None;
                }
                return Err(error);
            }
        };
        let position = match state
            .reader
            .as_mut()
            .expect("reopenable reader is installed")
            .stream_position()
        {
            Ok(position) => position,
            // cov:ignore-start: `ReopenableFile` owns a regular seekable File; a successful read followed by an injected stream_position failure is not representable through this concrete source.
            Err(error) => {
                if !state.stay_open {
                    state.reader = None;
                }
                return Err(error);
            } // cov:ignore-end
        };
        state.position = position;
        if !state.stay_open {
            state.reader = None;
        }
        Ok(bytes_read)
    }
}

impl Seek for ReopenableFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let mut state = self.control.0.borrow_mut();
        if state.reader.is_none() {
            let file = File::open(&state.path)?;
            let mut reader = BufReader::new(file);
            reader.seek(SeekFrom::Start(state.position))?;
            state.reader = Some(reader);
        }
        let result = state
            .reader
            .as_mut()
            .expect("reopenable reader is installed")
            .seek(from);
        if let Ok(position) = result {
            state.position = position;
        }
        if !state.stay_open {
            state.reader = None;
        }
        result
    }
}

/// Options for opening a PDF document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfOpenOptions {
    /// Enable qpdf-style xref/trailer recovery.
    ///
    /// qpdf's `QPDF::attempt_recovery` defaults to `true`; set this to `false`
    /// for the explicit strict/suppressed-recovery route.
    pub repair: bool,
    /// Never read a cross-reference stream, even where `startxref` or a
    /// `/Prev` chain points at one (qpdf `--ignore-xref-streams`).
    ///
    /// Such a section fails as though no cross-reference existed at that
    /// offset, so with [`repair`](Self::repair) also set the document falls
    /// back to reconstruction by scanning the body for object headers. Use it
    /// when a cross-reference stream is malformed but the objects it indexes
    /// are still readable.
    pub ignore_xref_streams: bool,
    /// Password bytes supplied to the Standard security handler.
    pub password: Vec<u8>,
    /// How `password` should be interpreted before authentication. qpdf only
    /// applies `hex-bytes` on the read path; the other modes pass these bytes
    /// through unchanged. See [`PasswordMode`] for the write-side semantics.
    pub password_mode: PasswordMode,
    /// Disable qpdf's alternate password-encoding retry path
    /// (`--suppress-password-recovery`).
    pub suppress_password_recovery: bool,
    /// Interpret [`password`](Self::password) as the precomputed file
    /// encryption key in hex, NOT a user/owner password (qpdf
    /// `--password-is-hex-key`). When set, all password→key derivation
    /// (Algorithm 2 / 2.A / 2.B / 6 / 7) is skipped and `hex_decode(password)`
    /// is used directly as the file key for stream/string decryption.
    pub password_is_hex_key: bool,
    /// Logger that receives document warnings as they occur. `None` selects
    /// the process-global qpdf-compatible default logger.
    pub logger: Option<crate::QPDFLogger>,
    /// Suppress warning delivery to the logger without removing warnings from
    /// [`Pdf::repair_diagnostics`].
    pub suppress_warnings: bool,
    /// Input-source description used in qpdf-compatible warning prefixes.
    ///
    /// qpdf's `InputSource` name is a byte-preserving `std::string`; keep raw
    /// bytes here so warning output can reproduce non-UTF-8 Unix paths.
    pub description: Vec<u8>,
}

impl Default for PdfOpenOptions {
    fn default() -> Self {
        Self {
            // qpdf `QPDF::Members::attempt_recovery` starts enabled
            // (`include/qpdf/QPDF.hh:1458-1462`). The opt-out is explicit,
            // matching `QPDFJob::setQPDFOptions` when suppress-recovery is
            // requested (`libqpdf/QPDF.cc:334-336`, `libqpdf/QPDFJob.cc:652-659`).
            repair: true,
            ignore_xref_streams: false,
            password: Vec::new(),
            password_mode: PasswordMode::default(),
            suppress_password_recovery: false,
            password_is_hex_key: false,
            logger: None,
            suppress_warnings: false,
            description: Vec::new(),
        }
    }
}

// Stack-growth protection for this module's two recursive hubs: `lift_bounded`
// here, and `ResolverHandle::resolve_indirect` in the `resolver` child module,
// which reaches these as `super::READER_STACK_RED_ZONE`/
// `super::READER_STACK_GROWTH_SIZE`. The red-zone value mirrors
// `parser.rs`'s own `STACK_RED_ZONE` (kept as a separate local constant rather
// than imported cross-module, matching this crate's existing per-module
// duplication in `object_handle.rs`); `resolver.rs` shares *these* rather
// than minting a third pair because it is a child of this module, not a module
// across the crate from it.
// Keep this red zone larger than parser.rs's 32 KiB value: resolver frames
// retain the post-object-stream offset and recovered-stream state, so the old
// value let a 256 KiB caller stack exhaust before stacker could switch to its
// growth segment.
const READER_STACK_RED_ZONE: usize = 128 * 1024;
// The resolver's object-attributed diagnostics and recovered-stream state add
// enough frame state that the deep-chain regression needs an earlier growth
// check than the original 32 KiB red zone. The callback stack remains 1 MiB;
// stacker switches to it before the caller's bounded stack can exhaust on any
// supported platform.
const READER_STACK_GROWTH_SIZE: usize = 1024 * 1024;

impl<R: Read + Seek> Pdf<R> {
    /// Return this document's current shared logger.
    pub fn logger(&self) -> crate::QPDFLogger {
        self.resolver.logger()
    }

    /// Replace the shared logger used for warnings raised after this call.
    pub fn set_logger(&mut self, logger: crate::QPDFLogger) {
        self.resolver.set_logger(logger);
    }

    /// Return whether warning delivery is currently suppressed.
    ///
    /// Suppression never removes warnings from [`Self::repair_diagnostics`].
    pub fn suppress_warnings(&self) -> bool {
        self.resolver.suppress_warnings()
    }

    /// Enable or disable warning delivery without changing warning collection.
    pub fn set_suppress_warnings(&mut self, suppress: bool) {
        self.resolver.set_suppress_warnings(suppress);
    }

    /// Diagnostics emitted while opening the document — typically warnings from the
    /// xref/trailer recovery path. Always non-empty when the parse hit a soft failure.
    ///
    /// Returns an owned snapshot. The collection is qpdf's `m->warnings` and
    /// lives on the crate-private resolver core rather than on this struct,
    /// so that `resolve_indirect`, which reaches this document through a
    /// `Weak` and never holds a `&mut Pdf`, can warn at all — and a
    /// `&Diagnostics` cannot be handed out from behind the `RefCell` that
    /// makes that possible. The alternative, `Ref<'_, Diagnostics>`, avoids
    /// the copy but leaks [`std::cell::Ref`] into the public API and lets a
    /// caller holding one across a resolving call hit a `BorrowMutError` at
    /// run time. The copy is cheap: the collection is empty for a document
    /// that opened cleanly.
    pub fn repair_diagnostics(&self) -> Diagnostics {
        self.resolver.repair_diagnostics()
    }

    /// Record a non-fatal processing warning on this handle.
    ///
    /// Used by recoverable code paths (e.g. form-field inheritance walks that hit
    /// a cyclic / over-deep / non-dictionary `/Parent` chain and fall back rather
    /// than aborting) so the soft failure is surfaced via [`Pdf::repair_diagnostics`]
    /// instead of being silently swallowed. Mirrors qpdf, which warns and continues
    /// on malformed field trees.
    ///
    /// Still takes `&mut self` although the sink no longer requires it: every
    /// caller already holds a `&mut Pdf`, and the resolver's own warnings go
    /// through [`resolver::ResolverHandle::push_warning`] instead. Both doors
    /// reach the one collection.
    pub(crate) fn push_warning(&mut self, message: impl Into<String>) -> Result<()> {
        self.resolver.push_warning(message)
    }

    /// Exact source framing recorded by the canonical ObjectHandle resolver
    /// after a repaired stream-length scan. Job-level stream inspection and
    /// writer consumers use this qpdf-shaped metadata without a second raw
    /// value cache.
    ///
    /// A recovered EOL is not removable framing when the stream's own bytes
    /// were replaced, it has a data provider, qpdf's `decryptStream` route
    /// would transform this stream, or the source went through qpdf-style xref
    /// reconstruction.
    pub(crate) fn canonical_recovered_stream_eol(
        &self,
        object_ref: ObjectRef,
        stream: &ObjectHandle,
    ) -> Result<Option<&'static [u8]>> {
        if self.resolver.reconstructed_xref() {
            return Ok(None);
        }
        if stream.as_stream_data().is_some() || stream.has_stream_data_provider() {
            return Ok(None);
        }
        if let Some(stream_dict) = stream.as_stream_dict() {
            if self
                .resolver
                .recovered_stream_eol_is_transformed(&stream_dict)?
            {
                return Ok(None);
            }
        }
        Ok(self
            .resolver
            .recovered_stream_eol(object_ref)
            .map(crate::parser::RecoveredStreamEol::as_bytes))
    }

    /// Whether this document has an `/Encrypt` dictionary and parsed
    /// encryption state. The dedicated inspection open path returns `true`
    /// even when password authentication failed, matching qpdf's
    /// `isEncrypted()` partial-initialization behavior.
    pub fn is_encrypted(&self) -> bool {
        self.encryption.borrow().is_some() || self.encryption_inspection.borrow().is_some()
    }

    pub(crate) fn encryption_ref(&self) -> Option<ObjectRef> {
        self.encryption
            .borrow()
            .as_ref()
            .and_then(|encryption| encryption.encrypt_ref)
    }

    /// Return qpdf `isEncrypted`'s `/V` projection, if the document is encrypted.
    pub fn encryption_version(&self) -> Option<i64> {
        self.encryption_inspection
            .borrow()
            .as_ref()
            .map(|inspection| inspection.v)
    }

    /// Return qpdf `isEncrypted`'s `/R` projection, if the document is encrypted.
    pub fn encryption_revision(&self) -> Option<i64> {
        self.encryption_inspection
            .borrow()
            .as_ref()
            .map(|inspection| inspection.r)
    }

    /// Return the initialized encryption key length in bits.
    ///
    /// The inspection state provides qpdf's revision-aware length selection
    /// before authentication and remains available after authentication. This
    /// is the same key length qpdf uses for its JSON encryption section.
    pub fn encryption_length_bits(&self) -> Option<i64> {
        self.encryption_inspection
            .borrow()
            .as_ref()
            .map(|inspection| inspection.length_bits)
    }

    /// Return qpdf `getTrimmedUserPassword()` bytes, if the document is encrypted.
    pub fn trimmed_user_password(&self) -> Option<Vec<u8>> {
        let user_password = self
            .encryption_inspection
            .borrow()
            .as_ref()
            .map(|inspection| inspection.user_password.clone())?;
        Some(crate::encryption::standard::trim_user_password(
            &user_password,
        ))
    }

    /// Return qpdf `isEncrypted`'s stream, string, and file encryption methods.
    pub fn encryption_methods(&self) -> Option<(&'static str, &'static str, &'static str)> {
        self.encryption_inspection
            .borrow()
            .as_ref()
            .map(|inspection| {
                (
                    inspection.stream_method,
                    inspection.string_method,
                    inspection.eff_method,
                )
            })
    }

    /// Whether the document uses a weak encryption method such as RC4 or R=5.
    ///
    /// Falls back to the pre-authentication `encryption_inspection` snapshot
    /// when the authenticated `EncryptionState` is absent -- for example a
    /// document returned by [`Self::open_for_encryption_inspection`] after a
    /// `BadPassword` open, which never populates `encryption`. Both sources
    /// compute the same revision/crypt-filter classification.
    pub fn uses_weak_crypto(&self) -> bool {
        self.encryption
            .borrow()
            .as_ref()
            .map(|encryption| encryption.weak_crypto)
            .or_else(|| {
                self.encryption_inspection
                    .borrow()
                    .as_ref()
                    .map(|inspection| inspection.weak_crypto)
            })
            .unwrap_or(false)
    }

    /// Advisory standard security handler permissions from `/P`, if the document is encrypted.
    pub fn permissions(&self) -> Option<Permissions> {
        self.encryption_inspection
            .borrow()
            .as_ref()
            .map(|inspection| inspection.permissions)
            .or_else(|| {
                self.encryption
                    .borrow()
                    .as_ref()
                    .map(|encryption| encryption.permissions)
            })
    }

    /// Whether the password supplied at open time authenticated against the
    /// document's user password (`/U`). Always `false` for plaintext PDFs.
    pub fn user_password_matched(&self) -> bool {
        self.encryption_inspection
            .borrow()
            .as_ref()
            .is_some_and(|inspection| inspection.user_password_matched)
            || self
                .encryption
                .borrow()
                .as_ref()
                .is_some_and(|encryption| encryption.user_password_matched)
    }

    /// Whether the password supplied at open time authenticated against the
    /// document's owner password (`/O`). Always `false` for plaintext PDFs.
    /// Many PDFs use an empty password for both, so this can be true at the
    /// same time as [`Pdf::user_password_matched`].
    pub fn owner_password_matched(&self) -> bool {
        self.encryption_inspection
            .borrow()
            .as_ref()
            .is_some_and(|inspection| inspection.owner_password_matched)
            || self
                .encryption
                .borrow()
                .as_ref()
                .is_some_and(|encryption| encryption.owner_password_matched)
    }

    /// The derived file encryption key, if the document was opened as an
    /// encrypted file. `None` for plaintext PDFs.
    ///
    /// Read-only accessor for the `show-encryption-key` inspection
    /// subcommand; does not run or alter authentication. Returns an owned
    /// copy rather than a borrowed slice, since this is an inspection
    /// accessor rather than a hot path.
    pub fn encryption_file_key(&self) -> Option<Vec<u8>> {
        self.encryption
            .borrow()
            .as_ref()
            .map(|encryption| encryption.file_key.clone())
    }

    /// Build the qpdf `copyEncryptionParameters` source for a writer attached
    /// to this already-authenticated document.
    ///
    /// The writer must not depend on reader implementation details such as
    /// `EncryptionState` or `EncryptionMode`. Keep that boundary here: the
    /// helper snapshots the authenticated file key, the source `/Encrypt`
    /// dictionary, and the permanent `/ID[0]`; the writer then applies qpdf's
    /// canonical copy rules (including forcing AES for V>=4).
    ///
    /// `/ID[0]` is read from the value cached at authentication time, not
    /// from a fresh live-trailer lookup: for a V<5 document `file_key` is
    /// itself derived from `/ID[0]` (PDF 1.7 §7.6.3.3 Algorithm 2), so this
    /// source's `id0` must stay paired with the SAME bytes `file_key` was
    /// derived from even if a caller mutates the live trailer's `/ID` after
    /// authentication completes -- otherwise the emitted `/ID[0]` and the
    /// copied `/O`/`/U`/`/P` would imply a different file key than the one
    /// actually used to encrypt the output. The R5/R6 (V=5) handler does not
    /// derive `file_key` from `/ID` at all, so no cached value is available
    /// there and a live read is safe.
    pub fn writer_copy_encryption_source(&mut self) -> Result<Option<CopyEncryptionSource>> {
        let (file_key, encryption_v, cached_id0) = {
            let guard = self.encryption.borrow();
            let Some(encryption) = guard.as_ref() else {
                return Ok(None);
            };
            (
                encryption.file_key.clone(),
                encryption.encryption_v,
                encryption.id0.clone(),
            )
        };
        let mut encrypt_dict = self.encrypt_dictionary_handle()?.ok_or_else(|| {
            Error::Unsupported("authenticated input has no /Encrypt dictionary".into())
        })?;
        // `CopyEncryptionSource` outlives this reader in the CLI and in
        // cross-document writer calls. Detach the authenticated dictionary
        // from the donor resolver while it is still alive, so later key reads
        // cannot observe a destroyed donor handle. The Standard encryption
        // dictionary is a value graph and contains no stream objects, making
        // qpdf's make-direct copy the appropriate ownership boundary here.
        encrypt_dict.make_direct(false)?;
        let id0 = match cached_id0 {
            Some(id0) => id0,
            None => {
                let id_handle = self.trailer_key_handle(b"ID");
                crate::encryption::state::first_file_id_handle(&id_handle)?
            }
        };

        Ok(Some(CopyEncryptionSource {
            encrypt_dict,
            file_key,
            id0,
            // qpdf's copy path forces AES for V>=4. The field remains part of
            // the public donor surface for the explicit copy route; the
            // canonical builder also validates/chooses from /V itself.
            object_key_alg: if encryption_v >= 4 {
                ObjectKeyAlg::Aes
            } else {
                ObjectKeyAlg::Rc4
            },
        }))
    }

    /// Return all signed AcroForm signature fields in document field order.
    ///
    /// This walks `/Catalog /AcroForm /Fields`, descends through field `/Kids`,
    /// and returns only `/FT /Sig` fields whose `/V` signature dictionary has a
    /// valid four-integer `/ByteRange`.
    ///
    /// # Errors
    ///
    /// - Propagates any error from resolving the catalog, `/AcroForm`, and
    ///   field-tree objects (for example I/O or parse failures surfaced by
    ///   [`Pdf::resolve`]).
    /// - [`Error::Parse`] when a signature field's `/ByteRange` is malformed (not a
    ///   four-element array of non-negative integers).
    pub fn signatures(&mut self) -> Result<Vec<crate::SignatureInfo>> {
        crate::signatures::signatures(self)
    }

    /// Remove the security restrictions that qpdf removes for
    /// `QPDF::removeSecurityRestrictions` (`libqpdf/QPDF.cc:2659-2667`).
    ///
    /// This is a document-level mutation: it unconditionally removes the
    /// catalog `/Perms` entry and replaces a visible `/AcroForm /SigFlags`
    /// value with the direct integer `0`. Signature-field mutation belongs to
    /// [`crate::AcroFormDocumentHelper::disable_digital_signatures`], matching
    /// qpdf's split between `QPDF` and `QPDFAcroFormDocumentHelper`.
    ///
    /// The boolean is an flpdf convenience for callers that need to report
    /// whether the in-memory document changed; qpdf's corresponding method is
    /// `void`.
    ///
    /// # Errors
    ///
    /// Returns the same root-resolution and live-handle errors as
    /// [`Pdf::root_handle`], and propagates failures while resolving the
    /// `/AcroForm` or `/SigFlags` values.
    pub fn remove_security_restrictions(&mut self) -> Result<bool> {
        let catalog = self.root_handle()?;
        let mut changed = false;

        // qpdf calls removeKey unconditionally. A present null-valued key is
        // still removed even though QPDF_Dictionary::hasKey treats it as
        // absent (`libqpdf/QPDF_Dictionary.cc:98-101,150-153`).
        if catalog
            .as_dictionary()
            .is_some_and(|entries| entries.keys().any(|key| key == b"/Perms"))
        {
            catalog.remove_key(b"/Perms");
            self.mark_object_handle_dirty(&catalog)?;
            changed = true;
        }

        let acroform = catalog.try_get_key(b"/AcroForm")?;
        self.resolve(&acroform)?;
        if acroform.as_dictionary().is_some() && acroform.try_has_key(b"/SigFlags")? {
            // qpdf replaces the key whenever its visible hasKey test
            // succeeds, including an already-zero direct integer. The
            // changed result is a crate-specific observation of structural
            // change, since qpdf's operation returns void.
            // qpdf-deviation-start: `changed` has no qpdf counterpart --
            // QPDF::removeSecurityRestrictions is void, so nothing classifies
            // the prior /SigFlags value.
            let previous = acroform.try_get_key(b"/SigFlags")?;
            self.resolve(&previous)?;
            let already_zero = previous.object_ref().is_none() && previous.as_integer() == Some(0);
            // qpdf-deviation-end
            acroform.replace_key(b"/SigFlags", ObjectHandle::integer(0))?;
            self.mark_object_handle_dirty(&acroform)?;
            if !already_zero {
                changed = true;
            }
        }

        Ok(changed)
    }

    pub(crate) fn authenticate_if_encrypted(&mut self, options: &PdfOpenOptions) -> Result<()> {
        if self.encrypt_dictionary_handle()?.is_none() {
            return Ok(());
        }
        if options.password_is_hex_key || options.suppress_password_recovery {
            return self.authenticate_if_encrypted_once(options);
        }

        let candidates = password_candidates_for_read(&options.password, options.password_mode)?;
        if candidates.len() == 1 {
            return self.authenticate_if_encrypted_once(options);
        }

        // qpdf tries the original candidate first, then each repaired encoding,
        // and appends the original one again so the terminal error has the
        // supplied password's wording and context (`QPDFJob.cc:1752-1790`).
        let original = candidates[0].clone();
        let mut final_bad_password = None;
        for candidate in candidates.into_iter().chain(std::iter::once(original)) {
            let mut attempt = options.clone();
            // Candidates are already decoded bytes. Mark them as bytes so a
            // hex-bytes input is not decoded a second time.
            attempt.password = candidate;
            attempt.password_mode = PasswordMode::Bytes;
            attempt.suppress_password_recovery = true;
            match self.authenticate_if_encrypted_once(&attempt) {
                Ok(()) => return Ok(()),
                Err(error) if matches!(error, Error::Encrypted(EncryptedError::BadPassword)) => {
                    final_bad_password = Some(error);
                }
                Err(error) => return Err(error),
            }
        }

        Err(final_bad_password.expect("qpdf password recovery always has the original candidate"))
    }

    /// Parse qpdf's encryption parameters before authentication so the
    /// read-only `--show-encryption` path can report them after BadPassword.
    pub(crate) fn initialize_encryption_inspection(&mut self) -> Result<()> {
        let encrypt_handle = self.trailer_key_handle(b"Encrypt");
        if encrypt_handle.try_is_null()? {
            return Ok(());
        }
        let id_handle = self.trailer_key_handle(b"ID");
        if !crate::encryption::state::first_file_id_handle_with_status(&id_handle)?.valid {
            // qpdf's initializeEncryption warns before validating /Encrypt and
            // then continues with an empty id1
            // (`QPDF_encryption.cc:718-751`). The warning is emitted here once,
            // while authentication consumes the same fallback value without
            // re-emitting it.
            let offset = self.resolver.last_offset();
            self.resolver
                .push_trailer_warning_at(offset, "invalid /ID in trailer dictionary")?;
        }
        let Some(encrypt) = self.encrypt_dictionary_handle()? else {
            return Ok(());
        };
        let inspection = crate::encryption::state::parse_inspection_state(&encrypt)?;
        *self.encryption_inspection.borrow_mut() = Some(inspection);
        Ok(())
    }

    fn authenticate_if_encrypted_once(&mut self, options: &PdfOpenOptions) -> Result<()> {
        let encrypt_handle = self.trailer_key_handle(b"Encrypt");
        let encrypt_ref = encrypt_handle.object_ref();
        let id_handle = self.trailer_key_handle(b"ID");
        let Some(encrypt) = self.encrypt_dictionary_handle()? else {
            return Ok(());
        };
        let authenticated = crate::encryption::state::authenticate(
            &encrypt,
            &id_handle,
            encrypt_ref,
            &options.password,
            options.password_mode,
            options.password_is_hex_key,
        )?;
        let state = authenticated.state;
        if let Some(warning) = authenticated.perms_warning {
            self.push_warning(warning)?;
        }
        if let Some(inspection) = self.encryption_inspection.borrow_mut().as_mut() {
            inspection.user_password = state.user_password.clone();
            inspection.user_password_matched = state.user_password_matched;
            inspection.owner_password_matched = state.owner_password_matched;
        }
        *self.encryption.borrow_mut() = Some(state);
        Ok(())
    }

    fn encrypt_dictionary_handle(&mut self) -> Result<Option<ObjectHandle>> {
        let encrypt = self.trailer_key_handle(b"Encrypt");
        if encrypt.is_null() {
            return Ok(None);
        }
        self.resolve(&encrypt)?;
        if encrypt.try_as_dictionary()?.is_none() {
            return Err(EncryptedError::Malformed {
                reason: "/Encrypt object is not a dictionary".into(),
            }
            .into());
        }
        Ok(Some(encrypt))
    }

    pub(crate) fn last_xref_form(&self) -> XrefForm {
        self.last_xref_form
    }

    /// Return qpdf's xref-parser-owned `first_xref_item_offset` used by the
    /// linearization `/T` check.
    pub(crate) fn first_xref_item_offset(&self) -> u64 {
        self.first_xref_item_offset
    }

    pub(crate) fn source_xref_entries(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        self.resolver.source_xref_entries()
    }

    /// Return qpdf's effective source cross-reference table.
    ///
    /// This is the reader-owned table represented by qpdf's
    /// `QPDF::getXRefTable` (`libqpdf/QPDF.cc:2370-2377`), not a writer
    /// reconstruction or a table derived from resolved values. The returned
    /// map is a snapshot because the resolver owns the table behind interior
    /// mutability; resolution-time recovery is reflected in a subsequent
    /// snapshot. Caller replacements that originated without an effective row
    /// stay in qpdf's object cache and do not manufacture an xref entry
    /// (`QPDF.cc:1986-1993`), while a later physical recovery may register and
    /// expose that source row.
    pub fn get_xref_table(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        let removed = &self.qpdf_removed_refs;
        self.resolver
            .xref_entries()
            .into_iter()
            .filter(|(object_ref, _)| !removed.contains(object_ref))
            .collect()
    }

    /// Return the qpdf-logical byte offset of an indirect stream's encoded data.
    ///
    /// This is normally the absolute offset in the original input. When repair
    /// finds a valid PDF header after leading material, qpdf treats that header
    /// as logical offset zero, and this method follows that origin.
    ///
    /// This uses the source xref entry and the same indirect-object parser as
    /// normal resolution, so `stream` text inside strings, names, comments, or
    /// earlier stream payloads cannot be mistaken for the stream marker.
    pub fn source_stream_data_offset(&mut self, object_ref: ObjectRef) -> Result<Option<u64>> {
        self.synchronize_cache_with_resolver_xref();
        let Some(XrefEntry::Uncompressed { offset }) = self.resolver.xref_entry(object_ref) else {
            return Ok(None);
        };
        let pending = self.parse_source_file_object_at(offset)?;
        let PendingHandleBody::Stream { data_start, .. } = pending.body else {
            return Ok(None);
        };
        Ok(Some(offset.saturating_add(data_start as u64)))
    }

    /// Return the source offset of the `/DecodeParms` value paired with one
    /// filter in an indirect stream dictionary.
    ///
    /// This compatibility hook is compiled only for `flpdf-test-driver`. It
    /// preserves qpdf's token-specific warning locations without adding source
    /// spans to the ordinary object model.
    #[doc(hidden)]
    #[cfg(feature = "qtest-driver")]
    pub fn qtest_decode_parms_source_offset(
        &mut self,
        object_ref: ObjectRef,
        filter_index: usize,
    ) -> Result<Option<u64>> {
        self.synchronize_cache_with_resolver_xref();
        let Some(XrefEntry::Uncompressed { offset }) = self.resolver.xref_entry(object_ref) else {
            return Ok(None);
        };
        let value_offset = self.qtest_read_source_object_with_retry(offset, |bytes| {
            decode_parms_value_offset_within(bytes, filter_index)
        })?;
        Ok(value_offset.map(|value_offset| offset.saturating_add(value_offset as u64)))
    }

    /// Return the source offset of an indirect object's own direct value.
    ///
    /// This compatibility hook is compiled only for `flpdf-test-driver`. It
    /// locates the position immediately after `N G obj`, matching qpdf's
    /// warning offset when a `/DecodeParms` reference chain terminates on a
    /// non-dictionary object: qpdf records this position once, before
    /// parsing the object's value, rather than the value token's own
    /// (whitespace/comment-skipped) start.
    #[doc(hidden)]
    #[cfg(feature = "qtest-driver")]
    pub fn qtest_object_value_source_offset(
        &mut self,
        object_ref: ObjectRef,
    ) -> Result<Option<u64>> {
        Ok(self
            .qtest_object_value_source_offsets(&[object_ref])?
            .into_iter()
            .next()
            .flatten())
    }

    /// Return source offsets for the direct values of multiple indirect
    /// objects, reading each distinct source object at most once.
    ///
    /// qpdf's `QPDF::resolve` stops at the persistent `obj_cache` entry once
    /// an object is resolved (`libqpdf/QPDF.cc:1700-1704`). The qtest offset
    /// surface is a compatibility-only source-position lookup, so it keeps
    /// the same one-read-per-object property across repeated warning
    /// attributions instead of charging the global fallback budget once per
    /// filter index.
    #[doc(hidden)]
    #[cfg(feature = "qtest-driver")]
    pub fn qtest_object_value_source_offsets(
        &mut self,
        object_refs: &[ObjectRef],
    ) -> Result<Vec<Option<u64>>> {
        self.synchronize_cache_with_resolver_xref();
        let mut offsets = BTreeMap::new();
        for &object_ref in object_refs {
            if offsets.contains_key(&object_ref) {
                continue;
            }
            let value_offset = match self.resolver.xref_entry(object_ref) {
                Some(XrefEntry::Uncompressed { offset }) => {
                    let body_start = self.qtest_read_source_object_with_retry(
                        offset,
                        Self::object_body_start_within,
                    )?; // cov:ignore: qtest-driver-only source-read error propagation is covered by feature-gated reader tests, not the workspace coverage profile
                    Some(offset.saturating_add(body_start as u64))
                }
                _ => None,
            };
            offsets.insert(object_ref, value_offset);
        }
        Ok(object_refs
            .iter()
            .map(|object_ref| offsets.get(object_ref).copied().flatten())
            .collect())
    }

    /// Return the source offset of the item at `array_index` in an indirect
    /// object whose own direct value is an array.
    ///
    /// This compatibility hook is compiled only for `flpdf-test-driver`. It
    /// covers a `/DecodeParms` array reached through a reference whose item
    /// at this position is not itself a reference: qpdf attributes the
    /// warning to the array's own indirect object, at that item's precise
    /// (whitespace/comment-skipped) token position — unlike
    /// [`Self::qtest_object_value_source_offset`], which reports the
    /// coarser "right after `obj`" position for a value that is the
    /// object's entire body.
    #[doc(hidden)]
    #[cfg(feature = "qtest-driver")]
    pub fn qtest_array_item_source_offset(
        &mut self,
        object_ref: ObjectRef,
        array_index: usize,
    ) -> Result<Option<u64>> {
        Ok(self
            .qtest_array_item_source_offsets(object_ref, &[array_index])?
            .into_iter()
            .next()
            .flatten())
    }

    /// Return source offsets for multiple items in one indirect array, using
    /// one bounded read and at most one full-source retry for the container.
    /// Duplicate indices therefore cannot consume duplicate fallback budget.
    #[doc(hidden)]
    #[cfg(feature = "qtest-driver")]
    pub fn qtest_array_item_source_offsets(
        &mut self,
        object_ref: ObjectRef,
        array_indices: &[usize],
    ) -> Result<Vec<Option<u64>>> {
        self.synchronize_cache_with_resolver_xref();
        let Some(XrefEntry::Uncompressed { offset }) = self.resolver.xref_entry(object_ref) else {
            return Ok(vec![None; array_indices.len()]);
        };
        let value_offsets = self.qtest_read_source_object_with_retry(offset, |bytes| {
            let body_start = Self::object_body_start_within(bytes)?;
            let body = &bytes[body_start..];
            array_indices
                .iter()
                .map(|&array_index| {
                    let value_offset = array_item_source_offset(body, array_index)?;
                    Ok(value_offset.map(|value_offset| body_start + value_offset))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        Ok(value_offsets
            .into_iter()
            .map(|value_offset| {
                value_offset.map(|value_offset| offset.saturating_add(value_offset as u64))
            })
            .collect())
    }

    /// Read an indirect object's bytes bounded by the next recorded object
    /// offset, retrying with an unbounded read (subject to
    /// [`Self::resolution_fallbacks_remaining`]) when `parse` fails on the
    /// bounded window — matching [`Self::parse_source_file_object_at`]'s
    /// guarded full-object retry for a corrupt or false next-xref offset.
    #[cfg(feature = "qtest-driver")]
    #[allow(deprecated)]
    fn qtest_read_source_object_with_retry<T>(
        &mut self,
        offset: u64,
        parse: impl Fn(&[u8]) -> Result<T>,
    ) -> Result<T> {
        let next = self.next_object_offset(offset);
        let bytes = self.resolver.read_window(offset, next)?;

        match parse(&bytes) {
            Ok(value) => Ok(value),
            Err(window_error) if next.is_some() && self.resolution_fallbacks_remaining > 0 => {
                self.resolution_fallbacks_remaining -= 1;
                let full = self.resolver.read_window(offset, None)?;
                parse(&full).or(Err(window_error))
            }
            Err(error) => Err(error),
        }
    }

    /// Position immediately after `N G obj`, without skipping the
    /// whitespace/comments that follow. qpdf records an indirect object's
    /// own "parsed offset" once, at this point, before parsing its value —
    /// not at the value token's own (later) start.
    #[cfg(feature = "qtest-driver")]
    fn object_body_start_within(bytes: &[u8]) -> Result<usize> {
        let mut tokenizer = Tokenizer::new(bytes);
        let _ = tokenizer.next_integer()?;
        let _ = tokenizer.next_integer()?;
        tokenizer.expect_word(b"obj")?;
        Ok(tokenizer.position())
    }

    #[allow(deprecated)]
    fn parse_source_file_object_at(&mut self, offset: u64) -> Result<PendingHandleFileObject> {
        let next = self.next_object_offset(offset);
        let bytes = self.resolver.read_window(offset, next)?;

        match parse_source_file_object_handles(&bytes) {
            Ok(pending)
                if next.is_some()
                    && self.resolution_fallbacks_remaining > 0
                    && matches!(
                        &pending.body,
                        PendingHandleBody::Direct { object, .. } if object.is_null()
                    ) =>
            {
                // A false next-object offset can make the live parser recover
                // an incomplete dictionary as null even though the source
                // object is a stream. Retry the complete source span so the
                // qtest stream-data warning keeps qpdf's data offset.
                self.resolution_fallbacks_remaining -= 1;
                let full = self.resolver.read_window(offset, None)?;
                parse_source_file_object_handles(&full).or(Ok(pending))
            }
            Ok(pending) => Ok(pending),
            Err(window_error) if next.is_some() && self.resolution_fallbacks_remaining > 0 => {
                self.resolution_fallbacks_remaining -= 1;
                let full = self.resolver.read_window(offset, None)?;
                parse_source_file_object_handles(&full).or(Err(window_error))
            }
            Err(error) => Err(error),
        }
    }

    /// Preserve resolved compressed members when their source ObjStm is
    /// replaced or removed. qpdf's `replaceObject` and `removeObject` mutate
    /// only the requested cache slot (`QPDF.cc:1980-2005`); a member already
    /// materialized in `m->obj_cache` remains available through its own
    /// `QPDFObject` handle. The legacy reader cache and the canonical handle
    /// graph are separate during this migration, so mirror either side's
    /// resolved value before changing the source slot. A planner can resolve
    /// a member canonically while the old body consumer still sees a
    /// `Compressed` cache entry; leaving that entry untouched would make the
    /// body consumer reparse through the source after it has been replaced.
    fn promote_resolved_object_stream_members(
        &mut self,
        object_stream_ref: ObjectRef,
    ) -> Result<()> {
        if object_stream_ref.generation != 0 {
            return Ok(());
        }
        if !matches!(
            self.cache.entry(object_stream_ref),
            Some(CacheEntry::Resolved(stream)) if stream.as_stream_dict().is_some()
        ) {
            // A compressed member can only have been resolved after its source
            // stream was loaded into the compatibility cache. Avoid scanning
            // the complete xref/cache for ordinary object mutations.
            return Ok(());
        }

        let mut members = Vec::new();
        for (member_ref, entry) in self.resolver.source_xref_entries() {
            let XrefEntry::Compressed { stream, index: _ } = entry else {
                continue;
            };
            if stream != object_stream_ref.number || self.qpdf_removed_refs.contains(&member_ref) {
                continue;
            }

            let cache_state = match self.cache.entry(member_ref) {
                Some(CacheEntry::Compressed { stream, index }) => {
                    Some((Some((*stream, *index)), None))
                }
                Some(CacheEntry::Resolved(handle)) => Some((None, Some(handle.clone()))),
                Some(CacheEntry::Missing | CacheEntry::Deleted)
                | Some(CacheEntry::Unresolved { .. } | CacheEntry::Reserved)
                | None => None,
            };
            let Some((compressed_entry, cached_handle)) = cache_state else {
                continue;
            };

            // Prefer the canonical value when the planner has already
            // resolved the member. This preserves direct ObjectHandle
            // mutations while the compatibility cache is being reconciled.
            let canonical_handle = self
                .resolver
                .registered_handle(member_ref)
                .filter(ObjectHandle::is_resolved)
                .or(cached_handle);
            let Some(handle) = canonical_handle else {
                continue;
            };
            members.push((member_ref, compressed_entry, handle));
        }

        for (member_ref, compressed_entry, handle) in members {
            if let Some((stream, index)) = compressed_entry {
                self.record_compressed_member_provenance(member_ref, stream, index);
            }
            self.cache.set_resolved(member_ref, handle);
        }
        Ok(())
    }

    fn record_compressed_member_provenance(
        &mut self,
        object_ref: ObjectRef,
        source_stream: u32,
        source_index: u32,
    ) {
        self.compressed_member_parents.insert(
            object_ref,
            CompressedMemberProvenance {
                source_stream,
                source_index,
            },
        );
    }

    /// Remove `object_ref`, marking it deleted.
    ///
    /// Subsequent canonical handle lookups for `object_ref` observe
    /// a null handle, matching the behavior for any
    /// other unknown or freed reference.
    pub fn delete_object(&mut self, object_ref: ObjectRef) {
        if object_ref.number != 0 {
            // The cache early return below must see the reconstructed live xref;
            // qpdf removes the corresponding cached object when a mutation
            // removes it (`libqpdf/QPDF.cc:1996-2004`).
            self.synchronize_cache_with_resolver_xref();
        }
        if object_ref.number != 0 {
            self.qpdf_removed_refs.insert(object_ref);
        }
        self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        if object_ref.number == 0 {
            return;
        }

        // qpdf erases the source xref row and canonical object-cache entry
        // while nullifying every outstanding handle (`QPDF.cc:1996-2004`).
        // Keep the indirect identity for this legacy public API, but remove
        // the source row and resolve the retained handle to null. The qpdf-facing
        // object snapshot filters this compatibility slot below.
        self.promote_resolved_object_stream_members(object_ref)
            .expect("a parsed ObjStm member must be representable as an ObjectHandle");
        self.resolver
            .remove_object_preserving_handle(object_ref)
            .expect("canonical resolver object removal is infallible");

        self.handle_mutated_object_refs.remove(&object_ref);
        self.get_object_handle(object_ref)
            .set_resolved(ObjectValue::Null);

        if matches!(
            self.cache.entry(object_ref),
            Some(CacheEntry::Deleted | CacheEntry::Missing)
        ) {
            return;
        }
        self.cache.set_deleted(object_ref);
        self.dirty_object_refs.insert(object_ref);
    }

    /// Number of objects currently resolved in the cache. Useful when you want to
    /// confirm that lazy resolution actually deferred work.
    pub fn resolved_count(&self) -> usize {
        self.cache.resolved_count()
    }

    pub(crate) fn deleted_object_refs(&self) -> Vec<ObjectRef> {
        self.cache.deleted_refs()
    }

    #[cfg(test)]
    pub(crate) fn dirty_object_refs(&self) -> Vec<ObjectRef> {
        self.dirty_object_refs.iter().copied().collect()
    }

    /// `true` when `object_ref` is currently marked dirty (i.e. has been
    /// mutated via [`Self::replace_object`] or [`Self::delete_object`] since the
    /// Pdf was opened). Used by the full-rewrite writer to detect whether a
    /// pre-existing dirty flag existed before an output-only Catalog mutation
    /// so the flag can be preserved through a restore.
    pub(crate) fn is_dirty(&self, object_ref: ObjectRef) -> bool {
        self.dirty_object_refs.contains(&object_ref)
    }

    /// Remove `object_ref` from the dirty set without touching the cache
    /// value. Used by the full-rewrite writer to undo a spurious dirty flag
    /// after restoring the pre-write Catalog snapshot: `Self::replace_object`
    /// unconditionally marks its target dirty, so the restore path calls
    /// `clear_dirty` when the caller's Pdf was clean prior to the write.
    pub(crate) fn clear_dirty(&mut self, object_ref: ObjectRef) {
        self.dirty_object_refs.remove(&object_ref);
    }

    /// Every object reference known from the cross-reference table or the
    /// canonical handle registry, including objects that have not yet been
    /// parsed. The registry half is needed for qpdf-shaped allocations made
    /// from an existing [`ObjectHandle`].
    pub fn object_refs(&self) -> Vec<ObjectRef> {
        let mut refs: BTreeSet<ObjectRef> = if self.resolver.reconstructed_xref() {
            self.cache
                .refs_after_xref_recovery(&self.resolver.source_xref_entries(), false)
                .into_iter()
                .collect()
        } else {
            self.cache
                .entries()
                .iter()
                .filter_map(|(object_ref, entry)| {
                    (!matches!(entry, CacheEntry::Missing)).then_some(*object_ref)
                })
                .collect()
        };

        refs.extend(self.canonical_object_refs(false));
        refs.into_iter().collect()
    }

    /// Object refs that the cross-reference table marks as live.
    ///
    /// Excludes:
    /// - `Deleted` — explicit `delete_object()` calls,
    /// - `Missing` — referenced but never present in any xref,
    /// - `Reserved` — forward-reference placeholders that
    ///   the canonical handle returns as null (no real indirect
    ///   object behind them).
    ///
    /// A `live_object_refs()` entry may still resolve to null; that
    /// is a real null indirect object (e.g. `1 0 obj null endobj`), not an
    /// absent one.
    pub fn live_object_refs(&self) -> Vec<ObjectRef> {
        let mut refs: BTreeSet<ObjectRef> = if self.resolver.reconstructed_xref() {
            self.cache
                .refs_after_xref_recovery(&self.resolver.source_xref_entries(), true)
                .into_iter()
                .collect()
        } else {
            self.cache
                .entries()
                .iter()
                .filter_map(|(object_ref, entry)| match entry {
                    crate::cache::CacheEntry::Deleted
                    | crate::cache::CacheEntry::Missing
                    | crate::cache::CacheEntry::Reserved => None,
                    _ => Some(*object_ref),
                })
                .collect()
        };

        refs.extend(self.canonical_object_refs(true));
        refs.retain(|object_ref| !self.qpdf_removed_refs.contains(object_ref));
        refs.into_iter().collect()
    }

    fn canonical_object_refs(&self, live_only: bool) -> BTreeSet<ObjectRef> {
        self.resolver
            .all_object_handles()
            .into_iter()
            .filter_map(|handle| {
                let object_ref = handle.object_ref()?;
                let cache_entry = self.cache.entry(object_ref);
                if matches!(cache_entry, Some(CacheEntry::Missing)) {
                    return None;
                }
                if self.resolver.xref_entry(object_ref).is_none()
                    && cache_entry.is_none()
                    && handle.is_null()
                    && !self.resolver.is_allocated_object(object_ref)
                    && !self.handle_mutated_object_refs.contains(&object_ref)
                {
                    // A canonical handle can resolve an absent reference to
                    // null without creating a legacy Missing entry. qpdf's
                    // source/cache provenance still distinguishes that
                    // dangling cache value from an allocated indirect object.
                    return None;
                }
                if live_only && self.qpdf_parsed_xref_stream_refs.contains(&object_ref) {
                    // A historical xref stream is in qpdf's object cache but
                    // its effective xref row is free/superseded. It belongs
                    // to getAllObjects/JSON cache visibility, not the
                    // effective live-xref view.
                    return None;
                }
                if live_only
                    && (matches!(
                        cache_entry,
                        Some(CacheEntry::Deleted | CacheEntry::Missing | CacheEntry::Reserved)
                    ) || (cache_entry.is_none() && !handle.is_resolved()))
                {
                    return None;
                }
                Some(object_ref)
            })
            .collect()
    }

    /// Returns the canonical [`ObjectHandle`] for `object_ref`, creating and
    /// registering an unresolved one on first request.
    ///
    /// Repeated calls with the same `object_ref` return the same shared
    /// handle rather than a new, independently-identified one, mirroring
    /// qpdf's per-document object cache (`QPDF::getObject`,
    /// `libqpdf/QPDF.cc:1951-1959`): once an indirect object has been
    /// requested, later requests for the same object number/generation
    /// observe the same cached identity.
    ///
    /// This does not perform file I/O or force object-body parsing: the
    /// returned handle's value is not read or resolved by this call.
    pub fn get_object_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        // The registry itself lives on the resolver, which is also what mints
        // the handle: it holds both halves `new_indirect_for_pdf_with_resolver`
        // needs — the document identity `belongs_to_pdf` answers on and the
        // `Weak` `try_dereference` upgrades — and it is the same door
        // `resolve_indirect` uses for a nested `N G R`, so the two can never
        // hand out different handles for one ref.
        self.resolver.get_object_handle(object_ref)
    }

    /// Whether this document holds the only strong reference to its resolver.
    ///
    /// Test-only: lets the resolver's own teardown regression assert that
    /// handing out `Weak`s cannot keep a dropped document's input source
    /// alive, rather than arguing it from the types.
    #[cfg(test)]
    pub(crate) fn resolver_is_uniquely_owned(&self) -> bool {
        Rc::strong_count(&self.resolver) == 1
    }

    /// Whether cross-reference table reconstruction has occurred for this document.
    ///
    /// qpdf `m->reconstructed_xref` (`include/qpdf/QPDF.hh:1480`).
    #[cfg(test)]
    pub(crate) fn reconstructed_xref(&self) -> bool {
        self.resolver.reconstructed_xref()
    }

    /// Prepare the canonical object cache and return qpdf's greatest object
    /// number (`QPDF::getObjectCount`, `libqpdf/QPDF.cc:1271-1283`). This is
    /// intentionally separate from fresh-object allocation, which belongs to
    /// `flpdf-25kg.3.24`.
    pub(crate) fn get_object_count(&mut self) -> Result<u32> {
        // qpdf's getObjectCount observes the same prepared object cache that
        // getAllObjects later enumerates. The document facade also owns
        // historical trailer/xref-stream registrations that are not part of
        // ResolverHandle's source-xref table, so prepare through the canonical
        // document route before deriving the largest visible object number.
        let objects = self.get_all_objects()?;
        Ok(objects
            .into_iter()
            .filter_map(|handle| handle.object_ref().map(|object_ref| object_ref.number))
            .max()
            .unwrap_or(0))
    }

    /// Return the next qpdf-shaped generation-zero object identity from the
    /// prepared canonical cache. Allocation itself belongs to the document's
    /// resolver so legacy cache values cannot silently become object-number
    /// inputs (`libqpdf/QPDF.cc:1271-1283,1872-1880`).
    pub(crate) fn next_obj_gen(&self) -> Result<ObjectRef> {
        self.resolver.next_obj_gen()
    }

    /// Promote and register an existing direct handle without cloning its
    /// allocation or scheduling writer output. This is qpdf's
    /// `makeIndirectFromQPDFObject` (`libqpdf/QPDF.cc:1882-1888`). The returned
    /// handle retains the existing object allocation and is not automatically
    /// scheduled for output.
    pub fn make_indirect_from_object_handle(&self, handle: ObjectHandle) -> Result<ObjectHandle> {
        self.resolver.make_indirect_from_object_handle(handle)
    }

    /// Create qpdf's owned empty stream object.
    ///
    /// qpdf's `QPDF::newStream()` first constructs an empty
    /// `QPDF_Stream` with parsed offset `0` and length `0`, then registers
    /// that same object allocation under a fresh generation-zero identity
    /// (`include/qpdf/QPDF.hh:319-340`; `libqpdf/QPDF.cc:1912-1931`). The
    /// stream constructor retains an empty dictionary and no source buffer
    /// (`libqpdf/QPDF_Stream.cc:109-137`). Parsed offset `0` is intentional:
    /// qpdf's `pipeStreamData` uses it to distinguish this no-data state from
    /// an original source stream (`libqpdf/QPDF_Stream.cc:571-607`).
    ///
    /// The new stream is registered with the document and can be mutated
    /// through the returned handle.
    pub fn new_stream(&self) -> Result<ObjectHandle> {
        self.resolver.new_stream_handle()
    }

    /// Create qpdf's document-owned reserved construction sentinel.
    ///
    /// A reserved object is an indirect identity with no serializable PDF
    /// value. It exists to make circular construction possible and must be
    /// replaced before writing, matching `QPDF::newReserved` and
    /// `QPDF_Reserved::unparse`
    /// (`libqpdf/QPDF.cc:1900-1903`; `libqpdf/QPDF_Reserved.cc:20-27`).
    pub fn new_reserved(&self) -> Result<ObjectHandle> {
        self.resolver.new_reserved_handle()
    }

    /// Replace a qpdf reserved object with a direct handle value.
    ///
    /// This is qpdf's `QPDF::replaceReserved`
    /// (`libqpdf/QPDF.cc:2008-2016`): only a reserved or null handle is
    /// accepted, and the replacement is installed into the existing object
    /// slot so every alias of `reserved` observes the new value. qpdf also
    /// accepts a direct null handle and passes its default `0 0` object
    /// identity to `replaceObject`; preserve that edge case rather than
    /// inventing a separate direct-null error path.
    ///
    /// The replacement keeps qpdf's `replaceObject` contract: it must be a
    /// direct handle owned by this document. The target handle's object
    /// identity is retained while its shared value state is rebound to the
    /// replacement.
    pub fn replace_reserved(
        &mut self,
        reserved: ObjectHandle,
        replacement: ObjectHandle,
    ) -> Result<()> {
        if !reserved.is_reserved() && !reserved.is_null() {
            return Err(Error::System(
                "replaceReserved called with non-reserved object".to_owned(),
            ));
        }
        let object_ref = reserved.object_ref().unwrap_or(ObjectRef::new(0, 0));
        self.replace_object(object_ref, replacement).map(|_| ())
    }

    /// Create an owned stream and replace its data with the supplied buffer.
    ///
    /// This follows qpdf's buffer overload: the empty factory runs first and
    /// `replaceStreamData` then installs the buffer and applies the
    /// zero/nonzero `/Length` boundary
    /// (`include/qpdf/QPDF.hh:319-340`; `libqpdf/QPDF.cc:1921-1931`;
    /// `libqpdf/QPDF_Stream.cc:640-684`). The `Rc<Vec<u8>>` is retained
    /// without copying, matching qpdf's shared buffer overload.
    pub fn new_stream_with_data(&self, data: Rc<Vec<u8>>) -> Result<ObjectHandle> {
        let stream = self.new_stream()?;
        stream.replace_stream_data(data, None, None);
        Ok(stream)
    }

    /// Copy one indirect object and its canonical foreign object graph into
    /// this document. This is qpdf's `QPDF::copyForeignObject`
    /// (`libqpdf/QPDF.cc:2019-2097`): indirect identities are reserved before
    /// recursive replacement, shared children and cycles reuse one
    /// destination handle, `/Pages` is a boundary, and stream payloads use
    /// the destination resolver's shared buffer/provider boundary.
    ///
    /// The destination retains the source-to-destination map, so copying the
    /// same source handle again returns the same destination identity. The
    /// source handle must be indirect and belong to a different live `Pdf`.
    ///
    /// A copied stream's data is not read until this document is written: a
    /// stream backed by [`StreamDataProvider`](crate::StreamDataProvider)
    /// stays a provider on the destination, not a materialized buffer, so it
    /// is re-read from the source on every write or read of the destination
    /// stream, not only the first. Matching qpdf's own documented contract
    /// (`include/qpdf/QPDF.hh:401-410`), **the source `Pdf` must remain
    /// alive for as long as this document may still read that copied
    /// stream** — including every later write, not just the first —
    /// because dropping it produces an [`Error::Internal`] the next time the
    /// writer tries to read the now-gone source. qpdf's escape hatch,
    /// `setImmediateCopyFrom`, is exposed as
    /// [`crate::Pdf::set_immediate_copy_from`]. Call it on the source before
    /// copying when provider-backed stream data must be materialized at copy
    /// time so the source need not survive until the destination is written.
    ///
    /// # Errors
    ///
    /// Returns [`Err`] when `foreign` is a direct handle, has no owning
    /// document, or is owned by this document itself, and when the
    /// underlying graph traversal fails: a foreign reserved sentinel
    /// encountered mid-copy, an unresolvable reference, or a prior call
    /// against the same source left this document's per-source copy state
    /// poisoned (qpdf never rolls this back on failure either, so a failed
    /// copy from a given source cannot be retried).
    pub fn copy_foreign_object(&mut self, foreign: &ObjectHandle) -> Result<ObjectHandle> {
        crate::object_copy::copy_foreign_object(self, foreign)
    }

    /// Copy a direct or indirect foreign value through the same persistent
    /// qpdf-shaped `ObjCopier` map used by [`Self::copy_foreign_object`]. This
    /// is the internal counterpart of qpdf's
    /// `replaceForeignIndirectObjects` (`libqpdf/QPDF.cc:2158-2213`) for
    /// direct Catalog/trailer children.
    pub(crate) fn copy_foreign_value(
        &mut self,
        source_id: u64,
        foreign: &ObjectHandle,
    ) -> Result<ObjectHandle> {
        crate::object_copy::copy_foreign_value(self, source_id, foreign)
    }

    /// Replace a canonical object value while retaining the target
    /// [`ObjectHandle`] identity. This is the qpdf-shaped mutation boundary;
    /// raw value snapshots and writer traversal remain outside this
    /// layer.
    ///
    /// This is qpdf's public `QPDF::replaceObject` surface
    /// (`include/qpdf/QPDF.hh:380-388`). qpdf accepts a direct, initialized
    /// handle and routes it through `updateCache`, whose existing cache slot
    /// adopts the replacement `QPDFValue` (`libqpdf/QPDF.cc:1980-1993`;
    /// `libqpdf/qpdf/QPDFObject_private.hh:117-120`), so outstanding handles
    /// observe the replacement.
    ///
    /// qpdf records the shared value transition itself rather than exposing a
    /// separate dirty bit. flpdf's writer still tracks dirty object
    /// references, so every successful replacement marks the target
    /// mutated; callers that temporarily restore a previously clean value
    /// must explicitly clear the target's dirty state after the restore.
    pub fn replace_object(
        &mut self,
        object_ref: ObjectRef,
        replacement: ObjectHandle,
    ) -> Result<ObjectHandle> {
        // Like `set_object`, this is canonical cache replacement only. Keep it
        // separate from `deleted_objects`: normal `read_xref` clears after
        // `/Size` (`QPDF.cc:686-708`), but `reconstruct_xref` clears its
        // line-scan filter at `:575` before a candidate re-read (`:516-607`).
        // Never clear or add either registration here. Exact xref/cache removal
        // remains `removeObject` (`QPDF.cc:1996-2005`).
        //
        // Refresh the compatibility-cache metadata before replacing the
        // canonical value. This keeps recovery-derived xref metadata aligned
        // without changing any other object-cache cell.
        self.synchronize_cache_with_resolver_xref();
        let target = self.resolver.replace_object(object_ref, replacement)?;
        self.qpdf_removed_refs.remove(&object_ref);
        self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.mark_object_handle_mutated(object_ref);
        Ok(target)
    }

    /// Swap the live values of two object generations while preserving each
    /// generation's canonical [`ObjectHandle`] identity.
    ///
    /// This is qpdf's public `QPDF::swapObjects` operation
    /// (`include/qpdf/QPDF.hh:396-399`; `libqpdf/QPDF.cc:2279-2289`). qpdf
    /// resolves both cache entries before exchanging their `QPDFValue`
    /// allocations, so outstanding handles continue to refer to their
    /// original object numbers while observing the swapped values. Unknown
    /// generations resolve to qpdf's ordinary null object before the swap.
    ///
    /// # Errors
    ///
    /// Propagates source resolution, recovery, and warning-delivery failures
    /// from either object.
    pub fn swap_objects(&mut self, first: ObjectRef, second: ObjectRef) -> Result<()> {
        self.synchronize_cache_with_resolver_xref();
        self.resolver.swap_objects(first, second)?;
        for object_ref in [first, second] {
            self.qpdf_removed_refs.remove(&object_ref);
            self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
            self.qpdf_dangling_refs.remove(&object_ref);
            self.mark_object_handle_mutated(object_ref);
            // qpdf's `removeObject` erases the object cache cell outright
            // (`QPDF.cc:1996-2005`); qpdf has no persistent "deleted" or
            // "missing" tombstone that a later resolve must clear. flpdf's
            // legacy `CacheEntry::Deleted`/`Missing` sentinel is scaffolding
            // this facade owns alone, so a swap that resolves a non-null
            // value into a previously deleted/missing/reserved slot must
            // clear that sentinel itself, or `live_object_refs()` keeps
            // filtering out a ref that the canonical handle now resolves.
            if matches!(
                self.cache.entry(object_ref),
                Some(CacheEntry::Deleted | CacheEntry::Missing | CacheEntry::Reserved)
            ) {
                let handle = self.get_object_handle(object_ref);
                self.cache.set_resolved(object_ref, handle);
            }
        }
        Ok(())
    }

    /// Remove a canonical object from the resolver's xref/cache view and
    /// leave outstanding handles as floating null values. The legacy snapshot
    /// metadata is maintained separately by the `Pdf` facade. This is qpdf
    /// `removeObject`'s exact xref/cache mutation (`QPDF.cc:1996-2005`), not
    /// the separate `qpdf_removed_refs` snapshot filter and not xref
    /// registration's transient free-row state (`QPDF.cc:686-708`,
    /// `:1187-1210`).
    #[cfg(test)]
    pub(crate) fn remove_object_handle(&mut self, object_ref: ObjectRef) -> Result<()> {
        // Refresh the legacy cache before removing the canonical value, or an
        // old object-stream entry can incorrectly retain provenance (mirrors
        // `replace_object`'s identical precondition above).
        self.synchronize_cache_with_resolver_xref();
        // qpdf's removeObject changes only the requested cache slot; already
        // resolved members of an ObjStm remain live in their own cache slots.
        // Promote those compatibility-cache values before the removal drops
        // the source container.
        self.promote_resolved_object_stream_members(object_ref)?;
        self.resolver.remove_object(object_ref)?;
        self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.mark_object_handle_mutated(object_ref);
        Ok(())
    }

    pub(crate) fn is_canonical_object_handle(&self, handle: &ObjectHandle) -> bool {
        handle.object_ref().is_some_and(|object_ref| {
            self.resolver
                .registered_handle(object_ref)
                .is_some_and(|canonical| canonical.is_same_object_as(handle))
        })
    }

    /// Allocate a fresh object number and register `handle`'s value as its
    /// indirect object, mirroring `QPDF::makeIndirectObject`
    /// (`libqpdf/QPDF.cc:1891-1896`, allocating via `nextObjGen()` and
    /// `obj_cache[next] = ObjCache(obj, -1, -1)`,
    /// `libqpdf/QPDF.cc:1885-1888`).
    ///
    /// The returned handle is a new, distinct object identity: replacing
    /// `handle`'s own top-level value afterward does not change the
    /// returned object, or vice versa. `handle`'s current value is only
    /// shallow-copied, though — an array or dictionary's own container is
    /// cloned, but its direct children remain the same shared handles, so
    /// mutating a nested child through either copy is visible through both.
    ///
    /// The new object uses the next unused generation-zero object number,
    /// including object numbers registered by earlier calls.
    ///
    /// Indirect handles nested in `handle`'s direct value are not validated
    /// or copied: they must already belong to this document. A nested
    /// indirect handle from another `Pdf` is neither copied nor registered
    /// here, so a later write can emit its foreign object number as a
    /// dangling reference or resolve it to an unrelated object that happens
    /// to share that number in this document. Use
    /// [`Self::copy_foreign_object`] first for handles that belong to
    /// another document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] if `handle` is indirect, mirroring
    /// qpdf's own rejection of an already-indirect input
    /// (`libqpdf/QPDF.cc:1891-1894`, `std::logic_error("attempted to make
    /// an uninitialized QPDFObjectHandle indirect")` — this crate has no
    /// "uninitialized handle" state to reject separately, since every
    /// `ObjectHandle` is always validly constructed).
    ///
    /// Also returns [`Error::Unsupported`] for a *direct* reserved `handle`
    /// (only reachable via [`ObjectHandle::shallow_copy`] on a reserved
    /// handle). Such a handle is not indirect, so this crate cannot
    /// honestly answer the "already indirect?" question with `false`
    /// either: reserved values cannot be promoted by this method. Use
    /// [`Self::make_indirect_from_object_handle`] when the existing handle
    /// identity must be preserved.
    pub fn make_indirect_object_handle(&mut self, handle: ObjectHandle) -> Result<ObjectHandle> {
        let Some(value) = handle.direct_value_clone()? else {
            return Err(Error::Unsupported(
                "cannot make an already-indirect ObjectHandle indirect".to_string(),
            ));
        };
        // qpdf's makeIndirectObject calls nextObjGen, which first calls
        // getObjectCount and therefore prepares every effective xref entry
        // before choosing the next object number. The shared helper below is
        // also used by lazy reservation paths, so keep this preparation on
        // the qpdf-shaped allocation boundary only.
        self.resolver.get_object_count()?;
        let new_ref = self.next_available_object_ref()?;
        let indirect = self.get_object_handle(new_ref);
        indirect.set_resolved(value);
        handle.copy_description_and_parsed_offset_to(&indirect);
        // QPDF::makeIndirectFromQPDFObject installs the same shared object
        // pointer in obj_cache. `handle_registry` is this port's shared
        // state, and `prepare_qpdf_json_objects` recognizes its resolved
        // cache-miss handles directly; materializing here would duplicate a
        // direct stream's payload into the legacy `Object` cache.
        // A new object left out of the dirty set would never get its own body
        // or xref entry in the canonical output, leaving any reference to it
        // dangling — see `mark_object_dirty`'s own doc comment for the full
        // explanation.
        self.mark_object_dirty(new_ref);
        Ok(indirect)
    }

    /// Return an unused generation-zero object reference.
    ///
    /// Both the legacy object cache and the canonical handle registry own
    /// object numbers. The enumeration includes both sources, and the
    /// resolver maximum is retained here so an unmaterialized handle cannot
    /// be skipped between the scan and allocation.
    pub(crate) fn next_available_object_ref(&self) -> Result<ObjectRef> {
        let max_number = self
            .object_refs()
            .iter()
            .map(|r| r.number)
            .chain(self.resolver.max_object_number())
            .max()
            .unwrap_or(0);
        if max_number >= i32::MAX as u32 {
            return Err(Error::Unsupported(
                "max object id is too high to create new objects".to_string(),
            ));
        }
        let next_number = max_number + 1;
        Ok(ObjectRef::new(next_number, 0))
    }

    pub(crate) fn unique_id(&self) -> u64 {
        self.unique_id
    }

    pub(crate) fn take_foreign_object_map(
        &mut self,
        source_id: u64,
    ) -> BTreeMap<ObjectRef, ObjectRef> {
        self.foreign_object_maps
            .remove(&source_id)
            .unwrap_or_default()
    }

    /// Return a snapshot of qpdf's persistent per-source foreign-object map
    /// without taking ownership of it. Page merge uses this boundary to keep
    /// selected-page membership distinct from later primary Catalog metadata
    /// copying while the same map remains live for subsequent copies.
    pub(crate) fn foreign_object_map_snapshot(
        &self,
        source_id: u64,
    ) -> BTreeMap<ObjectRef, ObjectRef> {
        self.foreign_object_maps
            .get(&source_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn set_foreign_object_map(
        &mut self,
        source_id: u64,
        map: BTreeMap<ObjectRef, ObjectRef>,
    ) {
        self.foreign_object_maps.insert(source_id, map);
    }

    /// qpdf's `ObjCopier::visiting` equivalent (see
    /// [`Pdf::foreign_object_visiting`]'s own doc). Used only by the
    /// canonical `copy_foreign_object` port.
    pub(crate) fn take_foreign_object_visiting(&mut self, source_id: u64) -> BTreeSet<ObjectRef> {
        self.foreign_object_visiting
            .remove(&source_id)
            .unwrap_or_default()
    }

    pub(crate) fn set_foreign_object_visiting(
        &mut self,
        source_id: u64,
        visiting: BTreeSet<ObjectRef>,
    ) {
        self.foreign_object_visiting.insert(source_id, visiting);
    }

    /// Mark `object_ref` dirty so canonical writer preparation and other live
    /// document consumers observe an in-place handle mutation.
    ///
    /// [`Self::replace_object`] and [`Self::delete_object`] already do this
    /// internally. Calling this after an in-place [`ObjectHandle`] mutation
    /// invalidates any materialized snapshot before scheduling the canonical
    /// live handle for writing, matching qpdf's single shared object state.
    pub fn mark_object_dirty(&mut self, object_ref: ObjectRef) {
        self.mark_object_handle_mutated(object_ref);
    }

    pub(crate) fn mark_object_handle_mutated(&mut self, object_ref: ObjectRef) {
        self.handle_mutated_object_refs.insert(object_ref);
        self.dirty_object_refs.insert(object_ref);
    }

    /// Mark the canonical indirect owner or owners of `handle` dirty after an
    /// in-place [`ObjectHandle`] mutation.
    ///
    /// This is the owner-aware dirty operation for qpdf-shaped live-handle
    /// mutations. An indirect array or dictionary has its own [`ObjectRef`],
    /// so passing that handle is equivalent to calling
    /// [`Self::mark_object_dirty`] with its reference. A direct array or
    /// dictionary nested inside an indirect object has no reference of its
    /// own; passing that direct child marks every containing indirect owner
    /// tracked by the handle graph. This is the form callers need after
    /// mutating a direct child array, for example with
    /// [`ObjectHandle::append_array_item`].
    ///
    /// Direct-child containment is recorded when the child is resolved from
    /// or inserted into a live indirect object. A detached direct child has no
    /// writer owner and therefore marks nothing. This method never resolves
    /// unrelated objects merely to rediscover an owner.
    ///
    /// Like qpdf's `QPDFObjectHandle` ownership checks, a handle from another
    /// [`Pdf`] is rejected rather than treating an equal-numbered object in
    /// this document as its owner.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] when `handle` belongs to another
    /// document or is otherwise not a canonical handle owned by this
    /// document.
    pub fn mark_object_handle_dirty(&mut self, handle: &ObjectHandle) -> Result<()> {
        if let Some(object_ref) = handle.object_ref() {
            if !self.is_canonical_object_handle(handle) {
                return Err(Error::Unsupported(
                    "ObjectHandle belongs to another Pdf".to_string(),
                ));
            }
            self.mark_object_handle_mutated(object_ref);
            return Ok(());
        }

        if !handle.belongs_to_pdf(self.unique_id) {
            return Err(Error::Unsupported(
                "ObjectHandle belongs to another Pdf".to_string(),
            ));
        }
        for object_ref in handle.containing_object_refs_for_pdf(self.unique_id) {
            self.mark_object_handle_mutated(object_ref);
        }
        Ok(())
    }

    /// Promote xref-stream objects parsed from the source `/Prev` chain into
    /// the canonical resolver cache. qpdf reads each xref stream before it
    /// merges the next xref section (`QPDF.cc:626-710`, `:1640-1716`), so a
    /// superseded or freed xref-stream object remains observable through
    /// `m->obj_cache` even when it has no effective xref row. Rebind the
    /// bootstrap handles into this document's resolver before registering them.
    pub(crate) fn install_parsed_xref_stream_handles(
        &mut self,
        parsed_xref_streams: BTreeMap<ObjectRef, ObjectHandle>,
    ) -> Result<()> {
        for (object_ref, source) in parsed_xref_streams {
            if object_ref.number == 0
                || object_ref.generation == u16::MAX
                || self.qpdf_removed_refs.contains(&object_ref)
                || self.resolver.xref_entry(object_ref).is_some()
            {
                continue;
            }
            let handle = self.get_object_handle(object_ref);
            if !handle.is_resolved()
                || matches!(self.cache.entry(object_ref), Some(CacheEntry::Missing))
            {
                let value = rebind_handle_value(&self.resolver, &source)?;
                handle.set_resolved(value);
            }
            self.qpdf_parsed_xref_stream_refs.insert(object_ref);
        }
        Ok(())
    }

    fn register_trailer_references(&mut self) -> Vec<ObjectRef> {
        let trailer_refs: Vec<_> = self
            .qpdf_trailer_references
            .iter()
            .copied()
            .filter(|object_ref| {
                object_ref.number != 0
                    && object_ref.generation != u16::MAX
                    && !self.qpdf_removed_refs.contains(object_ref)
            })
            .collect();
        for object_ref in &trailer_refs {
            self.get_object_handle(*object_ref);
        }
        trailer_refs
    }

    /// Return qpdf's complete canonical object cache in `ObjectRef` order.
    ///
    /// `QPDF::getAllObjects` first calls `fixDanglingReferences` and then
    /// walks `m->obj_cache` (`libqpdf/QPDF.cc:1258-1294`). The canonical
    /// resolver performs that preparation here: every effective source-xref
    /// object is resolved, and parser-discovered dangling references are
    /// retained as canonical indirect handles. Free rows are not in the
    /// effective source table, matching qpdf's `insertFreeXrefEntry` split
    /// between `xref_table` and `deleted_objects`.
    pub fn get_all_objects(&mut self) -> Result<Vec<ObjectHandle>> {
        let trailer_refs = self.register_trailer_references();
        self.resolver.fix_dangling_references()?;

        // qpdf's parser creates trailer-reference cache entries before
        // fixDanglingReferences. Resolve the registered seeds after the xref
        // pass so a trailer-only reference with no row becomes an explicit
        // missing/null cache entry rather than escaping enumeration as
        // Unresolved.
        for object_ref in trailer_refs {
            self.get_object_handle(object_ref).try_dereference()?;
        }

        let removed = self.qpdf_removed_refs.clone();
        Ok(self
            .resolver
            .all_object_handles()
            .into_iter()
            .filter(|handle| {
                handle.object_ref().is_none_or(|object_ref| {
                    object_ref.number != 0
                        && object_ref.generation != u16::MAX
                        && !removed.contains(&object_ref)
                })
            })
            .collect())
    }

    /// Resolve `handle` in place if it is an unresolved indirect handle.
    ///
    /// A direct handle, or an indirect handle that has already been resolved,
    /// is a no-op. Resolution is delegated directly to the canonical
    /// `ResolverHandle` cache; it never materializes an independent value
    /// snapshot.
    ///
    /// qpdf's typed `QPDFObjectHandle` accessors call `QPDF::resolve` lazily
    /// and retain the same shared object identity: resolving the same
    /// indirect reference more than once yields handles that alias the same
    /// cached value rather than independent copies.
    ///
    /// This resolves the supplied canonical handle once and leaves any
    /// already-resolved value in place; callers that need a terminal child
    /// explicitly resolve the child handle they obtained from the value.
    ///
    /// The canonical parser records source descriptions and offsets while it
    /// builds the graph. Streams retain their source filter dictionaries and
    /// are decrypted at pipe time, matching qpdf's `QPDF_Stream` path.
    ///
    /// # Errors
    ///
    /// I/O, parse, filter, or decryption failures propagate. Free, absent, or
    /// overridden references resolve to the canonical null fallback.
    pub fn resolve(&mut self, handle: &ObjectHandle) -> Result<()> {
        // ObjectHandle resolution is qpdf's canonical cache operation. The
        // resolver owns the source xref table, live parser, stream pipeline,
        // and one handle per object reference; no raw Object materialization
        // or metadata-only reparse belongs on this path.
        handle.try_dereference()
    }

    /// Resolve one canonical handle and return the same identity. This is a
    /// small convenience for callers that need an owned handle after the
    /// resolver call; it does not chase stored reference values because the
    /// canonical value model has no reference-as-value variant.
    pub(crate) fn resolve_handle(&mut self, handle: &ObjectHandle) -> Result<ObjectHandle> {
        self.resolve(handle)?;
        Ok(handle.clone())
    }

    /// Resolve one canonical handle and retain its own indirect identity.
    pub(crate) fn resolve_handle_ref(
        &mut self,
        handle: &ObjectHandle,
    ) -> Result<(ObjectHandle, Option<ObjectRef>)> {
        let object_ref = handle.object_ref();
        self.resolve(handle)?;
        Ok((handle.clone(), object_ref))
    }

    /// Read a linearization hint object and retain qpdf's source position for
    /// a following `damagedPDF` warning. The resolver distinguishes an already
    /// cached object from a newly parsed one, matching qpdf's
    /// `readObjectAtOffset`/`InputSource::getLastOffset` behavior.
    pub(crate) fn resolve_at_offset_with_damage_offset(
        &self,
        offset: u64,
        expected: ObjectRef,
    ) -> Result<(ObjectHandle, Option<u64>)> {
        self.resolver
            .resolve_at_offset_with_damage_offset(offset, expected)
    }

    /// Return qpdf's first-1024-byte linearization candidate as an exact
    /// generation-zero object reference.
    pub(crate) fn linearization_candidate_ref(&self) -> Result<Option<ObjectRef>> {
        let Some(number) = self.resolver.linearization_candidate()? else {
            return Ok(None);
        };
        let Ok(number) = u32::try_from(number) else {
            return Ok(None);
        };
        if number == 0 {
            return Ok(None);
        }
        Ok(Some(ObjectRef::new(number, 0)))
    }

    /// Resolve `object_ref` for the qpdf JSON projection through the canonical
    /// handle resolver. The returned handle is the persistent qpdf object-cache
    /// cell, so callers can inspect its value or stream without creating a raw
    /// `Object` snapshot.
    ///
    /// Unknown, freed, or compressed-but-broken entries resolve to a canonical
    /// null handle rather than an error, matching the behavior the PDF spec
    /// mandates for missing objects (§7.3.10).
    ///
    /// # Errors
    ///
    pub(crate) fn resolve_qpdf_json_handle(
        &mut self,
        object_ref: ObjectRef,
    ) -> Result<ObjectHandle> {
        let handle = self.get_object_handle(object_ref);
        self.resolve(&handle)?;
        Ok(handle)
    }

    /// Offset of the first recorded object that starts strictly after `offset`,
    /// or `None` when `offset` belongs to the last object in the file.
    fn next_object_offset(&self, offset: u64) -> Option<u64> {
        let index = self.sorted_object_offsets.partition_point(|&o| o <= offset);
        self.sorted_object_offsets.get(index).copied()
    }

    /// Bring the legacy cache and bounded-read offsets in line with the
    /// canonical resolver after a resolution-time xref reconstruction.
    pub(crate) fn synchronize_cache_with_resolver_xref(&mut self) {
        if self.legacy_resolution_state_synced || !self.resolver.reconstructed_xref() {
            return;
        }

        let removed = &self.qpdf_removed_refs;
        let entries = self
            .resolver
            .source_xref_entries()
            .into_iter()
            .filter(|(object_ref, _)| !removed.contains(object_ref))
            .collect();
        self.cache.synchronize_with_xref(&entries);
        // Keep the direct object-stream provenance for a still-identical
        // compressed xref entry, but never let a mapping survive a rebuilt
        // type-1 entry or a changed object-stream/index pair. qpdf's live
        // xref table is the authority after reconstruction
        // (`libqpdf/QPDF.cc:532-562`); the source identity stored with each
        // compressed member prevents the legacy writer from treating a
        // formerly compressed object as an object-stream member.
        self.compressed_member_parents
            .retain(|object_ref, provenance| {
                matches!(
                    entries.get(object_ref),
                    Some(XrefEntry::Compressed { stream, index })
                        if provenance.source_stream == *stream
                            && provenance.source_index == *index
                )
            });
        self.sorted_object_offsets = entries
            .values()
            .filter_map(|entry| match entry {
                XrefEntry::Uncompressed { offset } => Some(*offset),
                _ => None,
            })
            .collect();
        self.sorted_object_offsets.sort_unstable();
        self.sorted_object_offsets.dedup();
        self.legacy_resolution_state_synced = true;
    }
}

pub(crate) fn rebind_handle_value<R: Read + Seek + 'static>(
    resolver: &ResolverHandle<R>,
    source: &ObjectHandle,
) -> Result<ObjectValue> {
    // cov:ignore-start: the bootstrap xref loader passes the direct trailer
    // value by construction; an indirect handle here is an internal invariant
    // violation rather than a reachable PDF input shape.
    if let Some(object_ref) = source.object_ref() {
        return Err(Error::Internal(format!(
            "expected a direct bootstrap value, got {object_ref}"
        )));
    }
    // cov:ignore-end
    source.try_dereference()?;
    let value = source
        .with_value(|value| value.cloned())
        .ok_or_else(|| Error::Internal("bootstrap handle has no value".to_owned()))?;
    match value {
        ObjectValue::Array(children) => Ok(ObjectValue::Array(
            children
                .iter()
                .map(|child| rebind_handle(resolver, child))
                .collect::<Result<Vec<_>>>()?,
        )),
        ObjectValue::Dictionary(entries) => Ok(ObjectValue::Dictionary(
            entries
                .into_iter()
                .map(|(key, child)| Ok((key, rebind_handle(resolver, &child)?)))
                .collect::<Result<BTreeMap<_, _>>>()?,
        )),
        ObjectValue::Stream {
            stream_dict,
            stream_data,
            stream_provider,
            filter_on_write,
            stream_length,
        } => Ok(ObjectValue::Stream {
            stream_dict: rebind_handle(resolver, &stream_dict)?,
            stream_data,
            stream_provider,
            filter_on_write,
            stream_length,
        }),
        other => Ok(other),
    }
}

fn rebind_handle<R: Read + Seek + 'static>(
    resolver: &ResolverHandle<R>,
    source: &ObjectHandle,
) -> Result<ObjectHandle> {
    if let Some(object_ref) = source.object_ref() {
        return Ok(resolver.get_object_handle(object_ref));
    }
    Ok(resolver.direct_object_handle(rebind_handle_value(resolver, source)?))
}

#[cfg(feature = "qtest-driver")]
fn decode_parms_value_offset_within(bytes: &[u8], filter_index: usize) -> Result<Option<usize>> {
    let body_start = {
        let mut tokenizer = Tokenizer::new(bytes);
        let _ = tokenizer.next_integer()?;
        let _ = tokenizer.next_integer()?;
        tokenizer.expect_word(b"obj")?;
        tokenizer.position()
    };
    let object = parse_direct_handle_with_offsets(&bytes[body_start..])?;
    let Some(entries) = object.as_dictionary() else {
        // A damaged next-object offset can truncate the dictionary window.
        // The live parser recovers that truncated input as null with
        // diagnostics, but qtest's source-offset helper must retry against
        // the full object span before giving up.
        return Err(Error::parse(
            body_start,
            "source stream dictionary window is incomplete",
        ));
    };
    let Some(decode_parms) = entries.get(b"/DecodeParms".as_slice()) else {
        return Ok(None);
    };
    let value = decode_parms
        .as_array()
        .and_then(|items| items.get(filter_index).cloned())
        .unwrap_or_else(|| decode_parms.clone());
    let offset = value.get_parsed_offset();
    Ok((offset >= 0).then_some(usize::try_from(offset).unwrap_or(usize::MAX) + body_start))
}

#[cfg(feature = "qtest-driver")]
fn array_item_source_offset(input: &[u8], array_index: usize) -> Result<Option<usize>> {
    let handle = parse_direct_handle_with_offsets(input)?;
    Ok(handle
        .as_array()
        .and_then(|items| items.get(array_index).cloned())
        .and_then(|item| {
            let offset = item.get_parsed_offset();
            (offset >= 0).then_some(usize::try_from(offset).unwrap_or(usize::MAX))
        }))
}

#[cfg(feature = "qtest-driver")]
fn parse_direct_handle_with_offsets(input: &[u8]) -> Result<ObjectHandle> {
    let mut resolver = SourceFramingHandles;
    Ok(parse_qpdf_file_object_handle_with_diagnostics(input, 0, None, &mut resolver)?.value)
}

/// Parse source framing without attaching the temporary diagnostic lookup to
/// the document's canonical cache. The stream dictionary is still a canonical
/// handle graph; unresolved child references are deliberately left lazy.
fn parse_source_file_object_handles(input: &[u8]) -> Result<PendingHandleFileObject> {
    let mut resolver = SourceFramingHandles;
    parse_file_object_handle_syntax(input, &mut resolver)
}

struct SourceFramingHandles;

impl HandleResolver for SourceFramingHandles {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        ObjectHandle::new_indirect_unresolved(object_ref, NO_PARSED_OFFSET)
    }

    fn indirect_handle_at(&mut self, object_ref: ObjectRef, offset: i64) -> ObjectHandle {
        ObjectHandle::new_indirect_unresolved(object_ref, offset)
    }

    fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
        ObjectHandle::from_value(value)
    }
}

#[cfg(all(test, feature = "qtest-driver"))]
mod final_handle_tests {
    use super::*;

    #[test]
    fn qtest_decode_parms_offset_reports_absence_and_source_handles_keep_identity() {
        assert_eq!(
            decode_parms_value_offset_within(b"1 0 obj\n<< /Length 1 >>\nendobj", 0)
                .expect("source dictionary parses"),
            None
        );

        let mut resolver = SourceFramingHandles;
        let handle = resolver.indirect_handle(ObjectRef::new(17, 0));
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(17, 0)));
    }
}

#[cfg(test)]
mod source_window_tests {
    use super::Pdf;
    use crate::ObjectRef;

    #[test]
    fn source_stream_offset_retries_when_the_next_xref_offset_truncates_the_header() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (number, body) in [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".as_slice(),
            ),
        ] {
            offsets.push((number, bytes.len()));
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let stream_offset = bytes.len();
        bytes.extend_from_slice(b"4 0 obj\n<< /Length 5 >>\nstream\nhello\nendstream\nendobj\n");
        let truncated_header_offset = stream_offset + 4;
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for (_, offset) in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(format!("{stream_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(format!("{truncated_header_offset:010} 00000 n \n").as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );

        let mut pdf = Pdf::open_mem_owned(bytes).expect("synthetic source-window PDF opens");
        let data_offset = pdf
            .source_stream_data_offset(ObjectRef::new(4, 0))
            .expect("source stream offset")
            .expect("stream object has source data");
        assert_eq!(
            data_offset,
            (stream_offset + b"4 0 obj\n<< /Length 5 >>\nstream\n".len()) as u64
        );
    }
}

#[cfg(test)]
mod reopenable_source_tests {
    use super::{Pdf, PdfOpenOptions, ReopenableFile};
    use crate::engine::EMPTY_PDF_BYTES;
    use std::io::Read;

    #[test]
    fn close_input_source_releases_the_file_backed_reopen_controller() {
        // Codex review on PR #1470: close_input_source only replaced the
        // resolver's own StreamInput, leaving Pdf::input_source_control -- a
        // second, independent owner of the same reopen state -- holding the
        // OS file open until the whole Pdf was dropped.
        let temp = tempfile::tempdir().expect("temporary source directory");
        let path = temp.path().join("source.pdf");
        std::fs::write(&path, EMPTY_PDF_BYTES).expect("write source");

        let pdf = Pdf::open_file_with_options(&path, PdfOpenOptions::default())
            .expect("open file-backed document");
        let control = pdf
            .input_source_control
            .clone()
            .expect("file-backed documents install a reopen controller");
        assert!(
            !control.is_closed_for_test(),
            "the controller starts with an open file handle"
        );

        pdf.close_input_source();

        assert!(
            control.is_closed_for_test(),
            "close_input_source must release the reopen controller's file handle too"
        );
    }

    #[test]
    fn closed_source_reopens_at_the_last_reader_position() {
        let temp = tempfile::tempdir().expect("temporary source directory");
        let path = temp.path().join("source.pdf");
        std::fs::write(&path, b"abcdef").expect("write source");

        let mut source = ReopenableFile::new(&path).expect("open source");
        let controller = source.controller();
        controller.set_stay_open(false);

        let mut first = [0; 2];
        source.read_exact(&mut first).expect("read first bytes");
        assert_eq!(&first, b"ab");
        assert!(
            source.is_closed_for_test(),
            "source must close after a read"
        );

        controller.set_stay_open(true);
        let mut second = [0; 2];
        source.read_exact(&mut second).expect("reopen and read");
        assert_eq!(&second, b"cd");
        assert!(
            !source.is_closed_for_test(),
            "explicit keep-open must retain source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_error_closes_a_nonpersistent_source() {
        let temp = tempfile::tempdir().expect("temporary source directory");
        let directory = temp.path().join("not-a-file");
        std::fs::create_dir(&directory).expect("create directory source");

        let mut source = ReopenableFile::new(&directory).expect("open directory source");
        source.controller().set_stay_open(false);
        let error = source
            .read(&mut [0; 1])
            .expect_err("directory read must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::IsADirectory);
        assert!(
            source.is_closed_for_test(),
            "failed nonpersistent read must close source"
        );
    }
}

#[cfg(test)]
mod encryption_state_commit_tests {
    use super::{Pdf, PdfOpenOptions};
    use crate::{Error, ObjectHandle, QPDFLogger};
    use std::io::Cursor;

    #[test]
    fn authenticated_state_is_not_committed_before_perms_warning_delivery() {
        let fixture = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/encrypted/v5-aes-256-r6.pdf"),
        )
        .expect("R6 fixture");
        let options = PdfOpenOptions {
            password: b"user-v5-r6".to_vec(),
            ..PdfOpenOptions::default()
        };
        let mut pdf = Pdf::open_with_options(Cursor::new(fixture), options.clone())
            .expect("R6 fixture authenticates");

        let encrypt = pdf
            .encrypt_dictionary_handle()
            .expect("encryption dictionary lookup")
            .expect("encrypted fixture");
        encrypt
            .replace_key(b"/Perms", ObjectHandle::string(vec![0]))
            .expect("replace /Perms");
        *pdf.encryption.borrow_mut() = None;

        let logger = QPDFLogger::create();
        logger.set_warn(Some(crate::pipeline::PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));
        pdf.set_logger(logger);

        let error = pdf
            .authenticate_if_encrypted_once(&options)
            .expect_err("warning sink failure must propagate");
        assert!(matches!(
            error,
            Error::System(message) if message == "sink write failure 1"
        ));
        assert!(
            pdf.encryption.borrow().is_none(),
            "authentication state must remain uncommitted when /Perms warning delivery fails"
        );
    }
}
