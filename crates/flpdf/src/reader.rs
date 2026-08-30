//! qpdf correspondence: QPDF.cc object resolution, recovery, diagnostics, and authentication responsibilities.
pub(crate) mod file_object;
pub(crate) mod resolver;

use self::file_object::{parse_file_object_syntax, PendingBody, PendingFileObject};
use crate::cache::CacheEntry;
use crate::encryption::password::{password_candidates_for_read, PasswordMode};
use crate::encryption::permissions::Permissions;
use crate::encryption::standard::ObjectKeyAlg;
use crate::encryption::state::{EncryptionInfo, EncryptionInspectionState};
#[cfg(test)]
use crate::encryption::state::{EncryptionMode, EncryptionState};
use crate::encryption::CopyEncryptionSource;
use crate::error::EncryptedError;
#[cfg(test)]
use crate::object::collect_qpdf_object_references;
use crate::object_handle::{ObjectValue, NO_PARSED_OFFSET};
#[cfg(feature = "qtest-driver")]
use crate::parser::array_item_source_offset;
#[cfg(feature = "qtest-driver")]
use crate::parser::dictionary_value_source_offset;
#[cfg(test)]
use crate::parser::parse_qpdf_file_object;
#[cfg(any(test, feature = "qtest-driver"))]
use crate::tokenizer::Tokenizer;
use crate::{
    Diagnostics, Dictionary, Error, Object, ObjectHandle, ObjectRef, Result, Stream, XrefEntry,
    XrefForm,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::rc::Rc;

use crate::pdf::{CompressedMemberProvenance, Pdf};

#[cfg(test)]
pub(crate) struct QpdfPreparedObjects {
    pub(crate) refs: Vec<ObjectRef>,
    pub(crate) max_object_id: u32,
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
    pub description: String,
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
            description: String::new(),
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

// `QPDF::replaceObject` accepts any initialized direct object and does not
// impose the parser's input-depth bound on programmatically constructed
// replacements (`libqpdf/QPDF.cc:1980-1993`; `include/qpdf/QPDF.hh:380-388`).
// Keep the stack-growth guard in the lift walk, but do not reject a valid
// caller-owned Object tree merely because it is deeper than parsed input.
const PROGRAMMATIC_LIFT_MAX_DEPTH: usize = usize::MAX;

fn encryption_info_from_inspection(inspection: &EncryptionInspectionState) -> EncryptionInfo {
    let mut named_crypt_filters = inspection.named_crypt_filters.clone();
    named_crypt_filters.sort();
    EncryptionInfo {
        v: inspection.v,
        r: inspection.r,
        length_bits: inspection.length_bits,
        filter: inspection.filter.clone(),
        permissions: inspection.permissions,
        encrypt_metadata: inspection.encrypt_metadata,
        user_password: crate::encryption::standard::trim_user_password(&inspection.user_password),
        user_password_matched: inspection.user_password_matched,
        owner_password_matched: inspection.owner_password_matched,
        stream_method: inspection.stream_method,
        string_method: inspection.string_method,
        eff_method: inspection.eff_method,
        named_crypt_filters,
    }
}

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
        let encrypt_dict = self.encrypt_dictionary()?.ok_or_else(|| {
            Error::Unsupported("authenticated input has no /Encrypt dictionary".into())
        })?;
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

    /// Read-only snapshot of the `/Encrypt` parameters for the
    /// `show-encryption` inspection route. The snapshot is available after
    /// successful authentication and through the dedicated inspection-open
    /// path after `BadPassword`.
    ///
    /// Returns `None` for plaintext PDFs. Parsed fields come from the
    /// qpdf-shaped inspection state; authenticated fields are filled in when
    /// authentication succeeds. This does NOT re-run or alter authentication
    /// (layer-2 owns that ordering).
    ///
    /// # Errors
    ///
    /// - [`Error::Encrypted`] ([`EncryptedError::Malformed`]) when the re-read
    ///   `/Encrypt` dictionary is missing or has the wrong type for `/Filter`.
    ///   Returns `Ok(None)` for a plaintext document rather than an error.
    /// - [`Error::Io`] / [`Error::Parse`] when the `/Encrypt` entry is an indirect
    ///   reference whose resolution fails.
    pub fn encryption_info(&mut self) -> Result<Option<EncryptionInfo>> {
        if let Some(inspection) = self.encryption_inspection.borrow().as_ref().cloned() {
            return Ok(Some(encryption_info_from_inspection(&inspection)));
        }
        if self.encryption.borrow().is_none() {
            return Ok(None);
        }
        let Some(encrypt) = self.encrypt_dictionary()? else {
            return Ok(None);
        };
        // qpdf: "After we initialize encryption parameters, we must use stored
        // key information and never look at /Encrypt again"
        // (`libqpdf/QPDF_encryption.cc:727-729`).
        let (v, r) = {
            let guard = self.encryption.borrow();
            let encryption = guard
                .as_ref()
                .expect("checked is_some above; authenticate_if_encrypted set it");
            (encryption.encryption_v, encryption.encryption_r)
        };
        let filter = crate::encryption::state::required_name(&encrypt, "Filter")?.to_string();
        // /Length is in bits and absent for V<5 (defaulting to 40 per the
        // Standard handler); V=5 always uses a 256-bit key.
        let length_bits = match encrypt.get("Length") {
            Some(Object::Integer(value)) => *value,
            _ if v >= 5 => 256,
            _ => 40,
        };

        let encryption_guard = self.encryption.borrow();
        let encryption = encryption_guard
            .as_ref()
            .expect("checked is_some above; authenticate_if_encrypted set it");
        let permissions = encryption.permissions;
        let encrypt_metadata = encryption.encrypt_metadata;
        let user_password =
            crate::encryption::standard::trim_user_password(&encryption.user_password);
        // qpdf reports the stored crypt filter methods rather than re-reading
        // `/Encrypt`: "After we initialize encryption parameters, we must use
        // stored key information and never look at /Encrypt again"
        // (`libqpdf/QPDF_encryption.cc:727-729`).
        let stream_method = encryption.cf_stream.qpdf_name();
        let string_method = encryption.cf_string.qpdf_name();
        let eff_method = encryption.cf_file.qpdf_name();
        let named_crypt_filters = encryption
            .crypt_filters
            .iter()
            .map(|(name, mode)| (String::from_utf8_lossy(name).into_owned(), mode.qpdf_name()))
            .collect();

        Ok(Some(EncryptionInfo {
            v,
            r,
            length_bits,
            filter,
            permissions,
            encrypt_metadata,
            user_password,
            user_password_matched: encryption.user_password_matched,
            owner_password_matched: encryption.owner_password_matched,
            stream_method,
            string_method,
            eff_method,
            named_crypt_filters,
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
            // changed result is an flpdf-only observation of structural
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
        if self.encrypt_dictionary()?.is_none() {
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
        let Some(encrypt) = self.encrypt_dictionary()? else {
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
        let Some(encrypt) = self.encrypt_dictionary()? else {
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
        if let Some(inspection) = self.encryption_inspection.borrow_mut().as_mut() {
            inspection.user_password = state.user_password.clone();
            inspection.user_password_matched = state.user_password_matched;
            inspection.owner_password_matched = state.owner_password_matched;
        }
        *self.encryption.borrow_mut() = Some(state);
        if let Some(warning) = authenticated.perms_warning {
            self.push_warning(warning)?;
        }
        Ok(())
    }

    fn encrypt_dictionary(&mut self) -> Result<Option<Dictionary>> {
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
        let dict = encrypt
            .materialize()?
            .into_dict()
            .expect("try_as_dictionary confirmed that /Encrypt is a dictionary");
        Ok(Some(dict))
    }

    /// Lift an already-materialized legacy [`Object`] value into a canonical
    /// [`ObjectHandle`] — the reverse of [`ObjectHandle::materialize`], for a
    /// caller that holds an `Object` with no live handle of its own (e.g. a
    /// value copied out into a persisted, `Pdf`-detached structure) but does
    /// have `&mut Pdf` back in scope. A reference position becomes the same
    /// canonical registry-backed handle [`Pdf::get_object_handle`] would
    /// return for that ref (see [`Self::lift_to_handle_bounded`]), so identity
    /// is preserved with every other handle for the same object.
    ///
    /// Bounded at `parser::MAX_PARSE_DEPTH`, not `lift`'s default
    /// `MAX_INLINE_DEPTH`: `object` already parsed successfully at the looser
    /// bound (it came from an already-parsed source object),
    /// so re-lifting through the tighter structural-walk bound would reject a
    /// value that parse already accepted — the same reasoning
    /// [`Self::trailer_key_handle`] documents for the analogous trailer-entry
    /// case.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for [`Object::Operator`]/
    /// [`Object::InlineImage`] (content-stream-only tokens with no
    /// `ObjectValue` representation) or for nesting beyond the depth bound —
    /// see [`Self::lift_bounded`]. Neither case arises for a value that
    /// already came from ordinary indirect-object resolution.
    pub(crate) fn lift_object_to_handle(&mut self, object: &Object) -> Result<ObjectHandle> {
        self.lift_to_handle_bounded(object, 0, crate::parser::MAX_PARSE_DEPTH)
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
        let PendingBody::Stream { data_start, .. } = pending.body else {
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
            Self::decode_parms_value_offset_within(bytes, filter_index)
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

    #[cfg(feature = "qtest-driver")]
    fn decode_parms_value_offset_within(
        bytes: &[u8],
        filter_index: usize,
    ) -> Result<Option<usize>> {
        let body_start = Self::object_body_start_within(bytes)?;
        let body = &bytes[body_start..];
        let value_offset = dictionary_value_source_offset(body, b"DecodeParms", filter_index)?;
        Ok(value_offset.map(|value_offset| body_start + value_offset))
    }

    fn parse_source_file_object_at(&mut self, offset: u64) -> Result<PendingFileObject> {
        let next = self.next_object_offset(offset);
        let bytes = self.resolver.read_window(offset, next)?;

        match parse_file_object_syntax(&bytes) {
            Ok(pending) => Ok(pending),
            Err(window_error) if next.is_some() && self.resolution_fallbacks_remaining > 0 => {
                self.resolution_fallbacks_remaining -= 1;
                let full = self.resolver.read_window(offset, None)?;
                parse_file_object_syntax(&full).or(Err(window_error))
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn compressed_parent(&self, object_ref: ObjectRef) -> Option<(ObjectRef, u32)> {
        self.compressed_member_parents
            .get(&object_ref)
            .map(|provenance| (provenance.parent_ref, provenance.parent_index))
    }

    /// Return the current source xref entry for `object_ref` without cloning
    /// the complete xref table.
    pub(crate) fn source_xref_entry(&self, object_ref: ObjectRef) -> Option<XrefEntry> {
        self.resolver.xref_entry(object_ref)
    }

    /// Replace `object_ref` with `object` in the in-memory object cache.
    ///
    /// The original on-disk bytes are not touched; [`crate::PdfWriter`] will
    /// see the updated value when it walks the cache and emit it in the fresh
    /// output. Subsequent canonical handle lookups for `object_ref` observe
    /// `object`
    /// immediately.
    pub fn set_object(&mut self, object_ref: ObjectRef, object: Object) {
        // This is the canonical cache replacement boundary, not xref
        // registration. Normal `read_xref` keeps its transient free-row
        // `deleted_objects` through `/Size` then clears it (`QPDF.cc:686-708`),
        // while `reconstruct_xref` clears its line-scan filter before any
        // candidate re-read (`:516-575`, especially `:575`; `:576-607`). This
        // method must neither clear nor extend either registration. Canonical
        // xref/cache removal is separately `removeObject` (`:1996-2005`).
        // Refresh the legacy cache before replacing the canonical value, or
        // an old object-stream entry can incorrectly retain provenance.
        self.synchronize_cache_with_resolver_xref();
        // qpdf's replaceObject changes only the requested cache slot; already
        // resolved members of an ObjStm remain live in their own cache slots.
        // Promote those compatibility-cache values before the replacement
        // overwrites the source container, for the same reason delete_object
        // preserves them below.
        self.promote_resolved_object_stream_members(object_ref)
            .expect("a parsed ObjStm member must be representable as an ObjectHandle");
        self.qpdf_removed_refs.remove(&object_ref);
        self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        if let Some(CacheEntry::Compressed { stream, index }) =
            self.cache.entry(object_ref).cloned()
        {
            self.record_compressed_member_provenance(object_ref, stream, index);
        }

        // Write through to the canonical handle graph too, preserving the
        // single live value that every qpdf-shaped consumer observes.
        let handle = self.get_object_handle(object_ref);
        let value = self
            .lift_for_set_object(&object, &handle)
            .expect("programmatic Object values must be representable as ObjectHandle values");
        handle.set_resolved(value);
        // A caller-supplied replacement no longer describes the source bytes
        // that populated this handle. Clear source provenance after the
        // replacement has been lifted and installed.
        handle.clear_description();
        handle.reset_parsed_offset();
        handle.set_end_offsets(NO_PARSED_OFFSET, NO_PARSED_OFFSET);

        self.cache.set_resolved(object_ref, object);
        self.handle_mutated_object_refs.remove(&object_ref);
        self.dirty_object_refs.insert(object_ref);
    }

    // Convert `object` for `Pdf::set_object`'s handle-graph write-through.
    // This is intentionally broader than the ordinary legacy bounded lift:
    // qpdf's `replaceObject` accepts the already-constructed operator and
    // inline-image values (`QPDFObjectHandle.cc:1933-1941`) and does not apply
    // a parser-depth policy to programmatic values. Keep the canonical
    // replacement on the handle graph for the entire caller-owned Object
    // tree; the lift itself remains stack-safe through `stacker::maybe_grow`.
    //
    // Identical to the ordinary bounded lift except when `object` is a stream and
    // `existing_handle`'s current (pre-overwrite) value is also a stream:
    // the new dictionary is written into the existing stream's own
    // dictionary handle in place (`ObjectHandle::replace_direct_value`),
    // preserving its already-recorded parsed offset and shared identity,
    // rather than minting a fresh dictionary handle that would start at the
    // no-offset sentinel (`stream_dictionary_parsed_offset_survives_resolve_set_object_round_trip`
    // is the regression tripwire for getting this wrong). The handle value
    // supports the complete programmatic ObjectValue set, including operator
    // and inline-image leaves used by content-oriented tests.
    fn lift_for_set_object(
        &mut self,
        object: &Object,
        existing_handle: &ObjectHandle,
    ) -> Result<ObjectValue> {
        if let Object::Stream(stream) = object {
            if let Some(existing_dict) = existing_handle.as_stream_dict() {
                return self.lift_stream_with_existing_dictionary(
                    stream,
                    existing_dict,
                    PROGRAMMATIC_LIFT_MAX_DEPTH,
                    true,
                );
            }
            let stream_dict = self.lift_dictionary_bounded_with_options(
                &stream.dict,
                0,
                PROGRAMMATIC_LIFT_MAX_DEPTH,
                true,
            )?;
            return Ok(ObjectValue::Stream {
                stream_dict: self
                    .resolver
                    .direct_object_handle(ObjectValue::Dictionary(stream_dict)),
                stream_data: Some(Rc::new(stream.data.clone())),
                stream_length: 0,
                stream_provider: None,
            });
        }
        self.lift_bounded_with_content_tokens(object, 0, PROGRAMMATIC_LIFT_MAX_DEPTH)
    }

    fn lift_stream_with_existing_dictionary(
        &mut self,
        stream: &Stream,
        existing_dict: ObjectHandle,
        max_depth: usize,
        allow_content_tokens: bool,
    ) -> Result<ObjectValue> {
        let dict_value = ObjectValue::Dictionary(self.lift_dictionary_bounded_with_options(
            &stream.dict,
            0,
            max_depth,
            allow_content_tokens,
        )?);
        existing_dict.replace_direct_value(dict_value);
        existing_dict.clear_description();
        Ok(ObjectValue::Stream {
            stream_dict: existing_dict,
            stream_data: Some(Rc::new(stream.data.clone())),
            stream_length: 0,
            stream_provider: None,
        })
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
            Some(CacheEntry::Resolved(Object::Stream(_)))
        ) {
            // A compressed member can only have been materialized after its
            // source stream was loaded into the legacy cache. Avoid scanning
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
                Some(CacheEntry::Resolved(object)) => Some((None, Some(object.clone()))),
                Some(CacheEntry::Missing | CacheEntry::Deleted)
                | Some(CacheEntry::Unresolved { .. } | CacheEntry::Reserved)
                | None => None,
            };
            let Some((compressed_entry, legacy_object)) = cache_state else {
                continue;
            };

            // Prefer the canonical value when the planner has already
            // materialized the member. This preserves direct ObjectHandle
            // mutations even if the legacy cache still contains an older
            // snapshot from before the handle route was introduced.
            let canonical_object = self
                .resolver
                .registered_handle(member_ref)
                .filter(ObjectHandle::is_resolved)
                .map(|handle| handle.materialize())
                .transpose()?;
            let Some(object) = canonical_object.or(legacy_object) else {
                continue;
            };
            members.push((member_ref, compressed_entry, object));
        }

        for (member_ref, compressed_entry, object) in members {
            let handle = self
                .resolver
                .registered_handle(member_ref)
                .unwrap_or_else(|| self.get_object_handle(member_ref));
            if !handle.is_resolved() {
                let value = self.lift_for_set_object(&object, &handle)?;
                handle.set_resolved(value);
            }
            if let Some((stream, index)) = compressed_entry {
                self.record_compressed_member_provenance(member_ref, stream, index);
            }
            self.cache.set_resolved(member_ref, object);
        }
        Ok(())
    }

    fn record_compressed_member_provenance(
        &mut self,
        object_ref: ObjectRef,
        source_stream: u32,
        source_index: u32,
    ) {
        let stream_ref = ObjectRef::new(source_stream, 0);
        self.compressed_member_parents.insert(
            object_ref,
            CompressedMemberProvenance {
                parent_ref: stream_ref,
                parent_index: source_index,
                source_stream,
                source_index,
            },
        );
    }

    /// Remove `object_ref`, marking it deleted.
    ///
    /// Subsequent canonical handle lookups for `object_ref` observe
    /// [`Object::Null`], matching the behavior for any
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
    /// mutated via [`Self::set_object`] or [`Self::delete_object`] since the
    /// Pdf was opened). Used by the full-rewrite writer to detect whether a
    /// pre-existing dirty flag existed before an output-only Catalog mutation
    /// so the flag can be preserved through a restore.
    pub(crate) fn is_dirty(&self, object_ref: ObjectRef) -> bool {
        self.dirty_object_refs.contains(&object_ref)
    }

    /// Remove `object_ref` from the dirty set without touching the cache
    /// value. Used by the full-rewrite writer to undo a spurious dirty flag
    /// after restoring the pre-write Catalog snapshot: `Self::set_object`
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
    ///   the canonical handle returns as `Object::Null` (no real indirect
    ///   object behind them).
    ///
    /// A `live_object_refs()` entry may still resolve to `Object::Null`; that
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

    /// Resolve every live xref/cache object and register valid indirect
    /// references whose exact generation has no live target. This mirrors the
    /// object-cache preparation performed by qpdf's `fixDanglingReferences()`
    /// for JSON metadata without exposing placeholders through the public
    /// object enumeration APIs.
    #[cfg(test)]
    pub(crate) fn prepare_qpdf_json_objects(&mut self) -> Result<QpdfPreparedObjects> {
        let live_snapshot = self.qpdf_json_live_object_refs();
        let mut discovered = self.qpdf_trailer_references.clone();
        discovered.extend(self.qpdf_parsed_xref_stream_refs.iter().copied());

        for object_ref in live_snapshot {
            let object = self.resolve_qpdf_json_object(object_ref)?;
            collect_qpdf_object_references(&object, &mut discovered);
        }

        for object_ref in discovered {
            if object_ref.number == 0
                || object_ref.generation == u16::MAX
                || self.qpdf_removed_refs.contains(&object_ref)
            {
                continue;
            }
            let has_cached_target = matches!(
                self.cache.entry(object_ref),
                Some(
                    CacheEntry::Unresolved { .. }
                        | CacheEntry::Compressed { .. }
                        | CacheEntry::Resolved(_)
                )
            );
            let has_handle_only_target = self.cache.entry(object_ref).is_none()
                && self
                    .resolver
                    .registered_handle(object_ref)
                    .is_some_and(|handle| handle.is_resolved());
            if !(has_cached_target || has_handle_only_target) {
                self.qpdf_dangling_refs.insert(object_ref);
                if self.cache.entry(object_ref).is_none() {
                    self.cache.set_missing(object_ref);
                }
            }
        }

        let mut refs = self.qpdf_json_live_object_refs();
        refs.extend(self.qpdf_parsed_xref_stream_refs.iter().copied());
        refs.extend(self.qpdf_dangling_refs.iter().copied());
        refs.retain(|object_ref| !self.qpdf_removed_refs.contains(object_ref));
        refs.sort_unstable();
        refs.dedup();
        let max_object_id = refs
            .iter()
            .map(|object_ref| object_ref.number)
            .max()
            .unwrap_or(0);

        Ok(QpdfPreparedObjects {
            refs,
            max_object_id,
        })
    }

    /// qpdf's `obj_cache` owns both parsed objects and objects created with
    /// `makeIndirectObject`. During the ObjectHandle cutover, the latter live
    /// solely in the canonical handle registry: including resolved cache-miss
    /// handles here preserves that visibility without cloning their stream
    /// payloads into the legacy `Object` cache.
    #[cfg(test)]
    fn qpdf_json_live_object_refs(&self) -> Vec<ObjectRef> {
        let mut refs = self.live_object_refs();
        refs.extend(
            self.resolver
                .resolved_object_refs()
                .into_iter()
                .filter(|object_ref| self.cache.entry(*object_ref).is_none()),
        );
        refs.sort_unstable();
        refs.dedup();
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
    #[allow(dead_code)]
    pub(crate) fn reconstructed_xref(&self) -> bool {
        self.resolver.reconstructed_xref()
    }

    /// Prepare the canonical object cache and return qpdf's greatest object
    /// number (`QPDF::getObjectCount`, `libqpdf/QPDF.cc:1271-1283`). This is
    /// intentionally separate from fresh-object allocation, which belongs to
    /// `flpdf-25kg.3.24`.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub(crate) fn next_obj_gen(&self) -> Result<ObjectRef> {
        self.resolver.next_obj_gen()
    }

    /// Promote and register an existing direct handle without cloning its
    /// allocation or scheduling writer output. This is qpdf's
    /// `makeIndirectFromQPDFObject` (`libqpdf/QPDF.cc:1882-1888`), exposed as
    /// a document-owned canonical consumer boundary so callers do not fall
    /// back to the legacy clone-based allocator.
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
    /// The existing canonical promotion primitive registers this exact
    /// `ObjectHandle` allocation; this method does not use the legacy
    /// cloning allocator or an empty replacement buffer.
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
    /// raw [`Object`] materialization and writer traversal remain outside this
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
        // Refresh the legacy cache before replacing the canonical value, or
        // an old object-stream entry can incorrectly retain provenance
        // (mirrors `set_object`'s identical precondition above).
        self.synchronize_cache_with_resolver_xref();
        // qpdf's replaceObject changes only the requested cache slot; already
        // resolved members of an ObjStm remain live in their own cache slots.
        // Promote those compatibility-cache values before the replacement
        // overwrites the source container.
        self.promote_resolved_object_stream_members(object_ref)?;
        let target = self.resolver.replace_object(object_ref, replacement)?;
        self.qpdf_removed_refs.remove(&object_ref);
        self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.mark_object_handle_mutated(object_ref);
        Ok(target)
    }

    /// Remove a canonical object from the resolver's xref/cache view and
    /// leave outstanding handles as floating null values. The legacy cache is
    /// deliberately not rewritten here; its writer-facing cutover belongs to
    /// `flpdf-25kg.3.6.3`. This is qpdf `removeObject`'s exact xref/cache
    /// mutation (`QPDF.cc:1996-2005`), not the separate legacy
    /// `qpdf_removed_refs` snapshot filter and not xref registration's
    /// transient free-row state (`QPDF.cc:686-708`, `:1187-1210`).
    #[allow(dead_code)] // consumer cutover is flpdf-25kg.3.6.3
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
    /// The returned handle is a new, distinct object identity: unlike
    /// qpdf's uniform `shared_ptr<QPDFObject>` (where the caller's original
    /// handle and the new indirect one end up viewing the exact same
    /// underlying value, so mutating either mutates both), this crate's
    /// `Direct`/`Indirect` representations are different storage shapes
    /// (`object_handle.rs`'s own `Repr` enum) — an internal structural
    /// deviation only, not an output-byte difference. `handle`'s value is
    /// cloned into the new indirect slot; further mutation of `handle`
    /// itself (if the caller kept another clone of it) does not affect the
    /// returned handle, or vice versa.
    ///
    /// Allocation scans both [`Pdf::object_refs`] (the legacy object cache)
    /// and the handle registry for the highest existing object number
    /// (`max + 1`, generation `0`) rather than maintaining a running
    /// counter, matching this crate's existing
    /// `overlay_appearance_stream.rs::allocate_next_ref` convention.
    /// `object_refs()` alone is not enough: a ref allocated by a prior call
    /// to this same method is registered in [`Pdf::get_object_handle`]'s
    /// handle registry but never written through to the legacy cache, so
    /// scanning only `object_refs()` would let two back-to-back calls
    /// compute the same "next" number and the second silently clobber the
    /// first allocation's value.
    ///
    /// This does not validate that any indirect child handle reachable
    /// from `handle`'s direct value belongs to this same [`Pdf`] — no
    /// caller in this crate builds a direct value out of another
    /// document's indirect handles today. Doing so would embed a foreign
    /// document's live handle into this document's registry, which would
    /// observe that foreign document's own lifecycle (e.g. going to the
    /// destroyed state when the foreign `Pdf` is dropped) rather than this
    /// one's.
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
    /// either: this legacy allocator intentionally rejects reserved values;
    /// canonical qpdf-shaped promotion belongs to
    /// `ObjectHandle::promote_to_indirect`.
    pub fn make_indirect_object_handle(&mut self, handle: ObjectHandle) -> Result<ObjectHandle> {
        let Some(value) = handle.direct_value_clone()? else {
            return Err(Error::Unsupported(
                "cannot make an already-indirect ObjectHandle indirect".to_string(),
            ));
        };
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

    /// Whether no parsed or canonical object currently owns `number` at any
    /// generation.
    ///
    /// Tree-local allocation caches use this to detect an intervening PDF
    /// allocation before reusing their generation-zero candidate. qpdf's
    /// `nextObjGen()` allocates above the maximum object number, so a
    /// generation-one occupant reserves the number just as generation zero
    /// does.
    #[allow(dead_code)] // legacy test allocator; canonical consumers use next_obj_gen
    pub(crate) fn object_number_is_available(&self, number: u32) -> bool {
        !self.cache.contains_object_number(number) && !self.resolver.holds_object_number(number)
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
    /// [`Self::set_object`] and [`Self::delete_object`] already do this
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
    /// `m->obj_cache` even when it has no effective xref row. Keep the raw
    /// bootstrap map on `LoadedXrefState` only; the live `Pdf` must expose the
    /// value through the same `ObjectHandle` identity as every other cached
    /// object.
    pub(crate) fn install_parsed_xref_stream_handles(
        &mut self,
        parsed_xref_streams: BTreeMap<ObjectRef, Object>,
    ) -> Result<()> {
        for (object_ref, object) in parsed_xref_streams {
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
                // `parsed_xref_streams` is owned here, but every value is an
                // xref-stream object read by `read_xrefStream`, so the
                // `Object::Stream` arm is the one that always applies in
                // practice. Move the payload directly into `Rc` instead of
                // going through `lift_bounded`'s generic `&Object` match,
                // which can only clone `stream.data` for a borrowed source
                // (`lift_bounded_with_options`'s `Object::Stream` arm) --
                // avoidable copying of every retained historical xref-stream
                // payload otherwise, on top of the buffer this loop already
                // owns.
                let value = match object {
                    Object::Stream(stream) => {
                        let stream_dict = self.lift_dictionary_bounded_with_options(
                            &stream.dict,
                            0,
                            crate::parser::MAX_PARSE_DEPTH,
                            false,
                        )?; // cov:ignore: closing continuation of a covered multi-line call; llvm-cov misattributes this line, not an untested branch
                        ObjectValue::Stream {
                            stream_dict: self
                                .resolver
                                .direct_object_handle(ObjectValue::Dictionary(stream_dict)),
                            stream_data: Some(Rc::new(stream.data)),
                            stream_length: 0,
                            stream_provider: None,
                        }
                    }
                    other => self.lift_bounded(&other, 0, crate::parser::MAX_PARSE_DEPTH)?,
                };
                handle.set_resolved(value);
            }
            self.qpdf_parsed_xref_stream_refs.insert(object_ref);
        }
        Ok(())
    }

    #[cfg(test)]
    fn install_test_parsed_xref_stream_handle(
        &mut self,
        object_ref: ObjectRef,
        object: Object,
    ) -> Result<()> {
        self.install_parsed_xref_stream_handles(BTreeMap::from([(object_ref, object)]))
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

    // qpdf-deviation-start: registers the target handle of a bare top-level
    // ObjectValue::Reference redirect (producible only via Pdf::set_object)
    // so get_all_objects enumerates it; qpdf's top-level QPDFParser::parse
    // (QPDFParser.cc:87-88) never attempts an N-G-R lookahead and
    // QPDF::replaceObject (QPDF.cc:1980-1991) rejects an indirect handle, so
    // qpdf's object graph can never hold this redirect shape and has no
    // matching registration step.
    fn register_top_level_replacement_targets(&mut self) {
        // A bare `Object::Reference` supplied to `set_object` is represented
        // as an `ObjectValue::Reference` on the holder itself. Unlike an
        // indirect child, that lift does not mint the target handle. Register
        // the target before taking the cache snapshot so a dangling target is
        // visible exactly like a reference discovered inside a parsed value.
        let targets: BTreeSet<_> = self
            .resolver
            .all_object_handles()
            .into_iter()
            .filter_map(|handle| handle.as_reference())
            .filter(|object_ref| {
                object_ref.number != 0
                    && object_ref.generation != u16::MAX
                    && !self.qpdf_removed_refs.contains(object_ref)
            })
            .collect();
        for object_ref in targets {
            self.get_object_handle(object_ref);
        }
    }
    // qpdf-deviation-end

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

        self.register_top_level_replacement_targets();
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
    /// `ResolverHandle` cache; it never materializes a legacy [`Object`].
    ///
    /// qpdf's typed `QPDFObjectHandle` accessors call `QPDF::resolve` lazily
    /// and retain the same shared object identity: resolving the same
    /// indirect reference more than once yields handles that alias the same
    /// cached value rather than independent copies.
    ///
    /// This does not chase through an already-resolved
    /// [`Pdf::set_object`]-driven bare-reference redirect to its terminal
    /// value — see [`Pdf::resolve_to_terminal`] for that.
    /// `ref_chain.rs`'s bounded chain-follow primitive depends on this method
    /// exposing exactly one hop per call, not silently collapsing a
    /// multi-hop chain.
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

    /// Resolve `handle` (via [`Pdf::resolve`]), then chase
    /// through a [`Pdf::set_object`]-driven bare-reference redirect (if any)
    /// to its terminal (non-reference) value. Every accessor (`type_code`,
    /// `as_integer`, `unparse_resolved`, …) called on the *returned* handle
    /// then observes the terminal value directly, with no awareness a
    /// redirect ever happened. Callers that need `"N G R"` for the chain's
    /// *first* ref (matching
    /// `crates/flpdf-qtest-tools/src/driver/handle.rs`'s established
    /// `resolve_chain` contract — see
    /// `reference_chain_resolves_but_unparse_retains_the_first_reference`)
    /// call [`ObjectHandle::unparse`] on `handle` itself, not on the
    /// returned value.
    ///
    /// The returned handle is always the live, canonical handle for the
    /// terminal object, never a copy: an `Rc` clone of `handle` itself when
    /// its value is not a redirect at all (the common case), or of the last
    /// hop's [`Pdf::get_object_handle`] handle when one or more redirects
    /// were chased. Mutating it (`replace_key`, `insert_key`, …) therefore
    /// mutates that document object, exactly as mutating its own handle
    /// always would, and the returned [`ObjectRef`] is what the caller
    /// passes to [`Pdf::mark_object_dirty`] to make such an edit visible to
    /// the writer. This mirrors qpdf, whose `QPDFObjectHandle::dereference`
    /// (`libqpdf/QPDFObjectHandle.cc:2376-2383`, delegating to
    /// `QPDFObject::resolve`) hands back the canonical `QPDFObject` that
    /// `QPDF::resolve` cached and lets every mutator operate on it in
    /// place; qpdf has no copying dereference to port here. It is also the
    /// only contract compatible with a stream terminal, which
    /// [`ObjectHandle::shallow_copy`] refuses to copy at all, matching
    /// `QPDF_Stream::copy` (`libqpdf/QPDF_Stream.cc:140-145`).
    ///
    /// The chase itself still mutates nothing: only the *last* hop's own
    /// handle is returned, so `handle` and every intermediate hop keep the
    /// redirect [`Pdf::set_object`] recorded, and a later
    /// a later canonical handle lookup for any ref in the chain still observes
    /// it.
    ///
    /// `qpdf-cutover-delete(flpdf-25kg.3.3)`: terminal-chase legacy API.
    /// Delete with `ObjectValue::Reference`; do not call from new code.
    ///
    /// A self- or mutually-cyclic redirect chain is bounded by
    /// `ref_chain::MAX_REF_CHAIN_DEPTH` — this crate's one shared hop-count
    /// bound for exactly this kind of chase, also used by
    /// `ref_chain::resolve_ref_chain`. `ObjectValue::Reference` has no
    /// direct qpdf analog to bound against: qpdf's own
    /// `QPDF::replaceObject` (`libqpdf/QPDF.cc:1980-1991`) throws
    /// `std::logic_error` given an indirect handle, so qpdf's object graph
    /// can never itself hold a stored "this object's value is another
    /// reference" redirect the way [`Pdf::set_object`] permits here. The
    /// closest qpdf precedent is architectural rather than literal:
    /// `QPDF::resolve`'s own cycle guard (`libqpdf/QPDF.cc:1699-1712`)
    /// tracks in-progress resolutions in a `resolving` set and, on detecting
    /// a self/mutual loop, warns and falls back to `QPDF_Null` rather than
    /// hanging or overflowing the stack — the same "bounded, warn, fall back
    /// to null" shape this chase follows, via a hop counter instead of a
    /// visited set. Reaching the bound returns a null-resolved handle, with
    /// a recorded [`Pdf::repair_diagnostics`] warning, again without
    /// mutating any canonical handle.
    ///
    /// # Errors
    ///
    /// Same as [`Pdf::resolve`].
    pub fn resolve_to_terminal(&mut self, handle: &ObjectHandle) -> Result<ObjectHandle> {
        Ok(self.resolve_to_terminal_ref(handle)?.0)
    }

    /// `qpdf-cutover-delete(flpdf-25kg.3.3)`: terminal-ref legacy API.
    /// Delete with `ObjectValue::Reference`; do not call from new code.
    ///
    /// Same chase as [`Pdf::resolve_to_terminal`], additionally
    /// returning the object reference the terminal value was actually read
    /// from. This is `None` exactly when the returned handle is direct with
    /// no indirect identity of its own — either `handle` itself was direct,
    /// or the chase hit the `ref_chain::MAX_REF_CHAIN_DEPTH` bound and fell
    /// back to a null handle. Whenever it is `Some`, the returned handle is
    /// that object's own canonical, indirect handle. Otherwise it is `handle.object_ref()` when no
    /// [`Pdf::set_object`] redirect was chased (matching
    /// [`Pdf::resolve_to_terminal`]'s own "returns `handle`
    /// unchanged" case), or the *last* hop's ref when one or more redirects
    /// were followed — deliberately not the chain's first ref, which callers
    /// needing offset/diagnostic attribution for the terminal object itself
    /// (e.g. a stream's source offset) must not conflate with an intermediate
    /// redirect's own ref.
    ///
    /// # Errors
    ///
    /// Same as [`Pdf::resolve`].
    pub fn resolve_to_terminal_ref(
        &mut self,
        handle: &ObjectHandle,
    ) -> Result<(ObjectHandle, Option<ObjectRef>)> {
        self.resolve(handle)?;
        let Some(mut current_ref) = handle.as_reference() else {
            // already terminal (the common case) — or unresolved/null;
            // `handle.object_ref()` is already the correct terminal ref,
            // `None` for an originally-direct handle.
            return Ok((handle.clone(), handle.object_ref()));
        };
        // qpdf-deviation-start: chases a stored reference-as-value redirect
        // across multiple hops. Only Pdf::set_object can create this shape;
        // qpdf's QPDF::replaceObject (libqpdf/QPDF.cc:1980-1991) rejects an
        // indirect replacement, so a parsed qpdf object graph can never hold
        // a reference whose own value is another reference to chase here.
        for _ in 0..crate::ref_chain::MAX_REF_CHAIN_DEPTH {
            let hop = self.get_object_handle(current_ref);
            self.resolve(&hop)?;
            match hop.as_reference() {
                Some(next) => current_ref = next,
                // `hop` itself, not a copy of it: qpdf's own dereference
                // (`QPDFObjectHandle::dereference`,
                // `libqpdf/QPDFObjectHandle.cc:2376-2383`, delegating to
                // `QPDFObject::resolve`) hands back the canonical object
                // that `QPDF::resolve` cached, never a clone, and every
                // qpdf mutator then operates on that canonical handle. The
                // sibling "already terminal" arm above already returns
                // `handle.clone()` — the canonical handle — so copying only
                // on the redirect path split this one function into two
                // different aliasing contracts. Copying here also cannot be
                // reconciled with `ObjectValue::Stream`: qpdf's
                // `QPDF_Stream::copy` (`libqpdf/QPDF_Stream.cc:140-145`)
                // refuses to clone a stream at all.
                //
                // No canonical state is written either way, so a later call
                // simply redoes the chase rather than being stuck observing
                // a stale result.
                //
                // `hop` is always a resolved value here, so it presents a
                // value exactly as a copy of it would: the one state that
                // would differ, `Unresolved`, needs `resolve`
                // to have hit a `CacheEntry::Reserved` entry, and that guard
                // exists only while `resolve_pending_stream_length` is on the
                // stack (it is the sole `set_reserved` caller and clears the
                // entry before returning). This `&mut self` entry point cannot
                // be reached from inside a resolution.
                None => return Ok((hop, Some(current_ref))),
            }
        }
        self.push_warning(format!(
            "reference redirect chain reaching object {} {} exceeds \
             {} hops, treating as cyclic",
            current_ref.number,
            current_ref.generation,
            crate::ref_chain::MAX_REF_CHAIN_DEPTH
        ))?;
        // Ref and handle degrade together: a null value paired with a
        // live-looking ref would let a caller compute an "offset of
        // terminal" for an object it was just told is null.
        Ok((ObjectHandle::null(), None))
        // qpdf-deviation-end
    }

    fn lift_bounded(
        &mut self,
        object: &Object,
        depth: usize,
        max_depth: usize,
    ) -> Result<ObjectValue> {
        self.lift_bounded_with_options(object, depth, max_depth, false)
    }

    fn lift_bounded_with_content_tokens(
        &mut self,
        object: &Object,
        depth: usize,
        max_depth: usize,
    ) -> Result<ObjectValue> {
        self.lift_bounded_with_options(object, depth, max_depth, true)
    }

    fn lift_bounded_with_options(
        &mut self,
        object: &Object,
        depth: usize,
        max_depth: usize,
        allow_content_tokens: bool,
    ) -> Result<ObjectValue> {
        if depth > max_depth {
            return Err(Error::Unsupported(format!(
                "object handle lift: inline object nesting exceeds maximum of {max_depth}"
            )));
        }
        // Recursion hub for the mutually-recursive `lift_bounded` /
        // `lift_to_handle_bounded` / `lift_dictionary_bounded` triangle:
        // every nesting level returns back through here, so wrapping this
        // one call site protects the whole walk the same way
        // `object_handle.rs`'s own recursive hubs do. Needed for real, not
        // just for a test with an oversized stack to pass: unlike the
        // canonical live parser (`parser.rs`, itself `stacker`-protected), this
        // legacy materialization path has no protection of its own, and
        // `Pdf::trailer_key_handle`
        // now reaches it at the looser `MAX_PARSE_DEPTH` bound (500) — a
        // legitimately deep but successfully parsed value could otherwise
        // abort a production caller running on a small-stack thread instead
        // of returning a handle.
        stacker::maybe_grow(READER_STACK_RED_ZONE, READER_STACK_GROWTH_SIZE, || {
            let value = match object {
                Object::Null => ObjectValue::Null,
                Object::Boolean(b) => ObjectValue::Boolean(*b),
                Object::Integer(n) => ObjectValue::Integer(*n),
                Object::Real(r) => ObjectValue::Real(*r),
                Object::RealLiteral { value, literal } => ObjectValue::RealLiteral {
                    value: *value,
                    literal: literal.clone(),
                },
                Object::Name(name) => ObjectValue::Name(name.clone()),
                Object::String(s) => ObjectValue::String(s.clone()),
                Object::Array(items) => ObjectValue::Array(
                    items
                        .iter()
                        .map(|item| {
                            self.lift_to_handle_bounded_with_options(
                                item,
                                depth + 1,
                                max_depth,
                                allow_content_tokens,
                            )
                        })
                        .collect::<Result<Vec<_>>>()?,
                ),
                Object::Dictionary(dict) => {
                    ObjectValue::Dictionary(self.lift_dictionary_bounded_with_options(
                        dict,
                        depth,
                        max_depth,
                        allow_content_tokens,
                    )?)
                }
                // A stream's own dictionary is lifted the same way any other
                // nested dictionary is (see `lift_dictionary`), then wrapped in
                // its own fresh handle: this arm mints a new dictionary handle
                // every time, at the no-offset sentinel. `Pdf::set_object`
                // (the only caller that can reach this arm with a *replacement*
                // for an already-resolved stream) special-cases reusing the
                // pre-existing dictionary handle instead, via
                // `Pdf::lift_for_set_object`, so an established parsed offset
                // is not lost on a plain round trip.
                Object::Stream(stream) => {
                    let stream_dict = self.lift_dictionary_bounded_with_options(
                        &stream.dict,
                        depth,
                        max_depth,
                        allow_content_tokens,
                    )?;
                    ObjectValue::Stream {
                        stream_dict: self
                            .resolver
                            .direct_object_handle(ObjectValue::Dictionary(stream_dict)),
                        stream_data: Some(Rc::new(stream.data.clone())),
                        stream_length: 0,
                        stream_provider: None,
                    }
                }
                // A bare top-level reference never comes from a file/ObjStm
                // parse (`top_level_no_reference` integerizes it there,
                // matching qpdf), but `Pdf::set_object` callers pass one
                // directly throughout this crate to redirect or collapse a
                // holder chain in place (`ObjectRef` -> `ObjectRef`, no
                // recursive follow) -- `ObjectValue::Reference` is the handle
                // graph's representation for exactly that case; see its own
                // doc.
                // qpdf-deviation: ObjectValue::Reference as an indirect
                // object's own resolved value has no qpdf counterpart --
                // QPDF::replaceObject/replaceReserved (QPDF.cc:1985-2016)
                // both reject an indirect handle as a replacement value, and
                // QPDFParser::parse never looks ahead for "N G R" at the top
                // level, so only Pdf::set_object callers construct this
                // redirect shape.
                Object::Reference(object_ref) => ObjectValue::Reference(*object_ref),
                Object::Operator(bytes) if allow_content_tokens => {
                    ObjectValue::Operator(bytes.clone())
                }
                Object::InlineImage(bytes) if allow_content_tokens => {
                    ObjectValue::InlineImage(bytes.clone())
                }
                // In the normal lift mode, content-stream-only tokens are not
                // resolved file/ObjStm values and are rejected rather than
                // silently discarded as `Null`: `Pdf::set_object` treats this
                // failure as "cannot be represented in the handle graph".
                // Content-token lifting is explicit at the caller boundary;
                // ordinary object lifting does not silently discard a token.
                Object::Operator(_) | Object::InlineImage(_) => {
                    return Err(Error::Unsupported(
                        "object handle lift: content-stream-only token has no ObjectValue representation"
                            .to_string(),
                    ));
                }
            };
            Ok(value)
        })
    }

    fn lift_dictionary_bounded_with_options(
        &mut self,
        dict: &Dictionary,
        depth: usize,
        max_depth: usize,
        allow_content_tokens: bool,
    ) -> Result<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> {
        dict.iter()
            .map(|(k, v)| {
                Ok((
                    crate::object_handle::canonical_dictionary_key_from_legacy(k),
                    self.lift_to_handle_bounded_with_options(
                        v,
                        depth + 1,
                        max_depth,
                        allow_content_tokens,
                    )?,
                ))
            })
            .collect()
    }

    // Lift a child `Object` (array element or dictionary value) to a handle.
    //
    // An `Object::Reference` becomes the canonical indirect handle for that
    // reference (via `Pdf::get_object_handle`), preserving identity with any
    // other handle already registered for the same object — it is left
    // unresolved, not eagerly followed. Any other value is lifted directly
    // and wrapped in a fresh direct handle carrying this document's weak
    // context. `max_depth` bounds inline nesting the same way `lift_bounded`
    // does — see its own comment.
    pub(crate) fn lift_to_handle_bounded(
        &mut self,
        object: &Object,
        depth: usize,
        max_depth: usize,
    ) -> Result<ObjectHandle> {
        self.lift_to_handle_bounded_with_options(object, depth, max_depth, false)
    }

    fn lift_to_handle_bounded_with_options(
        &mut self,
        object: &Object,
        depth: usize,
        max_depth: usize,
        allow_content_tokens: bool,
    ) -> Result<ObjectHandle> {
        match object {
            Object::Reference(object_ref) => Ok(self.get_object_handle(*object_ref)),
            // qpdf's QPDFObjectHandle::newNull() is contextless
            // (`libqpdf/QPDFObjectHandle.cc:1891-1894`), including null
            // fallbacks returned by dictionary accessors. Keep a relifted
            // legacy null on that same boundary rather than making a
            // synthetic resolver-bearing value whose warning path would
            // incorrectly reach this Pdf's logger.
            Object::Null => {
                if depth > max_depth {
                    return Err(Error::Unsupported(format!(
                        "object handle lift: inline object nesting exceeds maximum of {max_depth}"
                    )));
                }
                Ok(ObjectHandle::null())
            }
            direct => {
                let value =
                    self.lift_bounded_with_options(direct, depth, max_depth, allow_content_tokens)?;
                Ok(self.resolver.direct_object_handle(value))
            }
        }
    }

    /// Resolve `object_ref` to its concrete value for the JSON projection,
    /// parsing on demand through the canonical handle resolver.
    ///
    /// Resolution caches the result so subsequent calls are constant-time. Unknown,
    /// freed, or compressed-but-broken entries return [`Object::Null`] rather than an
    /// error, matching the behavior the PDF spec mandates for missing objects (§7.3.10).
    ///
    /// # Errors
    ///
    pub(crate) fn resolve_qpdf_json_object(&mut self, object_ref: ObjectRef) -> Result<Object> {
        let handle = self.get_object_handle(object_ref);
        self.resolve(&handle)?;
        handle.materialize()
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

#[cfg(test)]
pub(crate) fn parse_object_stream_entry(
    stream_object: &crate::Stream,
    target_index: u32,
) -> Result<ParsedObjectStreamEntry> {
    let stream_data = crate::filters::test_dictionary_api::decode_stream_data(
        &stream_object.dict,
        &stream_object.data,
    )?;

    let stream_object_count = object_stream_count(stream_object)?;
    let stream_data_first = parse_non_negative_i64(
        stream_object
            .dict
            .get("First")
            .ok_or(Error::Missing("Object stream /First"))?,
        "Object stream /First",
    )?;

    let object_count = stream_object_count;
    let first = usize::try_from(stream_data_first)
        .map_err(|_| Error::parse(0, "Object stream /First does not fit usize"))?;

    let mut tokenizer = Tokenizer::new(&stream_data);
    let mut object_offsets = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        let _object_number =
            parse_non_negative_u64(tokenizer.next_integer()?, "object stream object number")?;
        let object_offset =
            parse_non_negative_u64(tokenizer.next_integer()?, "object stream object offset")?;
        object_offsets.push(object_offset);
    }

    let target_index = usize::try_from(target_index)
        .map_err(|_| Error::parse(0, "compressed object index does not fit usize"))?;
    if target_index >= object_offsets.len() {
        return Err(Error::parse(
            0,
            "compressed object index out of range for this stream",
        ));
    }

    let start = first
        .checked_add(
            usize::try_from(object_offsets[target_index])
                .map_err(|_| Error::parse(0, "object stream offset does not fit usize"))?,
        )
        .ok_or_else(|| Error::parse(0, "compressed object offset overflow"))?;

    if start > stream_data.len() {
        return Err(Error::parse(0, "compressed object offset out of range"));
    }

    let (object, _diagnostics) = parse_qpdf_file_object(&stream_data[start..])?;
    Ok(ParsedObjectStreamEntry { object })
}

#[cfg(test)]
pub(crate) struct ParsedObjectStreamEntry {
    pub(crate) object: Object,
}

#[cfg(test)]
pub(crate) fn object_stream_count(stream_object: &crate::Stream) -> Result<usize> {
    usize::try_from(parse_non_negative_i64(
        stream_object
            .dict
            .get("N")
            .ok_or(Error::Missing("Object stream /N"))?,
        "Object stream /N",
    )?)
    .map_err(|_| Error::parse(0, "Object stream /N does not fit usize"))
}

#[cfg(test)]
fn parse_non_negative_i64(value: &crate::Object, context: &str) -> Result<i64> {
    let crate::Object::Integer(integer) = value else {
        return Err(Error::parse(0, format!("{context} is not integer")));
    };
    if *integer < 0 {
        return Err(Error::parse(0, format!("{context} is negative")));
    }
    Ok(*integer)
}

#[cfg(test)]
fn parse_non_negative_u64(value: i64, context: &str) -> Result<u64> {
    if value < 0 {
        return Err(Error::parse(0, format!("{context} is negative")));
    }
    Ok(value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved_object<R: Read + Seek>(pdf: &mut Pdf<R>, object_ref: ObjectRef) -> Result<Object> {
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)?;
        handle.materialize()
    }

    fn canonical_recovered_eol<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        object_ref: ObjectRef,
    ) -> Result<Option<&'static [u8]>> {
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)?;
        pdf.canonical_recovered_stream_eol(object_ref, &handle)
    }
    use crate::pages::page_refs;
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::PipelineHandle;
    use crate::writer::ObjectWriterEmission;
    use crate::Stream;
    use std::io::{Cursor, SeekFrom};
    use std::rc::Rc;
    use std::sync::Arc;

    fn flate_encoded(plaintext: &[u8]) -> Vec<u8> {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        crate::filters::test_dictionary_api::encode_stream_data(&dict, plaintext)
            .expect("Flate encode")
    }

    fn explicit_rc4_encryption_state() -> EncryptionState {
        EncryptionState {
            file_key: vec![0x11, 0x22, 0x33, 0x44, 0x55],
            encryption_v: 4,
            encryption_r: 4,
            cf_stream: EncryptionMode::Identity,
            cf_string: EncryptionMode::Identity,
            cf_file: EncryptionMode::Identity,
            crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), EncryptionMode::Rc4)]),
            encrypt_metadata: true,
            encrypt_ref: None,
            weak_crypto: true,
            permissions: Permissions::new(-4),
            id0: Some(b"fixture-id0".to_vec()),
            user_password_matched: true,
            owner_password_matched: false,
            user_password: Vec::new(),
            cached_object_encryption_key: Vec::new(),
            cached_key_og: None,
        }
    }

    struct ReadFailingCursor {
        inner: Cursor<Vec<u8>>,
        fail_reads: bool,
    }

    impl ReadFailingCursor {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                inner: Cursor::new(bytes),
                fail_reads: false,
            }
        }
    }

    impl Read for ReadFailingCursor {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.fail_reads {
                return Err(std::io::Error::other("injected holder read failure"));
            }
            self.inner.read(buf)
        }
    }

    impl Seek for ReadFailingCursor {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    fn fail_warning_delivery<R: Read + Seek>(pdf: &mut Pdf<R>) {
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(NthWriteFailure::new(1))));
        pdf.set_logger(logger);
    }

    #[test]
    fn canonical_recovered_stream_eol_is_none_for_a_non_stream_handle() {
        // A non-stream Catalog has no recovered stream framing metadata.
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open minimal PDF");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);

        assert_eq!(
            pdf.canonical_recovered_stream_eol(object_ref, &handle)
                .expect("non-stream handle never queries encryption"),
            None
        );
    }

    #[test]
    fn encrypt_dictionary_rejects_a_non_dictionary_canonical_handle() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open minimal PDF");
        pdf.trailer.insert("Encrypt", Object::Integer(42));

        let error = pdf
            .encrypt_dictionary()
            .expect_err("a scalar /Encrypt value must be malformed");
        assert!(matches!(
            error,
            Error::Encrypted(EncryptedError::Malformed { reason })
                if reason == "/Encrypt object is not a dictionary"
        ));
    }

    #[test]
    fn canonical_recovered_stream_eol_treats_encrypted_recovery_as_ciphertext_framing() {
        // Regression test for job/inspection.rs's `show-stream`/`dump-object`
        // double-trimming real decrypted content as recovery-scan framing
        // (flpdf-egzr.3.2.7 review). `tests/fixtures/compat/
        // encrypted-recovered-eol.pdf` object 4 0 is an AESv2-encrypted
        // stream missing `/Length`; qpdf's `decryptStream` (and this
        // predicate's port of its classification) treats the recovered
        // trailing byte as ciphertext framing, so nothing here is eligible
        // to be trimmed as source-scan padding.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/encrypted-recovered-eol.pdf");
        let file = std::fs::File::open(path).expect("encrypted-recovered-eol fixture");
        let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("authenticate fixture");
        let object_ref = ObjectRef::new(4, 0);
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).expect("resolve object 4 0");
        handle
            .get_raw_stream_data()
            .expect("read raw stream data (triggers length recovery)");

        assert_eq!(
            pdf.canonical_recovered_stream_eol(object_ref, &handle)
                .expect("classify encrypted stream's recovered EOL"),
            None
        );
    }

    #[test]
    fn parsed_direct_array_rejects_a_foreign_indirect_item_like_qpdf() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut entries = vec![bytes.len()];
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Names << /Nums [] >> >>\nendobj\n");
        entries.push(bytes.len());
        bytes.extend_from_slice(b"2 0 obj\n7\nendobj\n");
        let startxref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        for offset in &entries {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("open destination PDF");
        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&root).expect("resolve destination root");
        let nums = root.get_key(b"/Names").get_key(b"/Nums");

        let mut foreign_pdf = Pdf::open(Cursor::new(bytes)).expect("open foreign PDF");
        let foreign_item = foreign_pdf.get_object_handle(ObjectRef::new(2, 0));
        // qpdf's QPDFParser stamps the direct array with its QPDF context
        // (`libqpdf/QPDFParser.cc:219-232,439-443` and
        // `libqpdf/qpdf/QPDFValue.hh:60-66`), so QPDF_Array::checkOwnership
        // (`libqpdf/QPDF_Array.cc:10-26`) rejects the foreign indirect item.
        let error = nums
            .append_array_item(foreign_item)
            .expect_err("a parsed direct array must reject a foreign indirect item");

        assert!(error.to_string().contains("different QPDF"));
    }

    #[test]
    fn keyword_token_at_requires_token_boundary() {
        // Keyword at EOF (nothing after) is a valid token.
        assert_eq!(
            crate::parser::keyword_token_end(b"endobj", 0, b"endobj"),
            Some(6)
        );
        // Followed by whitespace / a delimiter is a valid token.
        assert_eq!(
            crate::parser::keyword_token_end(b"endobj\n", 0, b"endobj"),
            Some(6)
        );
        assert_eq!(
            crate::parser::keyword_token_end(b"endobj/", 0, b"endobj"),
            Some(6)
        );
        // A longer run of regular chars (no boundary) is NOT the keyword token.
        assert_eq!(
            crate::parser::keyword_token_end(b"endobjX", 0, b"endobj"),
            None
        );
        // Non-match.
        assert_eq!(
            crate::parser::keyword_token_end(b"endstream", 0, b"endobj"),
            None
        );
    }

    #[test]
    fn endstream_and_endobj_tokens_are_validated_separately() {
        assert_eq!(
            crate::parser::keyword_token_end(b"endstream\nendobj", 0, b"endstream"),
            Some(9)
        );
        assert_eq!(
            crate::parser::keyword_token_end(b"endstream\nendobj", 10, b"endobj"),
            Some(16)
        );
        assert_eq!(
            crate::parser::keyword_token_end(b"endstreamendobj", 0, b"endstream"),
            None
        );
        assert_eq!(
            crate::parser::keyword_token_end(b"endstream more", 10, b"endobj"),
            None
        );
        assert_eq!(
            crate::parser::keyword_token_end(b"xendstream\nendobj", 0, b"endstream"),
            None
        );
    }

    /// Minimal valid single-page PDF used across `open_mem` tests.
    ///
    /// Structure:
    ///   1 0 obj  Catalog  /Root
    ///   2 0 obj  Pages    /Kids [3 0 R]  /Count 1
    ///   3 0 obj  Page
    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// End-to-end: resolving an indirect object whose dictionary repeats a
    /// key surfaces qpdf's `dictionary has duplicated key` warning through
    /// [`Pdf::repair_diagnostics`], matching `/usr/bin/qpdf --check`'s
    /// observed `WARNING: <file> (object 3 0, offset 125): dictionary has
    /// duplicated key /Foo; last occurrence overrides earlier ones` on an
    /// equivalent fixture.
    #[test]
    fn resolving_an_object_with_a_duplicate_dictionary_key_warns_through_open_mem() {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Foo 1 /Foo 2 >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());

        let mut pdf = Pdf::open_mem(Arc::from(&pdf[..])).expect("open_mem");
        let value = resolved_object(&mut pdf, ObjectRef::new(3, 0)).expect("resolve object 3");
        let dict = value.as_dict().expect("dictionary");
        assert_eq!(
            dict.get("Foo"),
            Some(&Object::Integer(2)),
            "last write wins"
        );

        let diags = pdf.repair_diagnostics();
        let messages: Vec<&str> = diags.entries().iter().map(|d| d.message.as_str()).collect();
        let expected_offset = off3 + "3 0 obj\n<<".len() as u64;
        let expected_message = format!(
            "(object 3 0, offset {expected_offset}): dictionary has duplicated key /Foo; last occurrence overrides earlier ones"
        );
        assert_eq!(messages, vec![expected_message.as_str()]);
    }

    fn pdf_with_one_stream(stream_data: &[u8]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let stream_offset = pdf.len() as u64;
        pdf.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", stream_data.len()).as_bytes(),
        );
        pdf.extend_from_slice(stream_data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{stream_offset:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn pdf_with_one_stream_with_null_filters(stream_data: &[u8]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let stream_offset = pdf.len() as u64;
        pdf.extend_from_slice(
            format!(
                "4 0 obj\n<< /Length {} /Filter null /DecodeParms null >>\nstream\n",
                stream_data.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(stream_data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{stream_offset:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    #[test]
    fn dropping_pdf_breaks_the_pages_parent_reference_cycle() {
        // `minimal_pdf_bytes`'s Pages node (2 0 obj) and Page (3 0 obj)
        // reference each other (`/Kids [3 0 R]` / `/Parent 2 0 R`); once both
        // are resolved, each slot's value embeds the other's canonical
        // handle, mirroring the CI fuzz LeakSanitizer failure reproduced
        // from a self-referential `/Catalog`.
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let pages = pdf.get_object_handle(ObjectRef::new(2, 0));
        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&pages).expect("resolve pages");
        pdf.resolve(&page).expect("resolve page");

        // Each handle is held by: this test's own local variable, the
        // registry, and the other object's resolved value (the cycle).
        assert_eq!(pages.strong_count(), 3);
        assert_eq!(page.strong_count(), 3);

        drop(pdf);

        // `Pdf::drop` disconnects every registry entry before the registry
        // itself drops, so only this test's own local handles remain live.
        assert_eq!(pages.strong_count(), 1);
        assert_eq!(page.strong_count(), 1);
        assert!(pages.is_direct());
        assert!(page.is_direct());
        assert!(!pages.is_null());
        assert!(!page.is_null());
    }

    #[test]
    fn dropping_pdf_preserves_a_surviving_literal_null_handle() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let object_ref = ObjectRef::new(90, 0);
        pdf.set_object(object_ref, Object::Null);
        let handle = pdf.get_object_handle(object_ref);
        assert!(handle.is_indirect());
        assert!(handle.is_null());

        drop(pdf);

        assert!(handle.is_direct());
        assert!(handle.is_null());
    }

    #[test]
    fn dropping_pdf_preserves_a_surviving_null_resolved_handle() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let handle = pdf.get_object_handle(ObjectRef::new(91, 0));
        handle.set_resolved(ObjectValue::Null);
        assert!(handle.is_indirect());
        assert!(handle.is_null());

        drop(pdf);

        assert!(handle.is_direct());
        assert!(handle.is_null());
    }

    #[test]
    fn object_number_availability_checks_every_registered_generation() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        pdf.get_object_handle(ObjectRef::new(90, 1));

        assert!(!pdf.object_number_is_available(90));
        assert!(pdf.object_number_is_available(91));
    }

    #[test]
    fn make_indirect_object_handle_allocates_a_fresh_ref_and_preserves_the_value() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let direct = ObjectHandle::integer(42);
        let indirect = pdf
            .make_indirect_object_handle(direct)
            .expect("make indirect");
        assert!(indirect.is_indirect());
        assert_eq!(indirect.as_integer(), Some(42));
    }

    #[test]
    fn make_indirect_object_handle_preserves_the_direct_description_and_offset() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let direct = ObjectHandle::parse(b"<< /Value 7 >>").expect("parse direct object");
        let source_description = direct.description();
        let source_offset = direct.get_parsed_offset();

        let indirect = pdf
            .make_indirect_object_handle(direct)
            .expect("make indirect");

        assert_eq!(indirect.description(), source_description);
        assert_eq!(indirect.get_parsed_offset(), source_offset);
    }

    #[test]
    fn make_indirect_object_handle_rejects_an_already_indirect_handle() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let already_indirect = pdf.get_object_handle(ObjectRef::new(1, 0));
        assert!(pdf.make_indirect_object_handle(already_indirect).is_err());
    }

    #[test]
    fn make_indirect_object_handle_rejects_a_direct_reserved_handle_without_misreporting_already_indirect(
    ) {
        // Codex Review round 5 on PR #789, databaseId 3773627592,
        // object_handle.rs:1476 (ObjectHandle::direct_value_clone). A
        // *direct* reserved handle -- only constructible via
        // `ObjectHandle::shallow_copy` on a reserved handle
        // (`QPDF_Reserved::copy`, never null, never a throw:
        // `libqpdf/QPDF_Reserved.cc:14-19`) -- used to fall into the same
        // `Ok(None)` bucket `direct_value_clone` returns for a genuinely
        // indirect handle, so this method reported "cannot make an
        // already-indirect ObjectHandle indirect" for a handle
        // `slot.object_ref.is_some()` had already confirmed was direct just
        // a few lines above.
        //
        // This legacy clone-based allocator is intentionally narrow; canonical
        // reserved values remain in ObjectValue for qpdf-shaped promotion and
        // replacement primitives.
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        let direct_copy = reserved
            .shallow_copy()
            .expect("QPDF_Reserved::copy never throws");
        assert!(
            direct_copy.is_direct(),
            "shallow_copy never carries an object number of its own"
        );

        let error = pdf
            .make_indirect_object_handle(direct_copy)
            .expect_err("a direct reserved handle's value cannot be cloned");
        assert!(
            !error.to_string().contains("already-indirect"),
            "the handle is direct; reporting it as already indirect would be false: {error}"
        );
    }

    #[test]
    fn make_indirect_object_handle_allocates_past_the_highest_existing_number() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let max_before = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let indirect = pdf
            .make_indirect_object_handle(ObjectHandle::integer(1))
            .expect("make indirect");
        assert!(indirect.object_ref().unwrap().number > max_before);
    }

    #[test]
    fn make_indirect_object_handle_works_for_a_handle_with_other_outstanding_clones() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let direct = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let clone_kept_by_caller = direct.clone();
        let indirect = pdf
            .make_indirect_object_handle(direct)
            .expect("make indirect even though a clone is still outstanding");
        assert_eq!(indirect.get_key(b"/A").as_integer(), Some(1));
        // The new indirect handle is its own object; mutating the original
        // direct clone the caller kept does not affect it (Rust's Direct
        // and Indirect slots are distinct storage, unlike qpdf's uniform
        // shared_ptr<QPDFObject> -- see make_indirect_object_handle's own
        // doc comment).
        clone_kept_by_caller
            .replace_key(b"/A", ObjectHandle::integer(99))
            .unwrap();
        assert_eq!(indirect.get_key(b"/A").as_integer(), Some(1));
    }

    #[test]
    fn make_indirect_object_handle_keeps_a_promoted_streams_metadata_consistent() {
        // qpdf shares the *whole* QPDFObject on promotion
        // (`QPDF::makeIndirectObject` -> `makeIndirectFromQPDFObject(oh.getObj())`,
        // `libqpdf/QPDF.cc:1883-1898`), so an edit through either handle is
        // one edit to one stream. This allocator still mints a separate slot
        // (flpdf-25kg.3.6), and there a *partial* share corrupts: stream_dict
        // is an ObjectHandle (shared mutability) while stream_data is a
        // per-value field, so a shared dictionary would let this
        // replace_stream_data rewrite the promoted stream's /Length while
        // leaving its bytes alone. Each slot must stay self-consistent.
        let direct_stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(
                b"Length".to_vec(),
                ObjectHandle::integer(3),
            )]),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_length: 3,
            stream_provider: None,
        });
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let promoted = pdf
            .make_indirect_object_handle(direct_stream.clone())
            .expect("make indirect");

        direct_stream.replace_stream_data(Rc::new(b"brand new bytes".to_vec()), None, None);

        let promoted_length = promoted
            .as_stream_dict()
            .expect("promoted stream dict")
            .get_key(b"/Length")
            .as_integer();
        assert_eq!(
            promoted_length,
            Some(3),
            "the promoted stream keeps its own /Length; the source's replacement \
             must not describe bytes the promoted object does not hold"
        );
        assert_eq!(
            promoted.as_stream_data(),
            Some(Rc::new(b"old".to_vec())),
            "and keeps its own bytes"
        );
        assert_eq!(
            direct_stream
                .as_stream_dict()
                .expect("source stream dict")
                .get_key(b"/Length")
                .as_integer(),
            Some(15),
            "while the source records its own replacement"
        );
    }

    #[test]
    fn make_indirect_object_handle_allocates_distinct_refs_across_repeated_calls() {
        // Regression test: a ref allocated by this method is registered in
        // `handle_registry` but never written through to the legacy
        // `object_refs()` cache, so scanning only `object_refs()` for the
        // "next" number would let a second call compute the same number as
        // the first and silently clobber its value via `set_resolved`.
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let first = pdf
            .make_indirect_object_handle(ObjectHandle::integer(1))
            .expect("make first indirect");
        let second = pdf
            .make_indirect_object_handle(ObjectHandle::integer(2))
            .expect("make second indirect");
        assert_ne!(
            first.object_ref().unwrap(),
            second.object_ref().unwrap(),
            "repeated calls must allocate distinct object numbers"
        );
        assert_eq!(first.as_integer(), Some(1));
        assert_eq!(second.as_integer(), Some(2));
    }

    #[test]
    fn make_indirect_object_handle_survives_a_full_rewrite() {
        // The new object is reachable through the mutated Catalog and must be
        // included by the canonical qpdf-style reachability walk.

        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let new_object = pdf
            .make_indirect_object_handle(ObjectHandle::integer(777))
            .expect("make indirect");
        let new_ref = new_object.object_ref().unwrap();

        let root_ref = ObjectRef::new(1, 0);
        let mut root_dict = resolved_object(&mut pdf, root_ref)
            .expect("resolve root")
            .into_dict()
            .unwrap();
        root_dict.insert("Extra", Object::Reference(new_ref));
        pdf.set_object(root_ref, Object::Dictionary(root_dict));

        let mut writer = crate::PdfWriter::new(&mut pdf);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("full rewrite");
        let emitted_ref = writer
            .get_renumbered_obj_gen(new_ref)
            .expect("query new-object mapping")
            .expect("new object must be emitted");
        let out = writer.get_buffer().expect("take full-rewrite output");

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let resolved_root =
            resolved_object(&mut reopened, root_ref).expect("resolve root in reopened output");
        let extra_ref = resolved_root
            .into_dict()
            .and_then(|d| d.get("Extra").cloned())
            .expect("root has /Extra");
        assert_eq!(extra_ref, Object::Reference(emitted_ref));
        assert_eq!(
            resolved_object(&mut reopened, emitted_ref).expect("resolve new object"),
            Object::Integer(777),
            "new object must not be dangling after a full rewrite"
        );
    }

    #[test]
    fn canonical_stream_allocation_and_mutation_survive_without_legacy_materialization() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let stream = pdf
            .make_indirect_object_handle(ObjectHandle::stream(
                ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(18))]),
                Rc::new(b"canonical stream data".to_vec()),
            ))
            .expect("make stream indirect");
        let stream_ref = stream.object_ref().expect("stream ref");
        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&root).expect("resolve root");
        root.replace_key(b"/Extra", stream.clone()).unwrap();
        pdf.mark_object_dirty(ObjectRef::new(1, 0));

        let mut writer = crate::PdfWriter::new(&mut pdf);
        writer.set_compress_streams(false);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("full rewrite");
        let emitted_ref = writer
            .get_renumbered_obj_gen(stream_ref)
            .expect("query stream mapping")
            .expect("allocated stream must be emitted");
        let out = writer.get_buffer().expect("take full-rewrite output");

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let extra_ref = resolved_object(&mut reopened, ObjectRef::new(1, 0))
            .expect("resolve rewritten root")
            .into_dict()
            .and_then(|dict| dict.get("Extra").cloned())
            .and_then(|object| object.as_ref_id())
            .expect("rewritten root has stream reference");
        assert_eq!(extra_ref, emitted_ref);
        assert_eq!(
            resolved_object(&mut reopened, emitted_ref)
                .expect("resolve rewritten stream")
                .as_stream()
                .expect("stream remains a stream")
                .data,
            b"canonical stream data"
        );
    }

    #[test]
    fn set_object_operator_replacement_survives_canonical_full_rewrite() {
        let page_ref = ObjectRef::new(3, 0);
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.set_object(page_ref, Object::Operator(b"Do".to_vec()));

        let (emitted_ref, output) = {
            let mut writer = crate::PdfWriter::new(&mut pdf);
            writer.set_object_stream_mode(crate::ObjectStreamMode::Disable);
            writer.set_output_memory().expect("configure memory output");
            writer.write().expect("write operator replacement");
            let emitted_ref = writer
                .get_renumbered_obj_gen(page_ref)
                .expect("query operator mapping")
                .expect("reachable operator replacement must be emitted");
            (emitted_ref, writer.get_buffer().expect("take output"))
        };

        let body = String::from_utf8_lossy(&output);
        let marker = format!(
            "{} {} obj\nDo\nendobj",
            emitted_ref.number, emitted_ref.generation
        );
        assert!(
            body.contains(&marker),
            "canonical writer must emit the replacement operator: {marker}\n{body}"
        );
    }

    #[test]
    fn set_object_inline_image_replacement_survives_canonical_full_rewrite() {
        let page_ref = ObjectRef::new(3, 0);
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.set_object(page_ref, Object::InlineImage(b"BI /W 1 ID x EI".to_vec()));

        let (emitted_ref, output) = {
            let mut writer = crate::PdfWriter::new(&mut pdf);
            writer.set_object_stream_mode(crate::ObjectStreamMode::Disable);
            writer.set_output_memory().expect("configure memory output");
            writer.write().expect("write inline-image replacement");
            let emitted_ref = writer
                .get_renumbered_obj_gen(page_ref)
                .expect("query inline-image mapping")
                .expect("reachable inline-image replacement must be emitted");
            (emitted_ref, writer.get_buffer().expect("take output"))
        };

        let body = String::from_utf8_lossy(&output);
        let marker = format!(
            "{} {} obj\nBI /W 1 ID x EI\nendobj",
            emitted_ref.number, emitted_ref.generation
        );
        assert!(
            body.contains(&marker),
            "canonical writer must emit the replacement inline image: {marker}\n{body}"
        );
    }

    #[test]
    fn set_object_over_depth_replacement_survives_canonical_full_rewrite() {
        let page_ref = ObjectRef::new(3, 0);
        let target_ref = ObjectRef::new(97, 0);
        let depth = crate::object::MAX_INLINE_DEPTH + 5;
        let mut replacement = Object::Reference(target_ref);
        for _ in 0..depth {
            replacement = Object::Array(vec![replacement]);
        }

        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.set_object(target_ref, Object::Integer(42));
        pdf.set_object(page_ref, replacement.clone());
        let (emitted_ref, emitted_target_ref, output) = {
            let mut writer = crate::PdfWriter::new(&mut pdf);
            writer.set_object_stream_mode(crate::ObjectStreamMode::Disable);
            writer.set_output_memory().expect("configure memory output");
            writer.write().expect("write over-depth replacement");
            let emitted_ref = writer
                .get_renumbered_obj_gen(page_ref)
                .expect("query over-depth mapping")
                .expect("reachable over-depth replacement must be emitted");
            let emitted_target_ref = writer
                .get_renumbered_obj_gen(target_ref)
                .expect("query nested target mapping")
                .expect("reference reachable through over-depth replacement must be emitted");
            (
                emitted_ref,
                emitted_target_ref,
                writer.get_buffer().expect("take output"),
            )
        };

        let mut reopened = Pdf::open_mem_owned(output).expect("reopen over-depth output");
        let mut leaf =
            resolved_object(&mut reopened, emitted_ref).expect("resolve over-depth replacement");
        for _ in 0..depth {
            leaf = match leaf {
                Object::Array(mut items) => items.remove(0),
                other => panic!("expected nested array, found {other:?}"), // cov:ignore: successful canonical rewrite emits the constructed array at every requested depth
            };
        }
        assert_eq!(leaf, Object::Reference(emitted_target_ref));
        assert_eq!(
            resolved_object(&mut reopened, emitted_target_ref).expect("resolve nested target"),
            Object::Integer(42)
        );
    }

    #[test]
    fn canonical_replacement_survives_without_legacy_materialization() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let replacement =
            ObjectHandle::dictionary(vec![(b"Marker".to_vec(), ObjectHandle::integer(779))]);
        let page_ref = ObjectRef::new(3, 0);
        pdf.replace_object(page_ref, replacement)
            .expect("replace page object canonically");

        let (emitted_ref, out) = {
            let mut writer = crate::PdfWriter::new(&mut pdf);
            writer.set_object_stream_mode(crate::ObjectStreamMode::Disable);
            writer.set_output_memory().expect("configure memory output");
            writer.write().expect("full rewrite");
            let emitted_ref = writer
                .get_renumbered_obj_gen(page_ref)
                .expect("query replacement mapping")
                .expect("replacement must be emitted");
            (emitted_ref, writer.get_buffer().expect("take output"))
        };

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        assert_eq!(
            resolved_object(&mut reopened, emitted_ref)
                .expect("resolve replacement")
                .into_dict()
                .and_then(|dict| dict.get("Marker").cloned())
                .and_then(|object| object.as_integer()),
            Some(779)
        );
    }

    #[test]
    fn canonical_removal_emits_a_null_child_without_legacy_materialization() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let pages = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.resolve(&pages).expect("resolve pages");
        let kids = pages.get_key(b"/Kids");
        assert_eq!(kids.try_array_len().expect("read page kids"), Some(1));

        pdf.remove_object_handle(ObjectRef::new(3, 0))
            .expect("remove page canonically");

        let out = {
            let mut writer = crate::PdfWriter::new(&mut pdf);
            writer.set_object_stream_mode(crate::ObjectStreamMode::Disable);
            writer.set_output_memory().expect("configure memory output");
            writer.write().expect("full rewrite");
            writer.get_buffer().expect("take output")
        };

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let rewritten_pages = resolved_object(&mut reopened, ObjectRef::new(2, 0))
            .expect("resolve rewritten pages")
            .into_dict()
            .expect("pages is a dictionary");
        let kids = rewritten_pages
            .get("Kids")
            .and_then(Object::as_array)
            .expect("rewritten pages has kids");
        assert_eq!(kids, &[Object::Null]);
        assert_eq!(
            resolved_object(&mut reopened, ObjectRef::new(3, 0))
                .expect("removed page resolves as qpdf null"),
            Object::Null,
            "removed page must not be emitted as an indirect body"
        );
    }

    #[test]
    fn canonical_mutator_rejects_a_foreign_indirect_child_before_writing() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open target");
        let mut foreign_pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open foreign");
        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&root).expect("resolve target root");
        let foreign = foreign_pdf.get_object_handle(ObjectRef::new(3, 0));
        let error = root
            .replace_key(b"/Foreign", foreign)
            .expect_err("foreign child must be rejected at the canonical mutation boundary");

        assert!(error
            .to_string()
            .contains("Attempting to add an object from a different QPDF"));
        assert!(!root.has_key(b"/Foreign"));
    }

    #[test]
    fn make_indirect_object_handle_is_visible_to_qpdf_json_without_materializing_cache() {
        // qpdf's QPDF::makeIndirectFromQPDFObject stores the same shared
        // QPDFObject in obj_cache. The handle registry is Rust's equivalent
        // shared state, so JSON preparation must see the new object without
        // cloning a stream payload into the legacy Object cache.
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let indirect = pdf
            .make_indirect_object_handle(ObjectHandle::stream(
                ObjectHandle::dictionary(vec![]),
                Rc::new(b"stream payload must remain handle-owned".to_vec()),
            ))
            .expect("make indirect");
        let object_ref = indirect.object_ref().expect("indirect ref");
        assert!(
            pdf.cache.entry(object_ref).is_none(),
            "a handle-created object must not be materialized into the legacy cache"
        );

        let prepared = pdf
            .prepare_qpdf_json_objects()
            .expect("prepare qpdf JSON objects");
        assert!(prepared.refs.contains(&object_ref));
        let object = pdf
            .resolve_qpdf_json_object(object_ref)
            .expect("resolve qpdf JSON object");
        assert_eq!(
            object.as_stream().expect("handle-created stream").data,
            b"stream payload must remain handle-owned"
        );
    }

    #[test]
    fn qpdf_json_resolution_materializes_a_handle_only_object_on_demand() {
        // The borrowed JSON path needs a stable Object reference, but a
        // handle-created object must still avoid a cache payload clone until
        // a caller actually asks to inspect it.
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let indirect = pdf
            .make_indirect_object_handle(ObjectHandle::integer(778))
            .expect("make indirect");
        let object_ref = indirect.object_ref().expect("indirect ref");

        assert!(pdf.cache.entry(object_ref).is_none());
        assert_eq!(
            pdf.resolve_qpdf_json_object(object_ref)
                .expect("borrow qpdf JSON object"),
            Object::Integer(778)
        );
        assert!(
            pdf.cache.entry(object_ref).is_none(),
            "on-demand borrowing must not populate the legacy cache"
        );
    }

    #[test]
    fn qpdf_json_mutated_cache_paths_reenter_canonical_resolution() {
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog >>\nendobj\n",
                b"2 0 obj\n42\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");

        // A raw cache entry can already be resolved while its canonical handle
        // remains unresolved. The qpdf-JSON path must resolve that handle before
        // materializing the value, rather than trusting the stale raw snapshot.
        let first_ref = ObjectRef::new(1, 0);
        pdf.handle_mutated_object_refs.insert(first_ref);
        pdf.cache.set_resolved(first_ref, Object::Null);
        let mut expected_catalog = Dictionary::new();
        expected_catalog.insert("Type", Object::Name(b"Catalog".to_vec()));
        assert_eq!(
            pdf.resolve_qpdf_json_object(first_ref)
                .expect("canonical JSON resolution"),
            Object::Dictionary(expected_catalog)
        );

        // The second path reaches the same canonical re-entry after its raw
        // cache read installs a fresh resolved entry.
        let second_ref = ObjectRef::new(2, 0);
        pdf.handle_mutated_object_refs.insert(second_ref);
        assert_eq!(
            pdf.resolve_qpdf_json_object(second_ref)
                .expect("borrowed canonical JSON resolution"),
            Object::Integer(42)
        );
    }

    #[test]
    fn mark_object_dirty_makes_a_replace_key_mutation_survive_a_full_rewrite() {
        // Regression test: ObjectHandle::replace_key mutates the live
        // handle graph directly and has no path back to Pdf's dirty
        // bookkeeping. Without an explicit `mark_object_dirty` call, the
        // canonical full-rewrite walk must observe the live handle graph.

        let page_ref = ObjectRef::new(3, 0);
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve(&page).expect("resolve page");
        page.replace_key(b"/Rotate", ObjectHandle::integer(90))
            .unwrap();
        pdf.mark_object_dirty(page_ref);

        let mut writer = crate::PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(crate::ObjectStreamMode::Disable);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("full rewrite");
        let out = writer.get_buffer().expect("take full-rewrite output");

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let resolved_page = resolved_object(&mut reopened, page_ref)
            .expect("resolve page in reopened output")
            .into_dict()
            .expect("page is a dictionary");
        assert_eq!(
            resolved_page.get("Rotate"),
            Some(&Object::Integer(90)),
            "replace_key mutation must survive a full rewrite once marked dirty"
        );
    }

    #[test]
    fn provider_replacement_on_loaded_stream_survives_marked_full_rewrite() {
        let stream_ref = ObjectRef::new(4, 0);
        let mut pdf = Pdf::open_mem_owned(pdf_with_one_stream(b"original payload"))
            .expect("open stream fixture");
        let stream = pdf.get_object_handle(stream_ref);
        pdf.resolve(&stream).expect("resolve loaded stream");

        stream
            .replace_stream_data_with_callback(
                |pipeline| {
                    pipeline
                        .write(b"provider replacement")
                        .map_err(Error::from)?;
                    pipeline.finish().map_err(Error::from)
                },
                None,
                None,
            )
            .expect("register provider on loaded stream");
        assert!(
            pdf.dirty_object_refs().is_empty(),
            "handle mutation remains explicitly dirty-marked"
        );
        pdf.mark_object_handle_dirty(&stream)
            .expect("mark provider stream dirty");

        let mut writer = crate::PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(crate::ObjectStreamMode::Disable);
        writer.set_preserve_unreferenced_objects(true);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("write provider-backed stream");
        let output = writer.get_buffer().expect("take rewritten output");

        let mut reopened = Pdf::open_mem_owned(output).expect("reopen rewritten output");
        let rewritten_stream = reopened.get_object_handle(stream_ref);
        reopened
            .resolve(&rewritten_stream)
            .expect("resolve rewritten stream");
        assert_eq!(
            rewritten_stream
                .get_stream_data(crate::writer::DecodeLevel::Generalized)
                .expect("decode provider bytes from rewritten stream")
                .as_slice(),
            b"provider replacement"
        );
    }

    fn classic_pdf_with_bodies(bodies: &[&[u8]], root: ObjectRef) -> Vec<u8> {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for body in bodies {
            offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(body);
        }
        let size = bodies.len() + 1;
        let xref_start = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {} {} R >>\nstartxref\n{xref_start}\n%%EOF\n",
                root.number, root.generation
            )
            .as_bytes(),
        );
        pdf
    }

    fn recovered_stream_fixture(
        length_entry: &[u8],
        framing_eol: &[u8],
        holder_body: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut stream_body = b"1 0 obj\n<<".to_vec();
        if !length_entry.is_empty() {
            stream_body.push(b' ');
            stream_body.extend_from_slice(length_entry);
        }
        stream_body.extend_from_slice(b" >>\nstream\nabc");
        stream_body.extend_from_slice(framing_eol);
        stream_body.extend_from_slice(b"endstream\nendobj\n");

        match holder_body {
            Some(holder_body) => classic_pdf_with_bodies(
                &[stream_body.as_slice(), holder_body],
                ObjectRef::new(1, 0),
            ),
            None => classic_pdf_with_bodies(&[stream_body.as_slice()], ObjectRef::new(1, 0)),
        }
    }

    #[test]
    fn endstream_scan_metadata_survives_every_non_authoritative_length() {
        for (eol, expected) in [
            (&b"\n"[..], &b"\n"[..]),
            (&b"\r"[..], &b"\r"[..]),
            (&b"\r\n"[..], &b"\r\n"[..]),
        ] {
            for length_entry in [&b""[..], &b"/Length /Bad"[..], &b"/Length null"[..]] {
                let bytes = recovered_stream_fixture(length_entry, eol, None);
                let mut pdf = Pdf::open_mem_owned(bytes).expect("open direct-length fixture");
                let object_ref = ObjectRef::new(1, 0);
                let stream =
                    resolved_object(&mut pdf, object_ref).expect("resolve recovered stream");
                assert_eq!(
                    stream.as_stream().unwrap().data,
                    [&b"abc"[..], expected].concat()
                );
                assert_eq!(
                    canonical_recovered_eol(&mut pdf, object_ref).expect("canonical recovered EOL"),
                    Some(expected)
                );
            }
        }

        for (holder_ref, holder_body, delete_holder) in [
            (
                b"2 0 R".as_slice(),
                Some(b"2 0 obj\nnull\nendobj\n".as_slice()),
                false,
            ),
            (
                b"2 0 R".as_slice(),
                Some(b"2 0 obj\n/Bad\nendobj\n".as_slice()),
                false,
            ),
            (
                b"2 0 R".as_slice(),
                Some(b"2 0 obj\n99\nendobj\n".as_slice()),
                false,
            ),
            (b"99 0 R".as_slice(), None, false),
            (
                b"2 0 R".as_slice(),
                Some(b"2 0 obj\n3\nendobj\n".as_slice()),
                true,
            ),
        ] {
            let mut length_entry = b"/Length ".to_vec();
            length_entry.extend_from_slice(holder_ref);
            let bytes = recovered_stream_fixture(&length_entry, b"\n", holder_body);
            let mut pdf = Pdf::open_mem_owned(bytes).expect("open indirect-length fixture");
            if delete_holder {
                pdf.delete_object(ObjectRef::new(2, 0));
            }
            let object_ref = ObjectRef::new(1, 0);
            let stream = resolved_object(&mut pdf, object_ref).expect("resolve recovered stream");
            assert_eq!(stream.as_stream().unwrap().data, b"abc\n");
            assert_eq!(
                canonical_recovered_eol(&mut pdf, object_ref).expect("canonical recovered EOL"),
                Some(&b"\n"[..])
            );
        }
    }

    #[test]
    fn qpdf_reader_reports_recoverable_name_warning_once() {
        let bytes = classic_pdf_with_bodies(&[b"1 0 obj\n/a#1x\nendobj\n"], ObjectRef::new(1, 0));
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stray-name fixture");
        let object_ref = ObjectRef::new(1, 0);

        assert_eq!(
            resolved_object(&mut pdf, object_ref).expect("recover stray name"),
            Object::Name(b"a\0\x31x".to_vec())
        );
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec!["(object 1 0, offset 17): name with stray # will not work with PDF >= 1.2"]
        );

        assert_eq!(
            resolved_object(&mut pdf, object_ref).unwrap(),
            Object::Name(b"a\0\x31x".to_vec())
        );
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
    }

    #[test]
    fn qpdf_reader_reports_compressed_member_name_warning_once() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 9 >>\nstream\n7 0 /a#1x\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open compressed stray-name fixture");
        let object_ref = ObjectRef::new(7, 0);
        pdf.cache.set_compressed(object_ref, 1, 0);
        pdf.resolver.insert_xref_entry(
            object_ref,
            XrefEntry::Compressed {
                stream: 1,
                index: 0,
            },
        );

        assert_eq!(
            resolved_object(&mut pdf, object_ref).expect("recover compressed stray name"),
            Object::Name(b"a\0\x31x".to_vec())
        );
        let snapshot = pdf.repair_diagnostics();
        let diagnostics = snapshot.entries();
        assert_eq!(
            diagnostics
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "object stream 1 (object 7 0, offset 4): name with stray # will not work with PDF >= 1.2"
            ]
        );

        assert_eq!(
            resolved_object(&mut pdf, object_ref).unwrap(),
            Object::Name(b"a\0\x31x".to_vec())
        );
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
    }

    #[test]
    fn compressed_member_warning_sink_failure_propagates_after_collection() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 9 >>\nstream\n7 0 /a#1x\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open compressed stray-name fixture");
        let object_ref = ObjectRef::new(7, 0);
        pdf.cache.set_compressed(object_ref, 1, 0);
        pdf.resolver.insert_xref_entry(
            object_ref,
            XrefEntry::Compressed {
                stream: 1,
                index: 0,
            },
        );
        fail_warning_delivery(&mut pdf);

        assert!(matches!(
            resolved_object(&mut pdf, object_ref),
            Err(crate::Error::System(ref message)) if message == "sink write failure 1"
        ));
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
    }

    #[test]
    fn valid_indirect_stream_length_clears_endstream_scan_metadata() {
        let bytes =
            recovered_stream_fixture(b"/Length 2 0 R", b"\n", Some(b"2 0 obj\n3\nendobj\n"));
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open valid indirect-length fixture");
        let object_ref = ObjectRef::new(1, 0);
        let stream = resolved_object(&mut pdf, object_ref).expect("resolve recovered stream");
        assert_eq!(stream.as_stream().unwrap().data, b"abc");
        assert_eq!(
            canonical_recovered_eol(&mut pdf, object_ref).expect("canonical recovered EOL"),
            None
        );
    }

    #[test]
    fn qpdf_reader_completes_adjacent_endstream_before_endobj_check() {
        let bytes = recovered_stream_fixture(b"/Length 2 0 R", b"", Some(b"2 0 obj\n3\nendobj\n"));
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let object_ref = ObjectRef::new(1, 0);
        assert_eq!(
            resolved_object(&mut pdf, object_ref)
                .unwrap()
                .as_stream()
                .unwrap()
                .data,
            b"abc"
        );
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("expected endobj")));
    }

    #[test]
    fn qpdf_reader_registers_file_object_diagnostics_once_after_cache_commit() {
        let mut pdf = Pdf::open_mem_owned(top_level_bare_reference_pdf()).unwrap();
        let object_ref = ObjectRef::new(4, 0);
        assert_eq!(
            resolved_object(&mut pdf, object_ref).unwrap(),
            Object::Integer(3)
        );
        assert_eq!(
            pdf.resolve_qpdf_json_object(object_ref).unwrap(),
            Object::Integer(3)
        );
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .filter(|entry| entry.message.contains("expected endobj"))
                .count(),
            1
        );
    }

    #[test]
    fn qpdf_reader_bounds_unusable_indirect_length_recovery() {
        let cases = [
            classic_pdf_with_bodies(
                &[b"1 0 obj\n<< /Length 1 0 R >>\nstream\nabc\nendstream\nendobj\n"],
                ObjectRef::new(1, 0),
            ),
            classic_pdf_with_bodies(
                &[b"1 0 obj\n<< /Length 99 0 R >>\nstream\nabc\nendstream\nendobj\n"],
                ObjectRef::new(1, 0),
            ),
            classic_pdf_with_bodies(
                &[
                    b"1 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n",
                    b"2 0 obj\n<< /Length 1 0 R >>\nstream\nxyz\nendstream\nendobj\n",
                ],
                ObjectRef::new(1, 0),
            ),
        ];

        for bytes in cases {
            let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
            assert_eq!(
                resolved_object(&mut pdf, ObjectRef::new(1, 0))
                    .unwrap()
                    .as_stream()
                    .unwrap()
                    .data,
                b"abc\n"
            );
        }
    }

    #[test]
    fn source_stream_data_offset_comes_from_parsed_object_framing() {
        let stream_body = b"1 0 obj\n\
                            << /Note (first\nstream\nsecond) /Length 3 >>\n\
                            stream\nabc\nendstream\nendobj\n";
        let direct_body = b"2 0 obj\ntrue\nendobj\n";
        let bytes = classic_pdf_with_bodies(&[stream_body, direct_body], ObjectRef::new(1, 0));
        let expected = bytes
            .windows(b"\nstream\nabc".len())
            .rposition(|window| window == b"\nstream\nabc")
            .expect("real stream marker")
            + b"\nstream\n".len();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream-offset fixture");

        assert_eq!(
            pdf.source_stream_data_offset(ObjectRef::new(1, 0))
                .expect("read parsed stream offset"),
            Some(expected as u64)
        );
        assert_eq!(
            pdf.source_stream_data_offset(ObjectRef::new(2, 0))
                .expect("direct object has no source stream offset"),
            None
        );
        assert_eq!(
            pdf.source_stream_data_offset(ObjectRef::new(99, 0))
                .expect("unknown object has no source stream offset"),
            None
        );
    }

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn qtest_decode_parms_offsets_follow_filter_array_items() {
        let stream_body = b"1 0 obj\n\
                            << /Filter [ /FlateDecode /LZWDecode ] \
                               /DecodeParms [ null 42 ] /Length 0 >>\n\
                            stream\n\nendstream\nendobj\n";
        let catalog_body = b"2 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let bytes = classic_pdf_with_bodies(&[stream_body, catalog_body], ObjectRef::new(2, 0));
        let expected = bytes
            .windows(b"/DecodeParms [ null 42 ]".len())
            .position(|window| window == b"/DecodeParms [ null 42 ]")
            .expect("DecodeParms array")
            + b"/DecodeParms [ null ".len();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open DecodeParms-offset fixture");

        assert_eq!(
            pdf.qtest_decode_parms_source_offset(ObjectRef::new(1, 0), 1)
                .expect("second array item offset"),
            Some(expected as u64)
        );
        assert_eq!(
            pdf.qtest_decode_parms_source_offset(ObjectRef::new(1, 0), 2)
                .expect("missing array item"),
            None
        );
        assert_eq!(
            pdf.qtest_decode_parms_source_offset(ObjectRef::new(2, 0), 0)
                .expect("last direct object"),
            None
        );
        assert_eq!(
            pdf.qtest_decode_parms_source_offset(ObjectRef::new(99, 0), 0)
                .expect("unknown object"),
            None
        );
    }

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn qtest_decode_parms_offsets_retry_after_false_next_object_offset() {
        let bytes = stream_with_false_next_xref_offset();
        let expected = bytes
            .windows(b"[ null ]".len())
            .position(|window| window == b"[ null ]")
            .expect("DecodeParms array")
            + b"[ ".len();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open false-next-offset PDF");

        assert_eq!(
            pdf.qtest_decode_parms_source_offset(ObjectRef::new(1, 0), 0)
                .expect("offset lookup uses the same bounded fallback"),
            Some(expected as u64)
        );
    }

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn qtest_decode_parms_offsets_preserve_bounded_retry_cap() {
        let mut pdf = Pdf::open_mem_owned(stream_with_false_next_xref_offset())
            .expect("open false-next-offset PDF");
        pdf.resolution_fallbacks_remaining = 0;

        assert!(pdf
            .qtest_decode_parms_source_offset(ObjectRef::new(1, 0), 0)
            .is_err());
        assert_eq!(pdf.resolution_fallbacks_remaining, 0);
    }

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn qtest_object_value_source_offset_locates_the_position_right_after_obj() {
        // qpdf records an indirect object's own "parsed offset" once, right
        // after `N G obj`, before parsing its value — not at the value
        // token's own start. Extra whitespace/comments between `obj` and
        // the value must not shift the reported offset.
        let stream_body = b"1 0 obj\n\
                            << /Filter /FlateDecode /DecodeParms 3 0 R /Length 0 >>\n\
                            stream\n\nendstream\nendobj\n";
        let catalog_body = b"2 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let scalar_body = b"3 0 obj\n  % a comment\n  42\nendobj\n";
        let bytes = classic_pdf_with_bodies(
            &[stream_body, catalog_body, scalar_body],
            ObjectRef::new(2, 0),
        );
        let expected = bytes
            .windows(b"3 0 obj".len())
            .rposition(|window| window == b"3 0 obj")
            .expect("scalar object")
            + b"3 0 obj".len();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open indirect-scalar fixture");

        assert_eq!(
            pdf.qtest_object_value_source_offset(ObjectRef::new(3, 0))
                .expect("position right after obj"),
            Some(expected as u64)
        );
        assert_eq!(
            pdf.qtest_object_value_source_offset(ObjectRef::new(99, 0))
                .expect("unknown object has no source offset"),
            None
        );
    }

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn qtest_array_item_source_offset_locates_precise_item_positions() {
        let stream_body = b"1 0 obj\n\
                            << /Filter [ /LZWDecode /FlateDecode ] /DecodeParms 3 0 R \
                               /Length 0 >>\n\
                            stream\n\nendstream\nendobj\n";
        let catalog_body = b"2 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let array_body = b"3 0 obj\n[ 9 9 ]\nendobj\n";
        let bytes = classic_pdf_with_bodies(
            &[stream_body, catalog_body, array_body],
            ObjectRef::new(2, 0),
        );
        let first = bytes
            .windows(b"[ 9 9 ]".len())
            .position(|window| window == b"[ 9 9 ]")
            .expect("array object")
            + b"[ ".len();
        let second = first + b"9 ".len();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open indirect-array fixture");

        assert_eq!(
            pdf.qtest_array_item_source_offset(ObjectRef::new(3, 0), 0)
                .expect("first item offset"),
            Some(first as u64)
        );
        assert_eq!(
            pdf.qtest_array_item_source_offset(ObjectRef::new(3, 0), 1)
                .expect("second item offset"),
            Some(second as u64)
        );
        assert_eq!(
            pdf.qtest_array_item_source_offset(ObjectRef::new(3, 0), 2)
                .expect("missing item index"),
            None
        );
        assert_eq!(
            pdf.qtest_array_item_source_offset(ObjectRef::new(1, 0), 0)
                .expect("non-array body"),
            None
        );
        assert_eq!(
            pdf.qtest_array_item_source_offset(ObjectRef::new(99, 0), 0)
                .expect("unknown object"),
            None
        );
    }

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn qtest_array_item_source_offsets_batch_duplicate_indices_with_one_retry() {
        let catalog_body = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let pages_body = b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n";
        let array_body = b"3 0 obj\n[ null null ]\nendobj\n";
        let bytes = classic_pdf_with_bodies(
            &[catalog_body, pages_body, array_body],
            ObjectRef::new(1, 0),
        );
        let array_offset = bytes
            .windows(b"3 0 obj".len())
            .position(|window| window == b"3 0 obj")
            .expect("array object") as u64;
        let false_next_offset = array_offset + b"3 0 ".len() as u64;
        let first = bytes
            .windows(b"[ null null ]".len())
            .position(|window| window == b"[ null null ]")
            .expect("array value")
            + b"[ ".len();
        let second = first + b"null ".len();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open indirect-array fixture");
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();
        let initial_budget = pdf.resolution_fallbacks_remaining;

        assert_eq!(
            pdf.qtest_array_item_source_offsets(ObjectRef::new(3, 0), &[0, 1, 0])
                .expect("batch array offset lookup"),
            vec![Some(first as u64), Some(second as u64), Some(first as u64)]
        );
        assert_eq!(
            pdf.resolution_fallbacks_remaining,
            initial_budget.saturating_sub(1),
            "one source container must consume one fallback retry"
        );
    }

    #[cfg(feature = "qtest-driver")]
    #[test]
    fn qtest_object_value_source_offsets_batch_duplicate_refs_with_one_retry() {
        let catalog_body = b"1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let pages_body = b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n";
        let scalar_body = b"3 0 obj\n42\nendobj\n";
        let bytes = classic_pdf_with_bodies(
            &[catalog_body, pages_body, scalar_body],
            ObjectRef::new(1, 0),
        );
        let scalar_offset = bytes
            .windows(b"3 0 obj".len())
            .position(|window| window == b"3 0 obj")
            .expect("scalar object") as u64;
        let false_next_offset = scalar_offset + b"3 0 ".len() as u64;
        let expected = scalar_offset + b"3 0 obj".len() as u64;
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open indirect-scalar fixture");
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();
        let initial_budget = pdf.resolution_fallbacks_remaining;
        let object_ref = ObjectRef::new(3, 0);

        assert_eq!(
            pdf.qtest_object_value_source_offsets(&[object_ref, object_ref])
                .expect("batch object offset lookup"),
            vec![Some(expected), Some(expected)]
        );
        assert_eq!(
            pdf.resolution_fallbacks_remaining,
            initial_budget.saturating_sub(1),
            "one source object must consume one fallback retry"
        );
    }

    #[test]
    fn source_stream_data_offset_retries_after_false_next_object_offset() {
        let bytes = stream_with_false_next_xref_offset();
        let expected = bytes
            .windows(b"\nstream\nabc".len())
            .position(|window| window == b"\nstream\nabc")
            .expect("stream marker")
            + b"\nstream\n".len();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open false-next-offset PDF");

        assert!(matches!(
            resolved_object(&mut pdf, ObjectRef::new(1, 0)).expect("ordinary bounded fallback"),
            Object::Stream(_)
        ));
        assert_eq!(
            pdf.source_stream_data_offset(ObjectRef::new(1, 0))
                .expect("offset lookup uses the same fallback"),
            Some(expected as u64)
        );
    }

    #[test]
    fn source_stream_data_offset_preserves_bounded_retry_cap() {
        let mut pdf = Pdf::open_mem_owned(stream_with_false_next_xref_offset())
            .expect("open false-next-offset PDF");
        pdf.resolution_fallbacks_remaining = 0;

        assert!(pdf.source_stream_data_offset(ObjectRef::new(1, 0)).is_err());
        assert_eq!(pdf.resolution_fallbacks_remaining, 0);
    }

    #[test]
    fn qpdf_object_resolution_covers_mismatch_indirect_length_compressed_and_absent() {
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n",
                b"2 0 obj\n3\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        );
        let first_offset = bytes
            .windows(b"1 0 obj".len())
            .position(|window| window == b"1 0 obj")
            .unwrap() as u64;
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open indirect-length fixture");
        let stream = pdf
            .resolve_qpdf_json_object(ObjectRef::new(1, 0))
            .expect("resolve qpdf stream");
        assert_eq!(stream.as_stream().unwrap().data, b"abc");

        let mismatched = ObjectRef::new(8, 0);
        pdf.cache.set_unresolved(mismatched, first_offset);
        assert_eq!(
            pdf.resolve_qpdf_json_object(mismatched).unwrap(),
            Object::Null
        );

        let mut objstm_dict = Dictionary::new();
        objstm_dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        objstm_dict.insert("N", Object::Integer(1));
        objstm_dict.insert("First", Object::Integer(4));
        let objstm_ref = ObjectRef::new(4, 0);
        pdf.set_object(
            objstm_ref,
            Object::Stream(Stream::new(objstm_dict, b"7 0 << /Value 1 >>".to_vec())),
        );
        let compressed_ref = ObjectRef::new(7, 0);
        pdf.cache.set_compressed(compressed_ref, 4, 0);
        pdf.resolver.insert_xref_entry(
            compressed_ref,
            XrefEntry::Compressed {
                stream: 4,
                index: 0,
            },
        );
        assert!(matches!(
            pdf.resolve_qpdf_json_object(compressed_ref).unwrap(),
            Object::Dictionary(_)
        ));
        assert_eq!(
            pdf.resolve_qpdf_json_object(ObjectRef::new(99, 0)).unwrap(),
            Object::Null
        );

        let invalid_length = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Length 2 0 R >>\nstream\nabcendstream\nendobj\n",
                b"2 0 obj\n99\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(invalid_length).expect("open invalid-length fixture");
        let object_ref = ObjectRef::new(1, 0);
        assert_eq!(
            pdf.resolve_qpdf_json_object(object_ref)
                .unwrap()
                .as_stream()
                .unwrap()
                .data,
            b"abc"
        );
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "(object 1 0, offset 143): expected endstream",
                "(object 1 0, offset 44): attempting to recover stream length",
                "(object 1 0, offset 44): recovered stream length: 3",
            ]
        );
        assert_eq!(
            pdf.resolve_qpdf_json_object(object_ref)
                .unwrap()
                .as_stream()
                .unwrap()
                .data,
            b"abc"
        );
        assert_eq!(pdf.repair_diagnostics().entries().len(), 3);
    }

    #[test]
    fn legacy_compressed_resolution_resolves_indirect_objstm_decode_parms() {
        let decoded_objstm = b"7 0 << /Value 1 >>";
        let mut predictor_input = vec![0];
        predictor_input.extend_from_slice(decoded_objstm);
        let encoded_objstm = flate_encoded(&predictor_input);
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        stream_dict.insert("N", Object::Integer(1));
        stream_dict.insert("First", Object::Integer(4));
        stream_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        stream_dict.insert("DecodeParms", Object::Reference(ObjectRef::new(5, 0)));

        let decode_parms = format!(
            "5 0 obj\n<< /Predictor 12 /Columns {} /Colors 1 /BitsPerComponent 8 >>\nendobj\n",
            decoded_objstm.len()
        )
        .into_bytes();
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog >>\nendobj\n",
                b"2 0 obj\nnull\nendobj\n",
                b"3 0 obj\nnull\nendobj\n",
                b"4 0 obj\nnull\nendobj\n",
                decode_parms.as_slice(),
            ],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open indirect DecodeParms fixture");
        stream_dict.insert("Length", Object::Integer(encoded_objstm.len() as i64));
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Stream(Stream::new(stream_dict, encoded_objstm)),
        );
        pdf.cache.set_compressed(ObjectRef::new(7, 0), 4, 0);
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(7, 0),
            XrefEntry::Compressed {
                stream: 4,
                index: 0,
            },
        );

        let object = pdf
            .resolve_qpdf_json_object(ObjectRef::new(7, 0))
            .expect("indirect DecodeParms must resolve through the canonical stream handle");
        assert_eq!(
            object.as_dict().and_then(|dict| dict.get("Value")),
            Some(&Object::Integer(1))
        );
    }

    #[test]
    fn compressed_resolution_rejects_a_stale_source_xref() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let object_ref = ObjectRef::new(7, 0);
        pdf.cache.set_compressed(object_ref, 4, 0);
        pdf.resolver.insert_xref_entry(
            object_ref,
            XrefEntry::Compressed {
                stream: 5,
                index: 0,
            },
        );

        assert_eq!(
            pdf.resolve_qpdf_json_object(object_ref)
                .expect("stale source must be treated as unresolved"),
            Object::Null
        );
    }

    #[test]
    fn qpdf_json_resolution_observes_a_handle_dictionary_mutation() {
        let object_ref = ObjectRef::new(1, 0);
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Value 1 >>\nendobj\n"],
            object_ref,
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");

        // Populate the legacy qpdf-JSON cache before the live handle changes.
        assert_eq!(
            pdf.resolve_qpdf_json_object(object_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Value"),
            Some(&Object::Integer(1))
        );
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).unwrap();
        handle
            .replace_key(b"/Value", ObjectHandle::integer(2))
            .unwrap();
        pdf.mark_object_dirty(object_ref);

        assert_eq!(
            pdf.resolve_qpdf_json_object(object_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Value"),
            Some(&Object::Integer(2))
        );
        assert_eq!(
            pdf.resolve_qpdf_json_object(object_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Value"),
            Some(&Object::Integer(2))
        );
    }

    #[test]
    fn marking_dirty_invalidates_an_ordinary_handle_materialization() {
        let object_ref = ObjectRef::new(1, 0);
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Value 1 >>\nendobj\n"],
            object_ref,
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        assert_eq!(
            resolved_object(&mut pdf, object_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Value"),
            Some(&Object::Integer(1))
        );

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)
            .expect("resolve the canonical handle before mutating it");
        handle
            .replace_key(b"/Value", ObjectHandle::integer(2))
            .unwrap();
        pdf.mark_object_dirty(object_ref);

        assert_eq!(
            resolved_object(&mut pdf, object_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Value"),
            Some(&Object::Integer(2))
        );
    }

    #[test]
    fn mark_object_handle_dirty_rejects_a_foreign_indirect_handle() {
        let object_ref = ObjectRef::new(1, 0);
        let bytes = classic_pdf_with_bodies(&[b"1 0 obj\n1\nendobj\n"], object_ref);
        let mut source = Pdf::open_mem_owned(bytes.clone()).expect("open source");
        let foreign = source.get_object_handle(object_ref);
        let mut destination = Pdf::open_mem_owned(bytes).expect("open destination");

        assert_eq!(
            destination
                .mark_object_handle_dirty(&foreign)
                .expect_err("foreign handle must not select a same-number destination object")
                .to_string(),
            "unsupported PDF feature: ObjectHandle belongs to another Pdf"
        );
    }

    #[test]
    fn mark_object_handle_dirty_rejects_a_foreign_direct_child() {
        let object_ref = ObjectRef::new(1, 0);
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Child << /Value 1 >> >>\nendobj\n"],
            object_ref,
        );
        let mut source = Pdf::open_mem_owned(bytes.clone()).expect("open source");
        let source_owner = source.get_object_handle(object_ref);
        source.resolve(&source_owner).unwrap();
        let foreign = source_owner.get_key(b"/Child");
        assert!(foreign.is_direct());
        let mut destination = Pdf::open_mem_owned(bytes).expect("open destination");

        assert_eq!(
            destination
                .mark_object_handle_dirty(&foreign)
                .expect_err("foreign direct child must not select a destination owner")
                .to_string(),
            "unsupported PDF feature: ObjectHandle belongs to another Pdf"
        );
    }

    #[test]
    fn mark_object_handle_dirty_finds_a_nested_direct_owner() {
        let object_ref = ObjectRef::new(1, 0);
        let bytes =
            classic_pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n"], object_ref);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let owner = pdf.get_object_handle(object_ref);
        pdf.resolve(&owner).unwrap();
        let inner = ObjectHandle::integer(42);
        let container = ObjectHandle::dictionary(vec![(b"Inner".to_vec(), inner.clone())]);
        owner.replace_key(b"/Container", container).unwrap();
        pdf.clear_dirty(object_ref);

        pdf.mark_object_handle_dirty(&inner).unwrap();
        assert!(pdf.is_dirty(object_ref));
    }

    #[test]
    fn mark_object_handle_dirty_finds_the_owner_through_an_array_cursor() {
        let object_ref = ObjectRef::new(1, 0);
        let bytes =
            classic_pdf_with_bodies(&[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n"], object_ref);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let owner = pdf.get_object_handle(object_ref);
        pdf.resolve(&owner).unwrap();
        let item = ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(1))]);
        let container = ObjectHandle::array(vec![item]);
        owner.replace_key(b"/Kids", container).unwrap();
        pdf.clear_dirty(object_ref);

        let kids = owner.get_key(b"/Kids");
        let items = kids.try_array_items().expect("array cursor");
        let mut cursor = items.begin();
        let via_cursor = cursor.current();
        via_cursor
            .replace_key(b"/Value", ObjectHandle::integer(2))
            .unwrap();

        pdf.mark_object_handle_dirty(&via_cursor).unwrap();
        assert!(
            pdf.is_dirty(object_ref),
            "a cursor-derived direct child must retain its containment provenance"
        );
    }

    #[test]
    fn detached_direct_child_does_not_dirty_owner_but_live_removal_is_written() {
        let owner_ref = ObjectRef::new(1, 0);
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Child << /Value 1 >> >>\nendobj\n"],
            owner_ref,
        );
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("open fixture");
        let owner = pdf.get_object_handle(owner_ref);
        pdf.resolve(&owner).unwrap();
        let child = owner.get_key(b"/Child");

        owner.remove_key(b"/Child");
        pdf.clear_dirty(owner_ref);
        child
            .replace_key(b"/Value", ObjectHandle::integer(2))
            .unwrap();
        pdf.mark_object_handle_dirty(&child).unwrap();

        assert!(!pdf.is_dirty(owner_ref));
        let mut writer = crate::PdfWriter::new(&mut pdf);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("full rewrite");
        let out = writer.get_buffer().expect("take full-rewrite output");
        assert_eq!(
            out.windows(b"1 0 obj\n".len())
                .filter(|window| *window == b"1 0 obj\n")
                .count(),
            1,
            "the detached child's former owner must be emitted only once"
        );

        let mut reopened = Pdf::open_mem_owned(out).expect("reopen full-rewrite output");
        let reopened_owner = reopened.get_object_handle(owner_ref);
        reopened.resolve(&reopened_owner).unwrap();
        assert!(
            reopened_owner.get_key(b"/Child").is_null(),
            "canonical writer must emit the live owner's remove_key mutation"
        );
    }

    fn top_level_bare_reference_pdf() -> Vec<u8> {
        classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Probe 4 0 R >>\nendobj\n",
                b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
                b"3 0 obj\n99\nendobj\n",
                b"4 0 obj\n3 0 R\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        )
    }

    fn stream_with_false_next_xref_offset() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let stream_offset = bytes.len();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Filter [ /FlateDecode /FlateDecode ] /DecodeParms [ null ] /Length 3 >>\nstream\nabc\nendstream\nendobj\n",
        );
        let false_next = stream_offset + b"1 0 obj\n<< /Filter".len();
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 3\n\
                 0000000000 65535 f \n\
                 {stream_offset:010} 00000 n \n\
                 {false_next:010} 00000 n \n\
                 trailer\n<< /Size 3 /Root 1 0 R /QTest 1 0 R >>\n\
                 startxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn normal_and_json_resolution_share_qpdf_file_object_value_and_warning() {
        let object_ref = ObjectRef::new(4, 0);

        let mut normal_first =
            Pdf::open_mem_owned(top_level_bare_reference_pdf()).expect("open fixture");
        assert_eq!(
            resolved_object(&mut normal_first, object_ref).unwrap(),
            Object::Integer(3)
        );
        assert_eq!(
            normal_first.resolve_qpdf_json_object(object_ref).unwrap(),
            Object::Integer(3)
        );
        let snapshot = normal_first.repair_diagnostics();
        let diagnostics = snapshot.entries();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("expected endobj"));

        let mut json_first =
            Pdf::open_mem_owned(top_level_bare_reference_pdf()).expect("open fixture");
        assert_eq!(
            json_first.resolve_qpdf_json_object(object_ref).unwrap(),
            Object::Integer(3)
        );
        assert_eq!(
            resolved_object(&mut json_first, object_ref).unwrap(),
            Object::Integer(3)
        );
        assert_eq!(json_first.repair_diagnostics().entries().len(), 1);
    }

    #[test]
    fn canonical_resolution_reads_a_complete_stream_even_with_a_header_like_payload() {
        let payload = b"before\n9 0 obj\nafter\n";
        let mut stream_body = b"2 0 obj\n<< /Length 3 0 R >>\nstream\n".to_vec();
        stream_body.extend_from_slice(payload);
        stream_body.extend_from_slice(b"endstream\nendobj\n");
        let length_body = format!("3 0 obj\n{}\nendobj\n", payload.len());
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog /Probe 2 0 R >>\nendobj\n",
                &stream_body,
                length_body.as_bytes(),
            ],
            ObjectRef::new(1, 0),
        );
        let false_next_offset = bytes
            .windows(b"9 0 obj".len())
            .position(|window| window == b"9 0 obj")
            .expect("header-like stream payload") as u64;

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();
        let initial_budget = pdf.resolution_fallbacks_remaining;

        let object = resolved_object(&mut pdf, ObjectRef::new(2, 0))
            .expect("canonical resolver must read the complete stream");
        assert_eq!(
            object.as_stream().map(|stream| stream.data.as_slice()),
            Some(payload.as_slice())
        );
        assert_eq!(
            pdf.resolution_fallbacks_remaining, initial_budget,
            "canonical resolver does not use the legacy bounded-window retry budget"
        );
    }

    #[test]
    fn normal_resolution_recovers_empty_file_object_once() {
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Probe 3 0 R >>\nendobj\n",
                b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
                b"3 0 obj\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");

        assert_eq!(
            resolved_object(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            Object::Null
        );
        assert_eq!(
            resolved_object(&mut pdf, ObjectRef::new(3, 0)).unwrap(),
            Object::Null
        );
        let snapshot = pdf.repair_diagnostics();
        let diagnostics = snapshot.entries();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0]
            .message
            .contains("empty object treated as null"));
    }

    #[test]
    fn repaired_xref_uses_same_qpdf_file_object_value() {
        let mut bytes = top_level_bare_reference_pdf();
        let marker = b"startxref\n";
        let start = bytes
            .windows(marker.len())
            .rposition(|window| window == marker)
            .expect("startxref marker")
            + marker.len();
        for byte in bytes[start..]
            .iter_mut()
            .take_while(|byte| byte.is_ascii_digit())
        {
            *byte = b'9';
        }

        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                repair: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("linear-scan xref repair");
        assert_eq!(
            resolved_object(&mut pdf, ObjectRef::new(4, 0)).unwrap(),
            Object::Integer(3)
        );
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .filter(|diagnostic| diagnostic.message.contains("expected endobj"))
                .count(),
            1
        );
    }

    #[test]
    fn object_stream_file_object_mode_only_integerizes_bare_reference_member() {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        dict.insert("N", Object::Integer(3));
        dict.insert("First", Object::Integer(13));
        let stream = Stream::new(dict, b"7 0 8 6 9 14 6 0 R [6 0 R] << /V 6 0 R >>".to_vec());

        assert_eq!(
            parse_object_stream_entry(&stream, 0).unwrap().object,
            Object::Integer(6)
        );
        assert_eq!(
            parse_object_stream_entry(&stream, 1).unwrap().object,
            Object::Array(vec![Object::Reference(ObjectRef::new(6, 0))])
        );
        let dictionary = parse_object_stream_entry(&stream, 2)
            .unwrap()
            .object
            .into_dict()
            .expect("dictionary member");
        assert_eq!(dictionary.get_ref("V"), Some(ObjectRef::new(6, 0)));
    }

    #[test]
    fn object_stream_file_object_mode_propagates_filter_decode_errors() {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        dict.insert("N", Object::Integer(1));
        dict.insert("First", Object::Integer(4));
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let stream = Stream::new(dict, b"not zlib".to_vec());

        assert!(parse_object_stream_entry(&stream, 0).is_err());
    }

    #[test]
    fn object_stream_file_object_helpers_reject_invalid_metadata() {
        let mut missing_first = Dictionary::new();
        missing_first.insert("N", Object::Integer(1));
        assert!(
            parse_object_stream_entry(&Stream::new(missing_first, b"7 0".to_vec()), 0).is_err()
        );

        let mut non_integer_first = Dictionary::new();
        non_integer_first.insert("N", Object::Integer(1));
        non_integer_first.insert("First", Object::Name(b"not-an-integer".to_vec()));
        assert!(
            parse_object_stream_entry(&Stream::new(non_integer_first, b"7 0".to_vec()), 0).is_err()
        );

        let mut out_of_range = Dictionary::new();
        out_of_range.insert("N", Object::Integer(1));
        out_of_range.insert("First", Object::Integer(4));
        assert!(
            parse_object_stream_entry(&Stream::new(out_of_range, b"7 0 42".to_vec()), 1).is_err()
        );

        let mut bad_start = Dictionary::new();
        bad_start.insert("N", Object::Integer(1));
        bad_start.insert("First", Object::Integer(100));
        assert!(parse_object_stream_entry(&Stream::new(bad_start, b"7 0".to_vec()), 0).is_err());

        let mut non_integer_count = Dictionary::new();
        non_integer_count.insert("N", Object::Name(b"one".to_vec()));
        assert!(object_stream_count(&Stream::new(non_integer_count, Vec::new())).is_err());

        let mut negative_count = Dictionary::new();
        negative_count.insert("N", Object::Integer(-1));
        assert!(object_stream_count(&Stream::new(negative_count, Vec::new())).is_err());

        assert!(parse_non_negative_i64(&Object::Name(b"not-an-integer".to_vec()), "test").is_err());
        assert!(parse_non_negative_i64(&Object::Integer(-1), "test").is_err());
        assert!(parse_non_negative_u64(-1, "test").is_err());
    }

    #[test]
    fn read_failing_cursor_returns_its_injected_error() {
        let mut reader = ReadFailingCursor::new(Vec::new());
        reader.fail_reads = true;
        let mut byte = [0_u8; 1];

        let error = reader.read(&mut byte).expect_err("injected read failure");
        assert_eq!(error.to_string(), "injected holder read failure");
    }

    #[test]
    fn objstm_container_uses_qpdf_completion_but_members_remain_direct_objects() {
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 2 0 R >>\nstream\n7 0 6 0 R\nendstream\nendobj\n",
                b"2 0 obj\n/Bad\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        pdf.cache.set_compressed(ObjectRef::new(7, 0), 1, 0);
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(7, 0),
            XrefEntry::Compressed {
                stream: 1,
                index: 0,
            },
        );
        assert_eq!(
            resolved_object(&mut pdf, ObjectRef::new(7, 0)).unwrap(),
            Object::Integer(6)
        );
        assert_eq!(
            canonical_recovered_eol(&mut pdf, ObjectRef::new(1, 0))
                .expect("canonical recovered EOL"),
            Some(&b"\n"[..])
        );
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .all(|entry| !entry.message.contains("expected endobj")));
    }

    // ------------------------------------------------------------------
    // Acceptance (1): open_mem_owned(Vec<u8>) opens an in-memory PDF
    // ------------------------------------------------------------------

    #[test]
    fn open_mem_owned_opens_minimal_pdf() {
        let bytes = minimal_pdf_bytes();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open_mem_owned should succeed");
        let refs = page_refs(&mut pdf).expect("page_refs should succeed");
        assert_eq!(refs.len(), 1, "expected 1 page");
        assert_eq!(
            pdf.root_ref(),
            Some(ObjectRef::new(1, 0)),
            "expected root at 1 0 R"
        );
    }

    // ------------------------------------------------------------------
    // Acceptance (2): open_mem(Arc<[u8]>) opens an in-memory PDF
    // ------------------------------------------------------------------

    /// `open_mem` shares the caller's buffer instead of copying it.
    ///
    /// This is the contract that makes `Arc<[u8]>` the right parameter rather
    /// than `&[u8]`: qpdf's own in-memory entry point does not copy either
    /// (`QPDF::processMemoryFile`, `libqpdf/QPDF.cc:259-268`, over a `Buffer`
    /// whose "memory is owned by the caller and will not be freed when the
    /// Buffer is destroyed", `include/qpdf/Buffer.hh:42-45`).
    ///
    /// A strong count of 2 is only reachable if the document holds *this*
    /// allocation. The mutation this guards against has to stay type-correct
    /// to be meaningful — `Cursor::new(bytes.to_vec())` merely fails to
    /// compile, since `open_mem` lives in `impl Pdf<Cursor<Arc<[u8]>>>`, and a
    /// compile error is not this test discriminating. The real one is
    /// `Self::open(Cursor::new(Arc::from(&bytes[..])))`: same signature, same
    /// bytes, fresh allocation. It fails the middle assertion below with
    /// `left: 1, right: 2`, and fails nothing else.
    #[test]
    fn open_mem_shares_the_callers_buffer_rather_than_copying_it() {
        let bytes: Arc<[u8]> = Arc::from(&minimal_pdf_bytes()[..]);
        let kept = Arc::clone(&bytes);
        assert_eq!(Arc::strong_count(&kept), 2, "caller's clone plus `bytes`");

        let mut pdf = Pdf::open_mem(bytes).expect("open_mem should succeed");
        assert_eq!(
            Arc::strong_count(&kept),
            2,
            "the document must hold the caller's allocation, not a copy of it"
        );
        assert_eq!(page_refs(&mut pdf).expect("page_refs").len(), 1);

        drop(pdf);
        assert_eq!(
            Arc::strong_count(&kept),
            1,
            "dropping the document must release its share of the buffer"
        );
        assert_eq!(&kept[..9], b"%PDF-1.4\n");
    }

    #[test]
    fn open_mem_opens_minimal_pdf() {
        let bytes = minimal_pdf_bytes();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open_mem should succeed");
        let refs = page_refs(&mut pdf).expect("page_refs should succeed");
        assert_eq!(refs.len(), 1, "expected 1 page");
        assert_eq!(
            pdf.root_ref(),
            Some(ObjectRef::new(1, 0)),
            "expected root at 1 0 R"
        );
    }

    // ------------------------------------------------------------------
    // Acceptance (3): sugar wrappers match Cursor::new(bytes) directly
    // ------------------------------------------------------------------

    #[test]
    fn open_mem_owned_matches_cursor_open() {
        let bytes = minimal_pdf_bytes();

        let mut pdf_cursor =
            Pdf::open(Cursor::new(bytes.clone())).expect("Cursor::new open should succeed");
        let refs_cursor = page_refs(&mut pdf_cursor).expect("page_refs from cursor");
        let root_cursor = pdf_cursor.root_ref();

        let mut pdf_owned = Pdf::open_mem_owned(bytes).expect("open_mem_owned should succeed");
        let refs_owned = page_refs(&mut pdf_owned).expect("page_refs from open_mem_owned");
        let root_owned = pdf_owned.root_ref();

        assert_eq!(
            refs_cursor, refs_owned,
            "page refs from Cursor::new vs open_mem_owned must match"
        );
        assert_eq!(
            root_cursor, root_owned,
            "root ref from Cursor::new vs open_mem_owned must match"
        );
    }

    #[test]
    fn open_mem_matches_cursor_open() {
        let bytes = minimal_pdf_bytes();

        let mut pdf_cursor =
            Pdf::open(Cursor::new(bytes.clone())).expect("Cursor::new open should succeed");
        let refs_cursor = page_refs(&mut pdf_cursor).expect("page_refs from cursor");
        let root_cursor = pdf_cursor.root_ref();

        let mut pdf_mem = Pdf::open_mem_owned(bytes).expect("open_mem should succeed");
        let refs_mem = page_refs(&mut pdf_mem).expect("page_refs from open_mem");
        let root_mem = pdf_mem.root_ref();

        assert_eq!(
            refs_cursor, refs_mem,
            "page refs from Cursor::new vs open_mem must match"
        );
        assert_eq!(
            root_cursor, root_mem,
            "root ref from Cursor::new vs open_mem must match"
        );
    }

    // ------------------------------------------------------------------
    // _with_options variants pass options through correctly (repair path)
    // ------------------------------------------------------------------

    #[test]
    fn open_mem_owned_with_options_accepts_repair_flag() {
        let bytes = minimal_pdf_bytes();
        let opts = PdfOpenOptions {
            repair: true,
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, opts).expect("open_mem_owned_with_options");
        let refs = page_refs(&mut pdf).expect("page_refs");
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn open_mem_with_options_accepts_repair_flag() {
        let bytes = minimal_pdf_bytes();
        let opts = PdfOpenOptions {
            repair: true,
            ..PdfOpenOptions::default()
        };
        let mut pdf = Pdf::open_mem_owned_with_options(bytes, opts).expect("open_mem_with_options");
        let refs = page_refs(&mut pdf).expect("page_refs");
        assert_eq!(refs.len(), 1);
    }

    /// The shared-buffer entry point takes the same options, and shares the
    /// caller's allocation rather than copying it — the property `open_mem`'s
    /// own doc rests on (qpdf's `Buffer` contract,
    /// `include/qpdf/Buffer.hh:42-45`).
    #[test]
    fn open_mem_with_options_shares_the_callers_buffer() {
        let bytes: Arc<[u8]> = Arc::from(&minimal_pdf_bytes()[..]);
        let kept = Arc::clone(&bytes);
        let opts = PdfOpenOptions {
            repair: true,
            ..PdfOpenOptions::default()
        };

        let mut pdf = Pdf::open_mem_with_options(bytes, opts).expect("open_mem_with_options");

        assert_eq!(page_refs(&mut pdf).expect("page_refs").len(), 1);
        assert_eq!(
            Arc::strong_count(&kept),
            2,
            "the document must read the caller's allocation, not a copy"
        );
    }

    /// `%PDF-1.7` document whose catalog reaches an Adobe extension level via
    /// an *indirect* `/Extensions` reference (object 4), with an inline `/ADBE`
    /// dictionary and an inline integer `/ExtensionLevel`.
    fn extension_level_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.7\n");
        let off1 = pdf.len();
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>\nendobj\n",
        );
        let off2 = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = pdf.len();
        pdf.extend_from_slice(
            b"4 0 obj\n<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >>\nendobj\n",
        );
        let xref_start = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn adobe_extension_level_reads_indirect_extensions_chain() {
        let mut pdf = Pdf::open_mem_owned(extension_level_pdf_bytes()).expect("open");
        assert_eq!(pdf.adobe_extension_level(), Some(8));
    }

    #[test]
    fn adobe_extension_level_reads_a_direct_root_catalog() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/compat/direct-root-adbe.pdf"
            ))
            .to_vec(),
        )
        .expect("open");
        assert_eq!(pdf.adobe_extension_level(), Some(8));
    }

    #[test]
    fn adobe_extension_level_absent_when_catalog_has_no_extensions() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        assert_eq!(pdf.adobe_extension_level(), None);
    }

    #[test]
    fn get_object_handle_returns_the_same_canonical_handle_for_repeated_calls() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let first = pdf.get_object_handle(object_ref);
        let second = pdf.get_object_handle(object_ref);
        assert!(first.is_same_object_as(&second));
    }

    #[test]
    fn get_object_handle_is_indirect_with_the_requested_ref() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        assert!(handle.is_indirect());
        assert_eq!(handle.object_ref(), Some(object_ref));
    }

    #[test]
    fn next_obj_gen_uses_the_prepared_canonical_cache_and_qpdfs_signed_limit() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");

        assert_eq!(
            pdf.next_obj_gen().expect("first fresh object"),
            ObjectRef::new(4, 0)
        );

        for generation in [1, 7] {
            let high_generation = pdf.get_object_handle(ObjectRef::new(99, generation));
            assert!(high_generation.is_indirect());
        }
        assert_eq!(
            pdf.next_obj_gen().expect("object above every generation"),
            ObjectRef::new(100, 0)
        );

        let max_object = pdf.get_object_handle(ObjectRef::new(i32::MAX as u32, 0));
        assert!(max_object.is_indirect());
        let error = pdf
            .next_obj_gen()
            .expect_err("qpdf rejects the INT_MAX allocation boundary");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: max object id is too high to create new objects"
        );
    }

    #[test]
    fn next_obj_gen_uses_the_effective_xref_stream_and_objstm_cache() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/three-page-objstm.pdf"
        );
        let pdf = Pdf::open_mem_owned(std::fs::read(fixture).expect("read ObjStm fixture"))
            .expect("open ObjStm fixture");

        // The pinned qpdf 11.9.0 fixture exposes object numbers 1 through 13
        // through its effective xref stream and object stream.
        assert_eq!(
            pdf.next_obj_gen().expect("next ObjGen"),
            ObjectRef::new(14, 0)
        );
    }

    #[test]
    fn new_stream_is_an_empty_canonical_indirect_object_with_qpdf_no_data_state() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let stream = pdf.new_stream().expect("new empty stream");
        let stream_dict = stream.as_stream_dict().expect("stream dictionary");

        assert!(stream.is_indirect());
        assert_eq!(stream.object_ref(), Some(ObjectRef::new(4, 0)));
        assert_eq!(stream.get_parsed_offset(), 0);
        assert_eq!(stream.as_stream_data(), None);
        assert_eq!(stream_dict.as_dictionary().expect("dictionary").len(), 0);
        assert!(pdf.is_canonical_object_handle(&stream));

        stream_dict
            .replace_key(b"/Marker", ObjectHandle::integer(7))
            .unwrap();
        assert_eq!(
            stream
                .as_stream_dict()
                .expect("stream dictionary")
                .get_key(b"/Marker")
                .as_integer(),
            Some(7)
        );
    }

    #[test]
    fn new_reserved_is_a_distinct_qpdf_internal_sentinel() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let first = pdf.new_reserved().expect("first reserved object");
        let second = pdf.new_reserved().expect("second reserved object");

        pdf.resolve(&first)
            .expect("reserved handles do not enter the source resolver");
        assert!(first.is_indirect());
        assert_eq!(first.object_ref(), Some(ObjectRef::new(4, 0)));
        assert!(first.is_reserved());
        assert!(first.is_resolved());
        assert!(!first.is_null());
        assert_eq!(first.type_code().expect("type code"), 1, "qpdf ot_reserved");
        assert_eq!(first.type_name().expect("type name"), "reserved");
        assert!(!first.is_same_object_as(&second));
        assert!(pdf.is_canonical_object_handle(&first));
        assert!(format!("{first:?}").contains("state: \"Reserved\""));

        let error = first
            .materialize()
            .expect_err("qpdf reserved objects cannot be materialized");
        assert_eq!(
            error.to_string(),
            "QPDFObjectHandle: attempting to unparse a reserved object"
        );
    }

    #[test]
    fn dropping_the_owner_turns_a_reserved_handle_into_destroyed() {
        let reserved = {
            let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
            pdf.new_reserved().expect("reserved object")
        };

        assert!(!reserved.is_reserved());
        assert_eq!(
            reserved.type_code().expect("type code"),
            14,
            "qpdf ot_destroyed"
        );
        assert_eq!(reserved.type_name().expect("type name"), "destroyed");
        assert!(!reserved.is_null());
    }

    #[test]
    fn shallow_copy_on_a_reserved_handle_produces_a_fresh_direct_reserved_sentinel() {
        // `QPDF_Reserved::copy(bool shallow)` ignores its `shallow` argument
        // and always returns `create()` -- a brand-new `QPDF_Reserved`
        // instance (`libqpdf/QPDF_Reserved.cc:14-19`), never null and never a
        // throw. `QPDFObjectHandle::shallowCopy` (`libqpdf/QPDFObjectHandle.cc
        // :2073-2079`) wraps that result the same way it wraps any other
        // type's `copy()`: a *direct* handle with no object number of its own
        // and no owning `QPDF*`, independent from the source. Codex Review on
        // PR #789, crates/flpdf/src/object_handle.rs:4158 (databaseId
        // 3773163232).
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");

        let copy = reserved
            .shallow_copy()
            .expect("QPDF_Reserved::copy never throws");

        assert!(
            copy.is_reserved(),
            "shallow_copy on Reserved must stay Reserved, not fall back to null"
        );
        assert_eq!(copy.type_code().expect("type code"), 1, "qpdf ot_reserved");
        assert!(
            copy.is_direct(),
            "a shallow copy has no object number of its own, matching every other value arm"
        );
        assert!(
            !copy.is_same_object_as(&reserved),
            "qpdf's copy() always mints a new object, never shares the source's identity"
        );
    }

    #[test]
    fn a_direct_reserved_child_materialize_rejects_like_a_top_level_reserved_handle() {
        // `ObjectHandle::shallow_copy` on a reserved handle produces a
        // *direct* reserved sentinel (see the previous test) -- a shape
        // the reserved value now lives directly in ObjectValue.
        // Nesting that direct copy as an array element and materializing
        // the array previously substituted `Object::Null` for it silently:
        // `materialize_child`'s direct branch (`handle.object_ref()` is
        // `None` for a direct handle) falls through to
        // `materialize_bounded`, which now dispatches the ObjectValue
        // sentinel to qpdf's own `QPDF_Reserved::unparse()`
        // (`libqpdf/QPDF_Reserved.cc:22-26`) throws once the reserved
        // object is the value actually being dereferenced -- which a
        // *direct* child always is, since `QPDFWriter::unparseChild`
        // (`libqpdf/QPDFWriter.cc:1144-1156`) only ever skips dereferencing
        // an *indirect* child. Codex Review on PR #789,
        // crates/flpdf/src/object_handle.rs:3226 (databaseId 3773501422).
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        let direct_copy = reserved
            .shallow_copy()
            .expect("QPDF_Reserved::copy never throws");
        assert!(
            direct_copy.is_direct(),
            "shallow_copy never carries an object number of its own"
        );

        let containing = ObjectHandle::array(vec![direct_copy]);
        let error = containing
            .materialize()
            .expect_err("a direct reserved child must reject, not substitute null");
        assert_eq!(
            error.to_string(),
            "QPDFObjectHandle: attempting to unparse a reserved object"
        );
    }

    #[test]
    fn a_direct_reserved_child_is_already_rejected_by_the_ref_map_writer_family() {
        // Unlike `materialize` (previous test), the production ref-map
        // writer family's direct branch --
        // `write_child_with_ref_map(handle, ..)` with `handle.object_ref()`
        // `None` -- recurses through `unparse_object_walk_with_ref_map`,
        // which checks `is_reserved()` on whatever handle it is entered
        // with, not only the original top-level `self`. A direct reserved
        // child re-enters that same check on its own recursive call, so
        // this path already rejects it and needs no additional fix; this
        // pins that existing (if under-documented) behavior against
        // regression.
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        let direct_copy = reserved
            .shallow_copy()
            .expect("QPDF_Reserved::copy never throws");

        let containing = ObjectHandle::dictionary(vec![(b"/Reserved".to_vec(), direct_copy)]);
        let mut out = Vec::new();
        let error = containing
            .write_object_with_ref_map_and_removed(&mut out, &|object_ref| Ok(object_ref), &BTreeSet::new())
            .expect_err(
                "a direct reserved child must be rejected, matching QPDF_Reserved::unparse()'s throw",
            );
        assert_eq!(
            error.to_string(),
            "QPDFObjectHandle: attempting to unparse a reserved object"
        );
    }

    #[test]
    fn reserved_objects_are_rejected_by_object_writer_entrypoints() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        let expected = "QPDFObjectHandle: attempting to unparse a reserved object";

        let mut out = Vec::new();
        assert_eq!(
            reserved
                .write_object(&mut out)
                .expect_err("plain writer must reject reserved objects")
                .to_string(),
            expected
        );
        out.clear();
        assert_eq!(
            reserved
                .write_object_qdf(&mut out, 0)
                .expect_err("QDF writer must reject reserved objects")
                .to_string(),
            expected
        );
        out.clear();
        assert_eq!(
            reserved
                .write_object_with_ref_map_and_removed(
                    &mut out,
                    &|object_ref| Ok(object_ref), // cov:ignore: reserved handles fail before the mapping callback runs
                    &BTreeSet::new(),
                )
                .expect_err("mapped writer must reject reserved objects")
                .to_string(),
            expected
        );

        // A reserved handle reached only as an indirect *child* of a direct
        // container is a different case from all of the above: qpdf's own
        // `QPDFWriter::unparseChild` (`libqpdf/QPDFWriter.cc:1144-1156`)
        // decides reference-vs-recurse from `child.isIndirect()` alone and
        // never inspects what the reference resolves to, so an indirect
        // reserved child writes as an ordinary bare `"N G R"` reference,
        // same as any other indirect child -- it is never dereferenced at
        // this position, so its unparseable body never matters here. Only
        // dereferencing the reserved object *itself* (the `reserved.foo()`
        // calls above and below, where `reserved` is `self`) reaches qpdf's
        // real throw. Codex Review on PR #789,
        // crates/flpdf/src/object_handle.rs:4529.
        let object_ref = reserved
            .object_ref()
            .expect("a reserved handle is always indirect by construction");
        let containing = ObjectHandle::dictionary(vec![(b"/Reserved".to_vec(), reserved.clone())]);
        out.clear();
        containing
            .write_object(&mut out)
            .expect("an indirect reserved child must serialize as a bare reference");
        assert_eq!(
            out,
            format!("<< /Reserved {object_ref} >>").into_bytes(),
            "the reserved child appears in its own reference form, never recursed into"
        );

        let materialized = containing
            .materialize()
            .expect("an indirect reserved child must materialize to Object::Reference");
        let Object::Dictionary(materialized) = materialized else {
            panic!("expected a dictionary"); // cov:ignore: unreachable in a passing run
        };
        assert_eq!(
            materialized.get("Reserved"),
            Some(&Object::Reference(object_ref))
        );

        out.clear();
        let qdf_containing =
            ObjectHandle::dictionary(vec![(b"/Reserved".to_vec(), reserved.clone())]);
        qdf_containing.write_object_qdf(&mut out, 0).expect(
            "an indirect reserved child must serialize as a bare reference in QDF mode too",
        );
        assert_eq!(
            out,
            format!("<<\n  /Reserved {object_ref}\n>>").into_bytes()
        );

        out.clear();
        assert_eq!(
            reserved
                .write_stream_body(&mut out, false)
                .expect_err("stream-body writer must reject reserved objects")
                .to_string(),
            expected
        );
        out.clear();
        assert_eq!(
            reserved
                .write_stream_body_qdf(&mut out, 0)
                .expect_err("QDF stream-body writer must reject reserved objects")
                .to_string(),
            expected
        );
        out.clear();
        let identity = |object_ref| Ok(object_ref);
        assert_eq!(
            identity(ObjectRef::new(99, 0)).expect("identity map"),
            ObjectRef::new(99, 0)
        );
        assert_eq!(
            reserved
                .write_stream_body_with_ref_map_and_removed(
                    &mut out,
                    false,
                    &identity,
                    &std::collections::BTreeSet::new(),
                )
                .expect_err("mapped stream-body writer must reject reserved objects")
                .to_string(),
            expected
        );
        out.clear();
        assert_eq!(
            reserved
                .write_trailer(&mut out, false, None)
                .expect_err("trailer writer must reject reserved objects")
                .to_string(),
            expected
        );
    }

    #[test]
    fn full_writer_rejects_a_reachable_reserved_object() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        pdf.resolve(&root).expect("resolve catalog");
        assert!(root.as_dictionary().is_some(), "catalog dictionary");
        root.replace_key(b"/Reserved", reserved).unwrap();

        let error = crate::writer::write_qpdf_to_memory(&mut pdf, |_| {})
            .expect_err("full writer must reject a reachable reserved object");
        assert_eq!(
            error.to_string(),
            "QPDFObjectHandle: attempting to unparse a reserved object"
        );
    }

    #[test]
    fn an_indirect_reserved_child_writes_as_a_bare_reference_through_the_ref_map_family() {
        // `write_object_with_ref_map_and_removed`/`write_child_with_ref_map`
        // are the primitives `writer/plain/body.rs`/`writer/plain/plan.rs`
        // actually call in production, unlike the still-`#[allow(dead_code)]`
        // `write_object`/`write_child` family exercised by
        // `reserved_objects_are_rejected_by_object_writer_entrypoints` above
        // -- so this pins the identical child-position fix against the code
        // path real document writes use today.
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        let object_ref = reserved
            .object_ref()
            .expect("a reserved handle is always indirect by construction");
        let containing = ObjectHandle::dictionary(vec![(b"/Reserved".to_vec(), reserved)]);

        let mut out = Vec::new();
        containing
            .write_object_with_ref_map_and_removed(
                &mut out,
                &|object_ref| Ok(object_ref),
                &BTreeSet::new(),
            )
            .expect("an indirect reserved child must serialize as a bare reference");
        assert_eq!(out, format!("<< /Reserved {object_ref} >>").into_bytes());
    }

    #[test]
    fn reserved_object_unparse_resolved_falls_back_to_null() {
        // `unparse_resolved` returns `Vec<u8>`, not `Result` -- unlike the
        // writer-facing top-level entry points above (which reject a
        // reserved handle with `reserved_unparse_error()` when it is `self`,
        // the value actually being dereferenced -- see
        // `reserved_objects_are_rejected_by_object_writer_entrypoints`), it
        // has no exception channel to mirror qpdf's `QPDF_Reserved::unparse()`
        // throw (`libqpdf/QPDF_Reserved.cc:22-26`) with, so it falls back to
        // `null` the same way it already does for `Unresolved`/
        // `Destroyed` (see `unparse_resolved`'s own doc for all three).
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        assert_eq!(reserved.unparse_resolved(), b"null");
    }

    #[test]
    fn new_stream_rejects_raw_piping_before_data_replacement() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let stream = pdf.new_stream().expect("new empty stream");

        let error = stream
            .get_raw_stream_data()
            .expect_err("qpdf's empty stream has no source bytes");
        assert!(matches!(error, Error::Internal(message)
            if message == "pipeStreamData called for stream with no data"));
    }

    #[test]
    fn new_stream_allocates_distinct_generation_zero_objects_and_honors_signed_limit() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let first = pdf.new_stream().expect("first stream");
        let second = pdf.new_stream().expect("second stream");

        assert_eq!(first.object_ref(), Some(ObjectRef::new(4, 0)));
        assert_eq!(second.object_ref(), Some(ObjectRef::new(5, 0)));
        assert!(!first.is_same_object_as(&second));

        let at_limit = pdf.get_object_handle(ObjectRef::new(i32::MAX as u32, 0));
        assert!(at_limit.is_indirect());
        let error = pdf
            .new_stream()
            .expect_err("qpdf rejects the signed object-number boundary");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: max object id is too high to create new objects"
        );
    }

    #[test]
    fn new_stream_with_data_preserves_buffer_identity_and_qpdf_length_boundary() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let data = Rc::new(b"shared stream data".to_vec());
        let stream = pdf
            .new_stream_with_data(Rc::clone(&data))
            .expect("new stream with data");

        let stored = stream.as_stream_data().expect("replacement data");
        assert!(Rc::ptr_eq(&stored, &data));
        assert_eq!(
            stream
                .as_stream_dict()
                .expect("stream dictionary")
                .get_key(b"/Length")
                .as_integer(),
            Some(data.len() as i64)
        );

        let empty = pdf
            .new_stream_with_data(Rc::new(Vec::new()))
            .expect("new empty replacement stream");
        assert!(!empty
            .as_stream_dict()
            .expect("stream dictionary")
            .has_key(b"/Length"));
    }

    #[test]
    fn copy_stream_shares_buffer_and_copies_dictionary_at_indirect_boundaries() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let data = Rc::new(b"copy stream payload".to_vec());
        let source = pdf
            .new_stream_with_data(Rc::clone(&data))
            .expect("new source stream");
        let shared = pdf
            .make_indirect_object_handle(ObjectHandle::integer(7))
            .expect("indirect dictionary child");
        let direct =
            ObjectHandle::dictionary(vec![(b"/Nested".to_vec(), ObjectHandle::integer(9))]);
        let source_dict = source.as_stream_dict().expect("source dictionary");
        source_dict
            .replace_key(b"/Filter", ObjectHandle::name(b"FlateDecode".to_vec()))
            .unwrap();
        source_dict
            .replace_key(b"/Indirect", shared.clone())
            .unwrap();
        source_dict.replace_key(b"/Direct", direct.clone()).unwrap();

        let copy = source.copy_stream().expect("copy stream");
        let copy_dict = copy.as_stream_dict().expect("copy dictionary");

        assert!(copy.is_indirect());
        assert_ne!(copy.object_ref(), source.object_ref());
        assert!(Rc::ptr_eq(
            &copy.as_stream_data().expect("copied buffer"),
            &data
        ));
        assert_eq!(
            copy_dict.get_key(b"/Filter").as_name(),
            Some(b"FlateDecode".to_vec())
        );
        assert!(copy_dict.get_key(b"/Indirect").is_same_object_as(&shared));
        let copied_direct = copy_dict.get_key(b"/Direct");
        assert!(!copied_direct.is_same_object_as(&direct));
        assert_eq!(copied_direct.get_key(b"/Nested").as_integer(), Some(9));
        copied_direct
            .replace_key(b"/Nested", ObjectHandle::integer(10))
            .unwrap();
        assert_eq!(direct.get_key(b"/Nested").as_integer(), Some(9));
    }

    #[test]
    fn resolver_copy_stream_data_replaces_destination_null_filter_and_decode_parms() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let destination = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Filter".to_vec(), ObjectHandle::null()),
                (b"/DecodeParms".to_vec(), ObjectHandle::null()),
            ]),
            Rc::new(b"destination payload".to_vec()),
        );
        let source = pdf
            .new_stream_with_data(Rc::new(b"source payload".to_vec()))
            .expect("new source stream");

        pdf.resolver
            .copy_stream_data(&destination, &source)
            .expect("copy stream data");

        let dictionary = destination
            .as_stream_dict()
            .expect("destination dictionary")
            .as_dictionary()
            .expect("raw destination dictionary");
        assert!(!dictionary.contains_key(b"/Filter".as_slice()));
        assert!(!dictionary.contains_key(b"/DecodeParms".as_slice()));
    }

    #[test]
    fn resolver_copy_stream_data_replaces_source_null_filter_and_decode_parms_during_immediate_copy(
    ) {
        let bytes = pdf_with_one_stream_with_null_filters(b"immediate payload");
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
        let source = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve(&source).expect("resolve source stream");
        pdf.set_immediate_copy_from(true);
        let destination = pdf.new_stream().expect("new destination stream");

        pdf.resolver
            .copy_stream_data(&destination, &source)
            .expect("copy stream data");

        let dictionary = source
            .as_stream_dict()
            .expect("source dictionary")
            .as_dictionary()
            .expect("raw source dictionary");
        assert!(!dictionary.contains_key(b"/Filter".as_slice()));
        assert!(!dictionary.contains_key(b"/DecodeParms".as_slice()));
    }

    #[test]
    fn copy_stream_keeps_provider_data_deferred_and_forwards_retry_flags() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let source = pdf.new_stream().expect("new source stream");
        let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
        let calls_for_provider = Rc::clone(&calls);
        source
            .replace_stream_data_with_retry_callback(
                move |pipeline, suppress_warnings, will_retry| {
                    calls_for_provider
                        .borrow_mut()
                        .push((suppress_warnings, will_retry));
                    pipeline.write(b"provider payload").map_err(Error::from)?;
                    pipeline.finish().map_err(Error::from)?;
                    Ok(true)
                },
                None,
                None,
            )
            .expect("register source provider");

        let copy = source.copy_stream().expect("copy provider stream");
        assert!(copy.as_stream_data().is_none());
        assert!(calls.borrow().is_empty(), "copy must not invoke the source");
        assert_eq!(
            copy.get_raw_stream_data()
                .expect("pipe copied provider")
                .as_ref(),
            b"provider payload"
        );
        assert_eq!(calls.borrow().as_slice(), &[(false, false)]);
    }

    #[test]
    fn copy_stream_keeps_original_file_source_deferred() {
        let bytes = pdf_with_one_stream(b"original payload");
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
        let source = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve(&source).expect("resolve source stream");

        assert!(source.as_stream_data().is_none());
        let copy = source.copy_stream().expect("copy original stream");
        assert!(copy.as_stream_data().is_none());
        assert_eq!(
            copy.get_raw_stream_data()
                .expect("pipe copied original source")
                .as_ref(),
            b"original payload"
        );
    }

    #[test]
    fn copy_stream_honors_source_immediate_copy_configuration() {
        let bytes = pdf_with_one_stream(b"immediate payload");
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
        let source = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve(&source).expect("resolve source stream");
        pdf.set_immediate_copy_from(true);

        let copy = source.copy_stream().expect("copy immediate stream");
        let source_data = source.as_stream_data().expect("materialized source data");
        let copy_data = copy.as_stream_data().expect("shared copied data");
        assert!(Rc::ptr_eq(&source_data, &copy_data));
        assert_eq!(source_data.as_ref(), b"immediate payload");
    }

    #[test]
    fn copy_stream_matches_qpdf_assertion_and_context_errors() {
        let non_stream = ObjectHandle::integer(1);
        assert!(matches!(
            non_stream.copy_stream(),
            Err(Error::System(message))
                if message == "operation for stream attempted on object of type integer"
        ));

        let direct_stream = ObjectHandle::stream(
            ObjectHandle::dictionary(Vec::new()),
            Rc::new(b"detached".to_vec()),
        );
        assert!(matches!(
            direct_stream.copy_stream(),
            Err(Error::Internal(message))
                if message == "copyStream called on a stream with no owning PDF"
        ));
    }

    #[test]
    fn resolver_copy_stream_data_rejects_non_stream_contract_inputs() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let source = pdf
            .make_indirect_object_handle(ObjectHandle::integer(1))
            .expect("make non-stream source");
        pdf.set_immediate_copy_from(true);

        let destination = ObjectHandle::integer(2);
        assert!(matches!(
            pdf.resolver.copy_stream_data(&destination, &source),
            Err(Error::System(message))
                if message == "operation for stream attempted on object of type integer"
        ));

        let destination = pdf.new_stream().expect("new destination stream");
        assert!(matches!(
            pdf.resolver.copy_stream_data(&destination, &source),
            Err(Error::System(message))
                if message == "operation for stream attempted on object of type integer"
        ));
    }

    #[test]
    fn new_stream_survives_owner_drop_as_the_same_live_stream_value() {
        let stream = {
            let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
            pdf.new_stream().expect("new empty stream")
        };

        assert!(stream.is_direct());
        assert_eq!(stream.object_ref(), None);
        assert_eq!(stream.type_code().expect("type code"), 14);
        assert_eq!(stream.get_parsed_offset(), NO_PARSED_OFFSET);
        assert_eq!(stream.as_stream_data(), None);
    }

    #[test]
    fn reachable_new_stream_survives_a_canonical_full_rewrite() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let stream = pdf
            .new_stream_with_data(Rc::new(b"new stream payload".to_vec()))
            .expect("new stream with data");
        let stream_ref = stream.object_ref().expect("stream reference");
        let root_ref = ObjectRef::new(1, 0);
        let root = pdf.get_object_handle(root_ref);
        pdf.resolve(&root).expect("resolve root");
        root.replace_key(b"/Extra", stream.clone()).unwrap();
        pdf.mark_object_dirty(root_ref);

        let mut writer = crate::PdfWriter::new(&mut pdf);
        writer.set_compress_streams(false);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("full rewrite");
        let emitted_ref = writer
            .get_renumbered_obj_gen(stream_ref)
            .expect("query stream mapping")
            .expect("reachable stream must be emitted");
        let output = writer.get_buffer().expect("take output");

        let mut reopened = Pdf::open_mem_owned(output).expect("reopen output");
        let emitted_stream_ref = resolved_object(&mut reopened, root_ref)
            .expect("resolve rewritten root")
            .into_dict()
            .and_then(|dict| dict.get("Extra").cloned())
            .and_then(|object| object.as_ref_id())
            .expect("rewritten root has stream reference");
        assert_eq!(emitted_stream_ref, emitted_ref);
        assert_eq!(
            resolved_object(&mut reopened, emitted_ref)
                .expect("resolve rewritten stream")
                .as_stream()
                .expect("rewritten object remains a stream")
                .data,
            b"new stream payload"
        );
    }

    #[test]
    fn make_indirect_from_object_handle_registers_the_same_shared_object_without_dirtying_it() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let direct = ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(7))]);
        let alias = direct.clone();

        let indirect = pdf
            .make_indirect_from_object_handle(direct)
            .expect("make indirect from the existing allocation");
        let object_ref = indirect.object_ref().expect("fresh indirect ref");

        assert_eq!(object_ref, ObjectRef::new(4, 0));
        assert!(indirect.is_same_object_as(&alias));
        assert!(alias.is_indirect());
        assert_eq!(alias.object_ref(), Some(object_ref));
        alias
            .replace_key(b"/Value", ObjectHandle::integer(11))
            .unwrap();
        assert_eq!(indirect.get_key(b"/Value").as_integer(), Some(11));
        assert!(!pdf.is_dirty(object_ref));

        let all = pdf
            .get_all_objects()
            .expect("enumerate the canonical allocation");
        let enumerated = all
            .iter()
            .find(|handle| handle.object_ref() == Some(object_ref))
            .expect("fresh allocation must be in canonical enumeration");
        assert!(enumerated.is_same_object_as(&indirect));
    }

    #[test]
    fn repeated_make_indirect_from_object_handle_allocations_do_not_collide() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let first = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(1))
            .expect("first allocation");
        let second = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(2))
            .expect("second allocation");

        assert_eq!(first.object_ref(), Some(ObjectRef::new(4, 0)));
        assert_eq!(second.object_ref(), Some(ObjectRef::new(5, 0)));
        assert_eq!(first.as_integer(), Some(1));
        assert_eq!(second.as_integer(), Some(2));
    }

    #[test]
    fn make_indirect_from_object_handle_rejects_an_already_indirect_handle() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let indirect = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(1))
            .expect("initial allocation");
        let original_ref = indirect.object_ref();

        let error = pdf
            .make_indirect_from_object_handle(indirect.clone())
            .expect_err("an indirect handle cannot be promoted a second time");

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: cannot make an already-indirect ObjectHandle indirect"
        );
        assert_eq!(indirect.object_ref(), original_ref);
        assert_eq!(
            pdf.next_obj_gen()
                .expect("failed promotion is non-mutating"),
            ObjectRef::new(5, 0)
        );
    }

    #[test]
    fn make_indirect_from_object_handle_rejects_an_uninitialized_handle() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let before = pdf
            .next_obj_gen()
            .expect("baseline allocation counter reads cleanly");

        let error = pdf
            .make_indirect_from_object_handle(ObjectHandle::uninitialized())
            .expect_err("an uninitialized handle must not be promoted");

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: attempted to make an uninitialized QPDFObjectHandle indirect"
        );
        assert_eq!(
            pdf.next_obj_gen()
                .expect("rejected promotion is non-mutating"),
            before
        );
    }

    #[test]
    fn replace_object_rejects_an_uninitialized_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(1))
            .expect("initial allocation")
            .object_ref()
            .expect("fresh indirect ref");

        let error = pdf
            .resolver
            .replace_object(object_ref, ObjectHandle::uninitialized())
            .expect_err("an uninitialized handle must not replace an existing object");

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: QPDF::replaceObject called with indirect object handle"
        );
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).unwrap();
        assert_eq!(
            handle.as_integer(),
            Some(1),
            "a rejected replacement must leave the original object untouched"
        );
    }

    #[test]
    fn failed_make_indirect_at_int_max_leaves_the_direct_allocation_untouched() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.get_object_handle(ObjectRef::new(i32::MAX as u32, 0));
        let direct = ObjectHandle::integer(42);
        let alias = direct.clone();

        let error = pdf
            .make_indirect_from_object_handle(direct)
            .expect_err("allocation must fail at INT_MAX");

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: max object id is too high to create new objects"
        );
        assert!(alias.is_direct());
        assert_eq!(alias.object_ref(), None);
        assert_eq!(alias.as_integer(), Some(42));
    }

    #[test]
    fn failed_make_indirect_at_int_max_does_not_leave_a_stale_tree_claim() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.get_object_handle(ObjectRef::new(i32::MAX as u32, 0));
        let direct = ObjectHandle::integer(42);
        let alias = direct.clone();

        pdf.make_indirect_from_object_handle(direct)
            .expect_err("allocation must fail at INT_MAX");

        // A failed promotion must leave `alias` fully untouched, including
        // the tree-claim token this Pdf's attempt would have set on success
        // -- an unrelated Pdf must still be free to claim it as a
        // name/number tree root afterward.
        let other_pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open other pdf");
        alias
            .claim_tree_pdf(other_pdf.unique_id())
            .expect("a failed promotion must not leave a stale tree claim behind");
    }

    #[test]
    fn replace_object_preserves_target_identity_and_shares_payload() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let target = pdf.get_object_handle(object_ref);
        target.try_dereference().expect("resolve target");
        assert!(target.get_parsed_offset() >= 0);

        let replacement =
            ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(7))]);
        let replacement_alias = replacement.clone();

        let returned = pdf
            .replace_object(object_ref, replacement)
            .expect("replace canonical object");

        assert!(returned.is_same_object_as(&target));
        assert_eq!(target.object_ref(), Some(object_ref));
        assert_eq!(target.get_key(b"/Value").as_integer(), Some(7));
        assert_eq!(target.get_parsed_offset(), NO_PARSED_OFFSET);
        assert_eq!(target.description(), "object 1 0");
        assert!(pdf.is_dirty(object_ref));

        // The replacement handle remains a distinct direct identity, but
        // qpdf's QPDFObject::assign makes its value payload shared with the
        // canonical target. Mutating either side must therefore be visible
        // through the other side.
        assert!(replacement_alias.is_direct());
        replacement_alias.replace_direct_value(ObjectValue::Integer(9));
        assert_eq!(target.as_integer(), Some(9));
    }

    #[test]
    fn delete_object_detaches_a_shared_replacement_before_nulling_the_target() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let replacement =
            ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(7))]);
        let replacement_alias = replacement.clone();

        pdf.replace_object(object_ref, replacement)
            .expect("replace canonical object");
        pdf.delete_object(object_ref);

        assert!(replacement_alias.as_dictionary().is_some());
        assert_eq!(replacement_alias.get_key(b"/Value").as_integer(), Some(7));
    }

    #[test]
    fn replace_object_is_the_dirty_marking_handle_form_of_set_object() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let target = pdf.get_object_handle(object_ref);
        target.try_dereference().expect("resolve target");
        let replacement = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"Catalog".to_vec()),
        )]);
        let replacement_alias = replacement.clone();

        pdf.replace_object(object_ref, replacement)
            .expect("replace canonical object through setter");

        let current = pdf.get_object_handle(object_ref);
        assert!(current.is_same_object_as(&target));
        assert_eq!(
            current.get_key(b"/Type").as_name(),
            Some(b"Catalog".to_vec())
        );
        assert!(pdf.is_dirty(object_ref));

        replacement_alias.replace_direct_value(ObjectValue::Integer(9));
        assert_eq!(current.as_integer(), Some(9));
    }

    #[test]
    fn replace_object_preserves_clean_and_dirty_restore_writer_snapshots() {
        let source = String::from_utf8(minimal_pdf_bytes())
            .expect("minimal fixture is UTF-8")
            .replace(
                "trailer\n<< /Size 4 /Root 1 0 R >>",
                "trailer\n<< /Size 4 /Root 1 0 R /ID [<0123456789abcdef0123456789abcdef><0123456789abcdef0123456789abcdef>] >>",
            )
            .into_bytes();
        let object_ref = ObjectRef::new(1, 0);

        let snapshot = |pdf: &mut Pdf<Cursor<Vec<u8>>>| {
            let bytes =
                crate::writer::write_qpdf_to_memory(pdf, |_| {}).expect("write setter output");
            let id_start = bytes
                .windows(b"/ID [".len())
                .position(|window| window == b"/ID [")
                .expect("writer output has an ID");
            let id_end = id_start
                + bytes[id_start..]
                    .iter()
                    .position(|byte| *byte == b']')
                    .expect("writer output closes the ID array");
            let mut normalized = bytes.clone();
            normalized.splice(id_start..=id_end, b"/ID []".iter().copied());
            let xref_start = bytes
                .windows(b"xref\n".len())
                .position(|window| window == b"xref\n")
                .expect("writer output has a classic xref");
            let trailer_start = bytes
                .windows(b"trailer ".len())
                .position(|window| window == b"trailer ")
                .expect("writer output has a trailer");
            (normalized, bytes[xref_start..trailer_start].to_vec())
        };

        for leave_dirty in [false, true] {
            let mut legacy = Pdf::open_mem_owned(source.clone()).expect("open legacy");
            let mut canonical = Pdf::open_mem_owned(source.clone()).expect("open canonical");

            let original_object =
                resolved_object(&mut legacy, object_ref).expect("resolve legacy catalog");
            let mut legacy_modified = original_object.clone();
            legacy_modified
                .as_dict_mut()
                .expect("legacy catalog is a dictionary")
                .insert("Marker", Object::Integer(7));

            let original_handle = canonical.get_object_handle(object_ref);
            original_handle
                .try_dereference()
                .expect("resolve canonical catalog");
            let original_handle_value = original_handle
                .shallow_copy()
                .expect("copy canonical catalog");
            let handle_modified = original_handle_value
                .shallow_copy()
                .expect("copy canonical replacement");
            handle_modified
                .replace_key(b"/Marker", ObjectHandle::integer(7))
                .expect("mark canonical replacement");

            legacy.set_object(object_ref, legacy_modified);
            canonical
                .replace_object(object_ref, handle_modified)
                .expect("replace canonical catalog");
            assert!(legacy.is_dirty(object_ref));
            assert!(canonical.is_dirty(object_ref));
            assert_eq!(
                resolved_object(&mut legacy, object_ref)
                    .expect("resolve modified legacy catalog")
                    .as_dict()
                    .and_then(|dict| dict.get("Marker"))
                    .and_then(Object::as_integer),
                Some(7)
            );
            assert_eq!(
                canonical
                    .get_object_handle(object_ref)
                    .get_key(b"/Marker")
                    .as_integer(),
                Some(7)
            );

            legacy.set_object(object_ref, original_object);
            canonical
                .replace_object(
                    object_ref,
                    original_handle_value
                        .shallow_copy()
                        .expect("copy canonical original value"),
                )
                .expect("restore canonical catalog");
            assert!(resolved_object(&mut legacy, object_ref)
                .expect("resolve restored catalog")
                .as_dict()
                .unwrap()
                .get("Marker")
                .is_none());
            assert!(!canonical.get_object_handle(object_ref).has_key(b"/Marker"));
            assert!(legacy.is_dirty(object_ref));
            assert!(canonical.is_dirty(object_ref));

            if !leave_dirty {
                legacy.clear_dirty(object_ref);
                canonical.clear_dirty(object_ref);
            }
            assert_eq!(legacy.is_dirty(object_ref), leave_dirty);
            assert_eq!(canonical.is_dirty(object_ref), leave_dirty);

            let (legacy_output, legacy_xref) = snapshot(&mut legacy);
            let (canonical_output, canonical_xref) = snapshot(&mut canonical);
            assert_eq!(
                legacy_output, canonical_output,
                "handle write-back must preserve the {leave_dirty:?} restore output snapshot"
            );
            assert_eq!(
                legacy_xref, canonical_xref,
                "handle write-back must preserve the {leave_dirty:?} restore xref bytes"
            );
        }
    }

    #[test]
    fn replace_object_clears_source_derived_side_tables() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        pdf.qpdf_parsed_xref_stream_refs.insert(object_ref);
        pdf.qpdf_dangling_refs.insert(object_ref);

        pdf.replace_object(object_ref, ObjectHandle::integer(7))
            .expect("replace canonical object");

        assert!(!pdf.qpdf_parsed_xref_stream_refs.contains(&object_ref));
        assert!(!pdf.qpdf_dangling_refs.contains(&object_ref));
    }

    #[test]
    fn replace_object_rejects_indirect_replacement_without_mutation() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let target = pdf.get_object_handle(object_ref);
        target.try_dereference().expect("resolve target");
        let before = target.get_key(b"/Type").as_name().expect("catalog type");

        let indirect_replacement = pdf.get_object_handle(ObjectRef::new(2, 0));
        let error = pdf
            .replace_object(object_ref, indirect_replacement)
            .expect_err("qpdf rejects an indirect replacement handle");

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: QPDF::replaceObject called with indirect object handle"
        );
        assert_eq!(target.object_ref(), Some(object_ref));
        assert_eq!(target.get_key(b"/Type").as_name(), Some(before));
        assert!(!pdf.is_dirty(object_ref));
    }

    #[test]
    fn replace_object_rejects_a_foreign_direct_value_without_mutation() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open target");
        let mut foreign_pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open foreign");
        let target_ref = ObjectRef::new(99, 0);
        let target = pdf.get_object_handle(target_ref);
        let foreign_root = foreign_pdf.get_object_handle(ObjectRef::new(1, 0));
        foreign_root
            .try_dereference()
            .expect("resolve foreign root");
        let foreign_direct = foreign_root.get_key(b"/Type");
        assert!(foreign_direct.is_direct());

        let error = pdf
            .replace_object(target_ref, foreign_direct)
            .expect_err("qpdf rejects a value owned by another document");

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: Attempting to add an object from a different QPDF. Use QPDF::copyForeignObject to add objects from another file."
        );
        assert!(target.is_same_object_as(&pdf.get_object_handle(target_ref)));
        assert!(!target.is_resolved());
        assert!(!pdf.is_dirty(target_ref));
    }

    #[test]
    fn replace_object_accepts_a_direct_reserved_value() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        let replacement = reserved.shallow_copy().expect("reserved copy");
        let target_ref = ObjectRef::new(99, 0);
        assert!(replacement.is_direct());
        assert!(replacement.is_reserved());
        assert!(pdf.resolver.registered_handle(target_ref).is_none());

        let target = pdf
            .replace_object(target_ref, replacement)
            .expect("qpdf replaceObject accepts an initialized direct reserved value");

        assert!(target.is_reserved());
        assert_eq!(target.object_ref(), Some(target_ref));
        assert!(pdf.resolver.registered_handle(target_ref).is_some());
        assert!(pdf.is_dirty(target_ref));
    }

    #[test]
    fn replace_object_accepts_a_destroyed_value() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let destroyed = pdf.new_reserved().expect("reserved object");
        destroyed.disconnect();
        let target_ref = ObjectRef::new(99, 0);
        assert!(destroyed.is_direct());
        assert_eq!(destroyed.type_code().expect("type code"), 14);
        assert!(pdf.resolver.registered_handle(target_ref).is_none());

        let target = pdf
            .replace_object(target_ref, destroyed)
            .expect("qpdf replaceObject accepts an initialized destroyed value");

        assert_eq!(target.type_code().expect("type code"), 14);
        assert_eq!(target.object_ref(), Some(target_ref));
        assert!(pdf.resolver.registered_handle(target_ref).is_some());
        assert!(pdf.is_dirty(target_ref));
    }

    #[test]
    fn set_object_preserves_an_objstm_default_xref_row() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(99, 0);
        let handle = pdf.get_object_handle(object_ref);
        // `resolveObjectsInStream` creates this effective type-0 row when an
        // ObjStm header names a member absent from the source xref table.
        pdf.resolver.insert_default_xref_entry_for_test(object_ref);
        assert!(pdf.resolver.xref_entry(object_ref).is_none());
        assert_eq!(
            pdf.get_xref_table().get(&object_ref),
            Some(&XrefEntry::Free { next: 0 })
        );

        pdf.set_object(object_ref, Object::Integer(41));

        assert_eq!(
            pdf.get_xref_table().get(&object_ref),
            Some(&XrefEntry::Free { next: 0 })
        );
        let current = pdf.get_object_handle(object_ref);
        assert!(current.is_same_object_as(&handle));
        assert_eq!(current.as_integer(), Some(41));
    }

    #[test]
    fn replace_object_preserves_an_objstm_default_xref_row() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(99, 0);
        let handle = pdf.get_object_handle(object_ref);
        // This is qpdf's `m->xref_table[og]` side effect for an ObjStm
        // header member not declared by the source xref table.
        pdf.resolver.insert_default_xref_entry_for_test(object_ref);
        assert!(pdf.resolver.xref_entry(object_ref).is_none());
        assert_eq!(
            pdf.get_xref_table().get(&object_ref),
            Some(&XrefEntry::Free { next: 0 })
        );

        let current = pdf
            .replace_object(object_ref, ObjectHandle::integer(42))
            .expect("replace canonical handle");

        assert_eq!(
            pdf.get_xref_table().get(&object_ref),
            Some(&XrefEntry::Free { next: 0 })
        );
        assert!(current.is_same_object_as(&handle));
        assert_eq!(current.as_integer(), Some(42));
    }

    #[test]
    fn set_object_then_objstm_default_xref_row_is_visible_later() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(99, 0);

        pdf.set_object(object_ref, Object::Integer(41));
        assert!(!pdf.get_xref_table().contains_key(&object_ref));

        pdf.resolver.insert_default_xref_entry_for_test(object_ref);

        assert_eq!(
            pdf.get_xref_table().get(&object_ref),
            Some(&XrefEntry::Free { next: 0 })
        );
        assert_eq!(pdf.get_object_handle(object_ref).as_integer(), Some(41));
    }

    #[test]
    fn replace_object_then_objstm_default_xref_row_is_visible_later() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(99, 0);

        pdf.replace_object(object_ref, ObjectHandle::integer(42))
            .expect("replace canonical handle");
        assert!(!pdf.get_xref_table().contains_key(&object_ref));

        pdf.resolver.insert_default_xref_entry_for_test(object_ref);

        assert_eq!(
            pdf.get_xref_table().get(&object_ref),
            Some(&XrefEntry::Free { next: 0 })
        );
        assert_eq!(pdf.get_object_handle(object_ref).as_integer(), Some(42));
    }

    #[test]
    fn remove_object_handle_nullifies_outstanding_handles_before_cache_removal() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        let alias = handle.clone();
        handle.try_dereference().expect("resolve target");
        assert!(handle.get_parsed_offset() >= 0);
        assert!(pdf.resolver.xref_entry(object_ref).is_some());

        pdf.remove_object_handle(object_ref)
            .expect("remove canonical object");

        assert!(alias.is_same_object_as(&handle));
        assert!(handle.is_direct());
        assert!(handle.is_null());
        assert_eq!(handle.object_ref(), None);
        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        assert_eq!(handle.description(), "");
        assert!(pdf.resolver.registered_handle(object_ref).is_none());
        assert!(pdf.resolver.xref_entry(object_ref).is_none());
        assert!(
            !pdf.qpdf_removed_refs.contains(&object_ref),
            "canonical removeObject cache mutation must not populate the legacy snapshot filter"
        );
        assert!(pdf.is_dirty(object_ref));

        let fresh = pdf.get_object_handle(object_ref);
        assert!(!fresh.is_same_object_as(&handle));
        fresh
            .try_dereference()
            .expect("removed ref resolves as missing");
        assert!(fresh.is_indirect());
        assert!(fresh.is_null());
    }

    #[test]
    fn get_all_objects_returns_indirect_handles_in_object_ref_order() {
        // `minimal_pdf_bytes` has three live objects (1 0, 2 0, 3 0) and one
        // free entry: the `0 65535 f` free-list head that every classic xref
        // table carries. The exact expected list below therefore also pins
        // that the free-list head is excluded, matching qpdf's
        // `getAllObjects()` (whose backing `xref_table` never contains free
        // entries in the first place).
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handles = pdf.get_all_objects().expect("get all object handles");
        assert!(handles.iter().all(ObjectHandle::is_indirect));
        let refs: Vec<_> = handles.iter().map(|h| h.object_ref().unwrap()).collect();
        let mut sorted = refs.clone();
        sorted.sort();
        assert_eq!(refs, sorted);
        assert_eq!(
            refs,
            vec![
                ObjectRef::new(1, 0),
                ObjectRef::new(2, 0),
                ObjectRef::new(3, 0),
            ]
        );
    }

    #[test]
    fn get_all_objects_reuses_the_canonical_handle_for_an_already_registered_ref() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(2, 0);
        let pre_registered = pdf.get_object_handle(object_ref);

        let handles = pdf.get_all_objects().expect("get all object handles");

        let found = handles
            .iter()
            .find(|handle| handle.object_ref() == Some(object_ref))
            .expect("ref 2 0 is present in the result");
        assert!(
            found.is_same_object_as(&pre_registered),
            "a ref already registered via get_object_handle must not be re-minted"
        );
    }

    #[test]
    fn get_all_objects_includes_a_ref_registered_only_via_handle_registry() {
        // Register a ref that never appears in the source xref table at all
        // (the dangling case): the union must not drop registry-only refs.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let dangling_ref = ObjectRef::new(99, 0);
        pdf.get_object_handle(dangling_ref);

        let handles = pdf.get_all_objects().expect("get all object handles");

        assert!(handles
            .iter()
            .any(|handle| handle.object_ref() == Some(dangling_ref)));
    }

    fn trailer_only_dangling_info_pdf(info_ref: &str) -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = BTreeMap::new();
        for (object_ref, body) in [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".as_slice()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".as_slice()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".as_slice(),
            ),
        ] {
            offsets.insert(object_ref, bytes.len() as u64);
            bytes.extend_from_slice(format!("{object_ref} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_start = bytes.len() as u64;
        bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        for object_ref in 1..=3 {
            bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&object_ref]).as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 4 /Root 1 0 R /Info {info_ref} >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn get_all_objects_excludes_deleted_objects_from_xref_and_canonical_snapshots() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(3, 0);
        let handle = pdf.get_object_handle(object_ref);
        handle
            .try_dereference()
            .expect("resolve object before deletion");
        assert!(pdf.get_xref_table().contains_key(&object_ref));

        pdf.delete_object(object_ref);

        assert!(!pdf.get_xref_table().contains_key(&object_ref));
        assert!(handle.is_null());
        assert!(!pdf
            .get_all_objects()
            .expect("enumerate after deletion")
            .iter()
            .any(|candidate| candidate.object_ref() == Some(object_ref)));
    }

    #[test]
    fn canonical_removal_does_not_tombstone_reconstructed_xref_entries() {
        let bytes = minimal_pdf_bytes();
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("open");
        // Use an unreferenced number so this assertion isolates repaired-xref
        // registration from ordinary child-reference discovery in the page
        // tree of `minimal_pdf_bytes`.
        let removed_ref = ObjectRef::new(99, 7);
        let repaired_ref = ObjectRef::new(99, 0);
        pdf.remove_object_handle(removed_ref)
            .expect("remove canonical object");

        // Simulate reconstruction repopulating the source table from the
        // original bytes after qpdf's exact cache/xref removal. This is not a
        // free-row registration, so the xref-local deleted set has no role.
        let object_offset = bytes
            .windows(b"3 0 obj".len())
            .position(|window| window == b"3 0 obj")
            .expect("object 3 offset") as u64;
        pdf.resolver.insert_xref_entry(
            repaired_ref,
            XrefEntry::Uncompressed {
                offset: object_offset,
            },
        );
        pdf.resolver.mark_reconstructed_xref_for_test();

        assert!(pdf.get_xref_table().contains_key(&repaired_ref));
    }

    #[test]
    fn replacing_a_removed_object_reopens_its_canonical_xref_slot() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let removed_ref = ObjectRef::new(99, 7);
        let replacement_ref = ObjectRef::new(99, 0);
        pdf.remove_object_handle(removed_ref)
            .expect("remove canonical object");

        pdf.set_object(replacement_ref, Object::Integer(7));
        pdf.resolver
            .insert_xref_entry(replacement_ref, XrefEntry::Uncompressed { offset: 0 });

        assert!(pdf.resolver.xref_entry(replacement_ref).is_some());
        assert!(pdf.resolver.registered_handle(replacement_ref).is_some());
    }

    #[test]
    fn get_all_objects_enumerates_a_deep_canonical_replacement() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(3, 0);
        let mut replacement = Object::Integer(7);
        for _ in 0..(crate::object::MAX_INLINE_DEPTH + 5) {
            replacement = Object::Array(vec![replacement]);
        }
        pdf.set_object(object_ref, replacement.clone());
        assert_eq!(
            resolved_object(&mut pdf, object_ref).expect("resolve replacement"),
            replacement
        );

        let found = pdf
            .get_all_objects()
            .expect("enumerate canonical replacement")
            .into_iter()
            .find(|candidate| candidate.object_ref() == Some(object_ref))
            .expect("replacement object remains enumerated");
        assert_eq!(
            found
                .materialize()
                .expect("materialize canonical replacement"),
            replacement
        );
    }

    #[test]
    fn get_all_objects_reuses_stream_dictionary_for_deep_replacement() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).expect("resolve original stream");
        let original_dict = handle.as_stream_dict().expect("original stream dictionary");
        let parsed_offset = original_dict.get_parsed_offset();
        assert!(
            parsed_offset >= 0,
            "source dictionary must have a parsed offset"
        );

        let mut nested = Object::Integer(7);
        for _ in 0..(crate::object::MAX_INLINE_DEPTH + 5) {
            let mut dict = Dictionary::new();
            dict.insert("Next", nested);
            nested = Object::Dictionary(dict);
        }
        let mut replacement_dict = Dictionary::new();
        replacement_dict.insert("Deep", nested);
        pdf.set_object(
            object_ref,
            Object::Stream(Stream::new(replacement_dict, b"new data".to_vec())),
        );

        let found = pdf
            .get_all_objects()
            .expect("enumerate deep stream replacement")
            .into_iter()
            .find(|candidate| candidate.object_ref() == Some(object_ref))
            .expect("replacement stream remains enumerated");
        let current_dict = found
            .as_stream_dict()
            .expect("replacement stream dictionary");
        assert!(
            current_dict.is_same_object_as(&original_dict),
            "canonical replacement must reuse the existing stream dictionary handle"
        );
        assert_eq!(
            current_dict.get_parsed_offset(),
            parsed_offset,
            "reusing the dictionary must preserve its parsed offset"
        );
        assert!(
            current_dict
                .as_dictionary()
                .expect("dictionary entries")
                .contains_key(b"/Deep".as_slice()),
            "the reused dictionary must contain the replacement entries"
        );
    }

    #[test]
    fn get_all_objects_accepts_an_overdeep_stream_dictionary_replacement() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).expect("resolve original stream");

        let mut nested = Object::Integer(7);
        for _ in 0..=crate::parser::MAX_PARSE_DEPTH {
            let mut dict = Dictionary::new();
            dict.insert("Next", nested);
            nested = Object::Dictionary(dict);
        }
        let mut replacement_dict = Dictionary::new();
        replacement_dict.insert("TooDeep", nested);
        pdf.set_object(
            object_ref,
            Object::Stream(Stream::new(replacement_dict, b"new data".to_vec())),
        );

        let found = pdf
            .get_all_objects()
            .expect("an over-deep programmatic stream dictionary is a valid replacement")
            .into_iter()
            .find(|candidate| candidate.object_ref() == Some(object_ref))
            .expect("replacement stream remains enumerated");
        assert!(
            found
                .as_stream_dict()
                .expect("replacement stream dictionary")
                .as_dictionary()
                .expect("replacement dictionary entries")
                .contains_key(b"/TooDeep".as_slice()),
            "the canonical stream dictionary must retain an over-deep replacement"
        );
    }

    #[test]
    fn get_all_objects_reconciles_a_deep_replacement_without_reading_old_stream_data() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open(ReadFailingCursor::new(bytes)).expect("open stream fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)
            .expect("resolve the original stream handle");
        assert!(
            handle.as_stream_data().is_none(),
            "source stream data must remain lazy before reconciliation"
        );

        let mut replacement = Object::Integer(7);
        for _ in 0..(crate::object::MAX_INLINE_DEPTH + 5) {
            replacement = Object::Array(vec![replacement]);
        }
        pdf.set_object(object_ref, replacement.clone());
        pdf.resolver
            .with_reader_mut(|reader| reader.fail_reads = true);

        let found = pdf
            .get_all_objects()
            .expect("reconcile the replacement without reading the old stream")
            .into_iter()
            .find(|candidate| candidate.object_ref() == Some(object_ref))
            .expect("replacement handle is enumerated");
        assert_eq!(
            found
                .materialize()
                .expect("materialize canonical replacement"),
            replacement
        );
    }

    #[test]
    fn get_all_objects_reconciles_memos_before_resolving_the_replaced_source() {
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
                b"3 0 obj\n<< /Old 9 0 R >>\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let object_ref = ObjectRef::new(3, 0);
        let mut replacement = Object::Integer(7);
        for _ in 0..(crate::object::MAX_INLINE_DEPTH + 5) {
            replacement = Object::Array(vec![replacement]);
        }
        pdf.set_object(object_ref, replacement);

        let objects = pdf
            .get_all_objects()
            .expect("enumerate the replacement without resolving the old source");
        assert!(!objects
            .iter()
            .any(|handle| handle.object_ref() == Some(ObjectRef::new(9, 0))));
    }

    #[test]
    fn get_all_objects_reconciles_content_stream_token_memos() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let replacements = [
            (ObjectRef::new(3, 0), Object::Operator(b"Do".to_vec())),
            (
                ObjectRef::new(99, 0),
                Object::InlineImage(b"BI /W 1 ID x EI".to_vec()),
            ),
        ];
        for (object_ref, object) in &replacements {
            pdf.set_object(*object_ref, object.clone());
        }

        let objects = pdf
            .get_all_objects()
            .expect("content-stream-only replacements remain enumerable");
        for (object_ref, expected) in replacements {
            let found = objects
                .iter()
                .find(|handle| handle.object_ref() == Some(object_ref))
                .expect("replacement handle is enumerated");
            assert_eq!(found.materialize().expect("materialize token"), expected);
        }
    }

    #[test]
    fn get_all_objects_reconciles_nested_content_stream_token_memos() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let mut nested_dict = Dictionary::new();
        nested_dict.insert("Operator", Object::Operator(b"Do".to_vec()));
        nested_dict.insert(
            "InlineImage",
            Object::InlineImage(b"BI /W 1 ID x EI".to_vec()),
        );
        let replacement = Object::Array(vec![
            Object::Operator(b"q".to_vec()),
            Object::Dictionary(nested_dict),
        ]);
        let object_ref = ObjectRef::new(98, 0);
        pdf.set_object(object_ref, replacement.clone());

        let found = pdf
            .get_all_objects()
            .expect("nested content-stream replacements remain enumerable")
            .into_iter()
            .find(|handle| handle.object_ref() == Some(object_ref))
            .expect("nested replacement handle is enumerated");
        assert_eq!(
            found.materialize().expect("materialize nested replacement"),
            replacement
        );
    }

    #[test]
    fn get_all_objects_excludes_the_object_zero_free_list_head() {
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
                b"3 0 obj\n<< /Free 0 65535 R /Invalid 77 65535 R >>\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let invalid_generation_ref = ObjectRef::new(77, u16::MAX);
        pdf.get_object_handle(invalid_generation_ref)
            .set_resolved(ObjectValue::Integer(7));

        let objects = pdf.get_all_objects().expect("enumerate source objects");
        assert!(objects.iter().all(|handle| {
            handle.object_ref().is_none_or(|object_ref| {
                object_ref.number != 0 && object_ref.generation != u16::MAX
            })
        }));
    }

    #[test]
    fn install_parsed_xref_stream_handles_registers_canonical_objects() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let historical_ref = ObjectRef::new(99, 0);
        let mut historical_dict = Dictionary::new();
        historical_dict.insert("Value", Object::Integer(7));
        let historical = Object::Dictionary(historical_dict);
        pdf.install_test_parsed_xref_stream_handle(historical_ref, historical.clone())
            .expect("install canonical historical entry");

        let found = pdf
            .get_all_objects()
            .expect("enumerate retained xref-stream cache entries")
            .into_iter()
            .find(|handle| handle.object_ref() == Some(historical_ref))
            .expect("historical xref-stream handle is enumerated");
        assert_eq!(
            found.materialize().expect("materialize historical entry"),
            historical
        );
    }

    #[test]
    fn install_parsed_xref_stream_handles_skips_invalid_or_stateful_entries() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let historical = Object::Integer(7);

        for object_ref in [ObjectRef::new(0, 0), ObjectRef::new(99, u16::MAX)] {
            pdf.install_test_parsed_xref_stream_handle(object_ref, historical.clone())
                .expect("skip invalid historical entry");
        }

        let removed_ref = ObjectRef::new(100, 0);
        pdf.qpdf_removed_refs.insert(removed_ref);
        pdf.install_test_parsed_xref_stream_handle(removed_ref, historical.clone())
            .expect("skip removed historical entry");

        let xref_ref = ObjectRef::new(1, 0);
        pdf.install_test_parsed_xref_stream_handle(xref_ref, historical.clone())
            .expect("skip effective xref entry");

        let resolved_ref = ObjectRef::new(101, 0);
        let resolved = pdf.get_object_handle(resolved_ref);
        resolved.set_resolved(ObjectValue::Integer(42));
        pdf.install_test_parsed_xref_stream_handle(resolved_ref, historical.clone())
            .expect("retain already-resolved historical entry");

        let missing_ref = ObjectRef::new(102, 0);
        let missing = pdf.get_object_handle(missing_ref);
        missing.set_resolved(ObjectValue::Null);
        pdf.cache.set_missing(missing_ref);
        pdf.install_test_parsed_xref_stream_handle(missing_ref, historical)
            .expect("restore missing historical entry");

        pdf.get_all_objects()
            .expect("enumerate canonical historical entries");

        assert_eq!(resolved.as_integer(), Some(42));
        assert_eq!(missing.as_integer(), Some(7));
    }

    #[test]
    fn qpdf_json_historical_unresolved_fallback_is_null_without_a_canonical_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let historical_ref = ObjectRef::new(103, 0);
        pdf.qpdf_parsed_xref_stream_refs.insert(historical_ref);

        assert_eq!(
            pdf.resolve_qpdf_json_object(historical_ref)
                .expect("unresolved qpdf historical lookup"),
            Object::Null
        );
        assert_eq!(
            pdf.resolve_qpdf_json_object(historical_ref)
                .expect("canonical unresolved qpdf historical lookup"),
            Object::Null
        );
    }

    #[test]
    fn get_all_objects_registers_trailer_only_dangling_references() {
        let mut pdf = Pdf::open_mem_owned(trailer_only_dangling_info_pdf("99 0 R")).expect("open");
        let info = pdf
            .get_all_objects()
            .expect("enumerate trailer-only reference")
            .into_iter()
            .find(|candidate| candidate.object_ref() == Some(ObjectRef::new(99, 0)))
            .expect("trailer-only reference is represented exactly once");
        assert!(
            info.is_resolved(),
            "dangling trailer references must be resolved before enumeration returns"
        );
        assert!(
            info.is_null(),
            "an absent trailer-only body resolves to null"
        );
    }

    #[test]
    fn get_all_objects_ignores_an_invalid_trailer_reference() {
        let mut pdf =
            Pdf::open_mem_owned(trailer_only_dangling_info_pdf("99 65535 R")).expect("open");
        let objects = pdf
            .get_all_objects()
            .expect("invalid trailer reference must not abort enumeration");

        assert!(!objects
            .iter()
            .any(|candidate| { candidate.object_ref() == Some(ObjectRef::new(99, u16::MAX)) }));
    }

    #[test]
    fn lift_bounded_propagates_stream_dictionary_errors() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let mut dict = Dictionary::new();
        dict.insert("Invalid", Object::Operator(b"q".to_vec()));
        let stream = Object::Stream(Stream::new(dict, Vec::new()));

        let error = pdf
            .lift_bounded(&stream, 0, crate::object::MAX_INLINE_DEPTH)
            .expect_err("content-only stream dictionary values must be rejected");
        assert!(error
            .to_string()
            .contains("content-stream-only token has no ObjectValue representation"));
    }

    #[test]
    fn get_all_objects_registers_a_top_level_replacement_target() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let redirect_ref = ObjectRef::new(100, 0);
        let target_ref = ObjectRef::new(999, 0);
        pdf.set_object(redirect_ref, Object::Reference(target_ref));

        let objects = pdf
            .get_all_objects()
            .expect("enumerate top-level replacement target");
        assert!(objects
            .iter()
            .any(|candidate| candidate.object_ref() == Some(redirect_ref)));
        assert!(objects
            .iter()
            .any(|candidate| candidate.object_ref() == Some(target_ref)));
    }

    #[test]
    fn live_object_refs_excludes_deleted_canonical_registry_slots() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let deleted_ref = ObjectRef::new(99, 0);
        // Keep the canonical handle registered while presenting the legacy
        // cache state that live enumeration must filter out. `delete_object`
        // also marks the cache entry deleted, which exercises the same
        // qpdf-facing source-xref guard.
        pdf.cache.set_deleted(deleted_ref);

        assert!(pdf.object_refs().contains(&deleted_ref));
        assert!(!pdf.live_object_refs().contains(&deleted_ref));

        let placeholder_ref = ObjectRef::new(5, 1);
        let placeholder = pdf.get_object_handle(placeholder_ref);
        assert!(!placeholder.is_resolved());
        assert!(pdf.object_refs().contains(&placeholder_ref));
        assert!(!pdf.live_object_refs().contains(&placeholder_ref));
    }

    #[test]
    fn live_object_refs_excludes_unresolved_registry_only_slots() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let placeholder_ref = ObjectRef::new(99, 0);
        let handle = pdf.get_object_handle(placeholder_ref);
        let allocated = pdf
            .make_indirect_object_handle(ObjectHandle::integer(7))
            .expect("make indirect");
        let allocated_ref = allocated.object_ref().expect("allocated ref");

        assert!(!handle.is_resolved());
        assert!(pdf.object_refs().contains(&placeholder_ref));
        assert!(!pdf.live_object_refs().contains(&placeholder_ref));
        assert!(pdf.live_object_refs().contains(&allocated_ref));
    }

    #[test]
    fn live_object_refs_includes_a_canonical_indirect_null_allocation() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let resources = ObjectHandle::dictionary(vec![(
            b"/XObject".to_vec(),
            ObjectHandle::dictionary(vec![(b"/Null".to_vec(), ObjectHandle::null())]),
        )]);

        resources
            .make_resources_indirect(&mut pdf)
            .expect("promote the direct null resource value");
        let allocated_null = resources.get_key(b"/XObject").get_key(b"/Null");
        let allocated_ref = allocated_null
            .object_ref()
            .expect("promoted null must have an indirect identity");

        assert!(allocated_null.is_null());
        assert!(pdf.object_refs().contains(&allocated_ref));
        assert!(pdf.live_object_refs().contains(&allocated_ref));
    }

    #[test]
    fn object_refs_excludes_a_resolved_dangling_registry_slot() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let dangling_ref = ObjectRef::new(99, 0);
        let handle = pdf.get_object_handle(dangling_ref);
        pdf.resolve(&handle).expect("resolve dangling reference");
        assert!(handle.is_null());

        assert!(!pdf.object_refs().contains(&dangling_ref));
        assert!(!pdf.live_object_refs().contains(&dangling_ref));
    }

    #[test]
    fn trailer_handle_is_direct_with_a_canonical_indirect_root_child() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handle = pdf.trailer();
        assert!(handle.is_direct());
        let dict = handle
            .as_dictionary()
            .expect("trailer is a dictionary handle");
        assert!(dict.contains_key(b"/Root".as_slice()) || dict.contains_key(b"/Size".as_slice()));

        let root_handle = dict.get(b"/Root".as_slice()).expect("trailer has /Root");
        assert!(root_handle.is_indirect());
        assert_eq!(root_handle.object_ref(), pdf.root_ref());
    }

    #[test]
    fn trailer_handle_is_the_same_canonical_handle_for_repeated_calls() {
        // A trailer with only an indirect `/Root` couldn't catch a
        // non-canonical `trailer_handle`: indirect children already route
        // through the memoized `handle_registry` regardless of how the
        // trailer handle itself is produced. A direct nested value (`/ID`,
        // as a real trailer carries) has no such registry to fall back on,
        // so it actually exercises the trailer handle's own identity.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.trailer.insert(
            "ID",
            Object::Array(vec![
                Object::String(b"0123456789abcdef".to_vec()),
                Object::String(b"0123456789abcdef".to_vec()),
            ]),
        );

        let first = pdf.trailer();
        let second = pdf.trailer();

        assert!(
            first.is_same_object_as(&second),
            "repeated trailer_handle calls must return the same canonical handle"
        );
    }

    #[test]
    fn trailer_handle_degrades_to_null_when_nesting_exceeds_the_parse_depth_bound() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let depth = crate::parser::MAX_PARSE_DEPTH + 5;
        let mut nested = Object::Integer(1);
        for _ in 0..depth {
            nested = Object::Array(vec![nested]);
        }
        pdf.trailer.insert("DeeplyNested", nested);

        let handle = pdf.trailer();

        assert!(handle.is_null());
    }

    #[test]
    fn trailer_key_handle_survives_an_unrelated_sibling_entrys_deep_nesting() {
        // The whole-trailer walk `trailer_handle` performs degrades every
        // key to null once *any* sibling entry exceeds `MAX_PARSE_DEPTH` —
        // `trailer_key_handle` must not inherit that coupling: `/QTest`
        // itself is shallow here, only its unrelated sibling `/Deep` is not.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.trailer.insert("QTest", Object::Boolean(true));
        let depth = crate::parser::MAX_PARSE_DEPTH + 5;
        let mut nested = Object::Integer(1);
        for _ in 0..depth {
            nested = Object::Array(vec![nested]);
        }
        pdf.trailer.insert("Deep", nested);

        assert!(
            pdf.trailer().is_null(),
            "sanity: the whole-trailer walk does degrade here"
        );
        let handle = pdf.trailer_key_handle(b"QTest");
        assert_eq!(handle.as_boolean(), Some(true));
    }

    #[test]
    fn root_ref_survives_a_trailer_handle_degraded_to_null() {
        // Mirrors `trailer_key_handle_survives_an_unrelated_sibling_entrys_deep_nesting`:
        // once `trailer_handle` has degraded the whole-tree walk to a bare,
        // context-less null handle, `root_ref` must fall back to the legacy
        // snapshot rather than calling `get_key`/`try_get_key` on that null
        // handle (which previously panicked -- the null handle has no `Pdf`
        // context to route the resulting type-mismatch warning through).
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let root = pdf.trailer.get_ref("Root");
        let depth = crate::parser::MAX_PARSE_DEPTH + 5;
        let mut nested = Object::Integer(1);
        for _ in 0..depth {
            nested = Object::Array(vec![nested]);
        }
        pdf.trailer.insert("Deep", nested);

        assert!(
            pdf.trailer().is_null(),
            "sanity: the whole-trailer walk does degrade here"
        );
        assert_eq!(pdf.root_ref(), root);
    }

    #[test]
    fn root_handle_survives_a_trailer_handle_degraded_to_null_with_no_prior_call() {
        // Mirrors `root_ref_survives_a_trailer_handle_degraded_to_null`, but
        // for `root_handle` when it has never been called before: the
        // trailer memo degrading to null must not be read as "no root at
        // all" -- it must fall back to the shallow, depth-safe
        // `trailer_key_handle("Root")` lookup instead of fabricating a null
        // candidate.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let depth = crate::parser::MAX_PARSE_DEPTH + 5;
        let mut nested = Object::Integer(1);
        for _ in 0..depth {
            nested = Object::Array(vec![nested]);
        }
        pdf.trailer.insert("Deep", nested);

        assert!(
            pdf.trailer().is_null(),
            "sanity: the whole-trailer walk does degrade here"
        );
        let root = pdf
            .root_handle()
            .expect("a valid /Root must survive unrelated trailer sibling damage");
        assert!(root.as_dictionary().is_some());
    }

    #[test]
    fn trailer_key_handle_is_null_for_a_missing_key() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handle = pdf.trailer_key_handle(b"NoSuchKey");
        assert!(handle.is_null());
    }

    #[test]
    fn trailer_key_handle_accepts_the_keys_own_value_nested_past_max_inline_depth() {
        // Codex Review on PR #610: a value nested between `MAX_INLINE_DEPTH`
        // and `MAX_PARSE_DEPTH` parses successfully (a real document can
        // legitimately contain it), so `resolve_borrowed`/the legacy
        // `resolve_chain` bridge already accepts it — `trailer_key_handle`
        // must too, or it would report `/QTest` as null while a caller
        // still using the legacy path for the same key sees the real value,
        // the same contradiction fixed for an unrelated sibling's nesting
        // above.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let depth = crate::object::MAX_INLINE_DEPTH + 5;
        let mut nested = Object::Integer(1);
        for _ in 0..depth {
            nested = Object::Array(vec![nested]);
        }
        pdf.trailer.insert("QTest", nested);

        let handle = pdf.trailer_key_handle(b"QTest");

        assert!(!handle.is_null());
        assert!(handle.as_array().is_some());
    }

    // `lift`/`lift_bounded` is a legacy materialization helper and has no
    // parser stack-growth guard of its own. Building this synthetic
    // `Pdf`/`Object` tree inside a larger spawned stack keeps the test focused
    // on its depth contract without making either type cross a thread.
    #[test]
    fn trailer_key_handle_is_null_when_the_keys_own_value_exceeds_the_parse_depth_bound() {
        std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(|| {
                let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
                let depth = crate::parser::MAX_PARSE_DEPTH + 5;
                let mut nested = Object::Integer(1);
                for _ in 0..depth {
                    nested = Object::Array(vec![nested]);
                }
                pdf.trailer.insert("QTest", nested);

                let handle = pdf.trailer_key_handle(b"QTest");

                assert!(handle.is_null());
            })
            .expect("nesting-guard test thread must start")
            .join()
            .expect("nesting guard must return null before exhausting the stack");
    }

    // coderabbit (PR #610): the `+ 5` margin above and below
    // `MAX_PARSE_DEPTH` in the two tests above cannot catch an off-by-one at
    // the bound itself. These pin the exact boundary: a value nested to
    // precisely `MAX_PARSE_DEPTH` is accepted, one hop deeper is null.
    #[test]
    fn trailer_key_handle_accepts_the_keys_own_value_at_exactly_the_parse_depth_bound() {
        std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(|| {
                let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
                let mut nested = Object::Integer(1);
                for _ in 0..crate::parser::MAX_PARSE_DEPTH {
                    nested = Object::Array(vec![nested]);
                }
                pdf.trailer.insert("QTest", nested);

                let handle = pdf.trailer_key_handle(b"QTest");

                assert!(!handle.is_null());
                assert!(handle.as_array().is_some());
            })
            .expect("nesting-guard test thread must start")
            .join()
            .expect(
                "nesting guard must accept exactly MAX_PARSE_DEPTH before exhausting the stack",
            );
    }

    #[test]
    fn trailer_key_handle_is_null_one_hop_past_the_parse_depth_bound() {
        std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(|| {
                let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
                let mut nested = Object::Integer(1);
                for _ in 0..crate::parser::MAX_PARSE_DEPTH + 1 {
                    nested = Object::Array(vec![nested]);
                }
                pdf.trailer.insert("QTest", nested);

                let handle = pdf.trailer_key_handle(b"QTest");

                assert!(handle.is_null());
            })
            .expect("nesting-guard test thread must start")
            .join()
            .expect(
                "nesting guard must return null one hop past the bound before exhausting the stack",
            );
    }

    #[test]
    fn trailer_key_handle_lifts_an_indirect_value_to_a_canonical_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let root_ref = pdf.root_ref().expect("root ref");
        pdf.trailer.insert("QTest", Object::Reference(root_ref));

        let handle = pdf.trailer_key_handle(b"QTest");

        assert_eq!(handle.object_ref(), Some(root_ref));
    }

    #[test]
    fn lifted_null_handles_remain_contextless() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handle = pdf
            .lift_to_handle_bounded(&Object::Null, 0, crate::parser::MAX_PARSE_DEPTH)
            .expect("null is representable as a handle");

        let error = handle
            .try_get_key(b"/Missing")
            .expect_err("a contextless null must keep the Error::System boundary");

        assert!(matches!(
            error,
            crate::Error::System(message)
                if message == "operation for dictionary attempted on object of type null: returning null for attempted key retrieval"
        ));
    }

    #[test]
    fn resolve_is_a_no_op_for_a_direct_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let direct = ObjectHandle::integer(7);
        pdf.resolve(&direct).expect("a direct handle is a no-op");
        assert_eq!(direct.as_integer(), Some(7));
    }

    #[test]
    fn resolve_matches_resolve_borrowed_for_a_live_object() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).expect("resolve handle");

        let legacy = resolved_object(&mut pdf, object_ref).expect("resolve legacy");
        let legacy_dict = legacy.as_dict().expect("legacy resolves to a dictionary");
        let dict = handle
            .as_dictionary()
            .expect("handle resolves to a dictionary");
        assert_eq!(dict.len(), legacy_dict.iter().count());
        assert_eq!(
            dict.get(b"/Pages".as_slice())
                .and_then(ObjectHandle::object_ref),
            legacy_dict.get_ref("Pages")
        );
    }

    #[test]
    fn resolve_to_terminal_is_a_no_op_for_an_already_terminal_value() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        let result = pdf
            .resolve_to_terminal(&handle)
            .expect("resolve a plain, never-redirected object");

        assert!(
            result.is_same_object_as(&handle),
            "no chase needed: same handle back"
        );
        assert!(result.as_dictionary().is_some());
        assert_eq!(result.as_reference(), None);
    }

    #[test]
    fn resolve_to_terminal_ref_reports_the_objects_own_ref_for_a_natural_single_hop() {
        // No `set_object` redirect is involved at all: `object_ref` resolves
        // directly to its dictionary. The terminal ref must be the object's
        // own ref, not `None` — this is the case
        // `resolve_to_terminal`'s "already terminal" fast path
        // takes, and it must still report a ref for a caller that needs one
        // (e.g. a diagnostic source-offset lookup keyed on that ref).
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        let (result, terminal_ref) = pdf
            .resolve_to_terminal_ref(&handle)
            .expect("resolve a plain, never-redirected object, also reporting its ref");

        assert!(result.as_dictionary().is_some());
        assert_eq!(terminal_ref, Some(object_ref));
    }

    #[test]
    fn resolve_to_terminal_ref_reports_no_ref_for_a_direct_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let direct = ObjectHandle::integer(7);

        let (result, terminal_ref) = pdf
            .resolve_to_terminal_ref(&direct)
            .expect("a direct handle has no ref to chase from");

        assert_eq!(result.as_integer(), Some(7));
        assert_eq!(terminal_ref, None);
    }

    #[test]
    fn resolve_does_not_chase_a_set_object_reference_redirect() {
        // `resolve` itself must keep its existing single-hop
        // contract: `Pdf::resolve_borrowed` (and `ref_chain.rs`'s own
        // bounded chain-follow primitive, used across ~20 production
        // modules) depends on observing an intermediate `Object::Reference`
        // per hop, not a silently pre-chased terminal value. Chasing
        // through to the terminal is `resolve_to_terminal`'s
        // job — see the tests below.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let target_ref = ObjectRef::new(100, 0);
        let redirect_ref = ObjectRef::new(200, 0);
        pdf.set_object(target_ref, Object::Boolean(true));
        pdf.set_object(redirect_ref, Object::Reference(target_ref));

        let handle = pdf.get_object_handle(redirect_ref);
        pdf.resolve(&handle).expect("resolve redirect handle");

        assert_eq!(handle.as_reference(), Some(target_ref));
        assert_eq!(
            handle.type_code().expect("type code"),
            13,
            "ot_unresolved, unchanged by this method"
        );
        assert_eq!(
            resolved_object(&mut pdf, redirect_ref).expect("legacy resolve"),
            Object::Reference(target_ref)
        );
    }

    #[test]
    fn resolve_to_terminal_chases_a_set_object_reference_redirect_to_its_terminal_value() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let target_ref = ObjectRef::new(100, 0);
        let redirect_ref = ObjectRef::new(200, 0);
        pdf.set_object(target_ref, Object::Boolean(true));
        pdf.set_object(redirect_ref, Object::Reference(target_ref));

        let handle = pdf.get_object_handle(redirect_ref);
        let result = pdf
            .resolve_to_terminal(&handle)
            .expect("resolve redirect handle to its terminal value");

        assert_eq!(result.as_boolean(), Some(true));
        assert_eq!(
            result.type_code().expect("type code"),
            3,
            "ot_boolean, not 13/unresolved"
        );
        assert_eq!(result.type_name().expect("type name"), "boolean");
        assert_eq!(
            result.object_ref(),
            Some(target_ref),
            "result is the terminal object's own canonical handle"
        );
        assert_eq!(
            result.unparse(),
            b"100 0 R",
            "indirect: its own reference form, the terminal's not the redirect's"
        );
        assert_eq!(result.unparse_resolved(), b"true");

        // The chain's first-ref identity is on the *original* handle, not
        // the returned terminal value — the same handle the caller already
        // holds. It, and the canonical handle `Pdf::resolve_borrowed`
        // shares, must stay untouched, or a later `resolve`/`resolve_borrowed`
        // call would silently start returning the chased value instead of
        // the redirect `Pdf::set_object` set.
        assert_eq!(handle.unparse(), b"200 0 R");
        assert_eq!(handle.as_reference(), Some(target_ref));
        assert_eq!(
            resolved_object(&mut pdf, redirect_ref).expect("legacy resolve"),
            Object::Reference(target_ref)
        );

        let (_ref_result, terminal_ref) = pdf
            .resolve_to_terminal_ref(&handle)
            .expect("resolve redirect handle, also reporting its terminal ref");
        assert_eq!(
            terminal_ref,
            Some(target_ref),
            "terminal ref is the redirect's target (100), not handle.object_ref() (200)"
        );
    }

    #[test]
    fn resolve_to_terminal_returns_the_canonical_handle_not_a_copy() {
        // qpdf's `QPDFObjectHandle::dereference`
        // (`libqpdf/QPDFObjectHandle.cc:2376-2383`) hands back the canonical
        // `QPDFObject` and every mutator edits it in place; there is no
        // copying dereference in qpdf to port. So the chased terminal is the
        // target's own handle — mutating a nested child through it reaches
        // the real document, exactly as mutating the target's own handle
        // would.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let target_ref = ObjectRef::new(100, 0);
        let redirect_ref = ObjectRef::new(200, 0);

        let mut nested = Dictionary::new();
        nested.insert(b"Inner", Object::Integer(1));
        let mut outer = Dictionary::new();
        outer.insert(b"Nested", Object::Dictionary(nested));
        pdf.set_object(target_ref, Object::Dictionary(outer));
        pdf.set_object(redirect_ref, Object::Reference(target_ref));

        let handle = pdf.get_object_handle(redirect_ref);
        let result = pdf
            .resolve_to_terminal(&handle)
            .expect("resolve redirect handle to its terminal dictionary");

        let nested_handle = result
            .as_dictionary()
            .expect("terminal is a dictionary")
            .get(b"/Nested".as_slice())
            .expect("Nested present")
            .clone();
        nested_handle
            .replace_key(b"/Inner", ObjectHandle::integer(999))
            .unwrap();

        let canonical_target = pdf.get_object_handle(target_ref);
        pdf.resolve(&canonical_target)
            .expect("resolve canonical target");
        assert!(
            result.is_same_object_as(&canonical_target),
            "the chased terminal is the target's own canonical handle"
        );
        assert_eq!(
            result.object_ref(),
            Some(target_ref),
            "and keeps that object's indirect identity, so a caller can \
             mark_object_dirty the edit it just made"
        );
        let canonical_inner = canonical_target
            .as_dictionary()
            .expect("canonical target is a dictionary")
            .get(b"/Nested".as_slice())
            .and_then(ObjectHandle::as_dictionary)
            .and_then(|nested| {
                nested
                    .get(b"/Inner".as_slice())
                    .and_then(ObjectHandle::as_integer)
            });
        assert_eq!(
            canonical_inner,
            Some(999),
            "mutating the chased terminal's nested child edits the real document"
        );
    }

    #[test]
    fn resolve_to_terminal_chases_a_multi_hop_reference_redirect_chain() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let terminal_ref = ObjectRef::new(100, 0);
        let middle_ref = ObjectRef::new(200, 0);
        let outer_ref = ObjectRef::new(300, 0);
        pdf.set_object(terminal_ref, Object::Integer(42));
        pdf.set_object(middle_ref, Object::Reference(terminal_ref));
        pdf.set_object(outer_ref, Object::Reference(middle_ref));

        let handle = pdf.get_object_handle(outer_ref);
        let result = pdf
            .resolve_to_terminal(&handle)
            .expect("resolve multi-hop redirect handle to its terminal value");

        assert_eq!(result.as_integer(), Some(42));
        assert_eq!(
            result.object_ref(),
            Some(terminal_ref),
            "result is the chain's *last* hop's own canonical handle, not the \
             first (outer_ref) or middle"
        );

        let (_ref_result, observed_terminal_ref) = pdf
            .resolve_to_terminal_ref(&handle)
            .expect("resolve multi-hop redirect handle, also reporting its terminal ref");
        assert_eq!(
            observed_terminal_ref,
            Some(terminal_ref),
            "terminal ref is the chain's *last* hop, not the first (outer_ref) or middle"
        );

        // The chain's first-ref identity is on `handle` itself. Neither it
        // (outer) nor the intermediate hop's own canonical handle is
        // mutated: `ref_chain.rs`'s own chain-walk over either ref still
        // sees exactly the one real hop `Pdf::set_object` recorded.
        assert_eq!(handle.unparse(), b"300 0 R");
        assert_eq!(handle.as_reference(), Some(middle_ref));
        let middle_handle = pdf.get_object_handle(middle_ref);
        assert_eq!(middle_handle.as_reference(), Some(terminal_ref));
        assert_eq!(
            resolved_object(&mut pdf, outer_ref).expect("legacy resolve"),
            Object::Reference(middle_ref)
        );
    }

    #[test]
    fn resolve_to_terminal_bounds_a_self_referential_redirect_without_hanging() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let self_ref = ObjectRef::new(100, 0);
        pdf.set_object(self_ref, Object::Reference(self_ref));

        let handle = pdf.get_object_handle(self_ref);
        let result = pdf
            .resolve_to_terminal(&handle)
            .expect("a self-referential redirect must not hang, overflow, or error");

        assert!(result.is_null());
        // The canonical handle is untouched by the cycle-bound fallback.
        assert_eq!(handle.as_reference(), Some(self_ref));
    }

    #[test]
    fn resolve_to_terminal_bounds_a_mutual_redirect_cycle_without_hanging() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let ref_a = ObjectRef::new(100, 0);
        let ref_b = ObjectRef::new(200, 0);
        pdf.set_object(ref_a, Object::Reference(ref_b));
        pdf.set_object(ref_b, Object::Reference(ref_a));

        let handle_a = pdf.get_object_handle(ref_a);
        let result = pdf
            .resolve_to_terminal(&handle_a)
            .expect("a mutual redirect cycle must not hang, overflow, or error");

        assert!(result.is_null());

        // Neither canonical handle is mutated by the cycle-bound fallback:
        // both are left exactly as `Pdf::set_object` wrote them.
        assert_eq!(handle_a.as_reference(), Some(ref_b));
        let handle_b = pdf.get_object_handle(ref_b);
        assert_eq!(handle_b.as_reference(), Some(ref_a));
    }

    #[test]
    fn resolve_to_terminal_accepts_a_chain_exactly_at_the_depth_limit() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let terminal_ref = ObjectRef::new(1000, 0);
        pdf.set_object(terminal_ref, Object::Integer(7));
        let mut current_ref = terminal_ref;
        for i in 0..crate::ref_chain::MAX_REF_CHAIN_DEPTH {
            let next_ref = ObjectRef::new(1001 + i as u32, 0);
            pdf.set_object(next_ref, Object::Reference(current_ref));
            current_ref = next_ref;
        }

        let handle = pdf.get_object_handle(current_ref);
        let result = pdf
            .resolve_to_terminal(&handle)
            .expect("a chain exactly at the depth limit must resolve, not be treated as cyclic");

        assert_eq!(result.as_integer(), Some(7));

        let (_ref_result, observed_terminal_ref) = pdf
            .resolve_to_terminal_ref(&handle)
            .expect("a chain exactly at the depth limit must resolve, also reporting its ref");
        assert_eq!(observed_terminal_ref, Some(terminal_ref));
    }

    #[test]
    fn resolve_to_terminal_treats_a_chain_one_hop_past_the_limit_as_cyclic() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let terminal_ref = ObjectRef::new(1000, 0);
        pdf.set_object(terminal_ref, Object::Integer(7));
        let mut current_ref = terminal_ref;
        for i in 0..=crate::ref_chain::MAX_REF_CHAIN_DEPTH {
            let next_ref = ObjectRef::new(2000 + i as u32, 0);
            pdf.set_object(next_ref, Object::Reference(current_ref));
            current_ref = next_ref;
        }

        let handle = pdf.get_object_handle(current_ref);
        let result = pdf
            .resolve_to_terminal(&handle)
            .expect("a too-long chain falls back rather than erroring");

        assert!(result.is_null());

        let (ref_fallback, observed_terminal_ref) = pdf
            .resolve_to_terminal_ref(&handle)
            .expect("a too-long chain falls back rather than erroring");
        assert!(ref_fallback.is_null());
        assert_eq!(
            observed_terminal_ref, None,
            "ref and handle degrade together on the depth-cap fallback"
        );
    }

    #[test]
    fn redirect_chain_warning_sink_failure_propagates_after_collection() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let terminal_ref = ObjectRef::new(1000, 0);
        pdf.set_object(terminal_ref, Object::Integer(7));
        let mut current_ref = terminal_ref;
        for i in 0..=crate::ref_chain::MAX_REF_CHAIN_DEPTH {
            let next_ref = ObjectRef::new(3000 + i as u32, 0);
            pdf.set_object(next_ref, Object::Reference(current_ref));
            current_ref = next_ref;
        }
        let handle = pdf.get_object_handle(current_ref);
        fail_warning_delivery(&mut pdf);

        assert!(matches!(
            pdf.resolve_to_terminal_ref(&handle),
            Err(crate::Error::System(ref message)) if message == "sink write failure 1"
        ));
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
    }

    /// White-box companion to the public canonical-handle null behavior:
    /// canonical resolution updates the ResolverHandle slot only. It does
    /// not populate the legacy raw-Object cache as a side effect.
    #[test]
    fn resolve_keeps_legacy_cache_untouched_for_nulls() {
        let bytes = classic_pdf_with_bodies(&[b"1 0 obj\nnull\nendobj\n"], ObjectRef::new(1, 0));
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open literal-null fixture");

        let literal_null_ref = ObjectRef::new(1, 0);
        let literal_null_handle = pdf.get_object_handle(literal_null_ref);
        pdf.resolve(&literal_null_handle)
            .expect("resolve literal null");
        assert!(literal_null_handle.is_null());
        assert!(
            !matches!(
                pdf.cache.entry(literal_null_ref),
                Some(CacheEntry::Resolved(_))
            ),
            "canonical resolution must not populate the legacy raw cache"
        );

        let dangling_ref = ObjectRef::new(999, 0);
        let dangling_handle = pdf.get_object_handle(dangling_ref);
        pdf.resolve(&dangling_handle).expect("resolve dangling ref");
        assert!(dangling_handle.is_null());
        assert!(
            pdf.cache.entry(dangling_ref).is_none(),
            "a ref absent from the xref table must never gain a cache entry just from resolving its handle"
        );
    }

    #[test]
    fn resolve_compressed_member_accepts_inline_nesting_past_max_inline_depth() {
        // Nesting between MAX_INLINE_DEPTH (256) and MAX_PARSE_DEPTH (500) is
        // accepted by the canonical live parser for ObjStm members. The
        // handle route must preserve that qpdf parser bound rather than
        // applying the tighter structural-walk limit used by legacy
        // materialization helpers.
        let depth = crate::object::MAX_INLINE_DEPTH + 5;
        let mut member_value = Vec::new();
        member_value.extend(std::iter::repeat_n(b'[', depth));
        member_value.push(b'1');
        member_value.extend(std::iter::repeat_n(b']', depth));

        let header = b"7 0 ".to_vec();
        let first = header.len();
        let mut objstm_body = header;
        objstm_body.extend_from_slice(&member_value);
        let stream_object = format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
            objstm_body.len()
        )
        .into_bytes();
        let mut body = stream_object;
        body.extend_from_slice(&objstm_body);
        body.extend_from_slice(b"\nendstream\nendobj\n");

        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n", &body],
            ObjectRef::new(1, 0),
        );
        let objstm_offset = bytes
            .windows(b"4 0 obj".len())
            .position(|window| window == b"4 0 obj")
            .unwrap() as u64;

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open deeply-nested ObjStm fixture");
        pdf.cache
            .set_unresolved(ObjectRef::new(4, 0), objstm_offset);
        pdf.cache.set_compressed(ObjectRef::new(7, 0), 4, 0);
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(4, 0),
            XrefEntry::Uncompressed {
                offset: objstm_offset,
            },
        );
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(7, 0),
            XrefEntry::Compressed {
                stream: 4,
                index: 0,
            },
        );

        let handle = pdf.get_object_handle(ObjectRef::new(7, 0));
        pdf.resolve(&handle)
            .expect("nesting between MAX_INLINE_DEPTH and MAX_PARSE_DEPTH must now succeed");

        let legacy = resolved_object(&mut pdf, ObjectRef::new(7, 0))
            .expect("resolve_borrowed must also accept it");
        assert!(legacy.as_array().is_some());
        assert!(handle.as_array().is_some());
    }

    #[test]
    fn resolve_compressed_member_recovers_qpdfs_excessive_nesting() {
        // `QPDFParser` does not make deep ObjStm data a hard parse failure:
        // its context-owned recovery warns and returns null when it encounters
        // the 501st container. The former slice parser was strict here, so
        // this regression pins the live parser's shared ObjStm behavior.
        let depth = crate::parser::MAX_PARSE_DEPTH + 5;
        let mut member_value = Vec::new();
        member_value.extend(std::iter::repeat_n(b'[', depth));
        member_value.push(b'1');
        member_value.extend(std::iter::repeat_n(b']', depth));

        let header = b"7 0 ".to_vec();
        let first = header.len();
        let mut objstm_body = header;
        objstm_body.extend_from_slice(&member_value);
        let stream_object = format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
            objstm_body.len()
        )
        .into_bytes();
        let mut body = stream_object;
        body.extend_from_slice(&objstm_body);
        body.extend_from_slice(b"\nendstream\nendobj\n");

        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n", &body],
            ObjectRef::new(1, 0),
        );
        let objstm_offset = bytes
            .windows(b"4 0 obj".len())
            .position(|window| window == b"4 0 obj")
            .unwrap() as u64;

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open over-deeply-nested ObjStm fixture");
        pdf.cache
            .set_unresolved(ObjectRef::new(4, 0), objstm_offset);
        pdf.cache.set_compressed(ObjectRef::new(7, 0), 4, 0);
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(4, 0),
            XrefEntry::Uncompressed {
                offset: objstm_offset,
            },
        );
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(7, 0),
            XrefEntry::Compressed {
                stream: 4,
                index: 0,
            },
        );

        let handle = pdf.get_object_handle(ObjectRef::new(7, 0));
        pdf.resolve(&handle)
            .expect("qpdf-style excessive nesting recovery");
        assert!(handle.is_null());
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .filter(|message| message.contains("excessively deeply nested"))
                .collect::<Vec<_>>(),
            vec!["object stream 4 (object 7 0, offset 504): ignoring excessively deeply nested data structure"]
        );
    }

    #[test]
    fn resolve_uncompressed_accepts_the_same_nesting_depth_via_canonical_parser() {
        // Task 6's `resolve` routed every indirect handle
        // (Uncompressed and Compressed alike) through the same `lift`
        // bridge, so this exact depth (between MAX_INLINE_DEPTH and
        // MAX_PARSE_DEPTH) used to be rejected for BOTH. This task reroutes
        // the Uncompressed case to the canonical live parser bounded only by
        // MAX_PARSE_DEPTH — so it now succeeds, matching what
        // `resolve_borrowed` (which was never subject to MAX_INLINE_DEPTH)
        // already accepted at this depth. The Compressed case now also
        // accepts this depth (see
        // `resolve_compressed_member_accepts_inline_nesting_past_max_inline_depth`),
        // via the canonical live parser rather than `lift_bounded`. This is an
        // intentional behavior change, not a weakened test: the assertion
        // below pins parity with `resolve_borrowed`, not just "no longer
        // errors".
        let depth = crate::object::MAX_INLINE_DEPTH + 5;
        let mut body = b"1 0 obj\n".to_vec();
        body.extend(std::iter::repeat_n(b'[', depth));
        body.push(b'1');
        body.extend(std::iter::repeat_n(b']', depth));
        body.extend_from_slice(b"\nendobj\n");

        let bytes = classic_pdf_with_bodies(&[&body], ObjectRef::new(1, 0));
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open deeply-nested fixture");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)
            .expect("nesting between MAX_INLINE_DEPTH and MAX_PARSE_DEPTH must now succeed");

        let legacy =
            resolved_object(&mut pdf, object_ref).expect("resolve_borrowed must also accept it");
        assert!(legacy.as_array().is_some());
        assert!(handle.as_array().is_some());
    }

    /// The canonical ObjStm route must preserve every direct scalar,
    /// dictionary, and nested indirect-reference variant emitted by qpdf's
    /// live parser. This is the coverage anchor for that source class.
    #[test]
    fn resolve_compressed_member_preserves_scalar_dictionary_and_reference_variants() {
        let member_value: &[u8] =
            b"<< /B true /R 1.5 /RL .5 /N /Foo /S (bar) /Nul null /Kid 5 0 R /Sub << /X 1 >> >>";
        let header = b"7 0 ".to_vec();
        let first = header.len();
        let mut objstm_body = header;
        objstm_body.extend_from_slice(member_value);
        let stream_object = format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length {} >>\nstream\n",
            objstm_body.len()
        )
        .into_bytes();
        let mut body = stream_object;
        body.extend_from_slice(&objstm_body);
        body.extend_from_slice(b"\nendstream\nendobj\n");

        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n", &body],
            ObjectRef::new(1, 0),
        );
        let objstm_offset = bytes
            .windows(b"4 0 obj".len())
            .position(|window| window == b"4 0 obj")
            .unwrap() as u64;

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open scalar-variant ObjStm fixture");
        pdf.cache
            .set_unresolved(ObjectRef::new(4, 0), objstm_offset);
        pdf.cache.set_compressed(ObjectRef::new(7, 0), 4, 0);
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(4, 0),
            XrefEntry::Uncompressed {
                offset: objstm_offset,
            },
        );
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(7, 0),
            XrefEntry::Compressed {
                stream: 4,
                index: 0,
            },
        );

        let handle = pdf.get_object_handle(ObjectRef::new(7, 0));
        pdf.resolve(&handle)
            .expect("resolve compressed scalar/dictionary/reference dict");

        let dict = handle.as_dictionary().expect("dictionary");
        assert!(dict.contains_key(b"/B".as_slice()));
        assert!(dict.contains_key(b"/R".as_slice()));
        assert_eq!(
            dict.get(b"/RL".as_slice())
                .and_then(ObjectHandle::as_real_literal),
            Some((0.5, b".5".to_vec()))
        );
        assert!(dict.contains_key(b"/N".as_slice()));
        assert!(dict.contains_key(b"/S".as_slice()));
        assert!(dict.get(b"/Nul".as_slice()).expect("Nul entry").is_null());

        let kid = dict.get(b"/Kid".as_slice()).expect("Kid entry");
        assert!(kid.is_indirect());
        assert_eq!(kid.object_ref(), Some(ObjectRef::new(5, 0)));

        let sub = dict
            .get(b"/Sub".as_slice())
            .expect("Sub entry")
            .as_dictionary()
            .expect("nested dictionary");
        assert_eq!(
            sub.get(b"/X".as_slice()).and_then(ObjectHandle::as_integer),
            Some(1)
        );
    }

    #[test]
    fn resolve_preserves_source_filter_array_alignment() {
        let object_ref = ObjectRef::new(1, 0);
        let stream_body = b"1 0 obj\n<< /Length 3 /Filter [/ASCIIHexDecode /Crypt /FlateDecode] /DecodeParms [<< /A 1 >> << /Name /Identity >> << /B 2 >>] >>\nstream\nabc\nendstream\nendobj\n";
        let bytes = classic_pdf_with_bodies(&[stream_body], object_ref);
        let mut pdf = Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                description: "input.pdf".to_owned(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open source-filter fixture");
        *pdf.encryption.borrow_mut() = Some(EncryptionState {
            encryption_v: 2,
            encryption_r: 3,
            cf_stream: EncryptionMode::Identity,
            ..explicit_rc4_encryption_state()
        });

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)
            .expect("resolve through the canonical stream parser");

        let dict = handle.as_stream_dict().expect("stream dictionary");
        let entries = dict.as_dictionary().expect("dictionary entries");
        let filters = entries
            .get(b"/Filter".as_slice())
            .and_then(ObjectHandle::as_array)
            .expect("source filter array");
        assert_eq!(
            filters
                .iter()
                .map(|filter| filter.as_name().expect("filter name"))
                .collect::<Vec<_>>(),
            vec![
                b"ASCIIHexDecode".to_vec(),
                b"Crypt".to_vec(),
                b"FlateDecode".to_vec()
            ]
        );

        let decode_parms = entries
            .get(b"/DecodeParms".as_slice())
            .and_then(ObjectHandle::as_array)
            .expect("source decode-params array");
        assert_eq!(decode_parms.len(), filters.len());
        assert_eq!(
            decode_parms[1]
                .as_dictionary()
                .and_then(|params| params.get(b"/Name".as_slice()).cloned())
                .and_then(|name| name.as_name()),
            Some(b"Identity".to_vec())
        );
    }

    /// Object 0 (the qpdf-style xref free-list head) is exempt from every
    /// tracking side effect `delete_object` otherwise performs, including
    /// the handle-graph invalidation the previous test pins for every other
    /// ref: it must return before touching `qpdf_removed_refs` or the
    /// canonical handle registry at all.
    #[test]
    fn delete_object_is_a_no_op_for_object_number_zero() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let free_list_head = ObjectRef::new(0, 65535);

        pdf.delete_object(free_list_head);

        assert!(
            !pdf.qpdf_removed_refs.contains(&free_list_head),
            "object 0 must never be tracked as an explicitly removed reference"
        );
        assert!(
            pdf.resolver.registered_handle(free_list_head).is_none(),
            "object 0 must not gain a handle just from delete_object"
        );
    }

    /// `Pdf::prepare_qpdf_json_objects` can mark a ref's cache entry
    /// `Missing` (discovered as a dangling reference from a live object's
    /// own content) before `Pdf::get_object_handle` has ever been called for
    /// it. `Pdf::delete_object` must still invalidate the handle-graph
    /// bridge state (the memo entry and the handle itself) in that case, not
    /// only when the cache entry was already `Deleted`/`Missing` via a prior
    /// `delete_object` call — pinned here so a later narrowing of
    /// `resolve`'s fallback wildcard arm (the one thing this
    /// self-healed silently through before this test existed) gets caught
    /// instead of silently regressing.
    #[test]
    fn delete_object_resets_the_parsed_offset_of_an_already_resolved_handle() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Count 1 >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).expect("resolve");
        assert!(
            handle.get_parsed_offset() >= 0,
            "canonical parsing must record a real offset before deletion"
        );

        pdf.delete_object(object_ref);

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        assert!(handle.is_null());
    }

    #[test]
    fn delete_object_drops_the_source_description_of_an_already_resolved_handle() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Count 1 >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        handle.try_dereference().expect("resolve");
        let source_description = handle.description();
        assert!(
            source_description.contains("offset"),
            "the fixture must establish a source description before deletion"
        );

        pdf.delete_object(object_ref);

        assert_eq!(handle.description(), "object 1 0");
        handle
            .object_warning("deleted object warning")
            .expect("deleted handle warning should use the fallback description");
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .last()
                .expect("deleted warning is recorded")
                .message,
            "object 1 0: deleted object warning"
        );
        assert_ne!(handle.description(), source_description);
    }

    #[test]
    fn deleting_an_objstm_container_promotes_resolved_members_to_canonical_handles() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/three-page-objstm.pdf");
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(path).expect("open ObjStm fixture"),
        ))
        .expect("open ObjStm fixture");
        let member_ref = ObjectRef::new(7, 0);

        // The legacy cache has already materialized the member, matching
        // qpdf's cached compressed object before QPDF::removeObject erases
        // only the requested ObjStm cache entry (QPDF.cc:1996-2004).
        assert!(matches!(
            resolved_object(&mut pdf, member_ref).expect("resolve ObjStm member"),
            Object::Dictionary(_)
        ));

        pdf.delete_object(ObjectRef::new(1, 0));

        let member = pdf.get_object_handle(member_ref);
        member
            .try_dereference()
            .expect("cached ObjStm member must survive source-container deletion");
        assert!(
            member.as_dictionary().is_some(),
            "deleting the source ObjStm must not turn an already-resolved member into null"
        );
    }

    #[test]
    fn replacing_an_objstm_container_promotes_resolved_members_to_canonical_handles() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/three-page-objstm.pdf");
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(path).expect("open ObjStm fixture"),
        ))
        .expect("open ObjStm fixture");
        let member_ref = ObjectRef::new(7, 0);

        assert!(matches!(
            resolved_object(&mut pdf, member_ref).expect("resolve ObjStm member"),
            Object::Dictionary(_)
        ));

        // Keep both an unrelated object-stream row and an unmaterialized row
        // in the source xref so the promotion walk exercises its filtering
        // boundaries without manufacturing a second parsed stream fixture.
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(90, 0),
            XrefEntry::Compressed {
                stream: 2,
                index: 0,
            },
        );
        pdf.resolver.insert_xref_entry(
            ObjectRef::new(91, 0),
            XrefEntry::Compressed {
                stream: 1,
                index: 99,
            },
        );

        pdf.set_object(ObjectRef::new(1, 0), Object::Null);

        let member = pdf.get_object_handle(member_ref);
        member
            .try_dereference()
            .expect("cached ObjStm member must survive source-container replacement");
        assert!(
            member.as_dictionary().is_some(),
            "replacing the source ObjStm must not turn an already-resolved member into null"
        );
    }

    #[test]
    fn replace_object_promotes_resolved_objstm_members_before_replacing_container() {
        // Same qpdf invariant as `replacing_an_objstm_container_promotes_resolved_members_to_canonical_handles`
        // above (`set_object`), exercised through the handle-shaped setter:
        // `QPDF::replaceObject` only changes the requested cache slot, so an
        // already-resolved ObjStm member must be promoted to its own live
        // handle before the source container's canonical value is replaced.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/three-page-objstm.pdf");
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(path).expect("open ObjStm fixture"),
        ))
        .expect("open ObjStm fixture");
        let member_ref = ObjectRef::new(7, 0);

        assert!(matches!(
            resolved_object(&mut pdf, member_ref).expect("resolve ObjStm member"),
            Object::Dictionary(_)
        ));

        pdf.replace_object(ObjectRef::new(1, 0), ObjectHandle::null())
            .expect("replace canonical object");

        let member = pdf.get_object_handle(member_ref);
        member
            .try_dereference()
            .expect("cached ObjStm member must survive source-container replacement");
        assert!(
            member.as_dictionary().is_some(),
            "replacing the source ObjStm via the handle-shaped setter must not turn an \
             already-resolved member into null"
        );
    }

    #[test]
    fn remove_object_handle_promotes_resolved_objstm_members_before_removing_container() {
        // Same qpdf invariant as
        // `replace_object_promotes_resolved_objstm_members_before_replacing_container`,
        // exercised through the handle-shaped removal primitive:
        // `QPDF::removeObject` only erases the requested cache slot, so an
        // already-resolved ObjStm member must be promoted to its own live
        // handle before the source container's canonical value is removed.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/three-page-objstm.pdf");
        let mut pdf = Pdf::open(std::io::BufReader::new(
            std::fs::File::open(path).expect("open ObjStm fixture"),
        ))
        .expect("open ObjStm fixture");
        let member_ref = ObjectRef::new(7, 0);

        assert!(matches!(
            resolved_object(&mut pdf, member_ref).expect("resolve ObjStm member"),
            Object::Dictionary(_)
        ));

        pdf.remove_object_handle(ObjectRef::new(1, 0))
            .expect("remove canonical object");

        let member = pdf.get_object_handle(member_ref);
        member
            .try_dereference()
            .expect("cached ObjStm member must survive source-container removal");
        assert!(
            member.as_dictionary().is_some(),
            "removing the source ObjStm via the handle-shaped primitive must not turn an \
             already-resolved member into null"
        );
    }

    #[test]
    fn set_object_drops_the_replaced_handle_source_description() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Count 1 >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        handle.try_dereference().expect("resolve");
        assert!(
            handle.description().contains("offset"),
            "the fixture must establish a source description before replacement"
        );

        pdf.set_object(object_ref, Object::Integer(42));

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        assert_eq!(handle.description(), "object 1 0");
        handle
            .object_warning("replacement warning")
            .expect("replacement warning should use the replacement description");
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .last()
                .expect("replacement warning is recorded")
                .message,
            "object 1 0: replacement warning"
        );
    }

    #[test]
    fn set_object_drops_the_reused_stream_dictionary_source_description() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Length 5 >>\nstream\nHello\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        handle.try_dereference().expect("resolve stream");
        let dictionary = handle.as_stream_dict().expect("stream dictionary");
        assert!(
            dictionary.description().contains("offset"),
            "the fixture must establish a source description before replacement"
        );

        let mut replacement_dict = Dictionary::new();
        replacement_dict.insert("Length", Object::Integer(3));
        pdf.set_object(
            object_ref,
            Object::Stream(Stream::new(replacement_dict, b"Bye".to_vec())),
        );

        assert_eq!(
            handle
                .as_stream_dict()
                .expect("stream dictionary after replacement")
                .description(),
            "",
            "the reused dictionary must no longer identify the replaced source bytes"
        );
    }

    #[test]
    fn resolve_stamps_the_public_canonical_root_and_nested_handles() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Nested << /Value 7 >> >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

        pdf.resolve(&handle)
            .expect("resolve through the public canonical path");

        assert!(
            handle.description().contains("object 1 0 at offset"),
            "the canonical root must retain its source description"
        );
        let nested = handle
            .as_dictionary()
            .and_then(|entries| entries.get(b"/Nested".as_slice()).cloned())
            .expect("nested dictionary");
        assert!(
            nested.description().contains("object 1 0 at offset"),
            "the nested direct handle must retain its source description"
        );
    }

    #[test]
    fn resolve_preserves_leading_whitespace_before_a_scalar_description() {
        let bytes = classic_pdf_with_bodies(&[b"1 0 obj\n\n 7\nendobj\n"], ObjectRef::new(1, 0));
        let mut pdf = Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                description: "input.pdf".to_owned(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open scalar fixture");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

        pdf.resolve(&handle)
            .expect("resolve through the public canonical path");

        assert_eq!(
            handle.get_parsed_offset(),
            16,
            "the scalar provenance starts immediately after `obj`, before leading whitespace"
        );
        assert_eq!(
            handle.description(),
            "input.pdf, object 1 0 at offset 16",
            "the qpdf-style top-level scalar description must include its pre-tokenization offset"
        );
    }

    #[test]
    fn resolve_gives_canonical_parser_children_the_document_resolver() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Outer << /Inner << /Value 7 >> >> >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

        pdf.resolve(&handle)
            .expect("resolve through the public canonical path");

        let inner = handle
            .as_dictionary()
            .and_then(|entries| entries.get(b"/Outer".as_slice()).cloned())
            .and_then(|outer| outer.as_dictionary())
            .and_then(|entries| entries.get(b"/Inner".as_slice()).cloned())
            .expect("nested dictionary");
        inner
            .object_warning("nested warning")
            .expect("canonical parser children must retain the document warning resolver");
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|entry| entry.message.contains("nested warning")));
    }

    #[test]
    fn canonical_resolution_preserves_placeholder_text_in_the_input_description() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Value 7 >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_with_options(
            Cursor::new(bytes),
            PdfOpenOptions {
                description: "input-$PO-$OG.pdf".to_owned(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open fixture");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

        handle
            .try_dereference()
            .expect("resolve through the canonical lazy resolver");

        let expected_offset = handle.get_parsed_offset() + 2;
        assert_eq!(
            handle.description(),
            format!("input-{expected_offset}-1 0.pdf, object 1 0 at offset $PO"),
            "canonical descriptions must use qpdf's one-pass marker replacement"
        );
    }

    #[test]
    fn set_object_replacement_over_parser_depth_discards_source_description() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Count 1 >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        handle.try_dereference().expect("resolve");
        let source_description = handle.description();
        let source_end_offsets = handle.end_offsets();
        assert!(
            source_description.contains("offset"),
            "the fixture must establish a source description before replacement"
        );
        assert!(
            source_end_offsets.0 >= 0 && source_end_offsets.1 >= 0,
            "the fixture must establish source extents before replacement"
        );

        let mut replacement = Object::Null;
        for _ in 0..=(crate::parser::MAX_PARSE_DEPTH + 5) {
            replacement = Object::Array(vec![replacement]);
        }
        pdf.set_object(object_ref, replacement);

        assert_eq!(
            handle.description(),
            "object 1 0",
            "a canonical replacement must discard the source provenance"
        );
        assert_eq!(
            handle.end_offsets(),
            (NO_PARSED_OFFSET, NO_PARSED_OFFSET),
            "a canonical replacement must discard the source extents"
        );
        handle
            .object_warning("failed replacement warning")
            .expect("replacement handle warning should use its canonical description");
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .last()
                .expect("replacement warning is recorded")
                .message,
            "object 1 0: failed replacement warning"
        );
    }

    #[test]
    fn set_object_replacement_over_parser_depth_updates_canonical_handle() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Count 1 >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        handle.try_dereference().expect("resolve source object");

        let mut replacement = Object::Integer(7);
        for _ in 0..=crate::parser::MAX_PARSE_DEPTH {
            replacement = Object::Array(vec![replacement]);
        }
        pdf.set_object(object_ref, replacement);

        assert!(
            handle.as_array().is_some(),
            "qpdf-style replacement must update the canonical handle even when the legacy Object tree is deeply nested"
        );
        let mut current = handle.clone();
        for _ in 0..=crate::parser::MAX_PARSE_DEPTH {
            current = current
                .as_array()
                .expect("every replacement level must remain canonical")
                .into_iter()
                .next()
                .expect("every replacement array must retain its child");
        }
        assert_eq!(
            current.as_integer(),
            Some(7),
            "the canonical replacement must retain the terminal value past the parser depth"
        );
    }

    #[test]
    fn set_object_replacements_rejected_by_legacy_lift_are_canonicalized() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");

        let operator_ref = ObjectRef::new(3, 0);
        let operator_handle = pdf.get_object_handle(operator_ref);
        operator_handle
            .try_dereference()
            .expect("resolve the source before replacing it");
        let operator = Object::Operator(b"Do".to_vec());
        pdf.set_object(operator_ref, operator.clone());

        assert_eq!(
            operator_handle
                .materialize()
                .expect("materialize canonical operator replacement"),
            operator
        );

        let inline_ref = ObjectRef::new(99, 0);
        let inline = Object::InlineImage(b"BI /W 1 ID x EI".to_vec());
        pdf.set_object(inline_ref, inline.clone());
        assert_eq!(
            pdf.get_object_handle(inline_ref)
                .materialize()
                .expect("materialize canonical inline-image replacement"),
            inline
        );

        let deep_ref = ObjectRef::new(98, 0);
        let mut deep = Object::Integer(7);
        for _ in 0..(crate::object::MAX_INLINE_DEPTH + 5) {
            deep = Object::Array(vec![deep]);
        }
        pdf.set_object(deep_ref, deep.clone());
        assert_eq!(
            pdf.get_object_handle(deep_ref)
                .materialize()
                .expect("materialize canonical over-depth replacement"),
            deep
        );
    }

    /// Compare the canonical live parser's dictionary value for a plain
    /// Uncompressed object with the raw-object compatibility path. The
    /// canonical live parser and the legacy `Parser::dictionary` are two
    /// independently maintained left-to-right `BTreeMap::insert` passes over
    /// the same token stream; this pins that they still agree on a duplicate
    /// dictionary key (last write wins),
    /// rather than relying on that being merely "true today, unverified".
    #[test]
    fn canonical_and_legacy_dictionary_parsers_agree_on_a_duplicate_key() {
        let body: &[u8] = b"1 0 obj\n<< /A 1 /A 2 /B 3 >>\nendobj\n";
        let object_ref = ObjectRef::new(1, 0);

        // Legacy-only path: `resolve_qpdf_json_object` reads straight from
        // `self.cache` (`resolve_to_cache`'s own output), entirely
        // independent of the `ObjectHandle` bridge this task adds.
        let mut legacy_pdf =
            Pdf::open_mem_owned(classic_pdf_with_bodies(&[body], object_ref)).expect("open");
        let legacy = legacy_pdf
            .resolve_qpdf_json_object(object_ref)
            .expect("legacy resolve");

        // Canonical path: resolve the same source through the live
        // ObjectHandle parser, then materialize it only for comparison with
        // the raw-object result.
        let mut canonical_pdf =
            Pdf::open_mem_owned(classic_pdf_with_bodies(&[body], object_ref)).expect("open");
        let canonical_handle = canonical_pdf.get_object_handle(object_ref);
        canonical_pdf
            .resolve(&canonical_handle)
            .expect("canonical resolution");
        let canonical = canonical_handle
            .materialize()
            .expect("canonical materialization");

        assert_eq!(legacy, canonical);
        let Object::Dictionary(dict) = &legacy else {
            panic!("fixture body is always a dictionary"); // cov:ignore: unreachable
        };
        assert_eq!(
            dict.get("A"),
            Some(&Object::Integer(2)),
            "last write wins for a duplicate key"
        );
        assert_eq!(dict.get("B"), Some(&Object::Integer(3)));
    }

    #[test]
    fn legacy_lift_preserves_a_decoded_leading_slash_in_dictionary_key() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let mut dictionary = Dictionary::new();
        // The legacy dictionary stores the decoded name body. For the PDF
        // name /#2Ffoo, that body begins with `/`, so it must not be mistaken
        // for an already-canonical ObjectHandle key.
        dictionary.insert(b"/foo", Object::Integer(1));

        let handle = pdf
            .lift_object_to_handle(&Object::Dictionary(dictionary))
            .expect("lift legacy dictionary");
        let entries = handle.as_dictionary().expect("dictionary handle");
        assert_eq!(
            entries
                .get(b"//foo".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(1)
        );
        assert!(!entries.contains_key(b"/foo".as_slice()));

        let materialized = handle.materialize().expect("materialize");
        let materialized = materialized
            .as_dict()
            .expect("lifted value remains a dictionary");
        assert_eq!(materialized.get(b"/foo"), Some(&Object::Integer(1)));
    }

    #[test]
    fn writer_copy_encryption_source_returns_none_for_plaintext() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open plaintext fixture");

        assert!(pdf
            .writer_copy_encryption_source()
            .expect("plaintext copy source lookup")
            .is_none());
    }

    #[test]
    fn writer_copy_encryption_source_rejects_authenticated_state_without_encrypt_dict() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        *pdf.encryption.borrow_mut() = Some(explicit_rc4_encryption_state());

        let error = pdf
            .writer_copy_encryption_source()
            .expect_err("an authenticated state without /Encrypt must be rejected");

        assert!(matches!(error, Error::Unsupported(message)
            if message == "authenticated input has no /Encrypt dictionary"));
    }

    #[test]
    fn writer_copy_encryption_source_keeps_the_id_paired_with_the_authenticated_file_key() {
        // A V<5 file key is derived from /ID[0] (PDF 1.7 §7.6.3.3 Algorithm
        // 2). If a caller mutates the live trailer's /ID after opening --
        // for example while assembling metadata for a downstream write --
        // the copy-encryption source must still report the /ID[0] that
        // `file_key` was actually derived from, not the mutated live value.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v2-rc4-128-r3.pdf"
        );
        let fixture = std::fs::read(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v2-rc4-128-r3.pdf");
        let mut pdf = Pdf::open_mem_owned_with_options(
            fixture,
            PdfOpenOptions {
                password: b"user-v2".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open RC4 fixture");

        let trailer = pdf.trailer();
        let original_id = trailer
            .try_get_key(b"/ID")
            .expect("fetch trailer /ID")
            .try_array_item(0)
            .expect("fetch /ID[0]")
            .expect("/ID[0] present")
            .as_string()
            .expect("/ID[0] is a string");
        trailer
            .replace_key(
                b"/ID",
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"mutated-after-authentication".to_vec()),
                    ObjectHandle::string(b"mutated-after-authentication".to_vec()),
                ]),
            )
            .expect("mutate live trailer /ID after authentication");

        let source = pdf
            .writer_copy_encryption_source()
            .expect("authenticated RC4 fixture has a copy-encryption source")
            .expect("copy source is Some for an encrypted document");

        assert_eq!(
            source.id0, original_id,
            "copy-encryption source must use the /ID[0] the file key was derived from, \
             not a live trailer value mutated after authentication"
        );
    }
}
