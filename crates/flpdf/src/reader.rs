//! qpdf correspondence: QPDF.cc object resolution, recovery, diagnostics, and authentication responsibilities.
pub(crate) mod file_object;
pub(crate) mod resolver;

use crate::encryption::password::{password_candidates_for_read, PasswordMode};
use crate::encryption::permissions::Permissions;
use crate::encryption::standard::ObjectKeyAlg;
use crate::encryption::CopyEncryptionSource;
use crate::error::EncryptedError;
use crate::object_handle::DocumentResolver;
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::{Diagnostics, Error, ObjectHandle, ObjectRef, QpdfExc, Result, XrefEntry, XrefForm};
use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::pdf::Pdf;

/// A seekable, owned input source that can cross a qpdf job's document
/// boundary without exposing the concrete reader kind to its consumers.
///
/// qpdf's `QPDF` stores an `InputSource` behind one document type regardless
/// of whether the source came from a file, memory, or a generated JSON seed.
/// This trait is the Rust equivalent for job-owned documents: callers retain
/// lazy reads while `JobDocument` can use one `Pdf` type for every source.
pub trait ReadSeek: Read + Seek {
    /// Rust-native downcast boundary corresponding to qpdf InputSource RTTI.
    fn as_any(&self) -> &dyn Any;
}

impl<T: Read + Seek + 'static> ReadSeek for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

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
    /// Emit qpdf job-level informational messages while opening the document.
    /// This carries `QPDFJob::doIfVerbose` policy into the reader-owned
    /// authentication retry boundary.
    pub verbose: bool,
    /// Message prefix used for qpdf job-level informational messages.
    pub message_prefix: Vec<u8>,
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
            verbose: false,
            message_prefix: b"qpdf".to_vec(),
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

    /// Enable or disable qpdf recovery on this live document.
    ///
    /// This is qpdf's `QPDF::setAttemptRecovery` (`include/qpdf/QPDF.hh:234`,
    /// `libqpdf/QPDF.cc:334`). The policy is read both while loading the
    /// cross-reference data and when a later object read considers rebuilding
    /// that data, so this mutator intentionally reaches the shared resolver
    /// state rather than changing a one-shot open option.
    pub fn set_attempt_recovery(&mut self, attempt_recovery: bool) {
        self.resolver.set_attempt_recovery(attempt_recovery);
    }

    /// Snapshot the document's currently undrained diagnostics — typically warnings from the
    /// xref/trailer recovery path when called immediately after opening.
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
    /// that opened cleanly or after [`Self::get_warnings`] has drained it.
    pub fn repair_diagnostics(&self) -> Diagnostics {
        self.resolver.repair_diagnostics()
    }

    /// Return and clear the document's ordered qpdf warnings.
    ///
    /// This is qpdf's `QPDF::getWarnings` (`include/qpdf/QPDF.hh:261-266`,
    /// `libqpdf/QPDF.cc:345-352`). Warning logger delivery happens when the
    /// warning is recorded, so draining this collection never emits the same
    /// warning a second time. Warnings raised after this call appear in the
    /// next returned collection.
    pub fn get_warnings(&self) -> Diagnostics {
        self.resolver.get_warnings()
    }

    /// Return whether this document currently has undrained warnings.
    ///
    /// This is qpdf's `QPDF::anyWarnings` (`include/qpdf/QPDF.hh:268-270`): it
    /// does not clear the collection and remains true until [`Self::get_warnings`]
    /// drains it.
    #[must_use]
    pub fn any_warnings(&self) -> bool {
        self.resolver.any_warnings()
    }

    /// The number of warnings recorded so far, without copying the
    /// collection: qpdf's `QPDF::numWarnings` (`libqpdf/QPDF.cc:360-363`),
    /// which reports the number since the last [`Self::get_warnings`] drain.
    #[must_use]
    pub fn num_warnings(&self) -> usize {
        self.resolver.num_warnings()
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

    /// Record and route a complete qpdf warning value without rebuilding its
    /// source/object context at a higher layer.
    pub(crate) fn push_qpdf_warning(&self, warning: QpdfExc) -> Result<()> {
        self.resolver.push_qpdf_warning(warning)
    }

    pub(crate) fn input_description(&self) -> Vec<u8> {
        self.resolver.input_description()
    }

    /// Exact source framing recorded by the canonical ObjectHandle resolver
    /// after a repaired stream-length scan. The `dump-object` reserialization
    /// consumer uses this qpdf-shaped metadata without a second raw value
    /// cache; stream-data output itself keeps the complete recovered span.
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
    /// After authentication, qpdf reports the actual `getEncryptionKey()`
    /// length, including an overlength value supplied through
    /// `--password-is-hex-key` (`QPDFJob.cc:1245-1252`). Before authentication,
    /// the inspection state provides qpdf's revision-aware dictionary length.
    pub fn encryption_length_bits(&self) -> Option<i64> {
        let authenticated_bits = self.encryption.borrow().as_ref().and_then(|encryption| {
            encryption
                .file_key
                .len()
                .checked_mul(8)
                .and_then(|bits| i64::try_from(bits).ok())
        });
        authenticated_bits.or_else(|| {
            self.encryption_inspection
                .borrow()
                .as_ref()
                .map(|inspection| inspection.length_bits)
        })
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
            changed = true;
        }

        let acroform = catalog.try_get_key(b"/AcroForm")?;
        if acroform.try_is_dictionary()? && acroform.try_has_key(b"/SigFlags")? {
            // qpdf replaces the key whenever its visible hasKey test
            // succeeds, including an already-zero direct integer. The
            // changed result is a crate-specific observation of structural
            // change, since qpdf's operation returns void.
            // qpdf-deviation-start: `changed` has no qpdf counterpart --
            // QPDF::removeSecurityRestrictions is void, so nothing classifies
            // the prior /SigFlags value.
            let previous = acroform.try_get_key(b"/SigFlags")?;
            let already_zero =
                previous.object_ref().is_none() && previous.try_as_integer()? == Some(0);
            // qpdf-deviation-end
            acroform.replace_key(b"/SigFlags", ObjectHandle::integer(0))?;
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
        let logger = options
            .logger
            .clone()
            .unwrap_or_else(crate::QPDFLogger::default_logger);
        let mut warned = false;
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
                    if options.verbose && !warned {
                        warned = true;
                        let mut message = options.message_prefix.clone();
                        message.extend_from_slice(
                            b": supplied password didn't work; trying other passwords based on interpreting password with different string encodings\n",
                        );
                        logger.info(message)?;
                    }
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
        // qpdf gates on `m->trailer.hasKey("/Encrypt")`
        // (`libqpdf/QPDF_encryption.cc:729`), which treats a key that
        // resolves to null the same as an absent key
        // (`QPDF_Dictionary::hasKey`, `libqpdf/QPDF_Dictionary.cc:98-101`).
        // A lazily-resolved indirect `/Encrypt` reference must go through
        // that same resolving check rather than the non-resolving
        // `ObjectHandle::is_null`, which only sees a direct null literal.
        if encrypt.try_is_null()? {
            return Ok(None);
        }
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

    /// Append source ObjStm membership without resolving object values.
    ///
    /// Ports the writer-private `QPDF::getObjectStreamData` document operation
    /// (`libqpdf/QPDF.cc:2381-2390`; `include/qpdf/QPDF.hh:757-761`). Existing
    /// map entries survive unless a type-2 source row overwrites their key.
    pub(crate) fn get_object_stream_data(&self, mapping: &mut BTreeMap<u32, u32>) {
        self.resolver.get_object_stream_data(mapping);
    }

    /// Record a target object's source-backed ObjStm membership for a fresh
    /// multi-source page-selection document.
    ///
    /// The merge target has no physical input xref row for a copied primary
    /// member, but the plain Preserve writer must see the qpdf-equivalent
    /// type-2 row when it reconstructs the primary source container.
    pub(crate) fn install_object_stream_member(
        &self,
        object_ref: ObjectRef,
        stream: ObjectRef,
        index: u32,
    ) {
        self.resolver.insert_source_xref_entry(
            object_ref,
            XrefEntry::Compressed {
                stream: stream.number,
                index,
            },
        );
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
        self.resolver.xref_entries()
    }

    /// Return qpdf's raw cross-reference table, preserving signed
    /// object/generation identity before the valid `ObjectRef` boundary.
    pub(crate) fn get_raw_xref_table(&self) -> BTreeMap<QpdfObjGen, XrefEntry> {
        self.resolver.raw_xref_entries()
    }

    /// Return object references from qpdf's one canonical object cache without
    /// forcing resolution. This is the internal counterpart of the writer's
    /// cache-key walks; unlike the removed facade enumeration, it reads only
    /// `ResolverCore::object_cache`.
    pub(crate) fn canonical_object_refs(&self) -> Vec<ObjectRef> {
        self.canonical_object_ref_set(false).into_iter().collect()
    }

    /// Return source-backed or qpdf-shaped allocated object references from the
    /// canonical cache. Historical xref-stream cache entries and unresolved
    /// dangling references are intentionally omitted from this live view.
    pub(crate) fn canonical_live_object_refs(&self) -> Vec<ObjectRef> {
        self.canonical_object_ref_set(true).into_iter().collect()
    }

    fn canonical_object_ref_set(&self, live_only: bool) -> BTreeSet<ObjectRef> {
        let mut refs: BTreeSet<_> = self
            .resolver
            .xref_refs()
            .into_iter()
            .filter(|object_ref| object_ref.number != 0 && object_ref.generation != u16::MAX)
            .collect();
        refs.extend(
            self.resolver
                .all_object_handles()
                .into_iter()
                .filter_map(|handle| {
                    let object_ref = handle.object_ref()?;
                    if object_ref.number == 0 || object_ref.generation == u16::MAX {
                        return None;
                    }
                    if self.resolver.xref_entry(object_ref).is_none()
                        && handle.is_null()
                        && !self.resolver.is_allocated_object(object_ref)
                    {
                        // A canonical handle can resolve an absent reference to
                        // null without an xref/cache source. It is not an object
                        // in qpdf's cache enumeration unless it was allocated by
                        // the document itself.
                        return None;
                    }
                    if live_only
                        && self.resolver.xref_entry(object_ref).is_none()
                        && !self.resolver.is_allocated_object(object_ref)
                    {
                        // A historical xref stream and a dangling unresolved
                        // handle remain in qpdf's complete cache, but neither is
                        // an effective live source object. A reserved sentinel
                        // is document-allocated, so it stays visible here and
                        // reaches `QPDF_Reserved::unparse`'s error like qpdf's
                        // `getAllObjects()`-seeded preserve-unreferenced walk
                        // (`libqpdf/QPDFWriter.cc:2909-2915`).
                        return None;
                    }
                    Some(object_ref)
                })
                .filter(|object_ref| {
                    !live_only
                        || self.resolver.xref_entry(*object_ref).is_some()
                        || self.resolver.is_allocated_object(*object_ref)
                }),
        );
        refs
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

    #[cfg(test)]
    pub(crate) fn dangling_references_fixed(&self) -> bool {
        self.resolver.dangling_references_fixed()
    }

    /// Prepare the canonical object cache and return qpdf's greatest object
    /// number (`QPDF::getObjectCount`, `libqpdf/QPDF.cc:1271-1283`). This is
    /// intentionally separate from fresh-object allocation, which belongs to
    /// `flpdf-25kg.3.24`.
    pub(crate) fn get_object_count(&mut self) -> Result<u32> {
        self.resolver.get_object_count()
    }

    /// Collect compressible objects through qpdf's document-owned live walk.
    ///
    /// Ports `QPDF::getCompressibleObjGens` (`libqpdf/QPDF.cc:2393-2474`).
    /// The cache is prepared before sizing a packed visited bitmap, matching
    /// C++ `vector<bool>`. Stale generations are removed from the actual
    /// document; all retained aliases become direct null rather than relying
    /// on writer-side reference suppression. Stream `/Length` edges are
    /// omitted here; filtering stream parameters belongs to the writer.
    #[cfg(test)]
    pub(crate) fn get_compressible_objgens(&mut self) -> Result<Vec<ObjectRef>> {
        self.get_compressible_objgens_with_removed()
            .map(|(eligible, _)| eligible)
    }

    /// Collect qpdf's compressible objects together with the stale generations
    /// removed by the same live walk. The writer must carry this operation-
    /// specific removal set into any later preserve-unreferenced seed pass:
    /// `QPDF::removeObject` erases the resolver cache entry; the returned
    /// operation-specific removal set lets a writer keep its own traversal
    /// bookkeeping without adding a second document cache.
    pub(crate) fn get_compressible_objgens_with_removed(
        &mut self,
    ) -> Result<(Vec<ObjectRef>, BTreeSet<ObjectRef>)> {
        let encryption = self.trailer().try_get_key(b"/Encrypt")?.object_ref();
        let max_object = self.get_object_count()? as usize;
        let mut visited = vec![0u64; max_object.div_ceil(64)];
        let mut queue = Vec::with_capacity(512);
        queue.push(self.trailer());
        let mut result = Vec::new();
        let mut removed = BTreeSet::new();
        while let Some(object) = queue.pop() {
            if let Some(og) = object.object_ref().filter(|og| og.number > 0) {
                let index = (og.number - 1) as usize;
                if index >= max_object {
                    return Err(Error::Internal(
                        "unexpected object id encountered in getCompressibleObjGens".into(),
                    ));
                }
                let bit = 1u64 << (index % 64);
                if visited[index / 64] & bit != 0 {
                    continue;
                }
                if self.resolver.has_newer_cached_generation(og) {
                    removed.insert(og);
                    self.resolver.remove_object(og)?;
                    continue;
                }
                visited[index / 64] |= bit;
                if Some(og) != encryption {
                    object.try_dereference()?;
                    if object.as_stream_dict().is_none()
                        && !(object.try_is_dictionary_of_type(b"Sig", b"")?
                            && object.try_has_key(b"/ByteRange")?
                            && object.try_has_key(b"/Contents")?)
                    {
                        result.push(og);
                    }
                }
            }
            object.try_dereference()?;
            if let Some(dict) = object.as_stream_dict() {
                for key in dict.try_get_keys()?.into_iter().rev() {
                    let value = dict.try_get_key(&key)?;
                    if key != b"/Length" {
                        queue.push(value);
                    }
                }
            } else if object.try_is_dictionary()? {
                for key in object.try_get_keys()?.into_iter().rev() {
                    queue.push(object.try_get_key(&key)?);
                }
            } else if object.try_is_array()? {
                let count = object.try_get_array_n_items()?;
                for index in (0..count).rev() {
                    queue.push(object.try_get_array_item(index as i64)?);
                }
            }
        }
        Ok((result, removed))
    }

    /// Return the next qpdf-shaped generation-zero object identity from the
    /// prepared canonical cache. Allocation itself belongs to the document's
    /// resolver so non-canonical values cannot silently become object-number
    /// inputs (`libqpdf/QPDF.cc:1271-1283,1872-1880`).
    pub(crate) fn next_obj_gen(&self) -> Result<ObjectRef> {
        self.resolver.next_obj_gen()
    }

    /// Promote and register an existing initialized handle without cloning its
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
    /// qpdf records the shared value transition in the canonical object cache;
    /// the writer observes that same live value without a separate dirty bit.
    pub fn replace_object(
        &mut self,
        object_ref: ObjectRef,
        replacement: ObjectHandle,
    ) -> Result<ObjectHandle> {
        // Like `set_object`, this is canonical cache replacement only.
        self.resolver.replace_object(object_ref, replacement)
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
        self.resolver.swap_objects(first, second)
    }

    /// Remove a canonical object from the resolver's xref/cache view and
    /// leave outstanding handles as floating null values. The legacy snapshot
    /// metadata is maintained separately by the `Pdf` facade. This is qpdf
    /// `removeObject`'s exact xref/cache mutation (`QPDF.cc:1996-2005`),
    /// separate from xref registration's transient free-row state
    /// (`QPDF.cc:686-708`, `:1187-1210`).
    #[cfg(test)]
    pub(crate) fn remove_object_handle(&mut self, object_ref: ObjectRef) -> Result<()> {
        // qpdf's removeObject changes only the requested cache slot; already
        // resolved members of an ObjStm remain live in their own cache slots.
        self.resolver.remove_object(object_ref)?;
        Ok(())
    }

    pub(crate) fn is_canonical_object_handle(&self, handle: &ObjectHandle) -> bool {
        handle.qpdf_obj_gen().is_some_and(|object_gen| {
            self.resolver
                .registered_qpdf_obj_gen_handle(object_gen)
                .is_some_and(|canonical| canonical.is_same_object_as(handle))
        })
    }

    /// Register the existing object allocation under a fresh indirect identity.
    ///
    /// qpdf's `makeIndirectObject` rejects only uninitialized input, prepares
    /// the canonical cache through `getObjectCount`, and stores the same
    /// object before setting its new identity (`libqpdf/QPDF.cc:1872-1897`).
    /// Direct, indirect, and reserved inputs retain their aliases. Existing
    /// cache entries for a re-promoted object remain present; looking one up
    /// sets the shared value's active identity to that entry's key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Internal`] for an uninitialized handle, or propagates
    /// cache preparation and object-number exhaustion errors.
    pub fn make_indirect_object_handle(&mut self, handle: ObjectHandle) -> Result<ObjectHandle> {
        self.resolver.make_indirect_from_object_handle(handle)
    }

    /// Return an unused generation-zero object reference.
    ///
    /// Delegate allocation to qpdf's canonical object cache. The resolver
    /// prepares dangling references and selects the next generation-zero
    /// identity from the same map used by every other document operation.
    pub(crate) fn next_available_object_ref(&self) -> Result<ObjectRef> {
        self.resolver.next_obj_gen()
    }

    /// This document's stable per-instance identity.
    ///
    /// This is qpdf's `QPDF::getUniqueId` (`include/qpdf/QPDF.hh:283`,
    /// `libqpdf/QPDF.cc:2294-2296`): constructed once when the document is
    /// created and never changes for the life of this [`Pdf`]. Compare it
    /// against [`ObjectHandle::owning_pdf_unique_id`] to test whether a
    /// handle is still owned by this document.
    pub fn unique_id(&self) -> u64 {
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

    /// Validate that historical xref-stream handles collected during bootstrap
    /// are already present in the canonical resolver cache. qpdf reads each
    /// xref stream before merging the next xref section and keeps the same
    /// object cache for those historical objects (`QPDF.cc:626-710,1640-1716`);
    /// this handoff must not create a second graph or provenance map.
    pub(crate) fn install_parsed_xref_stream_handles(
        &mut self,
        parsed_xref_streams: BTreeMap<ObjectRef, ObjectHandle>,
    ) -> Result<()> {
        for (object_ref, _source) in parsed_xref_streams {
            if object_ref.number == 0
                || object_ref.generation == u16::MAX
                || self.resolver.xref_entry(object_ref).is_some()
            {
                continue;
            }
            let handle = self.get_object_handle(object_ref);
            if !handle.is_resolved() {
                // Not a qpdf-modelled state, so this stays an internal error
                // rather than a null resolution. `read_xrefStream` reads the
                // stream through `readObjectAtOffset(..., skip_cache_if_in_xref
                // = true)` (`QPDF.cc:956`), and that skip applies only while
                // the object still has an effective xref row (`QPDF.cc:1664`).
                // A superseded or freed row therefore takes the
                // `updateCache(og, oh.getObj(), ...)` branch (`QPDF.cc:1691`)
                // and caches the real /XRef stream, and `QPDF::resolve` returns
                // immediately for anything already resolved
                // (`QPDF.cc:1700-1704`). Installing a null here would put a
                // null where qpdf holds the parsed stream.
                return Err(Error::Internal(format!(
                    "canonical xref-stream handle {object_ref} was not resolved"
                )));
            }
        }
        Ok(())
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
        Ok(self
            .resolver
            .get_all_objects()?
            .into_iter()
            .filter(|handle| {
                handle.object_ref().is_none_or(|object_ref| {
                    object_ref.number != 0 && object_ref.generation != u16::MAX
                })
            })
            .collect())
    }

    /// Prepare the canonical object cache through qpdf's
    /// `QPDF::fixDanglingReferences` boundary.
    ///
    /// `QPDFWriter::prepareFileForWrite` calls this before it touches the
    /// Catalog, and the writer must be able to perform that preparation
    /// without taking the broader `getAllObjects` enumeration route. Keep the
    /// resolver's idempotent fixed-state guard as the single owner of the
    /// operation (`libqpdf/QPDF.cc:1259-1269`).
    pub(crate) fn fix_dangling_references(&self) -> Result<()> {
        self.resolver.fix_dangling_references()
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

    /// Read a linearization hint object and retain qpdf's source position for
    /// a following `damagedPDF` warning. The resolver distinguishes an already
    /// cached object from a newly parsed one, matching qpdf's
    /// `readObjectAtOffset`/`InputSource::getLastOffset` behavior.
    /// Read one object at an offset with qpdf's caller-provided description.
    /// The description is part of the source warning context, as with
    /// `QPDF::readObjectAtOffset` used by `QPDF::readHintStream`
    /// (`libqpdf/QPDF_linearization.cc:241-245`).
    pub(crate) fn resolve_at_offset_with_description(
        &self,
        offset: u64,
        expected: ObjectRef,
        description: impl AsRef<[u8]>,
    ) -> Result<(ObjectHandle, Option<u64>)> {
        self.resolver
            .resolve_at_offset_with_description(offset, expected, description)
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
}

#[cfg(test)]
mod warning_api_tests {
    use super::Pdf;

    #[test]
    fn warning_api_drains_without_replaying_and_retains_suppressed_warnings() {
        let mut pdf =
            Pdf::open_mem_owned(crate::engine::EMPTY_PDF_BYTES.to_vec()).expect("empty PDF opens");
        pdf.set_suppress_warnings(true);

        pdf.push_warning("first warning").unwrap();
        assert!(pdf.any_warnings());
        assert_eq!(pdf.num_warnings(), 1);

        let first = pdf.get_warnings();
        assert_eq!(first.len(), 1);
        assert!(!pdf.any_warnings());
        assert_eq!(pdf.num_warnings(), 0);
        assert!(pdf.get_warnings().is_empty());

        pdf.push_warning("second warning").unwrap();
        let second = pdf.get_warnings();
        assert_eq!(second.len(), 1);
        assert_eq!(second.entries()[0].get_message_detail(), b"second warning");
        assert!(!pdf.any_warnings());
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
    fn overlength_raw_hex_key_reports_the_authenticated_key_length() {
        let fixture = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/encrypted/v5-aes-256-r6.pdf"),
        )
        .expect("R6 fixture");
        let pdf = Pdf::open_with_options(
            Cursor::new(fixture),
            PdfOpenOptions {
                password: b"abababababababababababababababababababababababababababababababababababababababab"
                    .to_vec(),
                password_is_hex_key: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("qpdf accepts an overlength raw key for this inspection fixture");

        assert_eq!(pdf.encryption_length_bits(), Some(320));
    }

    #[test]
    fn aes_object_key_follows_the_qpdf_provider_length_dispatch() {
        // 24 bytes selects AES-192 in qpdf's providers; every other length
        // that is not 16 or 32 takes the AES-128 prefix
        // (`QPDFCrypto_gnutls.cc:197-213`).
        assert_eq!(
            crate::encryption::state::aes192_object_key(&[0xa5; 24])
                .expect("24-byte keys use qpdf's AES-192 provider dispatch"),
            [0xa5; 24]
        );
        let error = crate::encryption::state::aes128_object_key(&[0; 8])
            .expect_err("qpdf reads past a shorter key buffer; this port rejects it");
        assert!(error.to_string().contains("not 16 bytes"));

        assert_eq!(
            crate::encryption::state::aes128_object_key(&[0xa5; 20])
                .expect("a 20-byte key uses the qpdf AES-128 provider fallback"),
            [0xa5; 16]
        );
        assert_eq!(
            crate::encryption::state::aes128_object_key(&[0xa5; 40])
                .expect("overlength keys use the qpdf AES-128 provider fallback"),
            [0xa5; 16]
        );
    }

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

#[cfg(test)]
mod compressible_owner_tests {
    use super::*;
    use crate::reader::resolver::ResolverHandle;
    use std::cell::Cell;
    use std::io::Cursor;

    fn pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap()
    }

    #[test]
    fn canonical_ref_views_ignore_non_indirect_object_generations() {
        let mut pdf = pdf();
        let zero = pdf.get_object_handle(ObjectRef::new(0, 0));
        let max_generation = pdf.get_object_handle(ObjectRef::new(1, u16::MAX));

        assert!(!pdf
            .canonical_object_refs()
            .contains(&zero.object_ref().unwrap()));
        assert!(!pdf
            .canonical_object_refs()
            .contains(&max_generation.object_ref().unwrap()));
        assert!(!pdf
            .canonical_live_object_refs()
            .contains(&zero.object_ref().unwrap()));
        assert!(!pdf
            .canonical_live_object_refs()
            .contains(&max_generation.object_ref().unwrap()));
    }

    #[test]
    fn parsed_xref_stream_handoff_rejects_an_unresolved_canonical_slot() {
        let mut pdf = pdf();
        let object_ref = ObjectRef::new(99, 0);
        // qpdf never observes this state. `read_xrefStream` reads the stream
        // through `readObjectAtOffset(..., skip_cache_if_in_xref = true)`
        // (`QPDF.cc:956`), and that skip only applies when the object still has
        // an effective xref row (`QPDF.cc:1664`); an absent or superseded row
        // therefore takes the `updateCache(og, oh.getObj(), ...)` branch
        // (`QPDF.cc:1691`) and caches the real /XRef stream. `QPDF::resolve`
        // returns immediately for anything already resolved
        // (`QPDF.cc:1700-1704`), so its absent-entry null path is unreachable
        // for a parsed xref stream. An unresolved canonical slot here means the
        // loader built a second object graph, which is a flpdf invariant
        // violation rather than a qpdf-modelled document state.
        let source = ObjectHandle::new_indirect_unresolved(object_ref, -1);
        let error = pdf
            .install_parsed_xref_stream_handles(BTreeMap::from([(object_ref, source)]))
            .expect_err("canonical xref-stream provenance must already be resolved");

        assert!(matches!(error, Error::Internal(message) if message.contains("99 0")));
    }

    #[test]
    fn compressible_walk_removes_stale_aliases_without_invalidating_preparation() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(
                "../../../tests/fixtures/compat/compressible-stale-generation-alias.pdf"
            )
            .to_vec(),
        ))
        .unwrap();
        let versions = pdf
            .root_handle()
            .unwrap()
            .try_get_key(b"/Versions")
            .unwrap();
        let old = versions.try_get_array_item(0).unwrap();
        let current = versions.try_get_array_item(1).unwrap();
        let result = pdf.get_compressible_objgens().unwrap();
        assert_eq!(
            result,
            vec![
                ObjectRef::new(1, 0),
                ObjectRef::new(2, 0),
                ObjectRef::new(3, 1)
            ]
        );
        assert!(old.is_direct() && old.is_null());
        assert_eq!(current.object_ref(), Some(ObjectRef::new(3, 1)));
        assert!(pdf
            .resolver
            .registered_handle(ObjectRef::new(3, 0))
            .is_none());
        assert!(pdf.dangling_references_fixed());
    }

    #[test]
    fn compressible_removal_erases_the_canonical_slot_before_replacement() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        ))
        .unwrap();
        let old = pdf
            .get_all_objects()
            .unwrap()
            .into_iter()
            .find(|object| object.as_stream_dict().is_some())
            .unwrap();
        let old_ref = old.object_ref().unwrap();
        pdf.replace_object(
            ObjectRef::new(old_ref.number, old_ref.generation + 1),
            ObjectHandle::integer(42),
        )
        .unwrap();

        let (_, removed) = pdf
            .get_compressible_objgens_with_removed()
            .expect("compressible walk succeeds");
        assert!(removed.contains(&old_ref));
        assert!(!pdf.canonical_object_refs().contains(&old_ref));

        pdf.replace_object(old_ref, ObjectHandle::integer(99))
            .expect("qpdf-style replacement re-registers the removed generation");
        assert!(pdf.canonical_live_object_refs().contains(&old_ref));
    }

    #[test]
    fn compressible_walk_checks_visited_before_stale_generation_removal() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(
                "../../../tests/fixtures/compat/compressible-stale-generation-alias.pdf"
            )
            .to_vec(),
        ))
        .unwrap();
        let root = pdf.root_handle().unwrap();
        let versions = root.try_get_key(b"/Versions").unwrap();
        let old = versions.try_get_array_item(0).unwrap();
        let current = versions.try_get_array_item(1).unwrap();
        versions
            .set_array_items(vec![current, old.clone()])
            .unwrap();
        root.replace_key(b"/ZCycle", root.clone()).unwrap();
        assert_eq!(
            pdf.get_compressible_objgens().unwrap(),
            vec![
                ObjectRef::new(1, 0),
                ObjectRef::new(2, 0),
                ObjectRef::new(3, 1)
            ]
        );
        assert_eq!(old.object_ref(), Some(ObjectRef::new(3, 0)));
        assert!(old.is_indirect());
    }

    #[test]
    fn compressible_walk_preserves_order_and_keeps_excluded_container_children() {
        let mut pdf = pdf();
        let root = pdf.root_handle().unwrap();
        for key in root.try_get_keys().unwrap() {
            root.remove_key(&key);
        }
        let a = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(1))
            .unwrap();
        let b = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(2))
            .unwrap();
        let c = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(3))
            .unwrap();
        let null = pdf
            .make_indirect_from_object_handle(ObjectHandle::null())
            .unwrap();
        let length = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(3))
            .unwrap();
        let sig = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
                (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
                (
                    b"/Contents".to_vec(),
                    ObjectHandle::string(b"signature".to_vec()),
                ),
                (b"/Child".to_vec(), a.clone()),
            ]))
            .unwrap();
        let incomplete_sig = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
                (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
                (b"/Contents".to_vec(), ObjectHandle::null()),
            ]))
            .unwrap();
        let encryption = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                b"/Child".to_vec(),
                b.clone(),
            )]))
            .unwrap();
        let stream = pdf.new_stream_with_data(Rc::new(b"xyz".to_vec())).unwrap();
        let calls = Rc::new(Cell::new(0));
        let provider_calls = calls.clone();
        stream
            .replace_stream_data_with_callback(
                move |pipeline| {
                    provider_calls.set(provider_calls.get() + 1);
                    pipeline.write(b"xyz")?;
                    pipeline.finish()?;
                    Ok(())
                },
                None,
                None,
            )
            .unwrap();
        let dict = stream.as_stream_dict().unwrap();
        dict.replace_key(b"/Type", ObjectHandle::name(b"ObjStm".to_vec()))
            .unwrap();
        dict.replace_key(b"/Length", length.clone()).unwrap();
        dict.replace_key(b"/Meta", c.clone()).unwrap();
        root.replace_key(
            b"/AArray",
            ObjectHandle::array(vec![a.clone(), null.clone(), a.clone()]),
        )
        .unwrap();
        root.replace_key(b"/BSig", sig.clone()).unwrap();
        root.replace_key(b"/CIncomplete", incomplete_sig.clone())
            .unwrap();
        root.replace_key(b"/DNull", null.clone()).unwrap();
        root.replace_key(b"/EStream", stream.clone()).unwrap();
        root.replace_key(b"/ZLengthElsewhere", length.clone())
            .unwrap();
        pdf.trailer()
            .replace_key(b"/Encrypt", encryption.clone())
            .unwrap();
        let expected: Vec<ObjectRef> =
            [b, root.clone(), a, null, incomplete_sig, c, length.clone()]
                .iter()
                .map(|object| object.object_ref().unwrap())
                .collect();
        assert_eq!(pdf.get_compressible_objgens().unwrap(), expected);
        assert_eq!(
            calls.get(),
            0,
            "document traversal must not pipe stream providers"
        );
        root.remove_key(b"/ZLengthElsewhere");
        let without_length = pdf.get_compressible_objgens().unwrap();
        assert!(!without_length.contains(&length.object_ref().unwrap()));
        assert!(!without_length.contains(&sig.object_ref().unwrap()));
        assert!(!without_length.contains(&encryption.object_ref().unwrap()));
        assert!(!without_length.contains(&stream.object_ref().unwrap()));
        assert_eq!(calls.get(), 0);
        assert_eq!(stream.get_raw_stream_data().unwrap().as_slice(), b"xyz");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn compressible_walk_reports_qpdfs_prepared_object_count_bound() {
        struct GrowOnDrop {
            resolver: std::rc::Weak<ResolverHandle<Cursor<Vec<u8>>>>,
            pending: ObjectHandle,
        }
        impl crate::StreamDataProvider for GrowOnDrop {}
        impl Drop for GrowOnDrop {
            fn drop(&mut self) {
                let resolver = self.resolver.upgrade().unwrap();
                let new_object = resolver
                    .make_indirect_from_object_handle(ObjectHandle::integer(7))
                    .unwrap();
                self.pending.append_array_item(new_object).unwrap();
            }
        }
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        ))
        .unwrap();
        let old = pdf
            .get_all_objects()
            .unwrap()
            .into_iter()
            .find(|object| object.as_stream_dict().is_some())
            .unwrap();
        let old_id = old.object_ref().unwrap();
        pdf.replace_object(
            ObjectRef::new(old_id.number, old_id.generation + 1),
            ObjectHandle::integer(42),
        )
        .unwrap();
        let pending = ObjectHandle::array(vec![]);
        pdf.root_handle()
            .unwrap()
            .replace_key(b"/ZPending", pending.clone())
            .unwrap();
        old.replace_stream_data_provider(
            Rc::new(GrowOnDrop {
                resolver: Rc::downgrade(&pdf.resolver),
                pending,
            }),
            None,
            None,
        )
        .unwrap();
        assert!(
            matches!(pdf.get_compressible_objgens(), Err(Error::Internal(message)) if message == "unexpected object id encountered in getCompressibleObjGens")
        );
    }
}
