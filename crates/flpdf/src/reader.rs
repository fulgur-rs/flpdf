//! qpdf correspondence: QPDF.cc object resolution, recovery, diagnostics, and authentication responsibilities.
pub(crate) mod file_object;
pub(crate) mod resolver;

use self::file_object::{
    finish_file_object, parse_file_object_syntax, FileObjectDiagnostic, PendingBody,
    PendingFileObject, RecoveryPolicy, ResolvedStreamLength,
};
use crate::cache::CacheEntry;
use crate::encrypt_setup::CopyEncryptionSource;
use crate::error::EncryptedError;
#[cfg(test)]
use crate::object::collect_qpdf_object_references;
use crate::object_handle::{ObjectValue, NO_PARSED_OFFSET};
#[cfg(feature = "qtest-driver")]
use crate::parser::array_item_source_offset;
#[cfg(feature = "qtest-driver")]
use crate::parser::dictionary_value_source_offset;
use crate::parser::parse_qpdf_file_object;
use crate::pipeline::rc4::PlRc4;
use crate::security::password::{normalize_password, PasswordMode};
use crate::security::standard::{
    check_owner_password, check_owner_password_r5, check_owner_password_r6,
    check_owner_password_v4, check_user_password, check_user_password_r5, check_user_password_r6,
    check_user_password_v4, decrypt_cipher_bytes, decrypt_strings_in_object, ObjectKeyAlg,
    StandardHandlerInputs, StandardHandlerR5Inputs, StringCipher,
};
use crate::tokenizer::Tokenizer;
use crate::{
    Diagnostics, Dictionary, Error, Object, ObjectHandle, ObjectRef, Result, Stream, XrefEntry,
    XrefForm,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::rc::Rc;

static NULL_OBJECT: Object = Object::Null;

use crate::pdf::{CompressedMemberProvenance, Pdf};

#[cfg(test)]
pub(crate) struct QpdfPreparedObjects {
    pub(crate) refs: Vec<ObjectRef>,
    pub(crate) max_object_id: u32,
}

/// qpdf `QPDF::EncryptionParameters` (`include/qpdf/QPDF.hh:899-923`).
///
/// qpdf's `encrypted` and `encryption_initialized` flags are folded into this
/// being an `Option<EncryptionState>`; its password fields are consumed during
/// authentication and not retained.
#[derive(Debug, Clone)]
pub(crate) struct EncryptionState {
    file_key: Vec<u8>,
    /// qpdf `encryption_V` / `encryption_R`. Read together and required, as
    /// qpdf requires them together before any password work
    /// (`libqpdf/QPDF_encryption.cc:770-777`).
    encryption_v: i64,
    encryption_r: i64,
    /// qpdf `cf_stream` / `cf_string` / `cf_file`. Only meaningful when
    /// `encryption_v >= 4`: qpdf leaves them at the constructor's `e_none`
    /// otherwise (`libqpdf/QPDF.cc:190-192`) and its consumers gate on `/V`
    /// before reading them. Go through [`Self::stream_method`] /
    /// [`Self::string_method`] rather than reading these directly.
    cf_stream: EncryptionMode,
    cf_string: EncryptionMode,
    cf_file: EncryptionMode,
    crypt_filters: BTreeMap<Vec<u8>, EncryptionMode>,
    encrypt_metadata: bool,
    encrypt_ref: Option<ObjectRef>,
    weak_crypto: bool,
    permissions: Permissions,
    /// Whether the supplied password authenticated as the user password.
    user_password_matched: bool,
    /// Whether the supplied password authenticated as the owner password.
    /// Many real PDFs share an empty password for both, so both flags can
    /// be true simultaneously.
    owner_password_matched: bool,
    /// qpdf `cached_object_encryption_key` / `cached_key_og`
    /// (`include/qpdf/QPDF.hh:918-919`). See [`Self::key_for_object`] for why
    /// the cache key deliberately omits `use_aes`.
    ///
    /// qpdf default-constructs `cached_key_og` to `QPDFObjGen(0, 0)` and
    /// compares with `!=`. Object 0 is never a real indirect object, so
    /// `Option<ObjectRef>` initialised to `None` is the same predicate in a
    /// shape that does not require a sentinel `ObjectRef`.
    cached_object_encryption_key: Vec<u8>,
    cached_key_og: Option<ObjectRef>,
}

/// What a crypt-filter switch decided: qpdf's `use_aes` when the object is to
/// be decrypted, `None` where qpdf `return`s from the `e_none` arm without
/// prepending anything, plus whether the caller owes an unknown-filter
/// warning.
type MethodChoice = (Option<bool>, bool);

impl EncryptionState {
    /// The crypt-filter switch qpdf writes twice: in `QPDF::decryptString`
    /// over `cf_string` (`libqpdf/QPDF_encryption.cc:982-1006`) and in
    /// `QPDF::decryptStream` over a `method` local that defaults to
    /// `cf_stream` (`:1062-1134`). qpdf does not factor the two together.
    ///
    /// `method` is what qpdf switches on; `cf` is the field its unknown-filter
    /// arm rewrites. They differ only for a stream whose own `/Crypt` filter
    /// named the method — qpdf still resets `cf_stream` there (`:1131`).
    fn select_method(
        method: EncryptionMode,
        cf: &mut EncryptionMode,
        encryption_v: i64,
    ) -> MethodChoice {
        if encryption_v < 4 {
            // qpdf initialises `use_aes = false` and enters the switch only
            // when `/V >= 4` (`:982-983`, `:1062-1063`), so everything older
            // is RC4 regardless of what the crypt filter fields hold.
            return (Some(false), false);
        }
        match method {
            EncryptionMode::Identity => (None, false),
            EncryptionMode::Aes128 | EncryptionMode::Aes256 => (Some(true), false),
            EncryptionMode::Rc4 => (Some(false), false),
            EncryptionMode::Unknown => {
                // qpdf warns once and then rewrites the filter to `e_aes`
                // specifically so the warning is not repeated for every
                // remaining object (`:1002-1004`, `:1130-1132`). The rewrite
                // is observable, so it is part of the port, not an
                // optimisation.
                *cf = EncryptionMode::Aes128;
                (Some(true), true)
            }
        }
    }

    /// qpdf `QPDF::decryptString`'s method selection (`:982-1006`).
    fn string_method(&mut self) -> MethodChoice {
        let (method, encryption_v) = (self.cf_string, self.encryption_v);
        Self::select_method(method, &mut self.cf_string, encryption_v)
    }

    /// qpdf `QPDF::decryptString` (`libqpdf/QPDF_encryption.cc:977-1039`)
    /// for one literal string owned by `object_ref`.
    fn decrypt_object_string(
        &mut self,
        object_ref: ObjectRef,
        bytes: &mut Vec<u8>,
    ) -> Result<bool> {
        let (use_aes, warn_unknown_string) = self.string_method();
        if let Some(use_aes) = use_aes {
            self.with_object_cipher(object_ref, use_aes, |cipher| {
                decrypt_cipher_bytes(bytes, cipher)
            })?;
        }
        Ok(warn_unknown_string)
    }

    /// qpdf `QPDF::decryptStream`'s method selection (`:1062-1134`), minus the
    /// `/Type` and `/Crypt` inspection that chooses `method` ahead of it.
    ///
    /// `method` is `None` for a stream that declared no `/Crypt` filter, which
    /// is qpdf falling back to `cf_stream` at `:1101`.
    fn stream_method(&mut self, method: Option<EncryptionMode>) -> MethodChoice {
        let (method, encryption_v) = (method.unwrap_or(self.cf_stream), self.encryption_v);
        Self::select_method(method, &mut self.cf_stream, encryption_v)
    }

    /// Whether qpdf's `decryptStream` would prepend a decryption stage for
    /// this stream method, without applying the unknown-filter state rewrite.
    /// The compatibility boundary uses this read-only form while deciding
    /// whether recovered source framing belongs to plaintext or ciphertext;
    /// warning/state mutation remains owned by the actual pipe operation.
    pub(crate) fn stream_method_transforms(&self, method: Option<EncryptionMode>) -> bool {
        let method = method.unwrap_or(self.cf_stream);
        self.encryption_v < 4 || !matches!(method, EncryptionMode::Identity)
    }

    /// qpdf `QPDF::compute_data_key` (`libqpdf/QPDF_encryption.cc:325-357`),
    /// Algorithm 3.1 from the PDF 1.7 Reference Manual.
    ///
    /// Not the same function as `security::standard::per_object_key`, which
    /// truncates to `min(file_key.len() + 5, 16)` and so drops the four salt
    /// bytes from the length qpdf takes the minimum against. The two agree for
    /// every `/V` and `/R` pair the standard handler actually admits, because
    /// AES requires a 128-bit key and `min(21, 16) == min(25, 16)`.
    fn compute_data_key(&self, og: ObjectRef, use_aes: bool) -> Vec<u8> {
        let mut result = self.file_key.clone();
        if self.encryption_v >= 5 {
            // Algorithm 3.1a (PDF 1.7 extension level 3): the encryption key
            // is used straight, so an object's key does not depend on its
            // object or generation number at all.
            return result;
        }

        // Low three bytes of the object ID and low two of the generation.
        let objid = og.number;
        let generation = u32::from(og.generation);
        result.push((objid & 0xff) as u8);
        result.push(((objid >> 8) & 0xff) as u8);
        result.push(((objid >> 16) & 0xff) as u8);
        result.push((generation & 0xff) as u8);
        result.push(((generation >> 8) & 0xff) as u8);
        if use_aes {
            result.extend_from_slice(b"sAlT");
        }

        let digest = crate::security::primitives::md5(&result);
        digest[..result.len().min(16)].to_vec()
    }

    /// Build the cipher qpdf keys for one object's strings and hand it to
    /// `apply`.
    ///
    /// qpdf's `if (use_aes) { Pl_AES_PDF } else { RC4 }`
    /// (`libqpdf/QPDF_encryption.cc:1011-1033`). The AES variant is not a
    /// separate decision: `Pl_AES_PDF` keys itself from the buffer it is
    /// handed (`libqpdf/Pl_AES_PDF.cc:12-34`), and `compute_data_key` makes
    /// that buffer 32 bytes exactly when `/V >= 5`.
    fn with_object_cipher<T>(
        &mut self,
        og: ObjectRef,
        use_aes: bool,
        apply: impl FnOnce(StringCipher<'_>) -> Result<T>,
    ) -> Result<T> {
        // qpdf's string and stream paths both enter through
        // `getKeyForObject`; copying releases the mutable cache borrow before
        // handing a key-backed cipher to the recursive object walk.
        let key = self.key_for_object(og, use_aes).to_vec();
        if !use_aes {
            return apply(StringCipher::Rc4 { key: &key });
        }
        if let Ok(key) = <&[u8; 32]>::try_from(key.as_slice()) {
            return apply(StringCipher::Aes256 { key });
        }
        let key = aes128_object_key(&key)?;
        apply(StringCipher::Aes128 { key: &key })
    }

    /// qpdf `QPDF::getKeyForObject` (`libqpdf/QPDF_encryption.cc:955-974`).
    ///
    /// The cache key is the object/generation pair **alone**: qpdf leaves
    /// `use_aes` out of it (`:962`). A document whose `/StrF` and `/StmF`
    /// disagree about AES therefore serves whichever key the first caller for
    /// that object derived, because `compute_data_key` appends `"sAlT"` only
    /// for AES. Reproducing that is the point of this method existing.
    ///
    /// qpdf's `!encrypted` `std::logic_error` guard (`:958-960`) has no
    /// counterpart: an `EncryptionState` only exists for an encrypted
    /// document.
    //
    fn key_for_object(&mut self, og: ObjectRef, use_aes: bool) -> &[u8] {
        if self.cached_key_og != Some(og) {
            let key = self.compute_data_key(og, use_aes);
            self.cached_object_encryption_key = key;
            self.cached_key_og = Some(og);
        }
        &self.cached_object_encryption_key
    }
}

/// Standard security handler permission bits from an encrypted document's `/P` entry.
///
/// These flags are advisory. They report the producer's requested restrictions but do
/// not enforce them while reading or rewriting the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    raw: i32,
}

impl Permissions {
    fn new(raw: i32) -> Self {
        Self { raw }
    }

    /// Raw signed `/P` value.
    pub fn raw(self) -> i32 {
        self.raw
    }

    /// Print the document, possibly at degraded quality if high-quality printing is denied.
    pub fn can_print(self) -> bool {
        self.has_bit(0x0004)
    }

    /// Modify document contents by operations other than controlled form/annotation edits.
    pub fn can_modify(self) -> bool {
        self.has_bit(0x0008)
    }

    /// Copy or otherwise extract text and graphics.
    pub fn can_copy(self) -> bool {
        self.has_bit(0x0010)
    }

    /// Add or modify annotations and interactive form fields.
    pub fn can_annotate(self) -> bool {
        self.has_bit(0x0020)
    }

    /// Fill in existing interactive form fields.
    pub fn can_fill_forms(self) -> bool {
        self.has_bit(0x0100)
    }

    /// Extract text and graphics for accessibility purposes.
    pub fn can_extract_for_accessibility(self) -> bool {
        self.has_bit(0x0200)
    }

    /// Assemble the document by inserting, rotating, or deleting pages/bookmarks.
    pub fn can_assemble(self) -> bool {
        self.has_bit(0x0400)
    }

    /// Print the document at high quality.
    pub fn can_print_high_quality(self) -> bool {
        self.has_bit(0x0800)
    }

    fn has_bit(self, bit: u32) -> bool {
        (self.raw as u32) & bit != 0
    }
}

/// qpdf `QPDF::encryption_method_e` (`include/qpdf/QPDF.hh:436`).
///
/// This names a *crypt filter method*, not a cipher. qpdf never picks the
/// cipher from the method: it hands `Pl_AES_PDF` whatever `compute_data_key`
/// returned and that pipeline keys itself from the buffer length
/// (`libqpdf/Pl_AES_PDF.cc:12-34`), while `compute_data_key` returns the
/// 32-byte file key unchanged once `/V >= 5`
/// (`libqpdf/QPDF_encryption.cc:337-340`). So an `/AESV2` crypt filter on a
/// `/V 5` document decrypts with AES-256, and an `/AESV3` one on a `/V 4`
/// document decrypts with AES-128. Go from a method to a cipher through
/// [`EncryptionState::select_method`] and
/// [`EncryptionState::with_object_cipher`], which carry qpdf's `use_aes` for
/// exactly this reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncryptionMode {
    Rc4,
    Aes128,
    Identity,
    Aes256,
    /// qpdf `e_unknown`: a `/CFM` qpdf does not recognise, or a crypt filter
    /// name with no `/CF` entry. qpdf deliberately keeps this rather than
    /// failing, so that a document whose unused crypt filter is unreadable
    /// still opens (`libqpdf/QPDF_encryption.cc:877-880`).
    Unknown,
}

impl EncryptionMode {
    /// qpdf's `show_encryption_method()` spelling for this method.
    ///
    /// Source: qpdf `libqpdf/QPDFJob.cc:674-697` `show_encryption_method()` —
    /// `e_rc4`→"RC4", `e_aes`→"AESv2", `e_aesv3`→"AESv3", `e_none`→"none",
    /// `e_unknown`→"unknown".
    /// flpdf's `Identity` (no-op crypt filter) maps to qpdf's `e_none`/"none".
    fn qpdf_name(self) -> &'static str {
        match self {
            EncryptionMode::Rc4 => "RC4",
            EncryptionMode::Aes128 => "AESv2",
            EncryptionMode::Aes256 => "AESv3",
            EncryptionMode::Identity => "none",
            EncryptionMode::Unknown => "unknown",
        }
    }
}

/// Read-only snapshot of an encrypted document's `/Encrypt` parameters,
/// surfaced for the `show-encryption` inspection subcommand.
/// Built by re-reading the `/Encrypt` dictionary plus the
/// already-authenticated `EncryptionState`; does not run or alter
/// authentication.
#[derive(Debug, Clone)]
pub struct EncryptionInfo {
    /// `/V` encryption algorithm version.
    pub v: i64,
    /// `/R` standard security handler revision.
    pub r: i64,
    /// Key length in bits (`/Length`, defaulting to 40 when absent for V<5;
    /// 256 for V=5).
    pub length_bits: i64,
    /// `/Filter` security handler name (e.g. `Standard`).
    pub filter: String,
    /// Raw signed `/P` permission bits.
    pub permissions: Permissions,
    /// `/EncryptMetadata` flag (defaults to true when absent).
    pub encrypt_metadata: bool,
    /// qpdf-style method name for the stream crypt filter (`StmF`).
    pub stream_method: &'static str,
    /// qpdf-style method name for the string crypt filter (`StrF`).
    pub string_method: &'static str,
    /// qpdf-style method name for the embedded-file crypt filter (`EFF`).
    ///
    /// A document that declares no `/EFF`, or whose `/EFF` is not a name,
    /// reports the stream method: `/EFF` is informational, and qpdf mirrors
    /// `cf_stream` into `cf_file` in that case.
    pub eff_method: &'static str,
    /// Named crypt filters from `/CF` mapped to their qpdf-style method
    /// names, e.g. `StdCF` → `AESv2`.
    pub named_crypt_filters: Vec<(String, &'static str)>,
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
    /// How `password` should be interpreted before key derivation. See
    /// [`PasswordMode`] for the qpdf-compatible semantics.
    pub password_mode: PasswordMode,
    /// Permit deprecated RC4-backed handlers and revision 5 AES-256.
    pub allow_weak_crypto: bool,
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
            allow_weak_crypto: false,
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

    /// Exact source framing removed by an authoritative `endstream` scan.
    pub(crate) fn recovered_stream_eol(&self, object_ref: ObjectRef) -> Option<&'static [u8]> {
        // The recovery scan runs against source bytes. When THIS stream's
        // payload was decrypted, the byte belongs to ciphertext framing rather
        // than to the plaintext returned by `resolve`. Document-wide
        // encryption is not sufficient: metadata under `/EncryptMetadata
        // false`, `/StmF /Identity`, and explicit `/Crypt /Identity` streams
        // keep plaintext source bytes and therefore retain the recovered EOL.
        if self.transformed_stream_refs.contains(&object_ref) {
            return None;
        }
        self.recovered_stream_eols
            .get(&object_ref)
            .copied()
            .map(crate::parser::RecoveredStreamEol::as_bytes)
    }

    /// Whether this document authenticated an `/Encrypt` dictionary while opening.
    pub fn is_encrypted(&self) -> bool {
        self.encryption.borrow().is_some()
    }

    pub(crate) fn encryption_ref(&self) -> Option<ObjectRef> {
        self.encryption
            .borrow()
            .as_ref()
            .and_then(|encryption| encryption.encrypt_ref)
    }

    /// Whether opening this document required the weak-crypto opt-in.
    pub fn uses_weak_crypto(&self) -> bool {
        self.encryption
            .borrow()
            .as_ref()
            .is_some_and(|encryption| encryption.weak_crypto)
    }

    /// Advisory standard security handler permissions from `/P`, if the document is encrypted.
    pub fn permissions(&self) -> Option<Permissions> {
        self.encryption
            .borrow()
            .as_ref()
            .map(|encryption| encryption.permissions)
    }

    /// Whether the password supplied at open time authenticated against the
    /// document's user password (`/U`). Always `false` for plaintext PDFs.
    pub fn user_password_matched(&self) -> bool {
        self.encryption
            .borrow()
            .as_ref()
            .is_some_and(|encryption| encryption.user_password_matched)
    }

    /// Whether the password supplied at open time authenticated against the
    /// document's owner password (`/O`). Always `false` for plaintext PDFs.
    /// Many PDFs use an empty password for both, so this can be true at the
    /// same time as [`Pdf::user_password_matched`].
    pub fn owner_password_matched(&self) -> bool {
        self.encryption
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
    pub fn writer_copy_encryption_source(&mut self) -> Result<Option<CopyEncryptionSource>> {
        let (file_key, encryption_v) = {
            let guard = self.encryption.borrow();
            let Some(encryption) = guard.as_ref() else {
                return Ok(None);
            };
            (encryption.file_key.clone(), encryption.encryption_v)
        };
        let encrypt_dict = self.encrypt_dictionary()?.ok_or_else(|| {
            Error::Unsupported("authenticated input has no /Encrypt dictionary".into())
        })?;
        let id0 = first_file_id(self.trailer())?.to_vec();

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
    /// `show-encryption` inspection subcommand.
    ///
    /// Returns `None` for plaintext PDFs. Version, revision and the
    /// crypt-filter methods come from the authenticated `EncryptionState`;
    /// only `/Length` and `/Filter`, which it does not retain, are re-read
    /// from the `/Encrypt` dictionary. This does NOT re-run or alter
    /// authentication (layer-2 owns that ordering); it only reflects state
    /// from a document already opened successfully.
    ///
    /// # Errors
    ///
    /// - [`Error::Encrypted`] ([`EncryptedError::Malformed`]) when the re-read
    ///   `/Encrypt` dictionary is missing or has the wrong type for `/Filter`.
    ///   Returns `Ok(None)` for a plaintext document rather than an error.
    /// - [`Error::Io`] / [`Error::Parse`] when the `/Encrypt` entry is an indirect
    ///   reference whose resolution fails.
    pub fn encryption_info(&mut self) -> Result<Option<EncryptionInfo>> {
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
        let filter = required_name(&encrypt, "Filter")?.to_string();
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

    pub(crate) fn authenticate_if_encrypted(&mut self, options: &PdfOpenOptions) -> Result<()> {
        let encrypt_ref = self.trailer().get_ref("Encrypt");
        let Some(encrypt) = self.encrypt_dictionary()? else {
            return Ok(());
        };

        let revision = required_revision(&encrypt)?;
        // qpdf requires `/V` and `/R` together, before any password work
        // (`libqpdf/QPDF_encryption.cc:770-777`), and stores both
        // (`:797-798`).
        let version = required_version(&encrypt)?;
        let permissions = Permissions::new(required_permissions(&encrypt)?);
        let crypt_filters = crypt_filter_modes(&encrypt, version);
        // qpdf `:886-904`. These do not depend on the password, so qpdf
        // resolves them in `initializeEncryption` before authenticating; the
        // three branches below only produce the key and the match flags.
        let (cf_stream, cf_string, cf_file) = if matches!(version, 4 | 5) {
            let cf_stream = interpret_cf(&crypt_filters, encrypt.get("StmF"));
            let cf_string = interpret_cf(&crypt_filters, encrypt.get("StrF"));
            // `/EFF` is informational in qpdf; when it is not a name the file
            // method simply mirrors the stream method (`:891-904`).
            let cf_file = match encrypt.get("EFF") {
                Some(eff) if eff.as_name().is_some() => interpret_cf(&crypt_filters, Some(eff)),
                _ => cf_stream,
            };
            (cf_stream, cf_string, cf_file)
        } else {
            // qpdf leaves all three at the `EncryptionParameters` constructor's
            // `e_none` for every other `/V` (`libqpdf/QPDF.cc:190-192`); its
            // consumers never read them without first checking `/V >= 4`.
            (
                EncryptionMode::Identity,
                EncryptionMode::Identity,
                EncryptionMode::Identity,
            )
        };
        // The method a consumer actually applies. qpdf enters the crypt-filter
        // switch only when `/V >= 4` and otherwise leaves `use_aes` false,
        // i.e. RC4 (`:982-983`, `:1062-1063`), so the weak-crypto
        // classification below has to ask the same question the decryption
        // sites will.
        let effective = |cf: EncryptionMode| {
            if version >= 4 {
                cf
            } else {
                EncryptionMode::Rc4
            }
        };
        // Under `--password-is-hex-key` the --password value is a raw hex key,
        // not a password, so password-encoding normalization (which can reject
        // e.g. `--password-mode=unicode` on V<5) must not run and is unused by
        // the hex-key branch. Skip it; the hex-key branch decodes the raw
        // value itself. The `else` (layer-2) branches still see the normalized
        // password unchanged.
        let password = if options.password_is_hex_key {
            Vec::new()
        } else {
            normalize_password(&options.password, options.password_mode, revision)?
        };
        // The RC4 classification the weak-crypto gate uses on every branch
        // that is not R=5/R=6. Reads the effective methods, not the raw
        // `cf_*`, so a pre-`/V 4` document stays classified as RC4.
        let rc4_in_use = || {
            matches!(effective(cf_stream), EncryptionMode::Rc4)
                || matches!(effective(cf_string), EncryptionMode::Rc4)
                || crypt_filters
                    .values()
                    .any(|mode| matches!(mode, EncryptionMode::Rc4))
        };
        let (
            file_key,
            encrypt_metadata,
            weak_crypto,
            user_password_matched,
            owner_password_matched,
        ) = if options.password_is_hex_key {
            // qpdf `--password-is-hex-key`: the value passed via --password is
            // the precomputed file encryption key as hex, NOT a user/owner
            // password. We skip ALL password→key derivation (Algorithm 2 /
            // 2.A / 2.B / 6 / 7) and the layer-2 user/owner attempt +
            // bad-password ordering block entirely. This is a SEPARATE
            // sibling branch: the `else` below preserves layer-2's reordered
            // password/weak-crypto logic verbatim (flpdf-9hc.3.21).
            //
            // revision / crypt_filters / encrypt_ref / permissions and the
            // crypt-filter methods are already determined above and do NOT
            // depend on the password. /EncryptMetadata is likewise
            // password-independent; compute it with the SAME revision-aware
            // split layer-2 uses.
            let file_key = decode_hex_file_key(&options.password)?;
            let (encrypt_metadata, weak_crypto) = if matches!(revision, 5 | 6) {
                let encrypt_metadata = encrypt_metadata_flag(&encrypt)?;
                // Same weak-crypto classification as layer-2's R5/R6 branch.
                (encrypt_metadata, revision == 5)
            } else {
                let inputs = standard_handler_inputs(&encrypt, self.trailer())?;
                (inputs.encrypt_metadata, rc4_in_use())
            };
            // Honor the weak-crypto gate consistently with the password path:
            // qpdf still requires --allow-weak-crypto for RC4 / R=5 even when a
            // raw key is supplied. Keep the existing post-key gate behavior;
            // do NOT special-case the explicit-key path.
            if weak_crypto && !options.allow_weak_crypto {
                return Err(EncryptedError::WeakCryptoNotAllowed.into());
            }
            // A raw key bypasses authentication, so neither the user nor the
            // owner password was matched. qpdf likewise reports no password
            // match for `--password-is-hex-key`; report both as false.
            (file_key, encrypt_metadata, weak_crypto, false, false)
        } else if matches!(revision, 5 | 6) {
            // Error-variant firing order (must match qpdf, see flpdf-9hc.3.21):
            //
            //   1. Password authentication runs FIRST.  If neither the user nor
            //      the owner password authenticates, return `BadPassword`.
            //   2. ONLY after a password authenticates do we apply the
            //      weak-crypto gate (`WeakCryptoNotAllowed`).  A correct
            //      password against a weak (R=5) file with `--allow-weak-crypto`
            //      absent still returns `WeakCryptoNotAllowed` — only the
            //      ordering relative to `BadPassword` changes here.
            //   3. A wrong-length `/U` or `/O` entry on this authentication
            //      path is reported as `BadPassword` (an unusable credential
            //      entry is indistinguishable from a wrong password to a
            //      caller), not `Malformed`.  This is scoped to the auth path
            //      via `standard_handler_r5_inputs` (its only caller); all
            //      other `Malformed` reclassification is intentionally NOT done
            //      (e.g. `/UE`/`/OE` length errors stay `Malformed`).
            //
            // Keep this ordering identical in the `else` (V<5 / V=4) branch
            // below; do not re-introduce the weak-crypto-before-auth bug in
            // either branch.
            let inputs =
                standard_handler_r5_inputs(&encrypt).map_err(map_uo_length_to_bad_password)?;
            let encrypt_metadata = encrypt_metadata_flag(&encrypt)?;
            let weak_crypto = revision == 5;
            let user_attempt = if revision == 5 {
                check_user_password_r5(&password, &inputs)
            } else {
                check_user_password_r6(&password, &inputs)
            };
            let owner_attempt = if revision == 5 {
                check_owner_password_r5(&password, &inputs)
            } else {
                check_owner_password_r6(&password, &inputs)
            };
            let user_password_matched = user_attempt.is_ok();
            let owner_password_matched = owner_attempt.is_ok();
            let file_key = match (user_attempt, owner_attempt) {
                (Ok(key), _) => key,
                (Err(_), Ok(key)) => key,
                (Err(user_err), Err(_owner_err)) => return Err(user_err),
            };
            // Authentication succeeded — now apply the weak-crypto gate.
            if weak_crypto && !options.allow_weak_crypto {
                return Err(EncryptedError::WeakCryptoNotAllowed.into());
            }
            (
                file_key,
                encrypt_metadata,
                weak_crypto,
                user_password_matched,
                owner_password_matched,
            )
        } else {
            let inputs = standard_handler_inputs(&encrypt, self.trailer())?;
            let encrypt_metadata = inputs.encrypt_metadata;
            let weak_crypto = rc4_in_use();
            // Same error-variant firing order as the R=5/R=6 branch above:
            // password authentication runs FIRST (both failing →
            // `BadPassword`); the weak-crypto gate (`WeakCryptoNotAllowed`)
            // is applied ONLY after a password authenticates.  A correct
            // password against an RC4 file without `--allow-weak-crypto`
            // still returns `WeakCryptoNotAllowed`; only the ordering relative
            // to `BadPassword` changes.  Do not move the gate back above the
            // auth attempts (flpdf-9hc.3.21).
            let v4_path = inputs.v == 4 && inputs.r == 4;
            let user_attempt = if v4_path {
                check_user_password_v4(&password, &inputs)
            } else {
                check_user_password(&password, &inputs)
            };
            let owner_attempt = if v4_path {
                check_owner_password_v4(&password, &inputs)
            } else {
                check_owner_password(&password, &inputs)
            };
            let user_password_matched = user_attempt.is_ok();
            let owner_password_matched = owner_attempt.is_ok();
            let file_key = match (user_attempt, owner_attempt) {
                (Ok(key), _) => key,
                (Err(_), Ok(key)) => key,
                (Err(user_err), Err(_owner_err)) => return Err(user_err),
            };
            // Authentication succeeded — now apply the weak-crypto gate.
            if weak_crypto && !options.allow_weak_crypto {
                return Err(EncryptedError::WeakCryptoNotAllowed.into());
            }
            (
                file_key,
                encrypt_metadata,
                weak_crypto,
                user_password_matched,
                owner_password_matched,
            )
        };
        let r6_perms_warning = if revision == 6 {
            r6_perms_warning(&encrypt, &file_key, permissions, encrypt_metadata)?
        } else {
            None
        };
        *self.encryption.borrow_mut() = Some(EncryptionState {
            file_key,
            encryption_v: version,
            encryption_r: revision,
            cf_stream,
            cf_string,
            cf_file,
            crypt_filters,
            encrypt_metadata,
            encrypt_ref,
            weak_crypto,
            permissions,
            user_password_matched,
            owner_password_matched,
            cached_object_encryption_key: Vec::new(),
            cached_key_og: None,
        });
        if let Some(warning) = r6_perms_warning {
            self.push_warning(warning)?;
        }
        Ok(())
    }

    fn encrypt_dictionary(&mut self) -> Result<Option<Dictionary>> {
        match self.trailer().get("Encrypt").cloned() {
            None => Ok(None),
            Some(Object::Dictionary(dict)) => Ok(Some(dict)),
            Some(Object::Reference(object_ref)) => match self.resolve_borrowed(object_ref)? {
                Object::Dictionary(dict) => Ok(Some(dict.clone())),
                _ => Err(EncryptedError::Malformed {
                    reason: "/Encrypt object is not a dictionary".into(),
                }
                .into()),
            },
            Some(_) => Err(EncryptedError::Malformed {
                reason: "/Encrypt entry is not a dictionary or reference".into(),
            }
            .into()),
        }
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
    /// bound (it came from [`Self::resolve_borrowed`] or an equivalent parse),
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
        self.synchronize_legacy_resolution_state();
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
        self.synchronize_legacy_resolution_state();
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
        self.synchronize_legacy_resolution_state();
        let Some(XrefEntry::Uncompressed { offset }) = self.resolver.xref_entry(object_ref) else {
            return Ok(None);
        };
        let body_start =
            self.qtest_read_source_object_with_retry(offset, Self::object_body_start_within)?;
        Ok(Some(offset.saturating_add(body_start as u64)))
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
        self.synchronize_legacy_resolution_state();
        let Some(XrefEntry::Uncompressed { offset }) = self.resolver.xref_entry(object_ref) else {
            return Ok(None);
        };
        let value_offset = self.qtest_read_source_object_with_retry(offset, |bytes| {
            let body_start = Self::object_body_start_within(bytes)?;
            let body = &bytes[body_start..];
            let value_offset = array_item_source_offset(body, array_index)?;
            Ok(value_offset.map(|value_offset| body_start + value_offset))
        })?;
        Ok(value_offset.map(|value_offset| offset.saturating_add(value_offset as u64)))
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

    /// Replace `object_ref` with `object` in the in-memory object cache.
    ///
    /// The original on-disk bytes are not touched; [`crate::PdfWriter`] will
    /// see the updated value when it walks the cache and emit it in the fresh
    /// output. Subsequent [`Pdf::resolve`]/
    /// [`Pdf::resolve_borrowed`] calls for `object_ref` observe `object`
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
        self.synchronize_legacy_resolution_state();
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
        self.recovered_stream_eols.remove(&object_ref);
        self.transformed_stream_refs.remove(&object_ref);
        if let Some(CacheEntry::Compressed { stream, index }) =
            self.cache.entry(object_ref).cloned()
        {
            self.record_compressed_member_provenance(object_ref, stream, index);
        }

        // Write through to the canonical handle graph too (not just
        // `self.cache`): `Pdf::resolve_borrowed` now materializes from the
        // handle, so leaving a stale already-resolved handle in place would
        // make a later resolve of this same ref keep observing the value
        // from *before* this call.
        self.legacy_materialized_memo.remove(&object_ref);
        self.legacy_materialized_replacement_refs
            .remove(&object_ref);
        let handle = self.get_object_handle(object_ref);
        let lifted = self.lift_for_set_object(&object, &handle);
        match lifted {
            Ok(value) => {
                handle.set_resolved(value);
                // A caller-supplied replacement no longer describes the
                // source bytes that populated this handle. Clear the parsed
                // description only after the replacement has been lifted and
                // installed; a failed lift leaves the canonical value intact.
                handle.clear_description();
                // The value is now caller-supplied, in-memory-constructed
                // data, not something parsed from a source position; any
                // previously recorded offset no longer describes it.
                handle.reset_parsed_offset();
                handle.set_end_offsets(NO_PARSED_OFFSET, NO_PARSED_OFFSET);
            }
            Err(_) => {
                // Programmatic depth is unbounded above, so this fallback is
                // only for an Object variant that has no canonical
                // ObjectValue representation (currently a content-stream
                // token nested in a stream dictionary). `set_object` is
                // infallible; preserve the exact caller value for the legacy
                // raw-object boundary while keeping valid replacements on the
                // canonical handle graph.
                self.legacy_materialized_memo
                    .insert(object_ref, object.clone());
                self.legacy_materialized_replacement_refs.insert(object_ref);
            }
        }

        self.cache.set_resolved(object_ref, object);
        self.handle_mutated_object_refs.remove(&object_ref);
        self.dirty_object_refs.insert(object_ref);
    }

    // Convert `object` for `Pdf::set_object`'s handle-graph write-through.
    // This is intentionally broader than the ordinary legacy bounded lift:
    // qpdf's `replaceObject` accepts the already-constructed operator and
    // inline-image values (`QPDFObjectHandle.cc:1933-1941`) and does not apply
    // the raw bridge's parser-depth policy to programmatic values. Keep the
    // canonical replacement on the handle graph for the entire caller-owned
    // Object tree; the lift itself remains stack-safe through
    // `stacker::maybe_grow`.
    //
    // Identical to the ordinary bounded lift except when `object` is a stream and
    // `existing_handle`'s current (pre-overwrite) value is also a stream:
    // the new dictionary is written into the existing stream's own
    // dictionary handle in place (`ObjectHandle::replace_direct_value`),
    // preserving its already-recorded parsed offset and shared identity,
    // rather than minting a fresh dictionary handle that would start at the
    // no-offset sentinel (`stream_dictionary_parsed_offset_survives_resolve_set_object_round_trip`
    // is the regression tripwire for getting this wrong). A stream dictionary
    // remains an ordinary PDF dictionary: qpdf's operator and inline-image
    // values are content-stream-only and therefore still use the legacy
    // fallback when supplied there.
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
                    false,
                );
            }
            let stream_dict = self.lift_dictionary_bounded_with_options(
                &stream.dict,
                0,
                PROGRAMMATIC_LIFT_MAX_DEPTH,
                false,
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
    /// Subsequent [`Pdf::resolve`]/[`Pdf::resolve_borrowed`] calls for
    /// `object_ref` observe [`Object::Null`], matching the behavior for any
    /// other unknown or freed reference.
    pub fn delete_object(&mut self, object_ref: ObjectRef) {
        if object_ref.number != 0 {
            // The cache early return below must see the reconstructed live xref;
            // qpdf removes the corresponding cached object when a mutation
            // removes it (`libqpdf/QPDF.cc:1996-2004`).
            self.synchronize_legacy_resolution_state();
        }
        if object_ref.number != 0 {
            self.qpdf_removed_refs.insert(object_ref);
        }
        self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.recovered_stream_eols.remove(&object_ref);
        self.transformed_stream_refs.remove(&object_ref);
        if object_ref.number == 0 {
            return;
        }

        // qpdf erases the source xref row and canonical object-cache entry
        // while nullifying every outstanding handle (`QPDF.cc:1996-2004`).
        // Keep the indirect identity for this legacy public API, but remove
        // the source row and mark the retained handle missing. The qpdf-facing
        // object snapshot filters this compatibility slot below.
        self.promote_resolved_object_stream_members(object_ref)
            .expect("a parsed ObjStm member must be representable as an ObjectHandle");
        self.resolver
            .remove_object_preserving_handle(object_ref)
            .expect("canonical resolver object removal is infallible");

        // Invalidate the handle-graph bridge state unconditionally, before
        // the cache-state early return below: `Pdf::prepare_qpdf_json_objects`
        // can mark a ref's cache entry `Missing` (discovered as a dangling
        // reference from a live object's own content) before any handle for
        // it has ever been created via `Pdf::get_object_handle`. If the two
        // lines below ran only past the early return, that combination
        // (cache already `Missing`, handle still `Unresolved`) would
        // leave stale compatibility data behind.
        self.legacy_materialized_memo.remove(&object_ref);
        self.legacy_materialized_replacement_refs
            .remove(&object_ref);
        self.handle_mutated_object_refs.remove(&object_ref);
        self.get_object_handle(object_ref).set_missing();

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
    ///   [`Pdf::resolve`] returns as `Object::Null` (no real indirect
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
                if handle.is_missing() || matches!(cache_entry, Some(CacheEntry::Missing)) {
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
    /// allocation or scheduling writer output. The writer-facing public
    /// migration remains in `flpdf-25kg.3.6`; this method is the reusable
    /// canonical primitive corresponding to qpdf's
    /// `makeIndirectFromQPDFObject` (`libqpdf/QPDF.cc:1882-1888`).
    #[allow(dead_code)]
    pub(crate) fn make_indirect_from_object_handle(
        &self,
        handle: ObjectHandle,
    ) -> Result<ObjectHandle> {
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
    /// is not materialized here, only referenced. Matching qpdf's own
    /// documented contract (`include/qpdf/QPDF.hh:401-410`), **the source
    /// `Pdf` must remain alive until every copied stream has been written**
    /// (or otherwise read) from this document; dropping it first produces an
    /// [`Error::Internal`] when the writer tries to read the now-gone
    /// source. qpdf's escape hatch, `setImmediateCopyFrom` (materializing
    /// provider-backed stream data into memory at copy time so the source
    /// need not survive), has no flpdf counterpart yet.
    pub fn copy_foreign_object(&mut self, foreign: &ObjectHandle) -> Result<ObjectHandle> {
        crate::object_copy::copy_foreign_object(self, foreign)
    }

    /// Replace an indirect object's canonical value from an [`ObjectHandle`].
    ///
    /// This is the handle-shaped counterpart of [`Self::set_object`]. qpdf's
    /// `QPDF::replaceObject` accepts a direct, initialized handle and routes it
    /// through `updateCache`, whose existing cache slot adopts the replacement
    /// `QPDFValue` (`libqpdf/QPDF.cc:1986-1993,1843-1857`;
    /// `libqpdf/qpdf/QPDFObject_private.hh:117-120`). The canonical
    /// [`Self::replace_object_handle`] primitive performs that same slot
    /// replacement while retaining the target handle identity.
    ///
    /// qpdf records the shared value transition itself rather than exposing a
    /// separate dirty bit. flpdf's writer still tracks dirty object references,
    /// so this setter deliberately delegates to the primitive that calls
    /// [`Self::mark_object_handle_mutated`] after a successful replacement. As
    /// with [`Self::set_object`], every successful write-back marks the target
    /// dirty; callers that temporarily restore a previously clean value must
    /// explicitly call [`Self::clear_dirty`] after the restore.
    ///
    /// The legacy [`Self::set_object`] API remains unchanged for consumers that
    /// still provide a materialized [`Object`].
    #[allow(dead_code)] // writer consumer cutover is flpdf-egzr.3.2.5
    pub(crate) fn set_object_handle(
        &mut self,
        object_ref: ObjectRef,
        replacement: ObjectHandle,
    ) -> Result<()> {
        self.replace_object_handle(object_ref, replacement)
            .map(|_| ())
    }

    /// Replace a canonical object value while retaining the target
    /// [`ObjectHandle`] identity. This is the qpdf-shaped mutation boundary;
    /// raw [`Object`] materialization and writer traversal remain outside this
    /// layer.
    #[allow(dead_code)] // consumer cutover is flpdf-25kg.3.6.3
    pub(crate) fn replace_object_handle(
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
        // Refresh the legacy cache before replacing the canonical value, or
        // an old object-stream entry can incorrectly retain provenance
        // (mirrors `set_object`'s identical precondition above).
        self.synchronize_legacy_resolution_state();
        // qpdf's replaceObject changes only the requested cache slot; already
        // resolved members of an ObjStm remain live in their own cache slots.
        // Promote those compatibility-cache values before the replacement
        // overwrites the source container.
        self.promote_resolved_object_stream_members(object_ref)?;
        let target = self.resolver.replace_object(object_ref, replacement)?;
        self.qpdf_removed_refs.remove(&object_ref);
        self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.recovered_stream_eols.remove(&object_ref);
        self.transformed_stream_refs.remove(&object_ref);
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
        // `replace_object_handle`'s identical precondition above).
        self.synchronize_legacy_resolution_state();
        // qpdf's removeObject changes only the requested cache slot; already
        // resolved members of an ObjStm remain live in their own cache slots.
        // Promote those compatibility-cache values before the removal drops
        // the source container.
        self.promote_resolved_object_stream_members(object_ref)?;
        self.resolver.remove_object(object_ref)?;
        self.qpdf_parsed_xref_stream_refs.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.recovered_stream_eols.remove(&object_ref);
        self.transformed_stream_refs.remove(&object_ref);
        self.legacy_materialized_memo.remove(&object_ref);
        self.legacy_materialized_replacement_refs
            .remove(&object_ref);
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
        // ObjectHandle mutation happens below the legacy materialization
        // bridge. Discard any snapshot previously returned by resolve so the
        // next resolve and the writer materialize the changed live handle.
        self.legacy_materialized_memo.remove(&object_ref);
        self.legacy_materialized_replacement_refs
            .remove(&object_ref);
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

    fn reconcile_legacy_materialized_memos(&mut self) -> Result<()> {
        let pending: Vec<ObjectRef> = self
            .legacy_materialized_replacement_refs
            .iter()
            .copied()
            .collect();

        for object_ref in pending {
            let Some(handle) = self.resolver.registered_handle(object_ref) else {
                continue;
            };
            let Some(replacement) = self.legacy_materialized_memo.remove(&object_ref) else {
                continue;
            };
            // `set_object` reaches this legacy path only when the supplied
            // value has no canonical ObjectValue representation. Reconcile
            // that narrow compatibility value with the existing bounded
            // materialization walk; valid programmatic depth replacements
            // never enter this path.
            // Content-stream tokens are valid `ObjectValue` leaves even
            // though the generic legacy lift rejects them outside a content
            // stream; preserve them at every nesting level rather than
            // making enumeration fail.
            let value = match replacement {
                Object::Stream(stream) => self
                    .lift_owned_stream_bounded_with_options(
                        stream,
                        handle.as_stream_dict(),
                        0,
                        crate::parser::MAX_PARSE_DEPTH,
                        true,
                    )
                    .map_err(|(error, stream)| (error, Object::Stream(stream))),
                replacement => self
                    .lift_bounded_with_content_tokens(
                        &replacement,
                        0,
                        crate::parser::MAX_PARSE_DEPTH,
                    )
                    .map_err(|error| (error, replacement)),
            };
            let value = match value {
                Ok(value) => value,
                Err((error, replacement)) => {
                    // Keep the infallible legacy replacement authoritative if
                    // canonical lifting fails. The next reconciliation call
                    // must see the same value, without requiring a payload
                    // clone merely to preserve this error path.
                    self.legacy_materialized_memo
                        .insert(object_ref, replacement);
                    return Err(error);
                }
            };
            handle.set_resolved(value);
            handle.clear_description();
            handle.reset_parsed_offset();
            handle.set_end_offsets(NO_PARSED_OFFSET, NO_PARSED_OFFSET);
            self.legacy_materialized_memo.remove(&object_ref);
            self.legacy_materialized_replacement_refs
                .remove(&object_ref);
        }
        Ok(())
    }

    fn lift_owned_stream_bounded_with_options(
        &mut self,
        stream: Stream,
        existing_dict: Option<ObjectHandle>,
        depth: usize,
        max_depth: usize,
        allow_content_tokens: bool,
    ) -> std::result::Result<ObjectValue, (Error, Stream)> {
        let dict_value = match self.lift_dictionary_bounded_with_options(
            &stream.dict,
            depth,
            max_depth,
            allow_content_tokens,
        ) {
            Ok(value) => value,
            Err(error) => return Err((error, stream)),
        };
        let stream_dict = match existing_dict {
            Some(existing_dict) => {
                existing_dict.replace_direct_value(ObjectValue::Dictionary(dict_value));
                existing_dict.clear_description();
                existing_dict
            }
            None => self
                .resolver
                .direct_object_handle(ObjectValue::Dictionary(dict_value)),
        };
        Ok(ObjectValue::Stream {
            stream_dict,
            // Moving the Vec into Rc preserves qpdf's shared-buffer boundary
            // without copying the legacy memo's payload.
            stream_data: Some(Rc::new(stream.data)),
            stream_length: 0,
            stream_provider: None,
        })
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
            if !handle.is_resolved() || handle.is_missing() {
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
        // Reconcile caller-supplied replacements before qpdf-style dangling
        // reference preparation. Otherwise resolving the superseded source
        // value can register references that the replacement no longer owns.
        self.reconcile_legacy_materialized_memos()?;
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

        self.reconcile_legacy_materialized_memos()?;
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

    /// Compatibility name for the canonical qpdf object enumeration.
    ///
    /// New consumers should use [`Self::get_all_objects`]. This forwarding
    /// name remains only while the remaining consumer-cutover slices move off
    /// the pre-qpdf naming; it has exactly the same preparation and ordering
    /// contract.
    ///
    pub fn get_all_object_handles(&mut self) -> Result<Vec<ObjectHandle>> {
        self.get_all_objects()
    }

    /// Resolve `handle` in place if it is an unresolved indirect handle.
    ///
    /// A direct handle, or an indirect handle that has already been resolved,
    /// is a no-op. Resolution is delegated directly to the canonical
    /// `ResolverHandle` cache; it never materializes a legacy [`Object`].
    ///
    /// This does not chase through an already-resolved
    /// [`Pdf::set_object`]-driven bare-reference redirect to its terminal
    /// value — see [`Pdf::resolve_object_handle_to_terminal`] for that.
    /// [`Pdf::resolve_borrowed`] (and everything built on it, notably
    /// `ref_chain.rs`'s own bounded chain-follow primitive) depends on this
    /// method exposing exactly one hop per call, not silently collapsing a
    /// multi-hop chain.
    ///
    /// The canonical parser records source descriptions and offsets while it
    /// builds the graph. Streams retain their source filter dictionaries and
    /// are decrypted at pipe time, matching qpdf's `QPDF_Stream` path.
    ///
    /// # Errors
    ///
    /// I/O, parse, filter, or decryption failures propagate. Free, absent, or
    /// overridden references resolve to the canonical null/missing state.
    pub fn resolve_object_handle(&mut self, handle: &ObjectHandle) -> Result<()> {
        // ObjectHandle resolution is qpdf's canonical cache operation. The
        // resolver owns the source xref table, live parser, stream pipeline,
        // and one handle per object reference; no raw Object materialization
        // or metadata-only reparse belongs on this path.
        handle.try_dereference()
    }

    /// Read and cache an indirect object from its physical source offset.
    ///
    /// This is the canonical equivalent of qpdf's `readObjectAtOffset`: the
    /// header at `offset`, rather than the effective xref row, determines the
    /// returned [`ObjectHandle`] identity. It is used by linearization hint
    /// streams, whose `/H[0]` value is a physical byte location.
    pub(crate) fn resolve_object_handle_at_offset(
        &self,
        offset: u64,
        expected: ObjectRef,
    ) -> Result<ObjectHandle> {
        self.resolver
            .resolve_object_handle_at_offset(offset, expected)
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

    /// Run an operation with qpdf's default stream-framing recovery enabled.
    ///
    /// This is the recovery scope used by full rewrites. It is also available
    /// to consumers that must prepare live `ObjectHandle` streams before
    /// invoking a writer, so malformed `/Length` entries produce the same
    /// lazy recovery diagnostics as qpdf 11.9.0.
    pub fn with_writer_stream_recovery<T, E>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        let resolver = Rc::clone(&self.resolver);
        resolver.with_writer_stream_recovery(|| operation(self))
    }

    /// Run the plain canonical writer with qpdf's default stream-framing
    /// recovery enabled. This preserves the writer's historical behavior for
    /// a document opened in flpdf's strict xref mode without changing the
    /// strict semantics of ordinary `ObjectHandle` resolution.
    pub(crate) fn with_plain_writer_stream_recovery<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.with_writer_stream_recovery(operation)
    }

    /// Resolve `handle` (via [`Pdf::resolve_object_handle`]), then chase
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
    /// [`Pdf::resolve_borrowed`] for any ref in the chain still observes it.
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
    /// Same as [`Pdf::resolve_object_handle`].
    pub fn resolve_object_handle_to_terminal(
        &mut self,
        handle: &ObjectHandle,
    ) -> Result<ObjectHandle> {
        Ok(self.resolve_object_handle_to_terminal_ref(handle)?.0)
    }

    /// `qpdf-cutover-delete(flpdf-25kg.3.3)`: terminal-ref legacy API.
    /// Delete with `ObjectValue::Reference`; do not call from new code.
    ///
    /// Same chase as [`Pdf::resolve_object_handle_to_terminal`], additionally
    /// returning the object reference the terminal value was actually read
    /// from. This is `None` exactly when the returned handle is direct with
    /// no indirect identity of its own — either `handle` itself was direct,
    /// or the chase hit the `ref_chain::MAX_REF_CHAIN_DEPTH` bound and fell
    /// back to a null handle. Whenever it is `Some`, the returned handle is
    /// that object's own canonical, indirect handle. Otherwise it is `handle.object_ref()` when no
    /// [`Pdf::set_object`] redirect was chased (matching
    /// [`Pdf::resolve_object_handle_to_terminal`]'s own "returns `handle`
    /// unchanged" case), or the *last* hop's ref when one or more redirects
    /// were followed — deliberately not the chain's first ref, which callers
    /// needing offset/diagnostic attribution for the terminal object itself
    /// (e.g. a stream's source offset) must not conflate with an intermediate
    /// redirect's own ref.
    ///
    /// # Errors
    ///
    /// Same as [`Pdf::resolve_object_handle`].
    pub fn resolve_object_handle_to_terminal_ref(
        &mut self,
        handle: &ObjectHandle,
    ) -> Result<(ObjectHandle, Option<ObjectRef>)> {
        self.resolve_object_handle(handle)?;
        let Some(mut current_ref) = handle.as_reference() else {
            // already terminal (the common case) — or unresolved/missing;
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
            self.resolve_object_handle(&hop)?;
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
                // `hop` is always Resolved or Missing here, so it presents a
                // value exactly as a copy of it would: the one state that
                // would differ, `Unresolved`, needs `resolve_object_handle`
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
                // failure as "cannot be represented in the handle graph" and
                // stores the caller-supplied value in
                // `legacy_materialized_memo`. Memo reconciliation opts into
                // the lossless ObjectValue leaves above, including nested
                // tokens, so enumeration can restore the authoritative value.
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

    /// Resolve `object_ref` to its concrete value, parsing on demand.
    ///
    /// Resolution caches the result so subsequent calls are constant-time. Unknown,
    /// freed, or compressed-but-broken entries return [`Object::Null`] rather than an
    /// error, matching the behavior the PDF spec mandates for missing objects (§7.3.10).
    ///
    /// # Errors
    ///
    /// Has the same error behavior as [`Pdf::resolve_borrowed`]:
    ///
    /// - [`Error::Io`] when seeking to or reading the object's bytes fails.
    /// - [`Error::Parse`] when the indirect object cannot be parsed.
    /// - [`Error::Encrypted`] when decrypting the resolved object fails.
    ///
    /// An unknown, freed, or compressed-but-broken reference is **not** an error;
    /// it resolves to [`Object::Null`].
    /// `qpdf-cutover-delete(flpdf-25kg.3.3)`: owned raw-`Object` resolver.
    /// Delete after its callers use canonical handle accessors; do not use it
    /// from the new resolver path.
    pub fn resolve(&mut self, object_ref: ObjectRef) -> Result<Object> {
        Ok(self.resolve_borrowed(object_ref)?.clone())
    }

    /// `qpdf-cutover-delete(flpdf-25kg.3.3)`: borrowed raw-`Object` resolver
    /// and its materialization memo are legacy-only. Delete after callers
    /// migrate; do not preserve this signature as a new design constraint.
    ///
    /// Resolve `object_ref` and borrow the cached concrete value.
    ///
    /// This has the same resolution behavior as [`Pdf::resolve`] but avoids cloning
    /// the resolved [`Object`]. The returned reference is tied to the mutable borrow
    /// of this [`Pdf`], so callers must finish using it before resolving or mutating
    /// other objects through the same reader.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] when seeking to or reading the object's bytes fails.
    /// - [`Error::Parse`] when the indirect object cannot be parsed.
    /// - [`Error::Encrypted`] when decrypting the resolved object fails.
    ///
    /// An unknown, freed, or compressed-but-broken reference is **not** an error;
    /// it resolves to [`Object::Null`].
    pub fn resolve_borrowed(&mut self, object_ref: ObjectRef) -> Result<&Object> {
        self.synchronize_canonical_recovered_stream_eol(object_ref)?;
        // This is intentionally the raw-Object compatibility boundary. The
        // canonical ObjectHandle API above owns qpdf-shaped lazy resolution;
        // the legacy reader still owns stream-length recovery, transformed
        // stream bookkeeping, and qpdf JSON's materialized Object contract.
        if self.handle_mutated_object_refs.contains(&object_ref) {
            if self.materialize_canonical_compatibility_value(object_ref)? {
                return Ok(self
                    .legacy_materialized_memo
                    .get(&object_ref)
                    .expect("inserted materialized ObjectHandle value"));
            }
            return Ok(&NULL_OBJECT); // cov:ignore: a successful canonical resolve always leaves a missing handle resolved
        }

        // qpdf has one object cache. A canonical producer may have resolved a
        // source object while the legacy compatibility cache still contains
        // only its initial xref entry. Prefer that live canonical value before
        // reading the unresolved legacy entry, otherwise the two staged
        // consumers parse the same malformed source twice and duplicate its
        // recovery diagnostics. This is a read-only compatibility snapshot;
        // canonical resolution itself still never populates the legacy cache.
        if !self.qpdf_parsed_xref_stream_refs.contains(&object_ref)
            && !matches!(self.cache.entry(object_ref), Some(CacheEntry::Resolved(_)))
        {
            if let Some(handle) = self
                .resolver
                .registered_handle(object_ref)
                .filter(ObjectHandle::is_resolved)
            {
                let value = self.materialize_handle_for_legacy(object_ref, &handle)?;
                self.legacy_materialized_memo.insert(object_ref, value);
                return Ok(self
                    .legacy_materialized_memo
                    .get(&object_ref)
                    .expect("inserted canonical compatibility value"));
            }
        }

        match self.resolve_to_cache(object_ref) {
            Ok(true) => {
                if let Some(CacheEntry::Resolved(object)) = self.cache.entry(object_ref) {
                    return Ok(object);
                } // cov:ignore: resolve_to_cache returns true only after installing a resolved cache entry
            }
            Ok(false)
                if self.resolver.attempt_recovery() && !self.resolver.reconstructed_xref() =>
            {
                if self.materialize_canonical_compatibility_value(object_ref)? {
                    return Ok(self
                        .legacy_materialized_memo
                        .get(&object_ref)
                        .expect("inserted recovered canonical value"));
                } // cov:ignore: canonical materialization cannot return false after resolving an indirect handle
            }
            Err(error)
                if self.resolver.attempt_recovery()
                    && !self.resolver.reconstructed_xref()
                    && matches!(error, Error::Parse { .. }) =>
            {
                if self.materialize_canonical_compatibility_value(object_ref)? {
                    return Ok(self
                        .legacy_materialized_memo
                        .get(&object_ref)
                        .expect("inserted recovered canonical value"));
                }
            }
            Err(error) => return Err(error),
            Ok(false) => {}
        }

        // A canonical handle may have been resolved by a caller without
        // populating the raw cache. Expose that value only as an explicit
        // compatibility read; it does not route canonical resolution back
        // through the legacy engine.
        // cov:ignore-start: the canonical handle branch above or a recovery branch returns immediately whenever a handle becomes resolved; this is the defensive single-Pdf fallback
        if !self.qpdf_parsed_xref_stream_refs.contains(&object_ref) {
            if let Some(handle) = self
                .resolver
                .registered_handle(object_ref)
                .filter(ObjectHandle::is_resolved)
            {
                let value = self.materialize_handle_for_legacy(object_ref, &handle)?; // cov:ignore: every resolved registered handle is returned by the earlier compatibility branch
                self.legacy_materialized_memo.insert(object_ref, value);
                return Ok(self
                    .legacy_materialized_memo
                    .get(&object_ref)
                    .expect("inserted materialized ObjectHandle value"));
            }
        }
        // cov:ignore-end

        Ok(&NULL_OBJECT)
    }

    /// Materialize a canonical handle only for an explicit raw-object
    /// compatibility read after the caller has mutated that handle. This is
    /// not recovery for ordinary raw resolution: the raw reader keeps its own
    /// bounded retry budget, and canonical parsing must never bypass it.
    fn materialize_canonical_compatibility_value(&mut self, object_ref: ObjectRef) -> Result<bool> {
        let handle = self.get_object_handle(object_ref);
        self.resolve_object_handle(&handle)?;
        self.synchronize_canonical_recovered_stream_eol(object_ref)?;
        self.synchronize_legacy_resolution_state();
        if !handle.is_resolved() {
            return Ok(false); // cov:ignore: the canonical resolver resolves or errors for every indirect handle
        }

        if handle.get_parsed_offset() < 0 && !self.handle_mutated_object_refs.contains(&object_ref)
        {
            if let Some(CacheEntry::Resolved(cached @ Object::Stream(_))) =
                self.cache.entry(object_ref)
            {
                self.legacy_materialized_memo
                    .insert(object_ref, cached.clone());
                return Ok(true);
            }
        }

        self.legacy_materialized_memo.insert(
            object_ref,
            self.materialize_handle_for_legacy(object_ref, &handle)?,
        );
        Ok(true)
    }

    /// Materialize a canonical value for a legacy `Object` consumer without
    /// exposing source framing that the legacy reader stores separately.
    ///
    /// qpdf's canonical `QPDF_Stream` keeps the recovered line ending inside
    /// its validated source length; the old flpdf `Stream` snapshot removes
    /// that framing and lets the writer restore it from
    /// [`Self::recovered_stream_eol`]. Keep this normalization at the
    /// compatibility boundary only: the live `ObjectHandle` remains the
    /// qpdf-shaped source of truth and continues to pipe the full recovered
    /// source span.
    fn synchronize_canonical_recovered_stream_eol(&mut self, object_ref: ObjectRef) -> Result<()> {
        if self.transformed_stream_refs.contains(&object_ref)
            || self.recovered_stream_eols.contains_key(&object_ref)
        {
            return Ok(());
        }
        // After qpdf-style xref reconstruction, the canonical raw stream view
        // is the public compatibility value itself; do not create a second
        // legacy-side framing marker that would make that raw byte look like a
        // writer-only payload suffix.
        if self.resolver.reconstructed_xref() {
            return Ok(());
        }
        let Some(handle) = self
            .resolver
            .registered_handle(object_ref)
            .filter(ObjectHandle::is_resolved)
        else {
            return Ok(());
        };
        let Some(stream_dict) = handle.as_stream_dict() else {
            return Ok(());
        };
        if handle.as_stream_data().is_some()
            || handle.has_stream_data_provider()
            || self
                .resolver
                .recovered_stream_eol_is_transformed(&stream_dict)?
        {
            return Ok(());
        }
        if let Some(eol) = self.resolver.recovered_stream_eol(object_ref) {
            self.recovered_stream_eols.insert(object_ref, eol);
        }
        Ok(())
    }

    fn materialize_handle_for_legacy(
        &self,
        object_ref: ObjectRef,
        handle: &ObjectHandle,
    ) -> Result<Object> {
        let mut value = handle.materialize()?;
        let eol = self.recovered_stream_eol(object_ref);
        if let Some(eol) = eol {
            if let Object::Stream(stream) = &mut value {
                if handle.stream_source_length() == Some(stream.data.len())
                    && stream.data.ends_with(eol)
                {
                    stream.data.truncate(stream.data.len() - eol.len());
                }
            }
        }
        Ok(value)
    }

    /// Read and parse the indirect object stored at `offset`, returning the read
    /// bytes alongside the parse result.
    ///
    /// The read is bounded to the start of the next object in the file. Objects
    /// in a well-formed PDF do not overlap, so that window contains the object in
    /// full, and resolving every object stays linear in the file size — an
    /// unbounded read-to-end per object is quadratic and a CPU DoS on a document
    /// (e.g. a repaired one) that exposes many objects whose bodies run toward
    /// EOF. When the bounded window does not parse — a recorded offset points
    /// inside this object (corrupt xref, or a header-like line captured inside
    /// stream data during repair) — it falls back to reading to EOF, but only
    /// while [`Self::resolution_fallbacks_remaining`] permits, so a flood of such
    /// objects cannot revive the quadratic cost.
    fn read_object_at(
        &mut self,
        expected_ref: ObjectRef,
        offset: u64,
    ) -> Result<file_object::FileObjectRead> {
        self.read_object_at_with_policy(
            expected_ref,
            offset,
            RecoveryPolicy::RequireTokenTerminator,
            RecoveryPolicy::Bounded,
        )
    }

    // Read the byte window starting at `offset`, bounded by the next known
    // object's offset when one exists, or by EOF for the last object in the
    // file. Shared by `read_object_at_with_policy`'s own (first-attempt)
    // window read, extracted so the legacy reader's bounded fast path has one
    // definition of its source window.
    fn read_bounded_object_window(&mut self, offset: u64) -> Result<Vec<u8>> {
        let next = self.next_object_offset(offset);
        self.resolver.read_window(offset, next)
    }

    fn read_object_at_with_policy(
        &mut self,
        expected_ref: ObjectRef,
        offset: u64,
        window_policy: RecoveryPolicy,
        full_policy: RecoveryPolicy,
    ) -> Result<file_object::FileObjectRead> {
        let next = self.next_object_offset(offset);
        let bytes = self.read_bounded_object_window(offset)?;

        let initial_policy = if next.is_some() {
            window_policy
        } else {
            full_policy
        };
        match self.parse_and_finish_file_object(expected_ref, &bytes, offset, initial_policy) {
            Ok(parsed) => Ok(parsed),
            Err(window_err) if next.is_some() && self.resolution_fallbacks_remaining > 0 => {
                self.resolution_fallbacks_remaining -= 1;
                // A fresh, short borrow: the borrow the window read above took
                // ended before `parse_and_finish_file_object`, which can
                // re-enter resolution through an indirect `/Length`.
                let full = self.resolver.read_window(offset, None)?;
                self.parse_and_finish_file_object(expected_ref, &full, offset, full_policy)
                    .or(Err(window_err))
            }
            Err(err) => Err(err),
        }
    }

    fn parse_and_finish_file_object(
        &mut self,
        expected_ref: ObjectRef,
        bytes: &[u8],
        offset: u64,
        policy: RecoveryPolicy,
    ) -> Result<file_object::FileObjectRead> {
        let pending = parse_file_object_syntax(bytes)?;
        let resolved_length = self.resolve_pending_stream_length(expected_ref, &pending, offset)?;
        let result = finish_file_object(bytes, pending, resolved_length, policy);
        self.cache.set_unresolved(expected_ref, offset);
        result
    }

    fn resolve_pending_stream_length(
        &mut self,
        expected_ref: ObjectRef,
        pending: &PendingFileObject,
        offset: u64,
    ) -> Result<Option<ResolvedStreamLength>> {
        let Some(holder) = pending.indirect_length_ref() else {
            return Ok(None);
        };
        if holder == pending.object_ref {
            return Ok(Some(ResolvedStreamLength::Missing));
        }

        self.cache.set_reserved(expected_ref);
        let length_handle = self.get_object_handle(holder);
        let resolved_object = if length_handle.is_resolved() {
            if length_handle.is_missing() {
                Ok(None)
            } else {
                match length_handle.try_as_integer()? {
                    Some(value) => Ok(Some(ResolvedStreamLength::Integer(value))),
                    None if length_handle.is_null() => Ok(None),
                    None => Ok(Some(ResolvedStreamLength::Invalid)),
                }
            }
        } else {
            // Stream framing is qpdf's readObjectAtOffset recovery boundary
            // (`QPDF.cc:1360-1402, 1485-1531`). The bounded legacy reader is
            // still authoritative while it is establishing that boundary:
            // the canonical parser intentionally reads a whole source window
            // and would otherwise replay warnings from a malformed holder.
            // Synchronize its result into the canonical child handle below so
            // later consumers share one ObjectHandle identity.
            let legacy_object = self.resolve_borrowed(holder).cloned();
            if let Ok(object) = &legacy_object {
                if !length_handle.is_resolved() {
                    let value = self.lift_for_set_object(object, &length_handle)?;
                    length_handle.set_resolved(value);
                } // cov:ignore: resolve_borrowed synchronizes the canonical holder before returning; this preserves a defensive cache-only guard
            }
            match legacy_object {
                Ok(Object::Integer(value)) => Ok(Some(ResolvedStreamLength::Integer(value))),
                Ok(Object::Null) => Ok(None),
                Ok(_) => Ok(Some(ResolvedStreamLength::Invalid)),
                Err(Error::Parse { .. }) => Ok(Some(ResolvedStreamLength::Invalid)),
                Err(err) => Err(err),
            }
        };
        let resolved = match resolved_object {
            Ok(Some(resolved)) => resolved,
            Ok(None)
                if matches!(
                    self.cache.entry(holder),
                    None | Some(CacheEntry::Missing | CacheEntry::Deleted)
                ) =>
            {
                ResolvedStreamLength::Missing
            }
            Ok(None) => ResolvedStreamLength::Invalid,
            Err(err) => {
                self.cache.set_unresolved(expected_ref, offset);
                return Err(err);
            }
        };
        self.cache.set_unresolved(expected_ref, offset);
        Ok(Some(resolved))
    }

    pub(crate) fn resolve_qpdf_json_object(&mut self, object_ref: ObjectRef) -> Result<Object> {
        if self.handle_mutated_object_refs.contains(&object_ref)
            || !matches!(self.cache.entry(object_ref), Some(CacheEntry::Resolved(_)))
        {
            if let Some(handle) = self
                .resolver
                .registered_handle(object_ref)
                .filter(ObjectHandle::is_resolved)
            {
                return handle.materialize();
            }
        }
        if self.resolve_to_cache(object_ref)? {
            if self.handle_mutated_object_refs.contains(&object_ref) {
                let handle = self.get_object_handle(object_ref);
                self.resolve_object_handle(&handle)?;
                return handle.materialize();
            }
            if let Some(CacheEntry::Resolved(object)) = self.cache.entry(object_ref) {
                return Ok(object.clone());
            }
        }

        Ok(Object::Null)
    }

    /// Resolve a qpdf-visible object without cloning its cached value.
    ///
    /// Unlike [`Self::resolve_borrowed`], this retains the historical xref-stream
    /// compatibility view used by qpdf JSON preparation when the object is
    /// absent from the legacy object cache. The value still comes from the
    /// canonical `ObjectHandle`; no raw xref-stream side table is consulted.
    pub(crate) fn resolve_qpdf_json_object_borrowed(
        &mut self,
        object_ref: ObjectRef,
    ) -> Result<&Object> {
        if self.handle_mutated_object_refs.contains(&object_ref)
            || !matches!(self.cache.entry(object_ref), Some(CacheEntry::Resolved(_)))
        {
            if let Some(handle) = self
                .resolver
                .registered_handle(object_ref)
                .filter(ObjectHandle::is_resolved)
            {
                self.legacy_materialized_memo
                    .insert(object_ref, handle.materialize()?);
                return Ok(self
                    .legacy_materialized_memo
                    .get(&object_ref)
                    .expect("inserted canonical qpdf-json value"));
            }
        }
        self.resolve_to_cache(object_ref)?;
        if self.handle_mutated_object_refs.contains(&object_ref) {
            let handle = self.get_object_handle(object_ref);
            self.resolve_object_handle(&handle)?;
            self.legacy_materialized_memo
                .insert(object_ref, handle.materialize()?);
            return Ok(self
                .legacy_materialized_memo
                .get(&object_ref)
                .expect("inserted materialized ObjectHandle value"));
        }
        match self.cache.entry(object_ref) {
            Some(CacheEntry::Resolved(object)) => Ok(object),
            _ => Ok(&NULL_OBJECT),
        }
    }

    /// Offset of the first recorded object that starts strictly after `offset`,
    /// or `None` when `offset` belongs to the last object in the file.
    fn next_object_offset(&self, offset: u64) -> Option<u64> {
        let index = self.sorted_object_offsets.partition_point(|&o| o <= offset);
        self.sorted_object_offsets.get(index).copied()
    }

    /// Bring the legacy cache and bounded-read offsets in line with the
    /// canonical resolver after a resolution-time xref reconstruction.
    pub(crate) fn synchronize_legacy_resolution_state(&mut self) {
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

    fn resolve_to_cache(&mut self, object_ref: ObjectRef) -> Result<bool> {
        self.synchronize_legacy_resolution_state();
        let entry = self.cache.entry(object_ref);
        if matches!(entry, Some(CacheEntry::Resolved(_))) {
            return Ok(true);
        }

        match entry.cloned() {
            Some(CacheEntry::Unresolved { offset }) => {
                let mut parsed = self.read_object_at(object_ref, offset)?;
                if parsed.object_ref != object_ref {
                    return Ok(false);
                }
                let recovered_eol = parsed.remove_included_recovery_eol_for_decryption();
                let recovered_eol_bytes =
                    recovered_eol.map(crate::parser::RecoveredStreamEol::as_bytes);
                let (object, stream_payload_transformed) =
                    self.decrypt_resolved_object(object_ref, parsed.object, recovered_eol_bytes)?;
                self.cache.set_resolved(object_ref, object);
                if stream_payload_transformed {
                    self.transformed_stream_refs.insert(object_ref);
                } else {
                    self.transformed_stream_refs.remove(&object_ref);
                }
                if let Some(eol) = recovered_eol {
                    self.recovered_stream_eols.insert(object_ref, eol);
                } else {
                    self.recovered_stream_eols.remove(&object_ref);
                }
                self.record_file_object_diagnostics(object_ref, offset, parsed.diagnostics)?;
                Ok(true)
            }
            Some(CacheEntry::Compressed { stream, index }) => {
                self.resolve_compressed_entry(object_ref, stream, index)
            }
            Some(
                CacheEntry::Resolved(_)
                | CacheEntry::Missing
                | CacheEntry::Deleted
                | CacheEntry::Reserved,
            )
            | None => Ok(false),
        }
    }

    fn record_file_object_diagnostics(
        &mut self,
        object_ref: ObjectRef,
        offset: u64,
        diagnostics: Vec<FileObjectDiagnostic>,
    ) -> Result<()> {
        for diagnostic in diagnostics {
            self.push_warning(format!(
                "(object {} {}, offset {}): {}",
                object_ref.number,
                object_ref.generation,
                offset.saturating_add(diagnostic.relative_offset as u64),
                diagnostic.kind.message()
            ))?;
        }
        Ok(())
    }

    fn resolve_compressed_entry(
        &mut self,
        object_ref: ObjectRef,
        stream: u32,
        index: u32,
    ) -> Result<bool> {
        // qpdf's effective xref is authoritative for the member/source pair
        // (`QPDF.cc:1792-1795`). Tests that seed only the legacy cache have
        // no source row, so the direct parser remains available for that
        // narrow compatibility setup; source-backed documents always take
        // this check.
        if let Some(source_entry) = self.resolver.xref_entry(object_ref) {
            if !matches!(
                source_entry,
                XrefEntry::Compressed {
                    stream: source_stream,
                    ..
                } if source_stream == stream
            ) {
                return Ok(false);
            }
        }

        let stream_ref = ObjectRef::new(stream, 0);
        // qpdf resolves the object-stream container through the same object
        // cache as every other indirect object (`QPDF.cc:1756-1833`). Keep
        // this compatibility-cache consumer on that canonical handle too:
        // its stream dictionary and payload must be read through the live
        // `QPDF_Stream` filter path, not through a second raw `Object` copy.
        let Some(stream_handle) = self.object_stream_handle_with_legacy_fallback(stream_ref)?
        else {
            return Ok(false);
        };
        if stream_handle.as_stream_dict().is_none() {
            return Ok(false);
        }
        if !matches!(
            self.cache.entry(stream_ref),
            Some(CacheEntry::Resolved(Object::Stream(_)))
        ) {
            let stream_object = stream_handle.materialize()?;
            if matches!(stream_object, Object::Stream(_)) {
                self.cache.set_resolved(stream_ref, stream_object);
            }
        }

        let Some(parsed) =
            self.parse_object_stream_entry_from_handle(&stream_handle, object_ref.number)?
        else {
            return Ok(false);
        };
        let ParsedObjectStreamEntry {
            object,
            diagnostics,
        } = parsed;
        let (object, _stream_payload_transformed) =
            self.decrypt_resolved_object(object_ref, object, None)?;
        self.compressed_member_parents.insert(
            object_ref,
            CompressedMemberProvenance {
                parent_ref: stream_ref,
                parent_index: index,
                source_stream: stream,
                source_index: index,
            },
        );
        self.cache.set_resolved(object_ref, object);
        self.record_object_stream_diagnostics(stream_ref, object_ref, diagnostics)?;
        Ok(true)
    }

    fn decrypt_resolved_object(
        &mut self,
        object_ref: ObjectRef,
        mut object: Object,
        recovered_stream_eol: Option<&[u8]>,
    ) -> Result<(Object, bool)> {
        let encrypt_ref = {
            let encryption_guard = self.encryption.borrow();
            let Some(encryption) = encryption_guard.as_ref() else {
                return Ok((object, false));
            };
            encryption.encrypt_ref
        };
        if Some(object_ref) == encrypt_ref {
            return Ok((object, false));
        }

        // qpdf `QPDF::decryptString` (`libqpdf/QPDF_encryption.cc:977-1039`).
        // Keep this borrow scoped to the string pass. The stream pass below
        // may resolve indirect filter parameters through the document resolver,
        // which shares this RefCell (unlike qpdf's shared_ptr state).
        let warn_unknown_string = {
            let mut encryption_guard = self.encryption.borrow_mut();
            let encryption = encryption_guard
                .as_mut()
                .expect("encryption state remains present after initial check");
            decrypt_object_strings(object_ref, &mut object, encryption)?
        };

        let mut stream_payload_transformed = false;
        let mut warn_unknown_stream = false;
        if let Object::Stream(stream) = &mut object {
            let (encrypt_metadata, has_explicit_crypt_filter) = {
                let encryption_guard = self.encryption.borrow();
                let encryption = encryption_guard
                    .as_ref()
                    .expect("encryption state remains present after string pass");
                (
                    encryption.encrypt_metadata,
                    stream_has_explicit_crypt_filter(&stream.dict),
                )
            };
            if !encrypt_metadata && is_metadata_stream(&stream.dict) {
                self.warn_unknown_crypt_filters(warn_unknown_string, false)?;
                return Ok((object, false));
            } else if has_explicit_crypt_filter {
                let explicit_result = {
                    let mut encryption_guard = self.encryption.borrow_mut();
                    let encryption = encryption_guard
                        .as_mut()
                        .expect("encryption state remains present before stream pass");
                    apply_explicit_crypt_filters(
                        object_ref,
                        stream,
                        encryption,
                        recovered_stream_eol,
                        &mut warn_unknown_stream,
                    )
                };
                if let Err(error) = explicit_result {
                    // qpdf's `decryptStream` warns from its method switch
                    // (`libqpdf/QPDF_encryption.cc:1121-1133`) before it
                    // derives the object key (`:1135`) or prepends the
                    // decryption stage (`:1137-1149`), so a failure in either
                    // of those later steps does not swallow the warning.
                    self.warn_unknown_crypt_filters(warn_unknown_string, warn_unknown_stream)?;
                    return Err(error);
                }
                stream_payload_transformed = true;
            } else {
                let mut encryption_guard = self.encryption.borrow_mut();
                let encryption = encryption_guard
                    .as_mut()
                    .expect("encryption state remains present before stream pass");
                // qpdf `QPDF::decryptStream`'s method selection
                // (`libqpdf/QPDF_encryption.cc:1062-1134`), for a stream that
                // declared no `/Crypt` filter of its own.
                //
                // qpdf runs this at pipe time, against the pipeline it is
                // about to prepend a stage to, not here at resolve time. This
                // is the resolve-time route's copy of the same switch; the
                // pipe-time home is `ResolverHandle::pipe_stream_data`, and
                // this copy goes away with the rest of the resolve-time
                // decryption when its consumers move over.
                let (use_aes, warn) = encryption.stream_method(None);
                warn_unknown_stream = warn;
                if let Some(use_aes) = use_aes {
                    decrypt_stream_bytes(object_ref, &mut stream.data, use_aes, encryption)?;
                    stream_payload_transformed = true;
                }
            }
        }
        self.warn_unknown_crypt_filters(warn_unknown_string, warn_unknown_stream)?;
        Ok((object, stream_payload_transformed))
    }

    /// Recover an object-stream container that exists only in the legacy
    /// compatibility cache. Normal source-backed objects are resolved by the
    /// canonical resolver above; this narrow bridge is for callers/tests that
    /// intentionally seed `Pdf::cache` directly, and for malformed stream
    /// framing where qpdf's recovery reader is the established owner.
    fn object_stream_handle_with_legacy_fallback(
        &mut self,
        stream_ref: ObjectRef,
    ) -> Result<Option<ObjectHandle>> {
        let stream_handle = self.get_object_handle(stream_ref);
        let prefer_legacy_framing = self.object_stream_needs_legacy_framing(stream_ref)?;
        let canonical_error = if prefer_legacy_framing {
            None
        } else {
            self.resolve_object_handle(&stream_handle).err()
        };
        if !prefer_legacy_framing && canonical_error.is_none() && !stream_handle.is_missing() {
            return Ok(Some(stream_handle));
        }

        match self.cache.entry(stream_ref).cloned() {
            Some(CacheEntry::Resolved(object)) => {
                if !matches!(object, Object::Stream(_)) {
                    return Ok(None);
                }
                let value = self.lift_for_set_object(&object, &stream_handle)?;
                stream_handle.set_resolved(value);
                Ok(Some(stream_handle))
            }
            Some(CacheEntry::Unresolved { offset }) => {
                let policy = RecoveryPolicy::RequireTokenTerminator;
                let mut parsed =
                    self.read_object_at_with_policy(stream_ref, offset, policy, policy)?;
                if parsed.object_ref != stream_ref {
                    return Ok(None);
                }
                // A false next-object offset can end the bounded window
                // right after the object-stream dictionary's closing `>>`
                // but before the `stream` keyword, so the parse above
                // succeeds as a bare direct object (not an error) instead of
                // failing -- read_object_at_with_policy's own retry-on-error
                // path never triggers for that shape. qpdf's
                // readObjectAtOffset recovery has no bounded/unbounded split
                // at all (`QPDF.cc:1330-1402`); reproduce its effective
                // behavior by retrying through EOF once more when the parse
                // succeeded but did not actually yield a stream.
                if !matches!(parsed.object, Object::Stream(_))
                    && self.resolution_fallbacks_remaining > 0
                {
                    self.resolution_fallbacks_remaining -= 1;
                    let full_bytes = self.resolver.read_window(offset, None)?;
                    let retried =
                        self.parse_and_finish_file_object(stream_ref, &full_bytes, offset, policy)?;
                    if retried.object_ref == stream_ref {
                        parsed = retried;
                    }
                }
                let recovered_eol = parsed.remove_included_recovery_eol_for_decryption();
                let recovered_eol_bytes =
                    recovered_eol.map(crate::parser::RecoveredStreamEol::as_bytes);
                let (object, stream_payload_transformed) =
                    self.decrypt_resolved_object(stream_ref, parsed.object, recovered_eol_bytes)?;
                if !matches!(object, Object::Stream(_)) {
                    return Ok(None); // cov:ignore: successful stream framing always yields Object::Stream here
                }
                self.cache.set_resolved(stream_ref, object.clone());
                if stream_payload_transformed {
                    self.transformed_stream_refs.insert(stream_ref);
                } else {
                    self.transformed_stream_refs.remove(&stream_ref);
                }
                if let Some(eol) = recovered_eol {
                    self.recovered_stream_eols.insert(stream_ref, eol);
                } else {
                    self.recovered_stream_eols.remove(&stream_ref);
                }
                self.record_file_object_diagnostics(stream_ref, offset, parsed.diagnostics)?;
                let value = self.lift_for_set_object(&object, &stream_handle)?;
                stream_handle.set_resolved(value);
                Ok(Some(stream_handle))
            }
            Some(
                CacheEntry::Compressed { .. }
                | CacheEntry::Missing
                | CacheEntry::Deleted
                | CacheEntry::Reserved,
            )
            | None => match canonical_error {
                Some(error) => Err(error), // cov:ignore: an error with no recoverable cache entry is a defensive propagation arm
                None => Ok(None),
            },
        }
    }

    fn object_stream_needs_legacy_framing(&mut self, stream_ref: ObjectRef) -> Result<bool> {
        let Some(CacheEntry::Unresolved { offset }) = self.cache.entry(stream_ref).cloned() else {
            return Ok(false);
        };
        // qpdf's readObjectAtOffset does not use the next xref offset as a
        // hard bound while recovering a malformed object. Probe the bounded
        // window first, but retry this inspection through EOF when the window
        // ends inside the object dictionary. This probe charges its own EOF
        // retry against `resolution_fallbacks_remaining`, independent of
        // object_stream_handle_with_legacy_fallback's actual parse retry
        // (`Self::resolution_fallbacks_remaining`'s doc): without its own
        // charge, a crafted xref where every compressed member's container
        // still parses as a bare dictionary at this bounded window would
        // re-run this full-file probe once per member with no cap, since
        // `self.cache` only starts reflecting a resolved container after a
        // member successfully resolves through it.
        let bytes = self.read_object_stream_framing_probe(offset)?;
        let pending = parse_file_object_syntax(&bytes)?;
        let PendingBody::Stream { dict, .. } = pending.body else {
            return Ok(false);
        };
        Ok(!matches!(
            dict.get("Length"),
            Some(Object::Integer(value)) if *value >= 0
        ))
    }

    fn read_object_stream_framing_probe(&mut self, offset: u64) -> Result<Vec<u8>> {
        let next = self.next_object_offset(offset);
        let bytes = self.read_bounded_object_window(offset)?;
        // A false next-object offset can land after the complete object
        // dictionary but before the `stream` keyword, so the bounded parse
        // succeeds as `PendingBody::Direct` (a bare dictionary) instead of
        // failing outright. That is just as truncated as a parse error for
        // an object stream, whose body must be `PendingBody::Stream` -- treat
        // it the same way and retry through EOF.
        let needs_retry = match parse_file_object_syntax(&bytes) {
            Err(_) => true,
            Ok(pending) => matches!(pending.body, PendingBody::Direct { .. }),
        };
        if next.is_some() && self.resolution_fallbacks_remaining > 0 && needs_retry {
            self.resolution_fallbacks_remaining -= 1;
            return self.resolver.read_window(offset, None);
        }
        Ok(bytes)
    }

    #[cfg(test)]
    fn filter_dictionary_to_handle(
        &self,
        dictionary: &Dictionary,
        depth: usize,
    ) -> Result<ObjectHandle> {
        if depth > crate::parser::MAX_PARSE_DEPTH {
            return Err(Error::Unsupported(format!(
                "filter prefix dictionary nesting exceeds maximum of {}",
                crate::parser::MAX_PARSE_DEPTH
            )));
        }
        let entries = dictionary
            .iter()
            .map(|(key, value)| {
                Ok((
                    crate::object_handle::canonical_dictionary_key_from_legacy(key),
                    self.filter_object_to_handle(value, depth + 1)?,
                ))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
        Ok(self
            .resolver
            .direct_object_handle(ObjectValue::Dictionary(entries)))
    }

    #[cfg(test)]
    fn filter_object_to_handle(&self, object: &Object, depth: usize) -> Result<ObjectHandle> {
        if depth > crate::parser::MAX_PARSE_DEPTH {
            return Err(Error::Unsupported(format!(
                "filter prefix object nesting exceeds maximum of {}",
                crate::parser::MAX_PARSE_DEPTH
            )));
        }
        stacker::maybe_grow(READER_STACK_RED_ZONE, READER_STACK_GROWTH_SIZE, || {
            let value = match object {
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
                Object::Array(values) => ObjectValue::Array(
                    values
                        .iter()
                        .map(|value| self.filter_object_to_handle(value, depth + 1))
                        .collect::<Result<Vec<_>>>()?,
                ),
                Object::Dictionary(dictionary) => {
                    let handle = self.filter_dictionary_to_handle(dictionary, depth + 1)?;
                    return Ok(handle);
                }
                Object::Stream(stream) => ObjectValue::Stream {
                    stream_dict: self.filter_dictionary_to_handle(&stream.dict, depth + 1)?,
                    stream_data: Some(Rc::new(stream.data.clone())),
                    stream_length: 0,
                    stream_provider: None,
                },
                Object::Reference(object_ref) => {
                    return Ok(self.resolver.get_object_handle(*object_ref));
                }
                Object::Operator(_) | Object::InlineImage(_) => {
                    return Err(Error::Unsupported(
                        "filter prefix contains a content-stream-only object".to_owned(),
                    ));
                }
            };
            Ok(self.resolver.direct_object_handle(value))
        })
    }

    /// qpdf's unknown-crypt-filter warnings, in the order qpdf emits them for
    /// one object: strings from `QPDF::decryptString`
    /// (`libqpdf/QPDF_encryption.cc:1000-1001`) before streams from
    /// `QPDF::decryptStream` (`:1123-1129`).
    ///
    /// Each fires at most once per document because the selection that
    /// requests it also rewrites the crypt filter it complained about.
    fn warn_unknown_crypt_filters(&self, strings: bool, streams: bool) -> Result<()> {
        if strings {
            self.resolver.push_warning(
                "unknown encryption filter for strings (check /StrF in /Encrypt dictionary); \
                 strings may be decrypted improperly",
            )?;
        }
        if streams {
            self.resolver.push_warning(
                "unknown encryption filter for streams (check /StmF from /Encrypt dictionary); \
                 streams may be decrypted improperly",
            )?;
        }
        Ok(())
    }

    fn parse_object_stream_entry_from_handle(
        &mut self,
        stream_handle: &ObjectHandle,
        target_object_number: u32,
    ) -> Result<Option<ParsedObjectStreamEntry>> {
        let stream_dict = stream_handle
            .as_stream_dict()
            .ok_or_else(|| Error::parse(0, "object stream is not a stream"))?;
        let stream_data = self.decode_object_stream_data(stream_handle)?;
        let stream_object_count = object_stream_count_from_handle(&stream_dict)?;
        let stream_data_first =
            object_stream_integer_from_handle(&stream_dict, b"/First", "/First")?;

        let first = usize::try_from(stream_data_first)
            .map_err(|_| Error::parse(0, "Object stream /First does not fit usize"))?;

        let mut tokenizer = Tokenizer::new(&stream_data);
        let mut object_offsets = BTreeMap::new();
        for _ in 0..stream_object_count {
            // qpdf stores these pairs in std::map keyed by object number
            // (`QPDF.cc:1788-1790`); the type-2 field2 is not this selector.
            let object_number = u32::try_from(parse_non_negative_u64(
                tokenizer.next_integer()?,
                "object stream object number",
            )?)
            .map_err(|_| Error::parse(0, "object stream object number does not fit u32"))?;
            let object_offset =
                parse_non_negative_u64(tokenizer.next_integer()?, "object stream object offset")?;
            object_offsets.insert(object_number, object_offset);
        }

        let Some(object_offset) = object_offsets.get(&target_object_number).copied() else {
            return Ok(None);
        };

        let start = first
            .checked_add(
                usize::try_from(object_offset)
                    .map_err(|_| Error::parse(0, "object stream offset does not fit usize"))?,
            )
            .ok_or_else(|| Error::parse(0, "compressed object offset overflow"))?;

        if start > stream_data.len() {
            return Err(Error::parse(0, "compressed object offset out of range"));
        }

        let (object, mut diagnostics) = parse_qpdf_file_object(&stream_data[start..])?;
        for diagnostic in &mut diagnostics {
            diagnostic.relative_offset += start;
        }
        Ok(Some(ParsedObjectStreamEntry {
            object,
            diagnostics,
        }))
    }

    fn decode_object_stream_data(&self, stream_handle: &ObjectHandle) -> Result<Vec<u8>> {
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
            return Err(Error::Unsupported(
                "getStreamData called on unfilterable stream".to_owned(),
            ));
        }
        Ok(decoded_stream.take_buffer()?)
    }

    fn record_object_stream_diagnostics(
        &mut self,
        stream_ref: ObjectRef,
        object_ref: ObjectRef,
        diagnostics: Vec<crate::parser::ParserDiagnostic>,
    ) -> Result<()> {
        for diagnostic in diagnostics {
            self.push_warning(format!(
                "object stream {} (object {} {}, offset {}): {}",
                stream_ref.number,
                object_ref.number,
                object_ref.generation,
                diagnostic.relative_offset,
                diagnostic.message
            ))?;
        }
        Ok(())
    }
}

/// qpdf `QPDF::decryptString`'s decryption half (`:1009-1038`), applied to
/// every string reachable from `object`.
fn decrypt_object_strings(
    object_ref: ObjectRef,
    object: &mut Object,
    encryption: &mut EncryptionState,
) -> Result<bool> {
    let encrypt_ref = encryption.encrypt_ref;
    if Some(object_ref) == encrypt_ref || !object_contains_string(object, 0)? {
        return Ok(false);
    }
    let (use_aes, warn) = encryption.string_method();
    let Some(use_aes) = use_aes else {
        return Ok(warn);
    };
    encryption.with_object_cipher(object_ref, use_aes, |cipher| {
        decrypt_strings_in_object(object_ref, object, cipher, encrypt_ref)
    })?;
    Ok(warn)
}

/// Whether qpdf's parser would encounter a string token while reading this
/// already-materialized legacy object. The scan is deliberately key-free:
/// `decryptString` and therefore `getKeyForObject` run only at an actual
/// string token (`QPDFParser.cc:114-121`).
fn object_contains_string(object: &Object, depth: usize) -> Result<bool> {
    if depth > crate::object::MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(format!(
            "decrypt: inline object nesting exceeds maximum of {}",
            crate::object::MAX_INLINE_DEPTH // cov:ignore-start: llvm maps the covered recursive error and String arm to trailing syntax
        )));
    }
    let values: Option<Box<dyn Iterator<Item = &Object> + '_>> = match object {
        Object::String(_) => return Ok(true),
        // cov:ignore-end
        Object::Array(values) => Some(Box::new(values.iter())),
        Object::Dictionary(dict) => Some(Box::new(dict.iter().map(|(_, value)| value))),
        Object::Stream(stream) => Some(Box::new(stream.dict.iter().map(|(_, value)| value))),
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::RealLiteral { .. }
        | Object::Name(_)
        | Object::Reference(_)
        | Object::Operator(_)
        | Object::InlineImage(_) => None,
    };
    if let Some(values) = values {
        for value in values {
            if object_contains_string(value, depth + 1)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// Test-only mirror of `decrypt_object_strings`'s mode dispatch (same key
// derivation, same `decrypt_cipher_bytes` leaf primitive), walking an
// `ObjectValue`/`ObjectHandle` tree in place. The production canonical parser
// decrypts literal strings as they are parsed; these helpers retain focused
// coverage for the handle-tree encryption primitive itself.
//
// Returns whether the caller must emit the unknown-crypt-filter warning for
// strings, exactly as `decrypt_resolved_object`'s string half does; the crypt
// filter selection is shared, so a document only ever warns once.
#[cfg(test)]
fn decrypt_object_value_strings(
    object_ref: ObjectRef,
    value: &mut ObjectValue,
    encryption: &mut EncryptionState,
) -> Result<bool> {
    if Some(object_ref) == encryption.encrypt_ref {
        return Ok(false);
    }
    if !object_value_contains_string(value)? {
        return Ok(false);
    }
    let (use_aes, warn) = encryption.string_method();
    let Some(use_aes) = use_aes else {
        return Ok(warn);
    };
    encryption.with_object_cipher(object_ref, use_aes, |cipher| {
        decrypt_strings_in_object_value(value, cipher)
    })?;
    Ok(warn)
}

#[cfg(test)]
fn object_value_contains_string(value: &ObjectValue) -> Result<bool> {
    match value {
        ObjectValue::String(_) => Ok(true),
        ObjectValue::Array(items) => handles_contain_string(items.iter(), 1),
        ObjectValue::Dictionary(entries) => handles_contain_string(entries.values(), 1),
        ObjectValue::Stream { stream_dict, .. } => {
            let entries = stream_dict
                .as_dictionary()
                .expect("a stream's own dictionary handle is always a direct Dictionary value");
            handles_contain_string(entries.values(), 1)
        }
        ObjectValue::Null
        | ObjectValue::Unresolved
        | ObjectValue::Reserved
        | ObjectValue::Destroyed
        | ObjectValue::Boolean(_)
        | ObjectValue::Integer(_)
        | ObjectValue::Real(_)
        | ObjectValue::RealLiteral { .. }
        | ObjectValue::Name(_)
        | ObjectValue::Reference(_)
        | ObjectValue::Operator(_)
        | ObjectValue::InlineImage(_) => Ok(false),
    }
}

#[cfg(test)]
fn handles_contain_string<'a>(
    handles: impl Iterator<Item = &'a ObjectHandle>,
    depth: usize,
) -> Result<bool> {
    for handle in handles {
        if handle_contains_string(handle, depth)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
fn handle_contains_string(handle: &ObjectHandle, depth: usize) -> Result<bool> {
    if handle.is_indirect() {
        return Ok(false);
    }
    if depth > crate::object::MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(format!(
            "decrypt: inline object nesting exceeds maximum of {}",
            crate::object::MAX_INLINE_DEPTH
        )));
    }
    if handle.as_string().is_some() {
        return Ok(true);
    }
    if let Some(items) = handle.as_array() {
        return handles_contain_string(items.iter(), depth + 1);
    }
    if let Some(entries) = handle.as_dictionary() {
        return handles_contain_string(entries.values(), depth + 1);
    }
    if let Some(dict) = handle.as_stream_dict() {
        let entries = dict
            .as_dictionary()
            .expect("a stream's own dictionary handle is always a direct Dictionary value");
        return handles_contain_string(entries.values(), depth + 1);
    }
    Ok(false)
}

// The top-level value itself: a bare `ObjectValue::String` is mutated
// directly (it is a plain owned local, not yet wrapped in a handle); a
// container recurses into its already-populated `ObjectHandle` children via
// `decrypt_handle_strings_in_place`, which tracks depth itself (this
// function is the sole, always-depth-0 entry point -- it never recurses
// into itself, so it carries no depth parameter of its own).
#[cfg(test)]
fn decrypt_strings_in_object_value(
    value: &mut ObjectValue,
    cipher: StringCipher<'_>,
) -> Result<()> {
    match value {
        ObjectValue::String(bytes) => decrypt_cipher_bytes(bytes, cipher),
        ObjectValue::Array(items) => {
            for item in items.iter() {
                decrypt_handle_strings_in_place(item, cipher, 1)?;
            }
            Ok(())
        }
        ObjectValue::Dictionary(entries) => {
            for item in entries.values() {
                decrypt_handle_strings_in_place(item, cipher, 1)?;
            }
            Ok(())
        }
        ObjectValue::Stream { stream_dict, .. } => {
            decrypt_stream_dict_strings_in_place(stream_dict, cipher, 1)
        }
        ObjectValue::Null
        | ObjectValue::Unresolved
        | ObjectValue::Reserved
        | ObjectValue::Destroyed
        | ObjectValue::Boolean(_)
        | ObjectValue::Integer(_)
        | ObjectValue::Real(_)
        | ObjectValue::RealLiteral { .. }
        | ObjectValue::Name(_)
        | ObjectValue::Reference(_)
        | ObjectValue::Operator(_)
        | ObjectValue::InlineImage(_) => Ok(()),
    }
}

// A child reached through direct containment: an indirect handle is a
// terminal leaf here (its own eventual resolution decrypts it separately,
// keyed by its own object ref, exactly like a legacy `Object::Reference`
// stops `decrypt_strings_in_value`'s walk) -- `handle.is_indirect()` is
// checked before touching the handle's value at all, rather than relying on
// `replace_direct_value`'s no-op-on-indirect behavior alone, so a resolved
// indirect child's own contents are never even read here, let alone
// decrypted with the wrong (parent's) key. A direct string is decrypted and
// written back in place via `replace_direct_value`, which preserves the
// handle's identity and already-recorded parsed offset.
#[cfg(test)]
fn decrypt_handle_strings_in_place(
    handle: &ObjectHandle,
    cipher: StringCipher<'_>,
    depth: usize,
) -> Result<()> {
    if handle.is_indirect() {
        return Ok(());
    }
    if depth > crate::object::MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(format!(
            "decrypt: inline object nesting exceeds maximum of {}",
            crate::object::MAX_INLINE_DEPTH
        )));
    }
    if let Some(mut bytes) = handle.as_string() {
        decrypt_cipher_bytes(&mut bytes, cipher)?;
        handle.replace_direct_value(ObjectValue::String(bytes));
        return Ok(());
    }
    if let Some(items) = handle.as_array() {
        for item in &items {
            decrypt_handle_strings_in_place(item, cipher, depth + 1)?;
        }
        return Ok(());
    }
    if let Some(entries) = handle.as_dictionary() {
        for item in entries.values() {
            decrypt_handle_strings_in_place(item, cipher, depth + 1)?;
        }
        return Ok(());
    }
    if let Some(dict) = handle.as_stream_dict() {
        return decrypt_stream_dict_strings_in_place(&dict, cipher, depth + 1);
    }
    Ok(())
}

// Walks a stream's own dictionary entries directly, at exactly the depth its
// caller specifies, without charging its own extra inline level for the
// dictionary handle itself first. Matches `decrypt_strings_in_value`'s
// `Object::Stream(stream) => stream.dict.values_mut()` arm, which visits a
// stream's dictionary entries at the *same* depth+1 the stream itself was
// reached at -- the legacy walk never treats the stream's own dictionary
// container as a nesting level in its own right, only its entries count.
// Getting this wrong is real, not cosmetic: the focused tests exercise the
// same-depth accounting used by the raw-object compatibility walker. Charging
// one extra level would reject a value that that explicitly legacy boundary
// accepts and decrypts.
#[cfg(test)]
fn decrypt_stream_dict_strings_in_place(
    dict: &ObjectHandle,
    cipher: StringCipher<'_>,
    depth: usize,
) -> Result<()> {
    let entries = dict
        .as_dictionary()
        .expect("a stream's own dictionary handle is always a direct Dictionary value");
    for item in entries.values() {
        decrypt_handle_strings_in_place(item, cipher, depth)?;
    }
    Ok(())
}

/// qpdf `QPDF::decryptStream`'s decryption half (`:1136-1153`).
///
/// qpdf prepends `Pl_RC4` for streams where `decryptString` uses the bare
/// `RC4` class (`:1024-1031` versus `:1146-1151`); this keeps that split.
fn decrypt_stream_bytes(
    object_ref: ObjectRef,
    bytes: &mut Vec<u8>,
    use_aes: bool,
    encryption: &mut EncryptionState,
) -> Result<()> {
    if !use_aes {
        let key = encryption.key_for_object(object_ref, false).to_vec();
        return PlRc4::transform_in_place("RC4 stream decryption", bytes, &key).map_err(Into::into);
    }
    encryption.with_object_cipher(object_ref, true, |cipher| {
        decrypt_cipher_bytes(bytes, cipher)
    })
}

/// Sets `warn_unknown` when the caller must emit the unknown-crypt-filter
/// warning for streams: a `/Crypt` filter naming a method qpdf does not
/// recognise still goes through `QPDF::decryptStream`'s switch, whose unknown
/// arm warns and resets `cf_stream` (`libqpdf/QPDF_encryption.cc:1121-1133`).
///
/// The flag is an out-parameter rather than part of the return value because
/// qpdf raises that warning at `:1121-1133`, ahead of the key derivation at
/// `:1135` and the pipeline construction at `:1137-1149`. Returning it would
/// discard it whenever one of those later steps fails, which is the opposite
/// of qpdf's ordering.
fn apply_explicit_crypt_filters(
    object_ref: ObjectRef,
    stream: &mut crate::Stream,
    encryption: &mut EncryptionState,
    recovered_stream_eol: Option<&[u8]>,
    warn_unknown: &mut bool,
) -> Result<()> {
    let filter = stream
        .dict
        .get("Filter")
        .cloned()
        .expect("caller checked for an explicit /Crypt filter");
    let decode_params = stream.dict.get("DecodeParms").cloned();
    if let Some(filters) = filter.as_array() {
        crate::filters::validate_filter_chain_len(filters)?;
    }

    // qpdf's QPDF::decryptStream prepends one decryption stage to the source
    // pipeline before QPDF_Stream applies any /Filter entry
    // (QPDF_encryption.cc:1041-1043, QPDF.cc:2477-2492). The legacy raw
    // Object compatibility path is still materialized here, but it must keep
    // that ordering: decrypt the raw payload once, then remove /Crypt and
    // leave every other filter's encoded bytes untouched. Decoding a prefix
    // and re-encoding it made the result depend on flpdf-only encoders and
    // was wrong whenever /Crypt followed ASCII85/ASCIIHex/Flate.
    let explicit_mode = match &filter {
        Object::Name(name) if name.as_slice() == b"Crypt" => {
            Some(explicit_crypt_mode(encryption, decode_params.as_ref()))
        }
        Object::Array(filters) => filters
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, filter)| {
                if !matches!(filter, Object::Name(name) if name.as_slice() == b"Crypt") {
                    return None;
                }
                let params = decode_params_at(decode_params.as_ref(), index)?;
                let params_dict = params.as_dict()?;
                params_dict.get("Name")?.as_name()?;
                Some(explicit_crypt_mode(encryption, Some(params)))
            }),
        _ => None, // cov:ignore: the caller only enters for a name or array containing Crypt
    };
    // For an array, a malformed or missing local `/Name` leaves qpdf's
    // `method` at `e_unknown`, which falls back to the document `/StmF`.
    let (use_aes, warn) = encryption.stream_method(explicit_mode);
    *warn_unknown |= warn;

    if let Some(eol) = recovered_stream_eol {
        stream.data.extend_from_slice(eol);
        if use_aes.is_some() {
            // The recovered line ending belongs to the parser's source span,
            // not to the encrypted stream payload. qpdf subtracts it from the
            // source length before its prepended decryption stage.
            stream.data.truncate(stream.data.len() - eol.len());
        }
    }
    if let Some(use_aes) = use_aes {
        decrypt_stream_bytes(object_ref, &mut stream.data, use_aes, encryption)?;
    }

    remove_explicit_crypt_filter_with_values(stream, filter, decode_params);
    Ok(())
}

fn remove_explicit_crypt_filter_with_values(
    stream: &mut crate::Stream,
    filter: Object,
    decode_params: Option<Object>,
) {
    if matches!(filter, Object::Name(ref name) if name.as_slice() == b"Crypt") {
        stream.dict.remove("Filter");
        stream.dict.remove("DecodeParms");
        return;
    }

    let Some(mut filters) = filter.into_array() else {
        return; // cov:ignore: the qpdf explicit-crypt shape is a name or array; malformed shape is defensive
    };
    let mut decode_params = decode_params;
    let mut index = 0;
    while index < filters.len() {
        if matches!(&filters[index], Object::Name(name) if name.as_slice() == b"Crypt") {
            filters.remove(index);
            if let Some(Object::Array(params)) = &mut decode_params {
                if index < params.len() {
                    params.remove(index);
                }
            }
        } else {
            index += 1;
        }
    }

    if filters.is_empty() {
        stream.dict.remove("Filter");
        stream.dict.remove("DecodeParms");
    } else {
        stream.dict.insert("Filter", Object::Array(filters));
        match decode_params {
            Some(params) => stream.dict.insert("DecodeParms", params),
            None => {
                stream.dict.remove("DecodeParms");
            }
        };
    }
}

fn decode_params_at(decode_params: Option<&Object>, index: usize) -> Option<&Object> {
    let params = decode_params?;
    if params.as_dict().is_some() {
        Some(params)
    } else {
        params.as_array()?.get(index)
    }
}

/// The crypt filter a stream's own `/Crypt` `/DecodeParms` names, read the way
/// qpdf reads it inside `QPDF::decryptStream` (`:1069-1073`, `:1083-1088`):
/// through `interpretCF`, so an unrecognised name is `e_unknown` rather than
/// an error.
fn explicit_crypt_mode(
    encryption: &EncryptionState,
    decode_params: Option<&Object>,
) -> EncryptionMode {
    let Some(params) = decode_params.and_then(Object::as_dict) else {
        return EncryptionMode::Identity;
    };
    interpret_cf(&encryption.crypt_filters, params.get("Name"))
}

fn stream_has_explicit_crypt_filter(dict: &Dictionary) -> bool {
    dict.get("Filter").is_some_and(|filter| {
        filter.as_name() == Some(b"Crypt".as_slice())
            || filter.as_array().is_some_and(|filters| {
                filters
                    .iter()
                    .any(|filter| filter.as_name() == Some(b"Crypt".as_slice()))
            })
    })
}

fn is_metadata_stream(dict: &Dictionary) -> bool {
    dict.get("Type")
        .and_then(Object::as_name)
        .is_some_and(|name| name == b"Metadata")
}

fn aes128_object_key(key: &[u8]) -> Result<[u8; 16]> {
    key.try_into().map_err(|_| {
        EncryptedError::Malformed {
            reason: "AES-128 object key is not 16 bytes".into(),
        }
        .into()
    })
}

#[cfg(test)]
pub(crate) fn parse_object_stream_entry(
    stream_object: &crate::Stream,
    target_index: u32,
) -> Result<ParsedObjectStreamEntry> {
    let stream_data = crate::filters::decode_stream_data(&stream_object.dict, &stream_object.data)?;

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

    let (object, mut diagnostics) = parse_qpdf_file_object(&stream_data[start..])?;
    for diagnostic in &mut diagnostics {
        diagnostic.relative_offset += start;
    }
    Ok(ParsedObjectStreamEntry {
        object,
        diagnostics,
    })
}

pub(crate) struct ParsedObjectStreamEntry {
    pub(crate) object: Object,
    diagnostics: Vec<crate::parser::ParserDiagnostic>,
}

fn standard_handler_inputs<'a>(
    encrypt: &'a Dictionary,
    trailer: &'a Dictionary,
) -> Result<StandardHandlerInputs<'a>> {
    let filter = required_name(encrypt, "Filter")?;
    let v = required_integer(encrypt, "V")?;
    let r = required_integer(encrypt, "R")?;
    if filter != "Standard" || !matches!((v, r), (1 | 2, 2 | 3) | (4, 4)) {
        return Err(EncryptedError::UnsupportedHandler {
            filter: filter.to_string(),
            v,
            r,
            cfm: crypt_filter_method(encrypt),
        }
        .into());
    }

    let length_bits = match encrypt.get("Length") {
        Some(Object::Integer(value)) => *value,
        Some(_) => {
            return Err(EncryptedError::Malformed {
                reason: "/Length entry is not an integer".into(),
            }
            .into())
        }
        None => 40,
    };
    let p = required_permissions(encrypt)?;
    let u = required_32_byte_string(encrypt, "U")?;
    let o = required_32_byte_string(encrypt, "O")?;
    let id0 = first_file_id(trailer)?;
    let encrypt_metadata = encrypt_metadata_flag(encrypt)?;

    Ok(StandardHandlerInputs {
        v,
        r,
        length_bits,
        p,
        id0,
        u,
        o,
        encrypt_metadata,
    })
}

/// Reclassify a wrong-length `/U` or `/O` `Malformed` error from
/// [`standard_handler_r5_inputs`] as [`EncryptedError::BadPassword`].
///
/// Scoped to the V=5 R=5/R=6 authentication path (the sole caller of
/// `standard_handler_r5_inputs`): a `/U` or `/O` entry that is not exactly
/// 48 bytes is an unusable credential entry that is indistinguishable, from a
/// caller's perspective, from supplying the wrong password — qpdf reports
/// "invalid password" here, so we map to `BadPassword` for parity.
///
/// Only the `/U` / `/O` *length* error is remapped. `/UE` / `/OE` length
/// errors, missing entries, and non-string entries stay `Malformed`: those are
/// genuine structural defects, not credential mismatches. No broader
/// `Malformed` reclassification is performed.
fn map_uo_length_to_bad_password(err: Error) -> Error {
    match &err {
        Error::Encrypted(EncryptedError::Malformed { reason })
            if reason == "/U entry is not 48 bytes" || reason == "/O entry is not 48 bytes" =>
        {
            EncryptedError::BadPassword.into()
        }
        _ => err,
    }
}

/// Decode the `--password` value as a raw hex file encryption key for
/// `--password-is-hex-key` (qpdf parity).
///
/// qpdf accepts upper- or lower-case hex and tolerates embedded whitespace;
/// the decoded key must be at most 32 bytes (the longest Standard-handler key,
/// AES-256). Invalid hex or an over-length key is reported as a clear
/// [`EncryptedError::Malformed`] — never a panic. An empty input decodes to an
/// empty key and is passed through unchanged (decryption then fails naturally
/// downstream; no special-casing here).
fn decode_hex_file_key(raw: &[u8]) -> Result<Vec<u8>> {
    let trimmed: Vec<u8> = raw
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let key = hex::decode(&trimmed).map_err(|err| EncryptedError::Malformed {
        reason: format!("--password-is-hex-key: --password is not valid hex ({err})"),
    })?;
    if key.len() > 32 {
        return Err(EncryptedError::Malformed {
            reason: format!(
                "--password-is-hex-key: decoded key is {} bytes; \
                 the Standard security handler key is at most 32 bytes",
                key.len()
            ),
        }
        .into());
    }
    Ok(key)
}

fn standard_handler_r5_inputs(encrypt: &Dictionary) -> Result<StandardHandlerR5Inputs<'_>> {
    let filter = required_name(encrypt, "Filter")?;
    let v = required_integer(encrypt, "V")?;
    let r = required_integer(encrypt, "R")?;
    if filter != "Standard" || v != 5 || !matches!(r, 5 | 6) {
        return Err(EncryptedError::UnsupportedHandler {
            filter: filter.to_string(),
            v,
            r,
            cfm: crypt_filter_method(encrypt),
        }
        .into());
    }

    Ok(StandardHandlerR5Inputs {
        u: required_48_byte_string(encrypt, "U")?,
        o: required_48_byte_string(encrypt, "O")?,
        ue: required_32_byte_string(encrypt, "UE")?,
        oe: required_32_byte_string(encrypt, "OE")?,
    })
}

fn encrypt_metadata_flag(encrypt: &Dictionary) -> Result<bool> {
    match encrypt.get("EncryptMetadata") {
        Some(Object::Boolean(value)) => Ok(*value),
        Some(_) => Err(EncryptedError::Malformed {
            reason: "/EncryptMetadata entry is not a boolean".into(),
        }
        .into()),
        None => Ok(true),
    }
}

fn required_permissions(encrypt: &Dictionary) -> Result<i32> {
    i32::try_from(required_integer(encrypt, "P")?).map_err(|_| {
        EncryptedError::Malformed {
            reason: "/P entry is out of i32 range".into(),
        }
        .into()
    })
}

fn r6_perms_warning(
    encrypt: &Dictionary,
    file_key: &[u8],
    permissions: Permissions,
    encrypt_metadata: bool,
) -> Result<Option<String>> {
    let Some(perms) = encrypt.get("Perms") else {
        return Ok(None);
    };
    let Object::String(bytes) = perms else {
        return Ok(Some("R=6 /Perms entry is not a string".into()));
    };
    let Ok(mut block) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        return Ok(Some("R=6 /Perms entry is not 16 bytes".into()));
    };
    let Ok(file_key) = <&[u8; 32]>::try_from(file_key) else {
        return Ok(Some(
            "R=6 /Perms cannot be verified with non-256-bit file key".into(),
        ));
    };

    crate::security::primitives::aes256_ecb_decrypt_block(file_key, &mut block);
    let perms_p = i32::from_le_bytes(block[..4].try_into().expect("slice length checked"));
    let perms_metadata = match block[8] {
        b'T' => true,
        b'F' => false,
        _ => {
            return Ok(Some(
                "R=6 /Perms encrypted-metadata flag is not T or F".into(),
            ))
        }
    };

    if perms_p != permissions.raw() {
        return Ok(Some(format!(
            "R=6 /Perms permissions value {perms_p} does not match /P {}",
            permissions.raw()
        )));
    }
    if block[4..8] != [0xff; 4] {
        return Ok(Some("R=6 /Perms reserved bytes are invalid".into()));
    }
    if perms_metadata != encrypt_metadata {
        return Ok(Some(
            "R=6 /Perms encrypted-metadata flag does not match /EncryptMetadata".into(),
        ));
    }
    if &block[9..12] != b"adb" {
        return Ok(Some("R=6 /Perms magic bytes are not 'adb'".into()));
    }
    Ok(None)
}

fn required_revision(encrypt: &Dictionary) -> Result<i64> {
    required_integer(encrypt, "R")
}

/// qpdf's `/V` requirement, checked alongside `/R` before any password work
/// (`libqpdf/QPDF_encryption.cc:770-777`) and stored at `:797`.
fn required_version(encrypt: &Dictionary) -> Result<i64> {
    required_integer(encrypt, "V")
}

fn interpret_cf_name(
    crypt_filters: &BTreeMap<Vec<u8>, EncryptionMode>,
    filter: Option<&[u8]>,
) -> EncryptionMode {
    let Some(filter) = filter else {
        return EncryptionMode::Identity;
    };
    if let Some(mode) = crypt_filters.get(filter) {
        return *mode;
    }
    if filter == b"Identity" {
        return EncryptionMode::Identity;
    }
    EncryptionMode::Unknown
}

/// qpdf `QPDF::interpretCF` (`libqpdf/QPDF_encryption.cc:700-716`).
///
/// The branch order is load-bearing: the `/CF` lookup runs **before** the
/// built-in `/Identity`, so a document that defines a crypt filter actually
/// named `/Identity` shadows the built-in and gets that filter's method. A
/// selector that is not a name at all is qpdf's "Default: /Identity" and
/// yields `e_none`.
///
/// This materialized adapter retains the existing `Object` caller boundary.
fn interpret_cf(
    crypt_filters: &BTreeMap<Vec<u8>, EncryptionMode>,
    cf: Option<&Object>,
) -> EncryptionMode {
    interpret_cf_name(crypt_filters, cf.and_then(Object::as_name))
}

/// qpdf `QPDF::interpretCF`'s ObjectHandle boundary
/// (`include/qpdf/QPDF.hh:1122-1127`,
/// `libqpdf/QPDF_encryption.cc:700-716`).
pub(in crate::reader) fn interpret_cf_from_handle(
    encryption: &EncryptionState,
    cf: &ObjectHandle,
) -> Result<EncryptionMode> {
    let filter = cf.try_as_name()?;
    Ok(interpret_cf_name(
        &encryption.crypt_filters,
        filter.as_deref(),
    ))
}

/// qpdf's `/CF` loop inside `QPDF::initializeEncryption`
/// (`libqpdf/QPDF_encryption.cc:860-884`).
///
/// Deliberately total: a `/CF` value that is not a dictionary is skipped, and
/// a `/CFM` that is not a name leaves the entry at `e_none`. Neither is an
/// error for qpdf, which defers judgement because the document may never
/// reference that filter ("Don't complain now -- maybe we won't need to
/// reference this type", `:878-879`). An unrecognised `/CFM` becomes
/// `e_unknown` for the same reason.
fn crypt_filter_modes(encrypt: &Dictionary, v: i64) -> BTreeMap<Vec<u8>, EncryptionMode> {
    let mut modes = BTreeMap::new();
    // qpdf gates the whole block on `/V`, not `/R` (`:860`).
    if !matches!(v, 4 | 5) {
        return modes;
    }
    let Some(cf) = encrypt.get("CF").and_then(Object::as_dict) else {
        return modes;
    };
    for (name, value) in cf.iter() {
        let Some(filter) = value.as_dict() else {
            continue;
        };
        // qpdf initialises to `e_none` and only enters the `/CFM` branch when
        // the entry is a name, so a missing or non-name `/CFM` is `e_none`.
        let mut mode = EncryptionMode::Identity;
        if let Some(cfm) = filter.get("CFM").and_then(Object::as_name) {
            mode = match cfm {
                b"V2" => EncryptionMode::Rc4,
                b"AESV2" => EncryptionMode::Aes128,
                b"AESV3" => EncryptionMode::Aes256,
                _ => EncryptionMode::Unknown,
            };
        }
        modes.insert(name.to_vec(), mode);
    }
    modes
}

fn required_integer(dict: &Dictionary, key: &'static str) -> Result<i64> {
    match dict.get(key) {
        Some(Object::Integer(value)) => Ok(*value),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not an integer"),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
    }
}

fn required_name<'a>(dict: &'a Dictionary, key: &'static str) -> Result<&'a str> {
    match dict.get(key) {
        Some(Object::Name(name)) => std::str::from_utf8(name).map_err(|_| {
            EncryptedError::Malformed {
                reason: format!("/{key} entry is not valid UTF-8"),
            }
            .into()
        }),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not a name"),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
    }
}

fn required_32_byte_string<'a>(dict: &'a Dictionary, key: &'static str) -> Result<&'a [u8; 32]> {
    match dict.get(key) {
        Some(Object::String(bytes)) => bytes.as_slice().try_into().map_err(|_| {
            EncryptedError::Malformed {
                reason: format!("/{key} entry is not 32 bytes"),
            }
            .into()
        }),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not a string"),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
    }
}

fn required_48_byte_string<'a>(dict: &'a Dictionary, key: &'static str) -> Result<&'a [u8; 48]> {
    match dict.get(key) {
        Some(Object::String(bytes)) => bytes.as_slice().try_into().map_err(|_| {
            EncryptedError::Malformed {
                reason: format!("/{key} entry is not 48 bytes"),
            }
            .into()
        }),
        Some(_) => Err(EncryptedError::Malformed {
            reason: format!("/{key} entry is not a string"),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
    }
}

fn first_file_id(trailer: &Dictionary) -> Result<&[u8]> {
    match trailer.get("ID") {
        Some(Object::Array(ids)) => match ids.first() {
            Some(Object::String(id0)) => Ok(id0),
            Some(_) => Err(EncryptedError::Malformed {
                reason: "/ID first entry is not a string".into(),
            }
            .into()),
            None => Err(EncryptedError::Malformed {
                reason: "/ID array is empty".into(),
            }
            .into()),
        },
        Some(_) => Err(EncryptedError::Malformed {
            reason: "/ID entry is not an array".into(),
        }
        .into()),
        None => Err(EncryptedError::Malformed {
            reason: "missing /ID entry".into(),
        }
        .into()),
    }
}

fn crypt_filter_method(encrypt: &Dictionary) -> Option<String> {
    let Some(Object::Dictionary(cf)) = encrypt.get("CF") else {
        return None;
    };
    let Object::Dictionary(std_cf) = cf.get("StdCF")? else {
        return None;
    };
    let Object::Name(cfm) = std_cf.get("CFM")? else {
        return None;
    };
    Some(String::from_utf8_lossy(cfm).to_string())
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

fn object_stream_integer_from_handle(
    stream_dict: &ObjectHandle,
    key: &[u8],
    label: &str,
) -> Result<i64> {
    let value = stream_dict.try_get_key(key)?.try_as_integer()?;
    let Some(value) = value else {
        return Err(Error::parse(
            0,
            format!("object stream {label} is not an integer"),
        ));
    };
    if value < 0 {
        return Err(Error::parse(
            0,
            format!("object stream {label} is negative"),
        ));
    }
    Ok(value)
}

fn object_stream_count_from_handle(stream_dict: &ObjectHandle) -> Result<usize> {
    usize::try_from(object_stream_integer_from_handle(stream_dict, b"/N", "/N")?)
        .map_err(|_| Error::parse(0, "object stream /N does not fit usize"))
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

fn parse_non_negative_u64(value: i64, context: &str) -> Result<u64> {
    if value < 0 {
        return Err(Error::parse(0, format!("{context} is negative")));
    }
    Ok(value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::page_refs;
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::PipelineHandle;
    use crate::Stream;
    use std::io::{Cursor, SeekFrom};
    use std::process::Command;
    use std::rc::Rc;
    use std::sync::Arc;

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
        pdf.resolve_object_handle(&root)
            .expect("resolve destination root");
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

    #[test]
    fn decrypt_resolved_object_never_decrypts_the_encrypt_dictionary() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
        let file = std::fs::File::open(path).expect("encrypted fixture");
        let mut pdf = Pdf::open(std::io::BufReader::new(file)).expect("authenticate fixture");
        let encrypt_ref = pdf.encryption_ref().expect("indirect /Encrypt");
        let sentinel = Object::Integer(42);

        let (object, stream_payload_decrypted) = pdf
            .decrypt_resolved_object(encrypt_ref, sentinel.clone(), None)
            .expect("/Encrypt bypass");

        assert_eq!(object, sentinel);
        assert!(!stream_payload_decrypted);
    }

    #[test]
    fn object_handle_as_string_decrypts_canonical_parsed_encrypted_strings() {
        use crate::encrypt_setup::EncryptParams;
        use crate::writer::{emit_canonical_pdf, CompressStreams, WriterOptions};
        use std::io::Cursor;

        // A minimal Catalog/Pages/Info fixture: /Info (trailer-reachable,
        // survives the writer's Catalog-first reachability walk) holds a
        // direct, uncompressed, plain-xref-table string -- exactly the shape
        // The canonical live parser builds the handle graph directly from
        // raw source bytes, which is the path this test targets.
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut entries: Vec<(u16, usize)> = Vec::new();
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        entries.push((0, bytes.len()));
        bytes.extend_from_slice(b"3 0 obj\n<< /Title (TopSecretTitle) >>\nendobj\n");
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

        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open plaintext fixture");
        let mut out = Vec::new();
        let options = WriterOptions {
            compress_streams: CompressStreams::No,
            encrypt: Some(EncryptParams::v4_aes128(
                b"user-pw".to_vec(),
                b"owner-pw".to_vec(),
            )),
            ..WriterOptions::default()
        };
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("V=4 AES128 encrypted write");

        let mut rt = Pdf::open_with_options(
            Cursor::new(out),
            PdfOpenOptions {
                password: b"user-pw".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("re-open of V=4 output with user-pw");

        let info_ref = match rt.trailer().get("Info") {
            Some(Object::Reference(r)) => *r,
            // Defensive-only: this test's own fixture always writes /Info
            // as a reference (the writer's Catalog-first renumber keeps it
            // that way), so this arm cannot run.
            other => panic!("trailer /Info must be a reference, got {other:?}"), // cov:ignore: unreachable given this test's own fixture
        };
        let info_handle = rt.get_object_handle(info_ref);
        rt.resolve_object_handle(&info_handle)
            .expect("resolve /Info handle");
        let dict = info_handle
            .as_dictionary()
            .expect("/Info must be a dictionary");
        let title_handle = dict.get(b"/Title".as_slice()).expect("/Title entry");
        let title = title_handle.as_string().expect("/Title must be a string");

        assert_eq!(
            title.as_slice(),
            b"TopSecretTitle",
            "ObjectHandle::as_string() must decrypt a canonical-parsed string from an \
             encrypted document, not return raw ciphertext -- the live parser \
             builds ObjectValue::String directly from raw (possibly still-encrypted) source \
             bytes, and this accessor reads that stored value directly"
        );
    }

    /// An `/Encrypt` dictionary carrying `entries`, for the `/CF` and
    /// `interpretCF` ports below.
    fn encrypt_dict(entries: &[(&str, Object)]) -> Dictionary {
        let mut dict = Dictionary::new();
        for (key, value) in entries {
            dict.insert(key, value.clone());
        }
        dict
    }

    fn cf_dict(entries: &[(&str, Object)]) -> Object {
        Object::Dictionary(encrypt_dict(entries))
    }

    /// qpdf `QPDF_encryption.cc:866-880`: the recognised `/CFM` names.
    #[test]
    fn crypt_filter_modes_maps_the_three_recognised_cfm_names() {
        let encrypt = encrypt_dict(&[(
            "CF",
            cf_dict(&[
                ("Rc4CF", cf_dict(&[("CFM", Object::Name(b"V2".to_vec()))])),
                (
                    "AesCF",
                    cf_dict(&[("CFM", Object::Name(b"AESV2".to_vec()))]),
                ),
                (
                    "Aes3CF",
                    cf_dict(&[("CFM", Object::Name(b"AESV3".to_vec()))]),
                ),
            ]),
        )]);

        let modes = crypt_filter_modes(&encrypt, 4);

        assert_eq!(modes[b"Rc4CF".as_slice()], EncryptionMode::Rc4);
        assert_eq!(modes[b"AesCF".as_slice()], EncryptionMode::Aes128);
        assert_eq!(modes[b"Aes3CF".as_slice()], EncryptionMode::Aes256);
    }

    /// An unrecognised `/CFM` is `e_unknown`, not an error: qpdf defers because
    /// the document may never reference the filter (`:877-880`). Observed with
    /// `qpdf --password=u --show-encryption` on a `/V 4` file whose `/CFM` was
    /// rewritten to `/AESVX`, which reports `unknown` and exits 0.
    #[test]
    fn crypt_filter_modes_keeps_an_unrecognised_cfm_as_unknown() {
        let encrypt = encrypt_dict(&[(
            "CF",
            cf_dict(&[(
                "StdCF",
                cf_dict(&[("CFM", Object::Name(b"AESVX".to_vec()))]),
            )]),
        )]);

        let modes = crypt_filter_modes(&encrypt, 4);

        assert_eq!(modes[b"StdCF".as_slice()], EncryptionMode::Unknown);
    }

    /// A missing or non-name `/CFM` leaves qpdf's `e_none` initialiser in place
    /// (`:865-866`). Observed: blanking `/CFM /AESV2`, and replacing it with
    /// `/CFM 12345`, both make qpdf report `none` and decline to decrypt.
    #[test]
    fn crypt_filter_modes_treats_a_missing_or_non_name_cfm_as_none() {
        let encrypt = encrypt_dict(&[(
            "CF",
            cf_dict(&[
                ("NoCFM", cf_dict(&[("Length", Object::Integer(16))])),
                ("BadCFM", cf_dict(&[("CFM", Object::Integer(12345))])),
            ]),
        )]);

        let modes = crypt_filter_modes(&encrypt, 4);

        assert_eq!(modes[b"NoCFM".as_slice()], EncryptionMode::Identity);
        assert_eq!(modes[b"BadCFM".as_slice()], EncryptionMode::Identity);
    }

    /// A `/CF` value that is not a dictionary is skipped rather than rejected
    /// (`:864`), so the name has no entry and later resolves to `e_unknown`.
    /// Observed: replacing the `/StdCF` value with `null` makes qpdf report
    /// `unknown` and exit 0.
    #[test]
    fn crypt_filter_modes_skips_a_non_dictionary_cf_entry() {
        let encrypt = encrypt_dict(&[("CF", cf_dict(&[("StdCF", Object::Null)]))]);

        let modes = crypt_filter_modes(&encrypt, 4);

        assert!(modes.is_empty());
        assert_eq!(
            interpret_cf(&modes, Some(&Object::Name(b"StdCF".to_vec()))),
            EncryptionMode::Unknown
        );
    }

    /// The `/CF` block is gated on `/V`, not `/R` (`:860`).
    #[test]
    fn crypt_filter_modes_only_runs_for_v4_and_v5() {
        let encrypt = encrypt_dict(&[(
            "CF",
            cf_dict(&[("StdCF", cf_dict(&[("CFM", Object::Name(b"V2".to_vec()))]))]),
        )]);

        for v in [4, 5] {
            assert_eq!(crypt_filter_modes(&encrypt, v).len(), 1, "/V {v}");
        }
        for v in [0, 1, 2, 3, 6] {
            assert!(crypt_filter_modes(&encrypt, v).is_empty(), "/V {v}");
        }
    }

    /// qpdf `interpretCF` (`:700-716`). The `/CF` lookup runs before the
    /// built-in `/Identity`, so a document that names a crypt filter
    /// `/Identity` shadows the built-in. Observed: a `/V 4` file with
    /// `/CF << /Identity << /CFM /AESV2 …>> >>` and `/StmF /Identity` makes
    /// qpdf report `AESv2` for streams.
    #[test]
    fn interpret_cf_prefers_a_named_filter_over_the_builtin_identity() {
        let modes = BTreeMap::from([(b"Identity".to_vec(), EncryptionMode::Aes128)]);

        assert_eq!(
            interpret_cf(&modes, Some(&Object::Name(b"Identity".to_vec()))),
            EncryptionMode::Aes128
        );
        assert_eq!(
            interpret_cf(&BTreeMap::new(), Some(&Object::Name(b"Identity".to_vec()))),
            EncryptionMode::Identity
        );
    }

    /// A selector with no `/CF` entry is `e_unknown` (`:710`); one that is not
    /// a name at all is qpdf's "Default: /Identity" (`:712-714`).
    #[test]
    fn interpret_cf_separates_an_absent_entry_from_a_non_name_selector() {
        let modes = BTreeMap::from([(b"StdCF".to_vec(), EncryptionMode::Aes128)]);

        assert_eq!(
            interpret_cf(&modes, Some(&Object::Name(b"NoSuchCF".to_vec()))),
            EncryptionMode::Unknown
        );
        assert_eq!(
            interpret_cf(&modes, Some(&Object::Integer(7))),
            EncryptionMode::Identity
        );
        assert_eq!(interpret_cf(&modes, None), EncryptionMode::Identity);
    }

    #[test]
    fn interpret_cf_from_handle_matches_the_materialized_selector_and_qpdf_order() {
        let mut encryption = explicit_rc4_encryption_state();
        encryption
            .crypt_filters
            .insert(b"Identity".to_vec(), EncryptionMode::Aes128);

        let cases = [
            (
                Object::Name(b"StdCF".to_vec()),
                ObjectHandle::name(b"StdCF".to_vec()),
                EncryptionMode::Rc4,
            ),
            (
                Object::Name(b"Identity".to_vec()),
                ObjectHandle::name(b"Identity".to_vec()),
                EncryptionMode::Aes128,
            ),
            (
                Object::Name(b"NoSuchCF".to_vec()),
                ObjectHandle::name(b"NoSuchCF".to_vec()),
                EncryptionMode::Unknown,
            ),
            (
                Object::Integer(7),
                ObjectHandle::integer(7),
                EncryptionMode::Identity,
            ),
            (Object::Null, ObjectHandle::null(), EncryptionMode::Identity),
        ];

        for (object, handle, expected) in cases {
            assert_eq!(
                interpret_cf(&encryption.crypt_filters, Some(&object)),
                expected
            );
            assert_eq!(
                interpret_cf_from_handle(&encryption, &handle).unwrap(),
                expected
            );
        }

        let builtin_identity = explicit_rc4_encryption_state();
        assert_eq!(
            interpret_cf_from_handle(&builtin_identity, &ObjectHandle::name(b"Identity".to_vec()),)
                .unwrap(),
            EncryptionMode::Identity
        );
    }

    #[test]
    fn interpret_cf_from_handle_lazily_resolves_an_indirect_name() {
        let encryption = explicit_rc4_encryption_state();
        let (handle, _resolver) = crate::object_handle::identity_tests::resolver_bearing_handle(
            ObjectValue::Name(b"StdCF".to_vec()),
        );

        assert!(!handle.is_resolved());
        assert_eq!(
            interpret_cf_from_handle(&encryption, &handle).unwrap(),
            EncryptionMode::Rc4
        );
        assert!(handle.is_resolved());
    }

    #[test]
    fn interpret_cf_from_handle_propagates_resolution_failures() {
        let encryption = explicit_rc4_encryption_state();

        let (dropped, resolver) = crate::object_handle::identity_tests::resolver_bearing_handle(
            ObjectValue::Name(b"StdCF".to_vec()),
        );
        drop(resolver);
        assert_eq!(
            interpret_cf_from_handle(&encryption, &dropped)
                .unwrap_err()
                .to_string(),
            "object 20 0 belongs to a dropped PDF"
        );

        let (failing, _resolver) =
            crate::object_handle::identity_tests::error_resolving_handle(ObjectRef::new(21, 0));
        assert_eq!(
            interpret_cf_from_handle(&encryption, &failing)
                .unwrap_err()
                .to_string(),
            "resolver failed"
        );
    }

    /// qpdf enters the crypt-filter switch only for `/V >= 4` (`:982-983`,
    /// `:1062-1063`), leaving `use_aes` false — so a pre-`/V 4` document is
    /// RC4 no matter what the crypt filter fields hold.
    #[test]
    fn select_method_is_rc4_below_v4_whatever_the_crypt_filter_says() {
        for method in [
            EncryptionMode::Identity,
            EncryptionMode::Rc4,
            EncryptionMode::Aes128,
            EncryptionMode::Aes256,
            EncryptionMode::Unknown,
        ] {
            let mut cf = method;
            assert_eq!(
                EncryptionState::select_method(method, &mut cf, 2),
                (Some(false), false),
                "{method:?}"
            );
            assert_eq!(cf, method, "the pre-/V 4 path must not rewrite the filter");
        }
    }

    /// The `e_none` arm returns without decrypting; the two AES arms and the
    /// RC4 arm map onto `use_aes` (`:1105-1120`).
    #[test]
    fn select_method_maps_each_known_filter_onto_use_aes() {
        let cases = [
            (EncryptionMode::Identity, None),
            (EncryptionMode::Rc4, Some(false)),
            (EncryptionMode::Aes128, Some(true)),
            (EncryptionMode::Aes256, Some(true)),
        ];
        for (method, expected) in cases {
            let mut cf = method;
            assert_eq!(
                EncryptionState::select_method(method, &mut cf, 4),
                (expected, false),
                "{method:?}"
            );
            assert_eq!(cf, method, "only the unknown arm rewrites the filter");
        }
    }

    /// `compute_data_key`'s `/V >= 5` shortcut (`:337-340`): the file key is
    /// used straight, so the object and generation numbers do not enter it.
    #[test]
    fn compute_data_key_uses_the_file_key_straight_from_v5() {
        let encryption = EncryptionState {
            file_key: (0..32).collect(),
            encryption_v: 5,
            encryption_r: 6,
            ..explicit_rc4_encryption_state()
        };

        for use_aes in [false, true] {
            assert_eq!(
                encryption.compute_data_key(ObjectRef::new(9, 3), use_aes),
                encryption.file_key
            );
        }
        assert_eq!(
            encryption.compute_data_key(ObjectRef::new(1, 0), true),
            encryption.compute_data_key(ObjectRef::new(4242, 7), true)
        );
    }

    /// Algorithm 3.1 below `/V 5` (`:342-356`): the low three bytes of the
    /// object number, the low two of the generation, then `"sAlT"` for AES,
    /// hashed and truncated to `min(len, 16)`.
    #[test]
    fn compute_data_key_follows_algorithm_3_1_below_v5() {
        let encryption = EncryptionState {
            file_key: (0..16).collect(),
            encryption_v: 4,
            encryption_r: 4,
            ..explicit_rc4_encryption_state()
        };
        let og = ObjectRef::new(0x03_0201, 0x0605);

        let expect = |use_aes: bool| {
            let mut input: Vec<u8> = (0..16).collect();
            input.extend_from_slice(&[0x01, 0x02, 0x03, 0x05, 0x06]);
            if use_aes {
                input.extend_from_slice(b"sAlT");
            }
            let digest = crate::security::primitives::md5(&input);
            digest[..input.len().min(16)].to_vec()
        };

        assert_eq!(encryption.compute_data_key(og, false), expect(false));
        assert_eq!(encryption.compute_data_key(og, true), expect(true));
        assert_ne!(
            encryption.compute_data_key(og, false),
            encryption.compute_data_key(og, true),
            "the AES salt must change the key"
        );
    }

    /// qpdf caches the object key on the object/generation pair alone, leaving
    /// `use_aes` out of the key (`:962`). A document whose `/StrF` and `/StmF`
    /// disagree about AES therefore reuses whichever key was derived first.
    #[test]
    fn key_for_object_caches_on_the_object_alone_and_ignores_use_aes() {
        let mut encryption = EncryptionState {
            file_key: (0..16).collect(),
            encryption_v: 4,
            encryption_r: 4,
            ..explicit_rc4_encryption_state()
        };
        let og = ObjectRef::new(3, 0);

        let rc4_key = encryption.key_for_object(og, false).to_vec();
        assert_eq!(rc4_key, encryption.compute_data_key(og, false));

        // Same object, now asking for AES: qpdf returns the cached RC4-derived
        // key rather than re-deriving with the salt.
        assert_eq!(encryption.key_for_object(og, true), rc4_key);
        assert_ne!(rc4_key, encryption.compute_data_key(og, true));

        // A different object misses the cache and derives afresh.
        let other = ObjectRef::new(4, 0);
        let expected = encryption.compute_data_key(other, true);
        assert_eq!(encryption.key_for_object(other, true), expected);
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
            user_password_matched: true,
            owner_password_matched: false,
            cached_object_encryption_key: Vec::new(),
            cached_key_og: None,
        }
    }

    fn rc4_ciphertext(
        object_ref: ObjectRef,
        plaintext: &[u8],
        encryption: &EncryptionState,
    ) -> Vec<u8> {
        let mut encryption = encryption.clone();
        let mut ciphertext = plaintext.to_vec();
        decrypt_stream_bytes(object_ref, &mut ciphertext, false, &mut encryption)
            .expect("RC4 encryption");
        ciphertext
    }

    #[test]
    fn production_explicit_crypt_keeps_indirect_decode_params() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let decode_params_ref = ObjectRef::new(5, 0);
        let plaintext = b"native prefix";
        let encryption = explicit_rc4_encryption_state();
        let mut predictor_input = vec![0];
        predictor_input.extend_from_slice(plaintext);
        let encoded = rc4_ciphertext(
            ObjectRef::new(4, 0),
            &flate_encoded(&predictor_input),
            &encryption,
        );

        let mut decode_params = Dictionary::new();
        decode_params.insert("Predictor", Object::Integer(12));
        decode_params.insert("Columns", Object::Integer(plaintext.len() as i64));
        decode_params.insert("Colors", Object::Integer(1));
        decode_params.insert("BitsPerComponent", Object::Integer(8));
        pdf.set_object(decode_params_ref, Object::Dictionary(decode_params));

        let mut stream_dict = Dictionary::new();
        stream_dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"Crypt".to_vec()),
            ]),
        );
        stream_dict.insert(
            "DecodeParms",
            Object::Array(vec![
                Object::Reference(decode_params_ref),
                crypt_params(b"StdCF"),
            ]),
        );
        *pdf.encryption.borrow_mut() = Some(encryption);

        let (object, transformed) = pdf
            .decrypt_resolved_object(
                ObjectRef::new(4, 0),
                Object::Stream(Stream::new(stream_dict, encoded)),
                None,
            )
            .expect("production explicit Crypt route");
        assert!(transformed);
        let Object::Stream(stream) = object else {
            panic!("fixture must remain a stream"); // cov:ignore: constructed as a stream above
        };
        let stream_handle = pdf
            .filter_dictionary_to_handle(&stream.dict, 0)
            .expect("lift remaining filter parameters");
        let decoded = crate::filters::decode_stream_data_from_handle(
            &stream_handle,
            &stream.data,
            crate::filters::DecodeLimits::default(),
        )
        .expect("decode rewritten stream through native params");
        assert_eq!(decoded, plaintext);
    }

    #[test]
    fn source_backed_explicit_crypt_keeps_decode_params_without_reentry() {
        let decode_params_ref = ObjectRef::new(5, 0);
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
                b"4 0 obj\nnull\nendobj\n",
                b"5 0 obj\n<< /Predictor 12 /Columns 13 /Colors 1 /BitsPerComponent 8 >>\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open source-backed fixture");
        let plaintext = b"native prefix";
        let encryption = explicit_rc4_encryption_state();
        let mut predictor_input = vec![0];
        predictor_input.extend_from_slice(plaintext);
        let encoded = rc4_ciphertext(
            ObjectRef::new(4, 0),
            &flate_encoded(&predictor_input),
            &encryption,
        );

        let mut stream_dict = Dictionary::new();
        stream_dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"Crypt".to_vec()),
            ]),
        );
        stream_dict.insert(
            "DecodeParms",
            Object::Array(vec![
                Object::Reference(decode_params_ref),
                crypt_params(b"StdCF"),
            ]),
        );
        *pdf.encryption.borrow_mut() = Some(encryption);

        let (object, transformed) = pdf
            .decrypt_resolved_object(
                ObjectRef::new(4, 0),
                Object::Stream(Stream::new(stream_dict, encoded)),
                None,
            )
            .expect("source-backed DecodeParms must not reenter encryption state");
        assert!(transformed);
        let Object::Stream(stream) = object else {
            panic!("fixture must remain a stream"); // cov:ignore: constructed as a stream above
        };
        let stream_handle = pdf
            .filter_dictionary_to_handle(&stream.dict, 0)
            .expect("lift remaining source-backed filter parameters");
        let decoded = crate::filters::decode_stream_data_from_handle(
            &stream_handle,
            &stream.data,
            crate::filters::DecodeLimits::default(),
        )
        .expect("decode rewritten source-backed stream through native params");
        assert_eq!(decoded, plaintext);
    }

    /// Compare the canonical source-backed indirect-`/DecodeParms` route with
    /// qpdf 11.9.0 on a real encrypted PDF. The stream stores
    /// `RC4(FlateDecode(plaintext))`, so qpdf must resolve object 5 while
    /// building its filtered pipeline and flpdf must select the same
    /// source-backed handles before decrypting the stream.
    #[test]
    fn source_backed_indirect_decode_params_match_pinned_qpdf_bytes() {
        let version = match Command::new("/usr/bin/qpdf").arg("--version").output() {
            Ok(version) => version,
            // cov:ignore-start: CI provides the pinned qpdf binary; this is a developer-host skip
            Err(error) => {
                eprintln!(
                    "/usr/bin/qpdf unavailable; skipping indirect DecodeParms oracle: {error}"
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
                "expected /usr/bin/qpdf 11.9.0; skipping indirect DecodeParms oracle: {first_line}"
            );
            return;
            // cov:ignore-end
        }

        use crate::encrypt_setup::{EncryptMethod, EncryptParams};
        use crate::security::standard::{
            build_v4_encrypt_dict, encrypt_cipher_bytes, per_object_key, ObjectKeyAlg,
            StringEncryptCipher, V4CryptMethod, V4EncryptParams,
        };

        const PASSWORD: &[u8] = b"user-pw";
        const OWNER_PASSWORD: &[u8] = b"owner-pw";
        const ID0: [u8; 16] = [0x42; 16];
        const PLAINTEXT: &[u8] = b"qpdf indirect DecodeParms oracle payload";
        let encryption_params = EncryptParams::rc4(
            EncryptMethod::V4Rc4128,
            PASSWORD.to_vec(),
            OWNER_PASSWORD.to_vec(),
        );
        let (encrypt_dict, file_key) = build_v4_encrypt_dict(&V4EncryptParams {
            method: V4CryptMethod::Rc4,
            user_password: PASSWORD,
            owner_password: OWNER_PASSWORD,
            p: encryption_params.permissions.to_p_bits(),
            id0: &ID0,
            encrypt_metadata: true,
        })
        .expect("build qpdf-compatible V=4 encryption dictionary");
        let object_key = per_object_key(&file_key, 4, 0, ObjectKeyAlg::Rc4);
        let mut predictor_input = vec![0];
        predictor_input.extend_from_slice(PLAINTEXT);
        let mut encoded = flate_encoded(&predictor_input);
        encrypt_cipher_bytes(
            &mut encoded,
            StringEncryptCipher::Rc4 { key: &object_key },
            &[0; 16],
        )
        .expect("encrypt explicit Crypt payload");

        let mut encrypt_wire = Vec::new();
        crate::writer::encrypted_strings::write_encryption_dictionary(
            &mut encrypt_wire,
            &encrypt_dict,
        );
        let mut bodies = Vec::new();
        bodies.push(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec());
        bodies.push(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec());
        bodies.push(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n"
                .to_vec(),
        );
        let mut stream = format!(
            "4 0 obj\n<< /Length {} /Filter [/FlateDecode /Crypt] /DecodeParms [5 0 R << /Type /CryptFilterDecodeParms /Name /StdCF >>] >>\nstream\n",
            encoded.len()
        )
        .into_bytes();
        stream.extend_from_slice(&encoded);
        stream.extend_from_slice(b"\nendstream\nendobj\n");
        bodies.push(stream);
        bodies.push(
            format!(
                "5 0 obj\n<< /Predictor 12 /Columns {} /Colors 1 /BitsPerComponent 8 >>\nendobj\n",
                PLAINTEXT.len()
            )
            .into_bytes(),
        );
        bodies.push({
            let mut body = b"6 0 obj\n".to_vec();
            body.extend_from_slice(&encrypt_wire);
            body.extend_from_slice(b"\nendobj\n");
            body
        });

        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(bodies.len());
        for body in &bodies {
            offsets.push(bytes.len());
            bytes.extend_from_slice(body);
        }
        let xref_start = bytes.len();
        bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        let id_hex = ID0
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size 7 /Root 1 0 R /Encrypt 6 0 R /ID [<{id_hex}><{id_hex}>] >>\nstartxref\n{xref_start}\n%%EOF\n"
            )
            .as_bytes(),
        );

        let directory = tempfile::tempdir().expect("temporary qpdf oracle directory");
        let path = directory.path().join("indirect-decode-parms.pdf");
        std::fs::write(&path, &bytes).expect("write qpdf oracle fixture");
        let qpdf = Command::new("/usr/bin/qpdf")
            .arg("--password=user-pw")
            .arg("--show-object=4")
            .arg("--filtered-stream-data")
            .arg(&path)
            .output()
            .expect("run pinned qpdf indirect DecodeParms probe");
        let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
        assert!(
            qpdf.status.success(),
            "qpdf indirect DecodeParms probe failed:\n{qpdf_stderr}"
        );

        let mut pdf = Pdf::open_with_options(
            Cursor::new(bytes),
            crate::PdfOpenOptions {
                password: PASSWORD.to_vec(),
                allow_weak_crypto: true,
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("open qpdf oracle fixture with flpdf");
        let stream = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve_object_handle(&stream)
            .expect("resolve source-backed encrypted stream");
        let flpdf = stream
            .get_stream_data(crate::writer::DecodeLevel::All)
            .expect("decode source-backed encrypted stream with flpdf")
            .as_ref()
            .clone();
        assert_eq!(qpdf.stdout, PLAINTEXT, "qpdf oracle plaintext");
        assert_eq!(flpdf, qpdf.stdout, "flpdf and qpdf stream bytes");
    }

    #[test]
    fn filter_prefix_lift_handles_legacy_kinds_and_depth_limits() {
        let pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let mut nested = Dictionary::new();
        nested.insert("Value", Object::Integer(7));

        let mut dictionary = Dictionary::new();
        dictionary.insert("Null", Object::Null);
        dictionary.insert("Boolean", Object::Boolean(true));
        dictionary.insert("Integer", Object::Integer(7));
        dictionary.insert("Real", Object::Real(1.5));
        dictionary.insert(
            "RealLiteral",
            Object::RealLiteral {
                value: 0.4,
                literal: b".4".to_vec(),
            },
        );
        dictionary.insert("Name", Object::Name(b"FlateDecode".to_vec()));
        dictionary.insert("String", Object::String(b"params".to_vec()));
        dictionary.insert(
            "Array",
            Object::Array(vec![Object::Boolean(false), Object::Integer(2)]),
        );
        dictionary.insert("Dictionary", Object::Dictionary(nested));
        dictionary.insert(
            "Stream",
            Object::Stream(Stream::new(Dictionary::new(), b"payload".to_vec())),
        );
        let referenced = ObjectRef::new(5, 0);
        dictionary.insert("Reference", Object::Reference(referenced));

        let handle = pdf
            .filter_dictionary_to_handle(&dictionary, 0)
            .expect("all ordinary PDF object kinds lift into handles");
        assert_eq!(
            handle
                .try_get_key(b"/Reference")
                .expect("reference child")
                .object_ref(),
            Some(referenced)
        );
        assert!(handle
            .try_get_key(b"/Dictionary")
            .expect("dictionary child")
            .as_dictionary()
            .is_some());
        assert!(handle
            .try_get_key(b"/Stream")
            .expect("stream child")
            .as_stream_dict()
            .is_some());

        assert!(pdf
            .filter_dictionary_to_handle(&Dictionary::new(), crate::parser::MAX_PARSE_DEPTH + 1)
            .is_err());
        assert!(pdf
            .filter_object_to_handle(&Object::Null, crate::parser::MAX_PARSE_DEPTH + 1)
            .is_err());
        assert!(pdf
            .filter_object_to_handle(&Object::Operator(b"q".to_vec()), 0)
            .is_err());
        assert!(pdf
            .filter_object_to_handle(&Object::InlineImage(b"BI".to_vec()), 0)
            .is_err());
    }

    #[test]
    fn cached_stream_length_handles_follow_qpdf_classification_and_sync() {
        let pending = parse_file_object_syntax(
            b"1 0 obj\n<< /Length 99 0 R >>\nstream\nabc\nendstream\nendobj\n",
        )
        .expect("pending stream");
        let target = ObjectRef::new(1, 0);
        let holder = ObjectRef::new(99, 0);

        let mut null_pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let null_handle = null_pdf.get_object_handle(holder);
        null_handle.set_resolved(ObjectValue::Null);
        assert!(null_handle.is_resolved());
        assert!(null_handle.is_null());
        assert!(null_pdf.cache.entry(holder).is_none());
        assert_eq!(
            null_pdf
                .resolve_pending_stream_length(target, &pending, 0)
                .expect("cached null holder"),
            Some(ResolvedStreamLength::Missing)
        );

        let mut invalid_pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        invalid_pdf
            .get_object_handle(holder)
            .set_resolved(ObjectValue::Name(b"not-an-integer".to_vec()));
        assert_eq!(
            invalid_pdf
                .resolve_pending_stream_length(target, &pending, 0)
                .expect("cached invalid holder"),
            Some(ResolvedStreamLength::Invalid)
        );

        let mut sync_pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let length_handle = sync_pdf.get_object_handle(holder);
        sync_pdf.cache.set_resolved(holder, Object::Integer(3));
        assert!(!length_handle.is_resolved());
        assert_eq!(
            sync_pdf
                .resolve_pending_stream_length(target, &pending, 0)
                .expect("legacy cache holder"),
            Some(ResolvedStreamLength::Integer(3))
        );
        assert!(length_handle.is_resolved());
    }

    #[test]
    fn rc4_stream_decryption_preserves_payload_allocation() {
        let object_ref = ObjectRef::new(7, 0);
        let mut encryption = explicit_rc4_encryption_state();
        let plaintext = vec![0x42; crate::pipeline::rc4::DEFAULT_OUT_BUFFER_SIZE + 17];
        let mut bytes = plaintext.clone();
        let original_ptr = bytes.as_ptr();
        let original_capacity = bytes.capacity();

        decrypt_stream_bytes(object_ref, &mut bytes, false, &mut encryption)
            .expect("RC4 transform");
        assert_eq!(bytes.as_ptr(), original_ptr);
        assert_eq!(bytes.capacity(), original_capacity);
        assert_ne!(bytes, plaintext);

        decrypt_stream_bytes(object_ref, &mut bytes, false, &mut encryption)
            .expect("RC4 inverse transform");
        assert_eq!(bytes.as_ptr(), original_ptr);
        assert_eq!(bytes, plaintext);
    }

    #[test]
    fn a_string_free_legacy_object_does_not_prime_qpdfs_key_cache() {
        let object_ref = ObjectRef::new(7, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        encryption.file_key = (0..16).collect();
        encryption.cf_stream = EncryptionMode::Aes128;
        let mut oracle_state = encryption.clone();
        let key: [u8; 16] = oracle_state
            .key_for_object(object_ref, true)
            .try_into()
            .expect("V4 AES object key");
        let plaintext = b"string-free stream uses its own method first";
        let mut ciphertext = plaintext.to_vec();
        crate::security::standard::encrypt_cipher_bytes(
            &mut ciphertext,
            crate::security::standard::StringEncryptCipher::Aes128 { key: &key },
            &[0x5a; 16],
        )
        .expect("build AES ciphertext");
        let mut object = Object::Stream(Stream::new(Dictionary::new(), Vec::new()));

        decrypt_object_strings(object_ref, &mut object, &mut encryption)
            .expect("walk string-free stream dictionary");

        assert_eq!(encryption.cached_key_og, None);
        assert!(encryption.cached_object_encryption_key.is_empty());
        decrypt_stream_bytes(object_ref, &mut ciphertext, true, &mut encryption)
            .expect("stream method primes and uses qpdf's cache");
        assert_eq!(ciphertext, plaintext);
    }

    #[test]
    fn a_string_free_object_value_does_not_prime_qpdfs_key_cache() {
        let object_ref = ObjectRef::new(7, 0); // cov:ignore: llvm test-prologue mapping artifact; the test body runs
        let mut encryption = explicit_rc4_string_encryption_state();
        encryption.cf_stream = EncryptionMode::Aes128;
        let mut value = ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![]),
            stream_data: Some(Rc::new(Vec::new())),
            stream_length: 0,
            stream_provider: None,
        };

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("walk string-free stream dictionary");

        assert_eq!(encryption.cached_key_og, None);
        assert!(encryption.cached_object_encryption_key.is_empty());
    }

    #[test]
    fn legacy_identity_string_does_not_prime_qpdfs_key_cache() {
        let object_ref = ObjectRef::new(7, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        encryption.cf_string = EncryptionMode::Identity;
        let mut object = Object::String(b"identity string".to_vec());

        let warn = decrypt_object_strings(object_ref, &mut object, &mut encryption)
            .expect("Identity is a no-op");

        assert!(!warn);
        assert_eq!(object.as_string(), Some(b"identity string".as_slice()));
        assert_eq!(encryption.cached_key_og, None);
    }

    #[test]
    fn legacy_string_scan_rejects_excess_inline_nesting() {
        let object_ref = ObjectRef::new(7, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let mut object = Object::String(b"leaf".to_vec());
        for _ in 0..=crate::object::MAX_INLINE_DEPTH {
            object = Object::Array(vec![object]);
        }

        let error = decrypt_object_strings(object_ref, &mut object, &mut encryption)
            .expect_err("excess inline nesting must error before key derivation");
        assert!(
            matches!(error, Error::Unsupported(ref message) if message.contains("inline object nesting exceeds maximum")), // cov:ignore: llvm maps this executed matches predicate as zero
            "got {error:?}"
        );
        assert_eq!(encryption.cached_key_og, None);
    }

    fn explicit_rc4_string_encryption_state() -> EncryptionState {
        EncryptionState {
            cf_string: EncryptionMode::Rc4,
            ..explicit_rc4_encryption_state()
        }
    }

    #[test]
    fn decrypt_object_value_strings_rc4_top_level_string() {
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let mut value =
            ObjectValue::String(rc4_ciphertext(object_ref, b"TopSecretTitle", &encryption));

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("RC4 top-level string decryption");

        let ObjectValue::String(bytes) = &value else {
            panic!("value must still be a string"); // cov:ignore: unreachable given this test's own construction of value
        };
        assert_eq!(bytes.as_slice(), b"TopSecretTitle");
    }

    // This catches a production regression where the parser callback treats
    // qpdf's `/StrF /Identity` as an RC4/AES method. Replacing the `None`
    // method branch with a cipher call changes these bytes.
    #[test]
    fn decrypt_object_string_leaves_identity_filter_bytes_unchanged() {
        let mut encryption = explicit_rc4_encryption_state();
        let mut bytes = b"identity string bytes".to_vec();

        let warn = encryption
            .decrypt_object_string(ObjectRef::new(3, 0), &mut bytes)
            .expect("Identity string method is a no-op");

        assert_eq!(bytes, b"identity string bytes");
        assert!(!warn);
    }

    #[test]
    fn decrypt_object_value_strings_skips_the_encrypt_dictionary_object() {
        let object_ref = ObjectRef::new(9, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        encryption.encrypt_ref = Some(object_ref);
        let ciphertext = rc4_ciphertext(object_ref, b"TopSecretTitle", &encryption);
        let mut value = ObjectValue::String(ciphertext.clone());

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("the /Encrypt object's own strings are exempt, not an error");

        let ObjectValue::String(bytes) = &value else {
            panic!("value must still be a string"); // cov:ignore: unreachable given this test's own construction of value
        };
        assert_eq!(
            bytes.as_slice(),
            ciphertext.as_slice(),
            "the /Encrypt dictionary's own strings must never be decrypted, \
             mirroring decrypt_object_strings's identical encrypt_ref guard"
        );
    }

    #[test]
    fn decrypt_object_value_strings_decrypts_a_string_inside_a_direct_array() {
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let ciphertext = rc4_ciphertext(object_ref, b"TopSecretTitle", &encryption);
        let mut value = ObjectValue::Array(vec![ObjectHandle::string(ciphertext)]);

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("array-contained string decryption");

        let ObjectValue::Array(items) = &value else {
            panic!("value must still be an array"); // cov:ignore: unreachable given this test's own construction of value
        };
        assert_eq!(
            items[0].as_string().as_deref(),
            Some(b"TopSecretTitle".as_slice())
        );
    }

    #[test]
    fn decrypt_object_value_strings_decrypts_a_string_inside_a_nested_dictionary() {
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let ciphertext = rc4_ciphertext(object_ref, b"TopSecretTitle", &encryption);
        let inner =
            ObjectHandle::dictionary(vec![(b"Title".to_vec(), ObjectHandle::string(ciphertext))]);
        let mut value = ObjectValue::Dictionary(BTreeMap::from([(b"/Nested".to_vec(), inner)]));

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("nested-dictionary string decryption");

        let ObjectValue::Dictionary(entries) = &value else {
            panic!("value must still be a dictionary"); // cov:ignore: unreachable given this test's own construction of value
        };
        let nested = entries.get(b"/Nested".as_slice()).expect("Nested entry");
        let nested_dict = nested.as_dictionary().expect("Nested must be a dictionary");
        let title = nested_dict.get(b"/Title".as_slice()).expect("Title entry");
        assert_eq!(
            title.as_string().as_deref(),
            Some(b"TopSecretTitle".as_slice())
        );
    }

    #[test]
    fn decrypt_object_value_strings_decrypts_a_string_inside_a_stream_dictionary() {
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let ciphertext = rc4_ciphertext(object_ref, b"TopSecretTitle", &encryption);
        let dict =
            ObjectHandle::dictionary(vec![(b"Title".to_vec(), ObjectHandle::string(ciphertext))]);
        let mut value = ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(
                b"stream payload, untouched by string decryption".to_vec(),
            )),
            stream_length: 0,
            stream_provider: None,
        };

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("stream-dictionary string decryption");

        let ObjectValue::Stream {
            stream_dict,
            stream_data,
            ..
        } = &value
        else {
            panic!("value must still be a stream"); // cov:ignore: unreachable given this test's own construction of value
        };
        let stream_dict = stream_dict.as_dictionary().expect("stream dict");
        let title = stream_dict.get(b"/Title".as_slice()).expect("Title entry");
        assert_eq!(
            title.as_string().as_deref(),
            Some(b"TopSecretTitle".as_slice())
        );
        assert_eq!(
            stream_data
                .as_ref()
                .expect("direct stream retains its data")
                .as_slice(),
            b"stream payload, untouched by string decryption",
            "stream payload bytes are never touched by string decryption"
        );
    }

    #[test]
    fn decrypt_handle_strings_in_place_decrypts_a_string_inside_a_directly_nested_stream() {
        // Distinct from the top-level-stream test above: here the stream is
        // a *child* reached through `decrypt_handle_strings_in_place`'s own
        // `as_stream_dict()` arm, not `decrypt_strings_in_object_value`'s
        // top-level `Stream` arm. A direct (non-indirect) `ObjectHandle`
        // wrapping a stream value is not the common case -- qpdf's own
        // streams are always indirect -- but it is reachable through the
        // public API the same way `object_handle.rs`'s own
        // `a_direct_stream_value_unparse_resolved_inlines_rather_than_referencing`
        // test documents: a nested `Object::Stream` passed to
        // `Pdf::set_object` inside an array or dictionary.
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let ciphertext = rc4_ciphertext(object_ref, b"TopSecretTitle", &encryption);
        let stream_dict =
            ObjectHandle::dictionary(vec![(b"Title".to_vec(), ObjectHandle::string(ciphertext))]);
        let stream_child = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict,
            stream_data: Some(Rc::new(b"payload".to_vec())),
            stream_length: 0,
            stream_provider: None,
        });
        let mut value = ObjectValue::Array(vec![stream_child]);

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("directly nested stream string decryption");

        let ObjectValue::Array(items) = &value else {
            panic!("value must still be an array"); // cov:ignore: unreachable given this test's own construction of value
        };
        let nested_dict = items[0]
            .as_stream_dict()
            .expect("array item must still be a stream")
            .as_dictionary()
            .expect("stream dict");
        let title = nested_dict.get(b"/Title".as_slice()).expect("Title entry");
        assert_eq!(
            title.as_string().as_deref(),
            Some(b"TopSecretTitle".as_slice())
        );
    }

    #[test]
    fn decrypt_object_value_strings_leaves_a_top_level_scalar_untouched() {
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let mut value = ObjectValue::Integer(42);

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("a bare scalar object has no string to decrypt");

        assert!(matches!(value, ObjectValue::Integer(42)));
    }

    #[test]
    fn decrypt_handle_strings_in_place_never_touches_an_indirect_childs_own_value() {
        // An indirect child is a terminal leaf: its own eventual resolution
        // decrypts it separately, keyed by its own object ref. Using the
        // *parent's* key here would corrupt it -- confirmed by resolving the
        // child to a string that would decrypt to something else entirely
        // under the parent's key, then asserting it is untouched.
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let child = ObjectHandle::new_indirect_unresolved(ObjectRef::new(4, 0), 0);
        child.set_resolved(ObjectValue::String(b"unrelated ciphertext".to_vec()));
        let mut value = ObjectValue::Dictionary(BTreeMap::from([(b"Ref".to_vec(), child.clone())]));

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("indirect children are skipped, not an error");

        assert_eq!(
            child.as_string().as_deref(),
            Some(b"unrelated ciphertext".as_slice()),
            "an indirect child's own resolved value must be left exactly as-is"
        );
    }

    #[test]
    fn decrypt_handle_strings_in_place_rejects_excess_direct_nesting() {
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let mut handle = ObjectHandle::string(b"leaf".to_vec());
        for _ in 0..=crate::object::MAX_INLINE_DEPTH {
            handle = ObjectHandle::array(vec![handle]);
        }
        let mut value = ObjectValue::Array(vec![handle]);

        let err = decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect_err("excess direct nesting must error, not overflow the stack");
        assert!(
            matches!(err, Error::Unsupported(ref m) if m.contains("inline object nesting exceeds maximum")),
            "got {err:?}"
        );
    }

    #[test]
    fn decrypt_object_value_strings_does_not_charge_the_stream_dictionary_container_its_own_inline_level(
    ) {
        // Codex Review on PR #603 (discussion_r3690827731): a stream's own
        // dictionary handle must not be charged its own inline-nesting level
        // before its entries are visited -- decrypt_strings_in_value's
        // legacy Object::Stream arm visits stream.dict.values_mut() at the
        // *same* depth+1 the stream itself was reached at, so a document the
        // legacy path (resolve_to_cache) accepts must not be rejected here
        // just because the canonical handle parser uses this walk instead.
        // Pin the exact boundary both paths must agree on: a stream
        // dictionary entry nested MAX_INLINE_DEPTH levels deep (accepted --
        // matches decrypt_strings_in_value's own depth+1-at-entries
        // accounting) versus MAX_INLINE_DEPTH+1 (rejected, one level over).
        let object_ref = ObjectRef::new(3, 0);
        let mut encryption = explicit_rc4_string_encryption_state();
        let ciphertext = rc4_ciphertext(object_ref, b"TopSecretTitle", &encryption);

        let nest = |extra_levels: usize| {
            let mut handle = ObjectHandle::string(ciphertext.clone());
            for _ in 0..extra_levels {
                handle = ObjectHandle::array(vec![handle]);
            }
            handle
        };

        let mut accepted = ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(
                b"Deep".to_vec(),
                nest(crate::object::MAX_INLINE_DEPTH - 1),
            )]),
            stream_data: Some(Rc::new(Vec::new())),
            stream_length: 0,
            stream_provider: None,
        };
        decrypt_object_value_strings(object_ref, &mut accepted, &mut encryption).expect(
            "a stream dictionary entry nested exactly MAX_INLINE_DEPTH levels deep, matching \
             the legacy decryptor's own boundary, must be accepted",
        );

        let mut rejected = ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(
                b"TooDeep".to_vec(),
                nest(crate::object::MAX_INLINE_DEPTH),
            )]),
            stream_data: Some(Rc::new(Vec::new())),
            stream_length: 0,
            stream_provider: None,
        };
        let err = decrypt_object_value_strings(object_ref, &mut rejected, &mut encryption)
            .expect_err("one level past the legacy decryptor's own boundary must still error");
        assert!(
            matches!(err, Error::Unsupported(ref m) if m.contains("inline object nesting exceeds maximum")),
            "got {err:?}"
        );
    }

    fn flate_encoded(plaintext: &[u8]) -> Vec<u8> {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        crate::filters::encode_stream_data(&dict, plaintext).expect("Flate encode")
    }

    fn ascii_hex_encoded_without_eod(plaintext: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::with_capacity(plaintext.len() * 2);
        for byte in plaintext {
            encoded.extend_from_slice(format!("{byte:02x}").as_bytes());
        }
        encoded
    }

    fn crypt_params(name: &[u8]) -> Object {
        let mut params = Dictionary::new();
        params.insert("Name", Object::Name(name.to_vec()));
        Object::Dictionary(params)
    }

    /// qpdf's `decryptStream` warns about an unrecognised crypt method at
    /// `libqpdf/QPDF_encryption.cc:1121-1133`, before it derives the object key
    /// at `:1135` and before it prepends the decryption stage at `:1137-1149`.
    /// A failure in either later step therefore cannot suppress the warning.
    ///
    /// The failure here comes from an underivable AES object key: the state's
    /// file key is five bytes, so `compute_data_key` yields neither an AES-128
    /// nor an AES-256 key.
    #[test]
    fn explicit_crypt_filter_warning_survives_a_later_failure() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let mut encryption = explicit_rc4_encryption_state();
        // An unrecognised /Name leaves qpdf's `method` at `e_unknown`, which is
        // the arm that warns and resets `cf_stream` to AES.
        encryption.cf_stream = EncryptionMode::Unknown;
        *pdf.encryption.borrow_mut() = Some(encryption);

        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
        dict.insert("DecodeParms", crypt_params(b"NoSuchFilter"));

        let error = pdf
            .decrypt_resolved_object(
                ObjectRef::new(4, 0),
                Object::Stream(Stream::new(dict, vec![0u8; 48])),
                None,
            )
            .expect_err("a five-byte file key cannot produce an AES object key");
        assert!(matches!(error, Error::Encrypted(_)), "got {error:?}");

        let warnings: Vec<String> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| entry.message.clone())
            .collect();
        assert!(
            warnings
                .iter()
                .any(|message| message.contains("unknown encryption filter for streams")),
            "the unknown-filter warning must outlive the later failure: {warnings:?}"
        );
    }

    #[test]
    fn decrypt_resolved_object_propagates_non_encrypted_explicit_filter_errors() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        *pdf.encryption.borrow_mut() = Some(explicit_rc4_encryption_state());
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![Object::Name(b"Crypt".to_vec()); 17]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![crypt_params(b"Identity"); 17]),
        );
        let error = pdf
            .decrypt_resolved_object(
                ObjectRef::new(4, 0),
                Object::Stream(Stream::new(dict, b"identity".to_vec())),
                None,
            )
            .expect_err("an overlong filter chain must remain a hard error");
        assert!(matches!(error, Error::Unsupported(message) if message.contains("filter chain")));
    }

    fn explicit_identity_crypt_chain(chain_len: usize) -> Stream {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![Object::Name(b"Crypt".to_vec()); chain_len]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![crypt_params(b"Identity"); chain_len]),
        );
        Stream::new(dict, b"identity".to_vec())
    }

    #[test]
    fn explicit_crypt_rejects_overlong_identity_chain_before_mutation() {
        let mut encryption = explicit_rc4_encryption_state();
        let mut stream = explicit_identity_crypt_chain(17);
        let original = stream.clone();

        let err = apply_explicit_crypt_filters(
            ObjectRef::new(4, 0),
            &mut stream,
            &mut encryption,
            Some(b"\n"),
            &mut false,
        )
        .expect_err("17 explicit Crypt filters must exceed the shared decode cap");

        assert!(
            matches!(
                err,
                Error::Unsupported(ref message)
                    if message == "filter chain length 17 exceeds maximum of 16"
            ),
            "got {err:?}"
        );
        assert_eq!(
            stream, original,
            "the chain cap must reject before recovered framing or filters are mutated"
        );
    }

    #[test]
    fn explicit_crypt_accepts_max_length_identity_chain() {
        let mut encryption = explicit_rc4_encryption_state();
        let mut stream = explicit_identity_crypt_chain(16);

        apply_explicit_crypt_filters(
            ObjectRef::new(4, 0),
            &mut stream,
            &mut encryption,
            None,
            &mut false,
        )
        .expect("16 explicit Crypt filters are within the shared decode cap");

        assert_eq!(stream.data, b"identity");
        assert_eq!(stream.dict.get("Filter"), None);
        assert_eq!(stream.dict.get("DecodeParms"), None);
    }

    /// A `/Crypt` `/DecodeParms` whose `/Name` is not a name is qpdf's
    /// "Default: /Identity" in `interpretCF` (`libqpdf/QPDF_encryption.cc:712-714`):
    /// `e_none`, so the payload is left alone. It used to be an flpdf-only
    /// `Malformed` error, which would refuse to open documents qpdf reads.
    #[test]
    fn explicit_crypt_with_a_non_name_param_leaves_the_payload_alone() {
        let mut encryption = explicit_rc4_encryption_state();
        let mut malformed = Dictionary::new();
        malformed.insert("Name", Object::Integer(1));
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![Object::Name(b"Crypt".to_vec())]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Dictionary(malformed)]),
        );
        let mut stream = Stream::new(dict, b"ciphertext".to_vec());

        let mut warn = false;
        apply_explicit_crypt_filters(
            ObjectRef::new(4, 0),
            &mut stream,
            &mut encryption,
            None,
            &mut warn,
        )
        .expect("a non-name /Crypt /Name is qpdf's Identity default, not an error");

        assert!(!warn, "e_none does not warn; only e_unknown does");
        assert_eq!(stream.data, b"ciphertext");
        assert_eq!(stream.dict.get("Filter"), None);
        assert_eq!(stream.dict.get("DecodeParms"), None);
    }

    /// The `/Crypt` counterpart of the same lookup: a `/Name` with no `/CF`
    /// entry is `e_unknown` (`:710`), which reaches `decryptStream`'s
    /// `default:` arm — warn, rewrite `cf_stream` to `e_aes`, decrypt anyway
    /// (`:1121-1133`).
    #[test]
    fn explicit_crypt_with_an_unknown_filter_name_warns_and_rewrites_cf_stream() {
        let mut encryption = EncryptionState {
            // `/V 4` always carries a 128-bit key, which is what the rewritten
            // `e_aes` arm needs to derive a 16-byte object key from.
            file_key: (0..16).collect(),
            ..explicit_rc4_encryption_state()
        };
        let mut params = Dictionary::new();
        params.insert("Name", Object::Name(b"NoSuchCF".to_vec()));
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
        dict.insert("DecodeParms", Object::Dictionary(params));
        // Real AES-128 ciphertext under the object key the rewritten `e_aes`
        // arm derives, so the assertion covers the decryption too, not just
        // the selection.
        let object_ref = ObjectRef::new(4, 0);
        let object_key = aes128_object_key(&encryption.compute_data_key(object_ref, true))
            .expect("V=4 AES object key");
        let mut payload = b"crypt filter payload".to_vec();
        crate::security::standard::encrypt_cipher_bytes(
            &mut payload,
            crate::security::standard::StringEncryptCipher::Aes128 { key: &object_key },
            &[0x5a; 16],
        )
        .expect("build AES ciphertext");
        let mut stream = Stream::new(dict, payload);

        let mut warn = false;
        apply_explicit_crypt_filters(object_ref, &mut stream, &mut encryption, None, &mut warn)
            .expect("unknown crypt filters decrypt as AES rather than failing");

        assert!(warn, "an unknown crypt filter must ask the caller to warn");
        assert_eq!(
            encryption.cf_stream,
            EncryptionMode::Aes128,
            "qpdf resets cf_stream so the warning is not repeated"
        );
        assert_eq!(stream.data, b"crypt filter payload");

        // The rewrite is what suppresses the second warning: the same state
        // now takes the `e_aes` arm silently.
        let (use_aes, warn_again) = encryption.stream_method(None);
        assert_eq!(use_aes, Some(true));
        assert!(!warn_again, "cf_stream was reset, so qpdf warns only once");
    }

    #[test]
    fn explicit_named_crypt_removes_recovered_framing_before_rc4() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = explicit_rc4_encryption_state();
        let plaintext = b"named explicit crypt";
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
        dict.insert("DecodeParms", crypt_params(b"StdCF"));
        let mut stream = Stream::new(dict, rc4_ciphertext(object_ref, plaintext, &encryption));

        apply_explicit_crypt_filters(
            object_ref,
            &mut stream,
            &mut encryption,
            Some(b"\n"),
            &mut false,
        )
        .expect("remove named Crypt");

        assert_eq!(stream.data, plaintext);
        assert_eq!(stream.dict.get("Filter"), None);
        assert_eq!(stream.dict.get("DecodeParms"), None);
    }

    #[test]
    fn explicit_crypt_first_removes_recovered_framing_before_rc4() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = explicit_rc4_encryption_state();
        let plaintext = b"array explicit crypt";
        let compressed = flate_encoded(plaintext);
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![crypt_params(b"StdCF"), Object::Null]),
        );
        let mut stream = Stream::new(dict, rc4_ciphertext(object_ref, &compressed, &encryption));

        apply_explicit_crypt_filters(
            object_ref,
            &mut stream,
            &mut encryption,
            Some(b"\n"),
            &mut false,
        )
        .expect("remove first Crypt");

        assert_eq!(
            crate::filters::decode_stream_data(&stream.dict, &stream.data)
                .expect("decode remaining Flate"),
            plaintext
        );
        assert_eq!(
            stream.dict.get("Filter"),
            Some(&Object::Array(vec![Object::Name(b"FlateDecode".to_vec())]))
        );
        assert_eq!(
            stream.dict.get("DecodeParms"),
            Some(&Object::Array(vec![Object::Null]))
        );
    }

    #[test]
    fn explicit_crypt_decrypts_before_ascii_hex_regardless_of_filter_slot() {
        let object_ref = ObjectRef::new(4, 0);
        let mut encryption = explicit_rc4_encryption_state();
        let plaintext = b"crypt before ascii hex";
        let encoded = ascii_hex_encoded_without_eod(plaintext);
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCIIHexDecode".to_vec()),
                Object::Name(b"Crypt".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Null, crypt_params(b"StdCF")]),
        );
        let mut stream = Stream::new(dict, rc4_ciphertext(object_ref, &encoded, &encryption));

        apply_explicit_crypt_filters(object_ref, &mut stream, &mut encryption, None, &mut false)
            .expect("remove Crypt without re-encoding the ASCIIHex prefix");

        assert_eq!(
            crate::filters::decode_stream_data(&stream.dict, &stream.data)
                .expect("decode remaining ASCIIHex"),
            plaintext
        );
        assert_eq!(
            stream.dict.get("Filter"),
            Some(&Object::Array(vec![Object::Name(
                b"ASCIIHexDecode".to_vec()
            )]))
        );
        assert_eq!(
            stream.dict.get("DecodeParms"),
            Some(&Object::Array(vec![Object::Null]))
        );
    }

    #[test]
    fn singleton_explicit_crypt_array_removes_filter_entries() {
        let mut encryption = explicit_rc4_encryption_state();
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![Object::Name(b"Crypt".to_vec())]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![crypt_params(b"Identity")]),
        );
        let mut stream = Stream::new(dict, b"identity".to_vec());

        apply_explicit_crypt_filters(
            ObjectRef::new(4, 0),
            &mut stream,
            &mut encryption,
            None,
            &mut false,
        )
        .expect("remove singleton Crypt");

        assert_eq!(stream.data, b"identity");
        assert_eq!(stream.dict.get("Filter"), None);
        assert_eq!(stream.dict.get("DecodeParms"), None);
    }

    #[test]
    fn explicit_crypt_array_without_decode_params_keeps_remaining_filter() {
        let mut encryption = explicit_rc4_encryption_state();
        let plaintext = b"no decode params";
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"Crypt".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        let mut stream = Stream::new(dict, flate_encoded(plaintext));

        apply_explicit_crypt_filters(
            ObjectRef::new(4, 0),
            &mut stream,
            &mut encryption,
            None,
            &mut false,
        )
        .expect("remove Crypt without DecodeParms");

        assert_eq!(
            crate::filters::decode_stream_data(&stream.dict, &stream.data)
                .expect("decode remaining Flate"),
            plaintext
        );
        assert_eq!(stream.dict.get("DecodeParms"), None);
    }

    #[test]
    fn explicit_crypt_preserves_short_decode_params_array() {
        let mut encryption = explicit_rc4_encryption_state();
        let plaintext = b"short decode params";
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"Crypt".to_vec()),
            ]),
        );
        dict.insert("DecodeParms", Object::Array(vec![Object::Null]));
        let mut stream = Stream::new(dict, flate_encoded(plaintext));

        apply_explicit_crypt_filters(
            ObjectRef::new(4, 0),
            &mut stream,
            &mut encryption,
            None,
            &mut false,
        )
        .expect("remove Crypt with short DecodeParms");

        assert_eq!(
            crate::filters::decode_stream_data(&stream.dict, &stream.data)
                .expect("decode remaining Flate"),
            plaintext
        );
        assert_eq!(
            stream.dict.get("DecodeParms"),
            Some(&Object::Array(vec![Object::Null]))
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
        let value = pdf.resolve(ObjectRef::new(3, 0)).expect("resolve object 3");
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
        pdf.resolve_object_handle(&pages).expect("resolve pages");
        pdf.resolve_object_handle(&page).expect("resolve page");

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
    fn dropping_pdf_preserves_a_surviving_missing_handle() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let handle = pdf.get_object_handle(ObjectRef::new(91, 0));
        handle.set_missing();
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
        let mut root_dict = pdf
            .resolve(root_ref)
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
        let resolved_root = reopened
            .resolve(root_ref)
            .expect("resolve root in reopened output");
        let extra_ref = resolved_root
            .into_dict()
            .and_then(|d| d.get("Extra").cloned())
            .expect("root has /Extra");
        assert_eq!(extra_ref, Object::Reference(emitted_ref));
        assert_eq!(
            reopened.resolve(emitted_ref).expect("resolve new object"),
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
        pdf.resolve_object_handle(&root).expect("resolve root");
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

        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "canonical stream output must not materialize a legacy object"
        );
        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let extra_ref = reopened
            .resolve(ObjectRef::new(1, 0))
            .expect("resolve rewritten root")
            .into_dict()
            .and_then(|dict| dict.get("Extra").cloned())
            .and_then(|object| object.as_ref_id())
            .expect("rewritten root has stream reference");
        assert_eq!(extra_ref, emitted_ref);
        assert_eq!(
            reopened
                .resolve(emitted_ref)
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
        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "over-depth replacements within qpdf's parser boundary stay canonical"
        );

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
        let mut leaf = reopened
            .resolve(emitted_ref)
            .expect("resolve over-depth replacement");
        for _ in 0..depth {
            leaf = match leaf {
                Object::Array(mut items) => items.remove(0),
                other => panic!("expected nested array, found {other:?}"), // cov:ignore: successful canonical rewrite emits the constructed array at every requested depth
            };
        }
        assert_eq!(leaf, Object::Reference(emitted_target_ref));
        assert_eq!(
            reopened
                .resolve(emitted_target_ref)
                .expect("resolve nested target"),
            Object::Integer(42)
        );
    }

    #[test]
    fn canonical_replacement_survives_without_legacy_materialization() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let replacement =
            ObjectHandle::dictionary(vec![(b"Marker".to_vec(), ObjectHandle::integer(779))]);
        let page_ref = ObjectRef::new(3, 0);
        pdf.replace_object_handle(page_ref, replacement)
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

        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "canonical replacement must not populate the legacy materialization bridge"
        );
        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        assert_eq!(
            reopened
                .resolve(emitted_ref)
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
        pdf.resolve_object_handle(&pages).expect("resolve pages");
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

        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "canonical removal must not populate the legacy materialization bridge"
        );
        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let rewritten_pages = reopened
            .resolve(ObjectRef::new(2, 0))
            .expect("resolve rewritten pages")
            .into_dict()
            .expect("pages is a dictionary");
        let kids = rewritten_pages
            .get("Kids")
            .and_then(Object::as_array)
            .expect("rewritten pages has kids");
        assert_eq!(kids, &[Object::Null]);
        assert_eq!(
            reopened
                .resolve(ObjectRef::new(3, 0))
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
        pdf.resolve_object_handle(&root)
            .expect("resolve target root");
        let foreign = foreign_pdf.get_object_handle(ObjectRef::new(3, 0));
        let error = root
            .replace_key(b"/Foreign", foreign)
            .expect_err("foreign child must be rejected at the canonical mutation boundary");

        assert!(error
            .to_string()
            .contains("Attempting to add an object from a different QPDF"));
        assert!(!root.has_key(b"/Foreign"));
        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "foreign-owner rejection must happen before legacy materialization"
        );
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
    fn qpdf_json_borrowed_resolution_materializes_a_handle_only_object_on_demand() {
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
            pdf.resolve_qpdf_json_object_borrowed(object_ref)
                .expect("borrow qpdf JSON object"),
            &Object::Integer(778)
        );
        assert!(
            pdf.cache.entry(object_ref).is_none(),
            "on-demand borrowing must not populate the legacy cache"
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
        pdf.resolve_object_handle(&page).expect("resolve page");
        page.replace_key(b"/Rotate", ObjectHandle::integer(90))
            .unwrap();
        pdf.mark_object_dirty(page_ref);
        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "canonical mutation setup must not materialize a legacy snapshot"
        );

        let mut writer = crate::PdfWriter::new(&mut pdf);
        writer.set_object_stream_mode(crate::ObjectStreamMode::Disable);
        writer.set_output_memory().expect("configure memory output");
        writer.write().expect("full rewrite");
        let out = writer.get_buffer().expect("take full-rewrite output");

        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "canonical writer traversal must not populate the legacy materialization bridge"
        );

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let resolved_page = reopened
            .resolve(page_ref)
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
        pdf.resolve_object_handle(&stream)
            .expect("resolve loaded stream");

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
            .resolve_object_handle(&rewritten_stream)
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
                let stream = pdf.resolve(object_ref).expect("resolve recovered stream");
                assert_eq!(stream.as_stream().unwrap().data, b"abc");
                assert_eq!(pdf.recovered_stream_eol(object_ref), Some(expected));
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
            let stream = pdf.resolve(object_ref).expect("resolve recovered stream");
            assert_eq!(stream.as_stream().unwrap().data, b"abc");
            assert_eq!(pdf.recovered_stream_eol(object_ref), Some(&b"\n"[..]));
        }
    }

    #[test]
    fn qpdf_reader_recovers_malformed_length_holder_as_invalid_once() {
        let bytes = recovered_stream_fixture(b"/Length 2 0 R", b"\n", Some(b"2 0 obj\n<<"));
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                repair: false,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open strict malformed-holder fixture");
        let object_ref = ObjectRef::new(1, 0);
        assert_eq!(
            pdf.resolve(object_ref)
                .expect("recover target stream")
                .as_stream()
                .unwrap()
                .data,
            b"abc"
        );
        assert_eq!(pdf.recovered_stream_eol(object_ref), Some(&b"\n"[..]));
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec![
                "(object 1 0, offset 9): /Length key in stream dictionary is not an integer",
                "(object 1 0, offset 44): attempting to recover stream length",
                "(object 1 0, offset 44): recovered stream length: 4",
            ]
        );
        assert_eq!(
            pdf.resolve(object_ref).unwrap().as_stream().unwrap().data,
            b"abc"
        );
        assert_eq!(pdf.repair_diagnostics().entries().len(), 3);
    }

    #[test]
    fn qpdf_reader_reports_recoverable_name_warning_once() {
        let bytes = classic_pdf_with_bodies(&[b"1 0 obj\n/a#1x\nendobj\n"], ObjectRef::new(1, 0));
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stray-name fixture");
        let object_ref = ObjectRef::new(1, 0);

        assert_eq!(
            pdf.resolve(object_ref).expect("recover stray name"),
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
            pdf.resolve(object_ref).unwrap(),
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

        assert_eq!(
            pdf.resolve(object_ref)
                .expect("recover compressed stray name"),
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
            pdf.resolve(object_ref).unwrap(),
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
        fail_warning_delivery(&mut pdf);

        assert!(matches!(
            pdf.resolve(object_ref),
            Err(crate::Error::System(ref message)) if message == "sink write failure 1"
        ));
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
    }

    #[test]
    fn qpdf_reader_restores_target_cache_after_length_holder_io_error() {
        let bytes =
            recovered_stream_fixture(b"/Length 2 0 R", b"\n", Some(b"2 0 obj\n3\nendobj\n"));
        let mut pdf =
            Pdf::open(ReadFailingCursor::new(bytes)).expect("open readable holder fixture");
        let target = ObjectRef::new(1, 0);
        let target_offset = 9;
        let pending = parse_file_object_syntax(
            b"1 0 obj\n<< /Length 2 0 R >>\nstream\nabc\nendstream\nendobj\n",
        )
        .expect("parse pending target stream");

        pdf.resolver
            .with_reader_mut(|reader| reader.fail_reads = true);
        let err = pdf
            .resolve_pending_stream_length(target, &pending, target_offset)
            .expect_err("holder I/O errors must remain unrecoverable");

        assert!(matches!(err, Error::Io(_)), "got {err:?}");
        assert!(matches!(
            pdf.cache.entry(target),
            Some(CacheEntry::Unresolved { offset }) if *offset == target_offset
        ));
    }

    #[test]
    fn valid_indirect_stream_length_clears_endstream_scan_metadata() {
        let bytes =
            recovered_stream_fixture(b"/Length 2 0 R", b"\n", Some(b"2 0 obj\n3\nendobj\n"));
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open valid indirect-length fixture");
        let object_ref = ObjectRef::new(1, 0);
        let stream = pdf.resolve(object_ref).expect("resolve recovered stream");
        assert_eq!(stream.as_stream().unwrap().data, b"abc");
        assert_eq!(pdf.recovered_stream_eol(object_ref), None);
    }

    #[test]
    fn qpdf_reader_completes_adjacent_endstream_before_endobj_check() {
        let bytes = recovered_stream_fixture(b"/Length 2 0 R", b"", Some(b"2 0 obj\n3\nendobj\n"));
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let object_ref = ObjectRef::new(1, 0);
        assert_eq!(
            pdf.resolve(object_ref).unwrap().as_stream().unwrap().data,
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
        assert_eq!(pdf.resolve(object_ref).unwrap(), Object::Integer(3));
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
                pdf.resolve(ObjectRef::new(1, 0))
                    .unwrap()
                    .as_stream()
                    .unwrap()
                    .data,
                b"abc"
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
            pdf.resolve(ObjectRef::new(1, 0))
                .expect("ordinary bounded fallback"),
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
    fn qpdf_object_read_uses_bounded_fallback_and_preserves_strict_errors() {
        let stream_body =
            b"1 0 obj\n<< /Length 2 0 R >>\nstream\n9 0 obj\nnull\nendobj\nendstream\nendobj\n";
        let length_body = b"2 0 obj\n19\nendobj\n";
        let bytes = classic_pdf_with_bodies(&[stream_body, length_body], ObjectRef::new(1, 0));
        let stream_offset = bytes
            .windows(b"1 0 obj".len())
            .position(|window| window == b"1 0 obj")
            .unwrap() as u64;
        let false_next_offset = stream_offset + b"1 0 obj\n<< /Length 2".len() as u64;
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fallback fixture");
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();

        let initial_budget = pdf.resolution_fallbacks_remaining;
        let parsed = pdf
            .read_object_at(ObjectRef::new(1, 0), stream_offset)
            .expect("full read fallback must recover stream");
        assert_eq!(parsed.object_ref, ObjectRef::new(1, 0));
        assert_eq!(
            parsed.object.as_stream().unwrap().data,
            b"9 0 obj\nnull\nendobj"
        );
        assert_eq!(
            pdf.resolution_fallbacks_remaining,
            initial_budget.saturating_sub(1)
        );

        let malformed = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n", b"2 0 obj\n<<"],
            ObjectRef::new(1, 0),
        );
        let malformed_offset = malformed
            .windows(b"2 0 obj".len())
            .position(|window| window == b"2 0 obj")
            .unwrap() as u64;
        let mut pdf = Pdf::open_mem_owned(malformed).expect("open malformed lazy object");
        assert!(pdf
            .read_object_at(ObjectRef::new(2, 0), malformed_offset)
            .is_err());
        pdf.sorted_object_offsets.push(malformed_offset + 5);
        pdf.sorted_object_offsets.sort_unstable();
        pdf.resolution_fallbacks_remaining = 1;
        let window_error = pdf
            .read_object_at(ObjectRef::new(2, 0), malformed_offset)
            .unwrap_err()
            .to_string();
        assert_eq!(pdf.resolution_fallbacks_remaining, 0);
        let exhausted_error = pdf
            .read_object_at(ObjectRef::new(2, 0), malformed_offset)
            .unwrap_err()
            .to_string();
        assert_eq!(window_error, exhausted_error);
        assert!(matches!(
            pdf.cache.entry(ObjectRef::new(2, 0)),
            Some(CacheEntry::Unresolved { offset }) if *offset == malformed_offset
        ));
    }

    #[test]
    fn compressed_entry_retries_objstm_when_bounded_window_ends_inside_payload() {
        let payload = b"7 0 << /Value 1 >>\n9 0 obj\nnull\nendobj\n";
        let mut body = format!(
            "4 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
            payload.len()
        )
        .into_bytes();
        body.extend_from_slice(payload);
        body.extend_from_slice(b"endstream\nendobj\n");
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n", &body],
            ObjectRef::new(1, 0),
        );
        let objstm_offset = bytes
            .windows(b"4 0 obj".len())
            .position(|window| window == b"4 0 obj")
            .unwrap() as u64;
        let false_next_offset = bytes
            .windows(b"9 0 obj".len())
            .position(|window| window == b"9 0 obj")
            .unwrap() as u64;

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open ObjStm fallback fixture");
        pdf.cache
            .set_unresolved(ObjectRef::new(4, 0), objstm_offset);
        pdf.cache.set_compressed(ObjectRef::new(7, 0), 4, 0);
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();
        let initial_budget = pdf.resolution_fallbacks_remaining;

        let object = pdf
            .resolve_qpdf_json_object(ObjectRef::new(7, 0))
            .expect("bounded ObjStm must retry against EOF");
        assert_eq!(
            object.as_dict().and_then(|dict| dict.get("Value")),
            Some(&Object::Integer(1))
        );
        assert_eq!(
            pdf.resolution_fallbacks_remaining,
            initial_budget.saturating_sub(1)
        );
        assert!(matches!(
            pdf.cache.entry(ObjectRef::new(4, 0)),
            Some(CacheEntry::Resolved(Object::Stream(_)))
        ));
    }

    #[test]
    fn compressed_entry_retries_legacy_framing_probe_when_bounded_window_is_truncated() {
        let payload = b"7 0 << /Value 1 >>\n9 0 obj\nnull\nendobj\n";
        let mut body = "4 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 6 0 R >>\nstream\n"
            .to_owned()
            .into_bytes();
        body.extend_from_slice(payload);
        body.extend_from_slice(b"endstream\nendobj\n");
        let length_body = format!("6 0 obj\n{}\nendobj\n", payload.len()).into_bytes();
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog >>\nendobj\n",
                &body,
                &length_body,
            ],
            ObjectRef::new(1, 0),
        );
        let objstm_offset = bytes
            .windows(b"4 0 obj".len())
            .position(|window| window == b"4 0 obj")
            .expect("object stream header") as u64;
        let false_next_offset = bytes
            .windows(b"/Length".len())
            .position(|window| window == b"/Length")
            .expect("object-stream dictionary key") as u64;

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open ObjStm legacy-framing fixture");
        pdf.cache
            .set_unresolved(ObjectRef::new(4, 0), objstm_offset);
        pdf.cache.set_compressed(ObjectRef::new(7, 0), 4, 0);
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();
        let initial_budget = pdf.resolution_fallbacks_remaining;

        let object = pdf
            .resolve_qpdf_json_object(ObjectRef::new(7, 0))
            .expect("legacy-framing probe must retry against EOF");
        assert_eq!(
            object.as_dict().and_then(|dict| dict.get("Value")),
            Some(&Object::Integer(1))
        );
        assert_eq!(
            pdf.resolution_fallbacks_remaining,
            initial_budget.saturating_sub(2),
            "the framing probe and the actual object-read retry each charge \
             their own EOF read against the same offset"
        );
    }

    #[test]
    fn compressed_entry_retries_legacy_framing_probe_when_bounded_window_stops_before_stream_keyword(
    ) {
        // A false next-object offset can land after the complete object
        // dictionary (past the closing `>>`) but before the `stream`
        // keyword. The bounded parse then succeeds as a bare dictionary
        // (`PendingBody::Direct`) instead of failing outright, which must
        // still be treated as truncated framing and retried through EOF --
        // otherwise object_stream_needs_legacy_framing wrongly concludes no
        // legacy framing is needed for an object stream whose /Length is an
        // indirect reference, and the compressed member resolves to null.
        let payload = b"7 0 << /Value 1 >>\n9 0 obj\nnull\nendobj\n";
        let mut body = "4 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length 6 0 R >>\nstream\n"
            .to_owned()
            .into_bytes();
        body.extend_from_slice(payload);
        body.extend_from_slice(b"endstream\nendobj\n");
        let length_body = format!("6 0 obj\n{}\nendobj\n", payload.len()).into_bytes();
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog >>\nendobj\n",
                &body,
                &length_body,
            ],
            ObjectRef::new(1, 0),
        );
        let objstm_offset = bytes
            .windows(b"4 0 obj".len())
            .position(|window| window == b"4 0 obj")
            .expect("object stream header") as u64;
        // Land the false next-object offset right after the dictionary's
        // closing `>>` and its trailing newline, but before `stream`.
        let false_next_offset = bytes
            .windows(b">>\nstream\n".len())
            .position(|window| window == b">>\nstream\n")
            .expect("object-stream dictionary close") as u64
            + b">>\n".len() as u64;

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open ObjStm legacy-framing fixture");
        pdf.cache
            .set_unresolved(ObjectRef::new(4, 0), objstm_offset);
        pdf.cache.set_compressed(ObjectRef::new(7, 0), 4, 0);
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();
        let initial_budget = pdf.resolution_fallbacks_remaining;

        let object = pdf
            .resolve_qpdf_json_object(ObjectRef::new(7, 0))
            .expect("legacy-framing probe must retry through EOF past a bare-dictionary parse");
        assert_eq!(
            object.as_dict().and_then(|dict| dict.get("Value")),
            Some(&Object::Integer(1)),
            "the compressed member must be recovered, not resolve to null"
        );
        assert_eq!(
            pdf.resolution_fallbacks_remaining,
            initial_budget.saturating_sub(2),
            "the framing probe and the actual object-read retry each charge \
             their own EOF read against the same offset"
        );
    }

    #[test]
    fn legacy_framing_probe_stays_within_the_fallback_budget_across_many_members() {
        // A crafted xref can mark many compressed members as belonging to
        // an object number that is not actually an object-stream container
        // at all -- an ordinary dictionary with no `stream` body, which the
        // canonical resolver happily resolves as a plain (non-stream)
        // handle. `object_stream_handle_with_legacy_fallback`'s early
        // return (`!prefer_legacy_framing && canonical_error.is_none() &&
        // !stream_handle.is_missing()`) then hands that non-stream handle
        // straight back without ever reaching the `Unresolved` match arm,
        // so `self.cache` for this ref stays `Unresolved` forever and
        // `resolve_compressed_entry`'s `as_stream_dict().is_none()` guard
        // rejects every member. Each member's resolution therefore
        // re-enters `object_stream_needs_legacy_framing` and re-probes the
        // same offset, with no other retry machinery along the way to
        // charge the budget. Without the probe charging its own EOF
        // retry, this would repeat an unbounded full-file read once per
        // member; with the charge, the shared budget caps the total number
        // of full-file reads regardless of how many members reference it.
        let body = "4 0 obj\n<< /Foo /Bar >>\nendobj\n".to_owned().into_bytes();
        // Object 4 must be a real xref entry (not merely seeded into the
        // legacy cache) so the canonical resolver actually resolves it as a
        // plain, non-missing dictionary handle instead of reporting it
        // missing outright -- that is what routes
        // object_stream_handle_with_legacy_fallback through its early
        // return instead of the `Unresolved` match arm, which has its own,
        // separately budget-charged EOF retry that would otherwise mask
        // the probe's own charge.
        let bytes = classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /Catalog >>\nendobj\n",
                b"2 0 obj\nnull\nendobj\n",
                b"3 0 obj\nnull\nendobj\n",
                &body,
            ],
            ObjectRef::new(1, 0),
        );
        let objstm_offset = bytes
            .windows(b"4 0 obj".len())
            .position(|window| window == b"4 0 obj")
            .expect("object stream header") as u64;
        // Land the false next-object offset right after the dictionary's
        // closing `>>` and its trailing newline, same as the
        // stops-before-stream-keyword case above -- except here there is no
        // `stream` keyword anywhere in the file, so retrying through EOF
        // still yields a bare dictionary, not a stream. Search only from
        // objstm_offset onward: the catalog body earlier in the file also
        // ends in `>>\nendobj\n` and would otherwise match first.
        let false_next_offset = objstm_offset
            + bytes[objstm_offset as usize..]
                .windows(b">>\nendobj\n".len())
                .position(|window| window == b">>\nendobj\n")
                .expect("object-stream dictionary close") as u64
            + b">>\n".len() as u64;

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open ObjStm legacy-framing fixture");
        pdf.cache
            .set_unresolved(ObjectRef::new(4, 0), objstm_offset);
        for (member, index) in [(7, 0), (8, 1), (9, 2)] {
            pdf.cache
                .set_compressed(ObjectRef::new(member, 0), 4, index);
        }
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();
        pdf.resolution_fallbacks_remaining = 2;

        for member in [7, 8, 9] {
            assert_eq!(
                pdf.resolve_qpdf_json_object(ObjectRef::new(member, 0))
                    .expect(
                        "a container that is not a stream resolves its member to null, not Err"
                    ),
                Object::Null,
                "member {member} must not recover from a container that never has a stream body"
            );
        }
        assert_eq!(
            pdf.resolution_fallbacks_remaining, 0,
            "the shared fallback budget caps total full-file reads across all members \
             instead of granting each member its own unbounded probe"
        );
        assert!(
            matches!(
                pdf.cache.entry(ObjectRef::new(4, 0)),
                Some(CacheEntry::Unresolved { offset }) if *offset == objstm_offset
            ),
            "the container never resolves, so later members keep re-probing until the budget caps them"
        );
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

        let object = pdf
            .resolve_qpdf_json_object(ObjectRef::new(7, 0))
            .expect("indirect DecodeParms must resolve through the canonical stream handle");
        assert_eq!(
            object.as_dict().and_then(|dict| dict.get("Value")),
            Some(&Object::Integer(1))
        );
    }

    #[test]
    fn object_stream_consumer_covers_legacy_cache_and_handle_error_boundaries() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let stream_ref = ObjectRef::new(7, 0);
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        stream_dict.insert("N", Object::Integer(1));
        stream_dict.insert("First", Object::Integer(4));
        stream_dict.insert("Length", Object::Integer(16));
        let stream = Stream::new(stream_dict, b"7 0 << /Value 1 >>".to_vec());
        pdf.cache.set_resolved(stream_ref, Object::Stream(stream));
        let stream_handle = pdf
            .object_stream_handle_with_legacy_fallback(stream_ref)
            .expect("cache-seeded stream")
            .expect("stream handle");
        assert!(stream_handle.as_stream_dict().is_some());

        let non_stream_ref = ObjectRef::new(8, 0);
        pdf.cache.set_resolved(non_stream_ref, Object::Integer(1));
        assert!(pdf
            .object_stream_handle_with_legacy_fallback(non_stream_ref)
            .expect("cache-seeded non-stream")
            .is_none());

        let mut source_pdf = Pdf::open_mem_owned(classic_pdf_with_bodies(
            &[
                b"1 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Filter /Crypt /DecodeParms << /Name /Identity >> /Length 2 0 R >>\nstream\n7 0 << /Value 1 >>\nendstream\nendobj\n",
                b"2 0 obj\n18\nendobj\n",
            ],
            ObjectRef::new(1, 0),
        ))
        .expect("open malformed-framing identity stream");
        *source_pdf.encryption.borrow_mut() = Some(explicit_rc4_encryption_state());
        source_pdf
            .object_stream_handle_with_legacy_fallback(ObjectRef::new(1, 0))
            .expect("legacy framing fallback")
            .expect("identity encrypted object stream");
        assert!(source_pdf
            .transformed_stream_refs
            .contains(&ObjectRef::new(1, 0)));

        let direct_dict = ObjectHandle::dictionary(vec![
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
        ]);
        let out_of_range =
            ObjectHandle::stream(direct_dict.clone(), Rc::new(b"7 0 << /Value 1 >>".to_vec()));
        assert!(source_pdf
            .parse_object_stream_entry_from_handle(&out_of_range, 1)
            .expect("missing header object must be unresolved")
            .is_none());

        let out_of_bounds = ObjectHandle::stream(direct_dict.clone(), Rc::new(b"7 999".to_vec()));
        assert!(source_pdf
            .parse_object_stream_entry_from_handle(&out_of_bounds, 7)
            .is_err());

        let malformed_header =
            ObjectHandle::stream(direct_dict.clone(), Rc::new(b"not-an-integer 0".to_vec()));
        assert!(source_pdf
            .parse_object_stream_entry_from_handle(&malformed_header, 7)
            .is_err());

        let negative_object_number =
            ObjectHandle::stream(direct_dict.clone(), Rc::new(b"-1 0".to_vec()));
        assert!(source_pdf
            .parse_object_stream_entry_from_handle(&negative_object_number, 7)
            .is_err());

        let too_large_object_number =
            ObjectHandle::stream(direct_dict, Rc::new(b"4294967296 0".to_vec()));
        assert!(source_pdf
            .parse_object_stream_entry_from_handle(&too_large_object_number, 7)
            .is_err());

        let invalid_filter_dict = ObjectHandle::dictionary(vec![
            (b"N".to_vec(), ObjectHandle::integer(1)),
            (b"First".to_vec(), ObjectHandle::integer(4)),
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
        ]);
        let invalid_filter =
            ObjectHandle::stream(invalid_filter_dict, Rc::new(b"not flate data".to_vec()));
        assert!(source_pdf
            .decode_object_stream_data(&invalid_filter)
            .is_err());

        let provider_ref = ObjectRef::new(10, 0);
        let mut provider_dict = Dictionary::new();
        provider_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        source_pdf.set_object(
            provider_ref,
            Object::Stream(Stream::new(provider_dict, Vec::new())),
        );
        let provider_handle = source_pdf.get_object_handle(provider_ref);
        provider_handle
            .replace_stream_data_with_retry_callback(|_, _, _| Ok(false), None, None)
            .expect("install retry-aware provider");
        assert!(source_pdf
            .decode_object_stream_data(&provider_handle)
            .is_err());

        let missing_count = ObjectHandle::dictionary(vec![]);
        assert!(object_stream_integer_from_handle(&missing_count, b"/N", "/N").is_err());
        let negative_count =
            ObjectHandle::dictionary(vec![(b"N".to_vec(), ObjectHandle::integer(-1))]);
        assert!(object_stream_integer_from_handle(&negative_count, b"/N", "/N").is_err());
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

        assert!(!pdf
            .resolve_compressed_entry(object_ref, 4, 0)
            .expect("stale source must be treated as unresolved"));
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
        pdf.resolve_object_handle(&handle).unwrap();
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
            pdf.resolve_qpdf_json_object_borrowed(object_ref)
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
            pdf.resolve(object_ref)
                .unwrap()
                .as_dict()
                .unwrap()
                .get("Value"),
            Some(&Object::Integer(1))
        );

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve_object_handle(&handle)
            .expect("resolve the canonical handle before mutating it");
        handle
            .replace_key(b"/Value", ObjectHandle::integer(2))
            .unwrap();
        pdf.mark_object_dirty(object_ref);

        assert_eq!(
            pdf.resolve(object_ref)
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
        source.resolve_object_handle(&source_owner).unwrap();
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
        pdf.resolve_object_handle(&owner).unwrap();
        let inner = ObjectHandle::integer(42);
        let container = ObjectHandle::dictionary(vec![(b"Inner".to_vec(), inner.clone())]);
        owner.replace_key(b"/Container", container).unwrap();
        pdf.clear_dirty(object_ref);

        pdf.mark_object_handle_dirty(&inner).unwrap();
        assert!(pdf.is_dirty(object_ref));
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
        pdf.resolve_object_handle(&owner).unwrap();
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
        reopened.resolve_object_handle(&reopened_owner).unwrap();
        assert!(
            reopened_owner.get_key(b"/Child").is_null(),
            "canonical writer must emit the live owner's remove_key mutation"
        );
    }

    #[test]
    fn borrowed_qpdf_resolution_uses_the_canonical_historical_stream() {
        let mut pdf = Pdf::open_mem_owned(top_level_bare_reference_pdf()).expect("open fixture");
        let live_ref = ObjectRef::new(8, 0);
        pdf.set_object(
            live_ref,
            Object::Stream(Stream::new(Dictionary::new(), vec![0x41; 1024 * 1024])),
        );
        let live_payload_ptr = pdf
            .resolve_borrowed(live_ref)
            .expect("resolve seeded live stream")
            .as_stream()
            .expect("seeded live object is a stream")
            .data
            .as_ptr();
        let resolved_live = pdf
            .resolve_qpdf_json_object_borrowed(live_ref)
            .expect("resolve live stream");
        assert_eq!(
            resolved_live
                .as_stream()
                .expect("live object is a stream")
                .data
                .as_ptr(),
            live_payload_ptr
        );

        let historical_ref = ObjectRef::new(9, 0);
        pdf.install_test_parsed_xref_stream_handle(
            historical_ref,
            Object::Stream(Stream::new(Dictionary::new(), vec![0x5a; 1024 * 1024])),
        )
        .expect("install canonical historical stream");
        let canonical_payload = pdf
            .get_object_handle(historical_ref)
            .as_stream_data()
            .expect("canonical historical stream payload");
        let repeated_payload = pdf
            .get_object_handle(historical_ref)
            .as_stream_data()
            .expect("repeated canonical historical stream payload");
        assert!(std::rc::Rc::ptr_eq(&canonical_payload, &repeated_payload));

        let resolved = pdf
            .resolve_qpdf_json_object_borrowed(historical_ref)
            .expect("resolve historical stream");
        let stream = resolved.as_stream().expect("historical object is a stream");

        assert_eq!(stream.data.len(), 1024 * 1024);
    }

    #[test]
    fn borrowed_resolution_materializes_a_pre_resolved_canonical_value_after_a_legacy_parse_error()
    {
        let object_ref = ObjectRef::new(1, 0);
        let bytes = classic_pdf_with_bodies(&[b"1 0 obj\n[2147483648 0 R]\nendobj\n"], object_ref);
        let xref_offset = bytes
            .windows(b"xref\n".len())
            .position(|window| window == b"xref\n")
            .expect("xref marker") as u64;
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                repair: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("open repair fixture");

        // Simulate a stale cache offset after a canonical caller has already
        // resolved the handle. The compatibility parser must fail first, then
        // the repair path may expose the canonical value without reparsing it.
        let handle = pdf.get_object_handle(object_ref);
        handle.set_resolved(ObjectValue::Integer(42));
        pdf.cache.set_unresolved(object_ref, xref_offset);

        assert_eq!(
            pdf.resolve_borrowed(object_ref)
                .expect("canonical value after legacy parse failure"),
            &Object::Integer(42)
        );
    }

    #[test]
    fn canonical_compatibility_materialization_reuses_a_resolved_legacy_stream() {
        let object_ref = ObjectRef::new(91, 0);
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let handle = pdf.get_object_handle(object_ref);
        let stream_data = b"canonical stream bytes".to_vec();
        handle.set_resolved(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(Vec::new()),
            stream_data: Some(Rc::new(stream_data.clone())),
            stream_length: stream_data.len(),
            stream_provider: None,
        });

        let legacy = Object::Stream(Stream::new(Dictionary::new(), b"legacy bytes".to_vec()));
        pdf.cache.set_resolved(object_ref, legacy.clone());

        assert!(pdf
            .materialize_canonical_compatibility_value(object_ref)
            .expect("canonical stream compatibility materialization"));
        assert_eq!(pdf.legacy_materialized_memo.get(&object_ref), Some(&legacy));
    }

    #[test]
    fn legacy_materialization_tolerates_stale_stream_eol_for_non_stream_handles() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let stream_ref = ObjectRef::new(91, 0);
        pdf.recovered_stream_eols
            .insert(stream_ref, crate::parser::RecoveredStreamEol::Lf);

        let stream_handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(Vec::new()),
            stream_data: Some(Rc::new(b"abc\n".to_vec())),
            stream_provider: None,
            stream_length: 4,
        });
        let stream = pdf
            .materialize_handle_for_legacy(stream_ref, &stream_handle)
            .expect("stream compatibility materialization");
        assert_eq!(stream.as_stream().expect("stream value").data, b"abc");

        let scalar = pdf
            .materialize_handle_for_legacy(stream_ref, &ObjectHandle::integer(42))
            .expect("scalar compatibility materialization");
        assert_eq!(scalar, Object::Integer(42));
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
            normal_first.resolve(object_ref).unwrap(),
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
        assert_eq!(json_first.resolve(object_ref).unwrap(), Object::Integer(3));
        assert_eq!(json_first.repair_diagnostics().entries().len(), 1);
    }

    #[test]
    fn normal_resolution_retries_when_bounded_window_ends_inside_stream_payload() {
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

        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fallback fixture");
        pdf.sorted_object_offsets.push(false_next_offset);
        pdf.sorted_object_offsets.sort_unstable();
        let initial_budget = pdf.resolution_fallbacks_remaining;

        let object = pdf
            .resolve(ObjectRef::new(2, 0))
            .expect("bounded stream must retry against EOF");
        assert_eq!(
            object.as_stream().map(|stream| stream.data.as_slice()),
            Some(payload.as_slice())
        );
        assert_eq!(
            pdf.resolution_fallbacks_remaining,
            initial_budget.saturating_sub(1)
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

        assert_eq!(pdf.resolve(ObjectRef::new(3, 0)).unwrap(), Object::Null);
        assert_eq!(pdf.resolve(ObjectRef::new(3, 0)).unwrap(), Object::Null);
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
            pdf.resolve(ObjectRef::new(4, 0)).unwrap(),
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
        assert_eq!(
            pdf.resolve(ObjectRef::new(7, 0)).unwrap(),
            Object::Integer(6)
        );
        assert_eq!(
            pdf.recovered_stream_eol(ObjectRef::new(1, 0)),
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

        pdf.resolve_object_handle(&first)
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
            .unparse_object_with_ref_map_and_removed(&mut out, &|object_ref| Ok(object_ref), &BTreeSet::new())
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
                .unparse_object(&mut out)
                .expect_err("plain writer must reject reserved objects")
                .to_string(),
            expected
        );
        out.clear();
        assert_eq!(
            reserved
                .unparse_object_qdf(&mut out, 0)
                .expect_err("QDF writer must reject reserved objects")
                .to_string(),
            expected
        );
        out.clear();
        assert_eq!(
            reserved
                .unparse_object_with_ref_map(&mut out, &|object_ref| Ok(object_ref))
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
            .unparse_object(&mut out)
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
        qdf_containing.unparse_object_qdf(&mut out, 0).expect(
            "an indirect reserved child must serialize as a bare reference in QDF mode too",
        );
        assert_eq!(
            out,
            format!("<<\n  /Reserved {object_ref}\n>>").into_bytes()
        );

        out.clear();
        assert_eq!(
            reserved
                .unparse_stream_body(&mut out, false)
                .expect_err("stream-body writer must reject reserved objects")
                .to_string(),
            expected
        );
        out.clear();
        assert_eq!(
            reserved
                .unparse_stream_body_qdf(&mut out, 0)
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
                .unparse_stream_body_with_ref_map_and_removed(
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
                .unparse_trailer(&mut out, false, None)
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
        pdf.resolve_object_handle(&root).expect("resolve catalog");
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
        // `unparse_object_with_ref_map_and_removed`/`write_child_with_ref_map`
        // are the primitives `writer/plain/body.rs`/`writer/plain/plan.rs`
        // actually call in production, unlike the still-`#[allow(dead_code)]`
        // `unparse_object`/`write_child` family exercised by
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
            .unparse_object_with_ref_map_and_removed(
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
        pdf.resolve_object_handle(&source)
            .expect("resolve source stream");
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
        pdf.resolve_object_handle(&source)
            .expect("resolve source stream");

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
        pdf.resolve_object_handle(&source)
            .expect("resolve source stream");
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
        pdf.resolve_object_handle(&root).expect("resolve root");
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
        let emitted_stream_ref = reopened
            .resolve(root_ref)
            .expect("resolve rewritten root")
            .into_dict()
            .and_then(|dict| dict.get("Extra").cloned())
            .and_then(|object| object.as_ref_id())
            .expect("rewritten root has stream reference");
        assert_eq!(emitted_stream_ref, emitted_ref);
        assert_eq!(
            reopened
                .resolve(emitted_ref)
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
            .get_all_object_handles()
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
    fn replace_object_handle_preserves_target_identity_and_shares_payload() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let target = pdf.get_object_handle(object_ref);
        target.try_dereference().expect("resolve target");
        assert!(target.get_parsed_offset() >= 0);

        let replacement =
            ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(7))]);
        let replacement_alias = replacement.clone();

        let returned = pdf
            .replace_object_handle(object_ref, replacement)
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
    fn set_object_handle_is_the_dirty_marking_handle_form_of_set_object() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let target = pdf.get_object_handle(object_ref);
        target.try_dereference().expect("resolve target");
        let replacement = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"Catalog".to_vec()),
        )]);
        let replacement_alias = replacement.clone();

        pdf.set_object_handle(object_ref, replacement)
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
    fn set_object_handle_preserves_clean_and_dirty_restore_writer_snapshots() {
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

            let original_object = legacy.resolve(object_ref).expect("resolve legacy catalog");
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
                .set_object_handle(object_ref, handle_modified)
                .expect("replace canonical catalog");
            assert!(legacy.is_dirty(object_ref));
            assert!(canonical.is_dirty(object_ref));
            assert_eq!(
                legacy
                    .resolve(object_ref)
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
                .set_object_handle(
                    object_ref,
                    original_handle_value
                        .shallow_copy()
                        .expect("copy canonical original value"),
                )
                .expect("restore canonical catalog");
            assert!(legacy
                .resolve(object_ref)
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
    fn replace_object_handle_clears_source_derived_side_tables() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        pdf.recovered_stream_eols
            .insert(object_ref, crate::parser::RecoveredStreamEol::Lf);
        pdf.transformed_stream_refs.insert(object_ref);
        pdf.qpdf_parsed_xref_stream_refs.insert(object_ref);
        pdf.qpdf_dangling_refs.insert(object_ref);

        pdf.replace_object_handle(object_ref, ObjectHandle::integer(7))
            .expect("replace canonical object");

        assert!(!pdf.recovered_stream_eols.contains_key(&object_ref));
        assert!(!pdf.transformed_stream_refs.contains(&object_ref));
        assert!(!pdf.qpdf_parsed_xref_stream_refs.contains(&object_ref));
        assert!(!pdf.qpdf_dangling_refs.contains(&object_ref));
    }

    #[test]
    fn replace_object_handle_rejects_indirect_replacement_without_mutation() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);
        let target = pdf.get_object_handle(object_ref);
        target.try_dereference().expect("resolve target");
        let before = target.get_key(b"/Type").as_name().expect("catalog type");

        let indirect_replacement = pdf.get_object_handle(ObjectRef::new(2, 0));
        let error = pdf
            .replace_object_handle(object_ref, indirect_replacement)
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
    fn replace_object_handle_rejects_a_foreign_direct_value_without_mutation() {
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
            .replace_object_handle(target_ref, foreign_direct)
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
    fn replace_object_handle_accepts_a_direct_reserved_value() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let reserved = pdf.new_reserved().expect("reserved object");
        let replacement = reserved.shallow_copy().expect("reserved copy");
        let target_ref = ObjectRef::new(99, 0);
        assert!(replacement.is_direct());
        assert!(replacement.is_reserved());
        assert!(pdf.resolver.registered_handle(target_ref).is_none());

        let target = pdf
            .replace_object_handle(target_ref, replacement)
            .expect("qpdf replaceObject accepts an initialized direct reserved value");

        assert!(target.is_reserved());
        assert_eq!(target.object_ref(), Some(target_ref));
        assert!(pdf.resolver.registered_handle(target_ref).is_some());
        assert!(pdf.is_dirty(target_ref));
    }

    #[test]
    fn replace_object_handle_accepts_a_destroyed_value() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let destroyed = pdf.new_reserved().expect("reserved object");
        destroyed.disconnect();
        let target_ref = ObjectRef::new(99, 0);
        assert!(destroyed.is_direct());
        assert_eq!(destroyed.type_code().expect("type code"), 14);
        assert!(pdf.resolver.registered_handle(target_ref).is_none());

        let target = pdf
            .replace_object_handle(target_ref, destroyed)
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
    fn replace_object_handle_preserves_an_objstm_default_xref_row() {
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
            .replace_object_handle(object_ref, ObjectHandle::integer(42))
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
    fn replace_object_handle_then_objstm_default_xref_row_is_visible_later() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(99, 0);

        pdf.replace_object_handle(object_ref, ObjectHandle::integer(42))
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
    fn get_all_object_handles_returns_indirect_handles_in_object_ref_order() {
        // `minimal_pdf_bytes` has three live objects (1 0, 2 0, 3 0) and one
        // free entry: the `0 65535 f` free-list head that every classic xref
        // table carries. The exact expected list below therefore also pins
        // that the free-list head is excluded, matching qpdf's
        // `getAllObjects()` (whose backing `xref_table` never contains free
        // entries in the first place).
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handles = pdf
            .get_all_object_handles()
            .expect("get all object handles");
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
    fn get_all_object_handles_reuses_the_canonical_handle_for_an_already_registered_ref() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(2, 0);
        let pre_registered = pdf.get_object_handle(object_ref);

        let handles = pdf
            .get_all_object_handles()
            .expect("get all object handles");

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
    fn get_all_object_handles_includes_a_ref_registered_only_via_handle_registry() {
        // Register a ref that never appears in the source xref table at all
        // (the dangling case): the union must not drop registry-only refs.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let dangling_ref = ObjectRef::new(99, 0);
        pdf.get_object_handle(dangling_ref);

        let handles = pdf
            .get_all_object_handles()
            .expect("get all object handles");

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
            pdf.resolve(object_ref).expect("resolve replacement"),
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
        pdf.resolve_object_handle(&handle)
            .expect("resolve original stream");
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
        pdf.resolve_object_handle(&handle)
            .expect("resolve original stream");

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
        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "an over-deep programmatic stream replacement must not use the legacy memo"
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
        pdf.resolve_object_handle(&handle)
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
    fn get_all_objects_moves_legacy_stream_memo_payload_without_copying_it() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open stream fixture");
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve_object_handle(&handle)
            .expect("resolve the original stream handle");

        let mut replacement_dict = Dictionary::new();
        // Content-stream-only tokens make the infallible legacy set_object
        // bridge retain the replacement in its memo for later reconciliation.
        replacement_dict.insert("Invalid", Object::Operator(b"q".to_vec()));
        pdf.set_object(
            object_ref,
            Object::Stream(Stream::new(replacement_dict, vec![0x5a; 1024 * 1024])),
        );

        let memo_payload_ptr = pdf
            .legacy_materialized_memo
            .get(&object_ref)
            .and_then(Object::as_stream)
            .expect("set_object stores a stream replacement in the legacy memo")
            .data
            .as_ptr();

        let reconciled = pdf
            .get_all_objects()
            .expect("reconcile the replacement")
            .into_iter()
            .find(|candidate| candidate.object_ref() == Some(object_ref))
            .expect("reconciled replacement handle is enumerated");
        let canonical_payload = reconciled
            .as_stream_data()
            .expect("reconciled value is a stream with payload");

        assert_eq!(
            canonical_payload.as_ptr(),
            memo_payload_ptr,
            "reconciliation must move the memo payload into the shared canonical buffer"
        );
    }

    #[test]
    fn get_all_objects_moves_legacy_stream_memo_payload_into_a_new_dictionary() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open fixture");
        let object_ref = ObjectRef::new(99, 0);
        let mut replacement_dict = Dictionary::new();
        replacement_dict.insert("Invalid", Object::Operator(b"q".to_vec()));
        pdf.set_object(
            object_ref,
            Object::Stream(Stream::new(replacement_dict, vec![0x5a; 4096])),
        );

        let memo_payload_ptr = pdf
            .legacy_materialized_memo
            .get(&object_ref)
            .and_then(Object::as_stream)
            .expect("set_object stores a stream replacement in the legacy memo")
            .data
            .as_ptr();
        let reconciled = pdf
            .get_all_objects()
            .expect("reconcile the replacement")
            .into_iter()
            .find(|candidate| candidate.object_ref() == Some(object_ref))
            .expect("reconciled replacement handle is enumerated");

        assert_eq!(
            reconciled
                .as_stream_data()
                .expect("reconciled value is a stream with payload")
                .as_ptr(),
            memo_payload_ptr,
            "a replacement without an existing stream dictionary must also move its payload"
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
        missing.set_missing();
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
        assert!(matches!(
            pdf.resolve_qpdf_json_object_borrowed(historical_ref)
                .expect("borrowed unresolved qpdf historical lookup"),
            Object::Null
        ));
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
    fn get_all_objects_ignores_a_stale_memo_without_a_registered_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let stale_ref = ObjectRef::new(999, 0);
        pdf.legacy_materialized_memo
            .insert(stale_ref, Object::Integer(7));
        pdf.legacy_materialized_replacement_refs.insert(stale_ref);

        let objects = pdf
            .get_all_objects()
            .expect("a stale memo must not block canonical enumeration");
        assert!(!objects
            .iter()
            .any(|candidate| candidate.object_ref() == Some(stale_ref)));
        assert!(pdf.legacy_materialized_memo.contains_key(&stale_ref));
        assert!(pdf
            .legacy_materialized_replacement_refs
            .contains(&stale_ref));
    }

    #[test]
    fn get_all_objects_ignores_a_registered_replacement_without_a_memo() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(999, 0);
        pdf.get_object_handle(object_ref);
        pdf.legacy_materialized_replacement_refs.insert(object_ref);

        pdf.get_all_objects()
            .expect("a replacement marker without a memo is harmless");
        assert!(pdf
            .legacy_materialized_replacement_refs
            .contains(&object_ref));
    }

    #[test]
    fn get_all_objects_propagates_an_unliftable_materialized_replacement() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(999, 0);
        pdf.get_object_handle(object_ref);

        let mut replacement = Object::Null;
        for _ in 0..=crate::parser::MAX_PARSE_DEPTH {
            replacement = Object::Array(vec![replacement]);
        }
        pdf.legacy_materialized_memo.insert(object_ref, replacement);
        pdf.legacy_materialized_replacement_refs.insert(object_ref);

        let error = pdf
            .get_all_objects()
            .expect_err("an over-deep replacement must fail during reconciliation");
        assert!(error
            .to_string()
            .contains("object handle lift: inline object nesting exceeds maximum"));
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
        let handle = pdf.get_object_handle(deleted_ref);
        assert!(!handle.is_missing());

        // Keep the canonical handle registered while presenting the legacy
        // cache state that live enumeration must filter out. `delete_object`
        // also marks the handle missing, which exercises a different guard.
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
    fn trailer_handle_is_direct_with_a_canonical_indirect_root_child() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handle = pdf.trailer_handle();
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

        let first = pdf.trailer_handle();
        let second = pdf.trailer_handle();

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

        let handle = pdf.trailer_handle();

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
            pdf.trailer_handle().is_null(),
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
            pdf.trailer_handle().is_null(),
            "sanity: the whole-trailer walk does degrade here"
        );
        assert_eq!(pdf.root_ref(), root);
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
    fn resolve_object_handle_is_a_no_op_for_a_direct_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let direct = ObjectHandle::integer(7);
        pdf.resolve_object_handle(&direct)
            .expect("a direct handle is a no-op");
        assert_eq!(direct.as_integer(), Some(7));
    }

    #[test]
    fn resolve_object_handle_matches_resolve_borrowed_for_a_live_object() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve_object_handle(&handle).expect("resolve handle");

        let legacy = pdf.resolve_borrowed(object_ref).expect("resolve legacy");
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
    fn resolve_object_handle_to_terminal_is_a_no_op_for_an_already_terminal_value() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        let result = pdf
            .resolve_object_handle_to_terminal(&handle)
            .expect("resolve a plain, never-redirected object");

        assert!(
            result.is_same_object_as(&handle),
            "no chase needed: same handle back"
        );
        assert!(result.as_dictionary().is_some());
        assert_eq!(result.as_reference(), None);
    }

    #[test]
    fn resolve_object_handle_to_terminal_ref_reports_the_objects_own_ref_for_a_natural_single_hop()
    {
        // No `set_object` redirect is involved at all: `object_ref` resolves
        // directly to its dictionary. The terminal ref must be the object's
        // own ref, not `None` — this is the case
        // `resolve_object_handle_to_terminal`'s "already terminal" fast path
        // takes, and it must still report a ref for a caller that needs one
        // (e.g. a diagnostic source-offset lookup keyed on that ref).
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        let (result, terminal_ref) = pdf
            .resolve_object_handle_to_terminal_ref(&handle)
            .expect("resolve a plain, never-redirected object, also reporting its ref");

        assert!(result.as_dictionary().is_some());
        assert_eq!(terminal_ref, Some(object_ref));
    }

    #[test]
    fn resolve_object_handle_to_terminal_ref_reports_no_ref_for_a_direct_handle() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let direct = ObjectHandle::integer(7);

        let (result, terminal_ref) = pdf
            .resolve_object_handle_to_terminal_ref(&direct)
            .expect("a direct handle has no ref to chase from");

        assert_eq!(result.as_integer(), Some(7));
        assert_eq!(terminal_ref, None);
    }

    #[test]
    fn resolve_object_handle_does_not_chase_a_set_object_reference_redirect() {
        // `resolve_object_handle` itself must keep its existing single-hop
        // contract: `Pdf::resolve_borrowed` (and `ref_chain.rs`'s own
        // bounded chain-follow primitive, used across ~20 production
        // modules) depends on observing an intermediate `Object::Reference`
        // per hop, not a silently pre-chased terminal value. Chasing
        // through to the terminal is `resolve_object_handle_to_terminal`'s
        // job — see the tests below.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let target_ref = ObjectRef::new(100, 0);
        let redirect_ref = ObjectRef::new(200, 0);
        pdf.set_object(target_ref, Object::Boolean(true));
        pdf.set_object(redirect_ref, Object::Reference(target_ref));

        let handle = pdf.get_object_handle(redirect_ref);
        pdf.resolve_object_handle(&handle)
            .expect("resolve redirect handle");

        assert_eq!(handle.as_reference(), Some(target_ref));
        assert_eq!(
            handle.type_code().expect("type code"),
            13,
            "ot_unresolved, unchanged by this method"
        );
        assert_eq!(
            pdf.resolve(redirect_ref).expect("legacy resolve"),
            Object::Reference(target_ref)
        );
    }

    #[test]
    fn resolve_object_handle_to_terminal_chases_a_set_object_reference_redirect_to_its_terminal_value(
    ) {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let target_ref = ObjectRef::new(100, 0);
        let redirect_ref = ObjectRef::new(200, 0);
        pdf.set_object(target_ref, Object::Boolean(true));
        pdf.set_object(redirect_ref, Object::Reference(target_ref));

        let handle = pdf.get_object_handle(redirect_ref);
        let result = pdf
            .resolve_object_handle_to_terminal(&handle)
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
            pdf.resolve(redirect_ref).expect("legacy resolve"),
            Object::Reference(target_ref)
        );

        let (_ref_result, terminal_ref) = pdf
            .resolve_object_handle_to_terminal_ref(&handle)
            .expect("resolve redirect handle, also reporting its terminal ref");
        assert_eq!(
            terminal_ref,
            Some(target_ref),
            "terminal ref is the redirect's target (100), not handle.object_ref() (200)"
        );
    }

    #[test]
    fn resolve_object_handle_to_terminal_returns_the_canonical_handle_not_a_copy() {
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
            .resolve_object_handle_to_terminal(&handle)
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
        pdf.resolve_object_handle(&canonical_target)
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
    fn resolve_object_handle_to_terminal_chases_a_multi_hop_reference_redirect_chain() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let terminal_ref = ObjectRef::new(100, 0);
        let middle_ref = ObjectRef::new(200, 0);
        let outer_ref = ObjectRef::new(300, 0);
        pdf.set_object(terminal_ref, Object::Integer(42));
        pdf.set_object(middle_ref, Object::Reference(terminal_ref));
        pdf.set_object(outer_ref, Object::Reference(middle_ref));

        let handle = pdf.get_object_handle(outer_ref);
        let result = pdf
            .resolve_object_handle_to_terminal(&handle)
            .expect("resolve multi-hop redirect handle to its terminal value");

        assert_eq!(result.as_integer(), Some(42));
        assert_eq!(
            result.object_ref(),
            Some(terminal_ref),
            "result is the chain's *last* hop's own canonical handle, not the \
             first (outer_ref) or middle"
        );

        let (_ref_result, observed_terminal_ref) = pdf
            .resolve_object_handle_to_terminal_ref(&handle)
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
            pdf.resolve(outer_ref).expect("legacy resolve"),
            Object::Reference(middle_ref)
        );
    }

    #[test]
    fn resolve_object_handle_to_terminal_bounds_a_self_referential_redirect_without_hanging() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let self_ref = ObjectRef::new(100, 0);
        pdf.set_object(self_ref, Object::Reference(self_ref));

        let handle = pdf.get_object_handle(self_ref);
        let result = pdf
            .resolve_object_handle_to_terminal(&handle)
            .expect("a self-referential redirect must not hang, overflow, or error");

        assert!(result.is_null());
        // The canonical handle is untouched by the cycle-bound fallback.
        assert_eq!(handle.as_reference(), Some(self_ref));
    }

    #[test]
    fn resolve_object_handle_to_terminal_bounds_a_mutual_redirect_cycle_without_hanging() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let ref_a = ObjectRef::new(100, 0);
        let ref_b = ObjectRef::new(200, 0);
        pdf.set_object(ref_a, Object::Reference(ref_b));
        pdf.set_object(ref_b, Object::Reference(ref_a));

        let handle_a = pdf.get_object_handle(ref_a);
        let result = pdf
            .resolve_object_handle_to_terminal(&handle_a)
            .expect("a mutual redirect cycle must not hang, overflow, or error");

        assert!(result.is_null());

        // Neither canonical handle is mutated by the cycle-bound fallback:
        // both are left exactly as `Pdf::set_object` wrote them.
        assert_eq!(handle_a.as_reference(), Some(ref_b));
        let handle_b = pdf.get_object_handle(ref_b);
        assert_eq!(handle_b.as_reference(), Some(ref_a));
    }

    #[test]
    fn resolve_object_handle_to_terminal_accepts_a_chain_exactly_at_the_depth_limit() {
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
            .resolve_object_handle_to_terminal(&handle)
            .expect("a chain exactly at the depth limit must resolve, not be treated as cyclic");

        assert_eq!(result.as_integer(), Some(7));

        let (_ref_result, observed_terminal_ref) = pdf
            .resolve_object_handle_to_terminal_ref(&handle)
            .expect("a chain exactly at the depth limit must resolve, also reporting its ref");
        assert_eq!(observed_terminal_ref, Some(terminal_ref));
    }

    #[test]
    fn resolve_object_handle_to_terminal_treats_a_chain_one_hop_past_the_limit_as_cyclic() {
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
            .resolve_object_handle_to_terminal(&handle)
            .expect("a too-long chain falls back rather than erroring");

        assert!(result.is_null());

        let (ref_fallback, observed_terminal_ref) = pdf
            .resolve_object_handle_to_terminal_ref(&handle)
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
            pdf.resolve_object_handle_to_terminal_ref(&handle),
            Err(crate::Error::System(ref message)) if message == "sink write failure 1"
        ));
        assert_eq!(pdf.repair_diagnostics().entries().len(), 1);
    }

    /// White-box companion to the public canonical-handle null behavior:
    /// canonical resolution updates the ResolverHandle slot only. It does
    /// not populate the legacy raw-Object cache as a side effect.
    #[test]
    fn resolve_object_handle_keeps_legacy_cache_untouched_for_nulls() {
        let bytes = classic_pdf_with_bodies(&[b"1 0 obj\nnull\nendobj\n"], ObjectRef::new(1, 0));
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open literal-null fixture");

        let literal_null_ref = ObjectRef::new(1, 0);
        let literal_null_handle = pdf.get_object_handle(literal_null_ref);
        pdf.resolve_object_handle(&literal_null_handle)
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
        pdf.resolve_object_handle(&dangling_handle)
            .expect("resolve dangling ref");
        assert!(dangling_handle.is_null());
        assert!(
            pdf.cache.entry(dangling_ref).is_none(),
            "a ref absent from the xref table must never gain a cache entry just from resolving its handle"
        );
    }

    #[test]
    fn resolve_object_handle_compressed_member_accepts_inline_nesting_past_max_inline_depth() {
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
        pdf.resolve_object_handle(&handle)
            .expect("nesting between MAX_INLINE_DEPTH and MAX_PARSE_DEPTH must now succeed");

        let legacy = pdf
            .resolve(ObjectRef::new(7, 0))
            .expect("resolve_borrowed must also accept it");
        assert!(legacy.as_array().is_some());
        assert!(handle.as_array().is_some());
    }

    #[test]
    fn resolve_object_handle_compressed_member_recovers_qpdfs_excessive_nesting() {
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
        pdf.resolve_object_handle(&handle)
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
    fn resolve_object_handle_uncompressed_accepts_the_same_nesting_depth_via_canonical_parser() {
        // Task 6's `resolve_object_handle` routed every indirect handle
        // (Uncompressed and Compressed alike) through the same `lift`
        // bridge, so this exact depth (between MAX_INLINE_DEPTH and
        // MAX_PARSE_DEPTH) used to be rejected for BOTH. This task reroutes
        // the Uncompressed case to the canonical live parser bounded only by
        // MAX_PARSE_DEPTH — so it now succeeds, matching what
        // `resolve_borrowed` (which was never subject to MAX_INLINE_DEPTH)
        // already accepted at this depth. The Compressed case now also
        // accepts this depth (see
        // `resolve_object_handle_compressed_member_accepts_inline_nesting_past_max_inline_depth`),
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
        pdf.resolve_object_handle(&handle)
            .expect("nesting between MAX_INLINE_DEPTH and MAX_PARSE_DEPTH must now succeed");

        let legacy = pdf
            .resolve(object_ref)
            .expect("resolve_borrowed must also accept it");
        assert!(legacy.as_array().is_some());
        assert!(handle.as_array().is_some());
    }

    /// The canonical ObjStm route must preserve every direct scalar,
    /// dictionary, and nested indirect-reference variant emitted by qpdf's
    /// live parser. This is the coverage anchor for that source class.
    #[test]
    fn resolve_object_handle_compressed_member_preserves_scalar_dictionary_and_reference_variants()
    {
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
        pdf.resolve_object_handle(&handle)
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
    fn resolve_object_handle_preserves_source_filter_array_alignment() {
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
        pdf.resolve_object_handle(&handle)
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
    /// ref: it must return before touching `qpdf_removed_refs`,
    /// `legacy_materialized_memo`, or `handle_registry` at all.
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
    /// `resolve_object_handle`'s fallback wildcard arm (the one thing this
    /// self-healed silently through before this test existed) gets caught
    /// instead of silently regressing.
    #[test]
    fn delete_object_invalidates_bridge_state_for_a_dangling_ref_seeded_only_via_cache() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Dangling 99 0 R >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open dangling-ref fixture");
        let dangling_ref = ObjectRef::new(99, 0);

        pdf.prepare_qpdf_json_objects()
            .expect("discover the dangling reference");
        assert!(
            matches!(pdf.cache.entry(dangling_ref), Some(CacheEntry::Missing)),
            "prepare_qpdf_json_objects must have seeded the cache as Missing"
        );
        assert!(
            pdf.resolver.registered_handle(dangling_ref).is_none(),
            "no handle must exist yet for this ref"
        );

        pdf.delete_object(dangling_ref);

        assert_eq!(pdf.resolve_borrowed(dangling_ref).unwrap(), &Object::Null);
    }

    /// Design's Parsed-Offset Contract: "An absent, freed, dangling, cyclic,
    /// or otherwise unresolvable indirect object ... resolves to null with
    /// parsed offset -1." Deleting an object whose handle was already
    /// resolved (here, natively parsed with a real source offset) must
    /// reset that offset -- an outstanding clone of the handle must not go
    /// on reporting the deleted object's former body position once it reads
    /// as null.
    #[test]
    fn delete_object_resets_the_parsed_offset_of_an_already_resolved_handle() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Count 1 >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve_object_handle(&handle).expect("resolve");
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
            pdf.resolve(member_ref).expect("resolve ObjStm member"),
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
            pdf.resolve(member_ref).expect("resolve ObjStm member"),
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
    fn replace_object_handle_promotes_resolved_objstm_members_before_replacing_container() {
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
            pdf.resolve(member_ref).expect("resolve ObjStm member"),
            Object::Dictionary(_)
        ));

        pdf.replace_object_handle(ObjectRef::new(1, 0), ObjectHandle::null())
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
        // `replace_object_handle_promotes_resolved_objstm_members_before_replacing_container`,
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
            pdf.resolve(member_ref).expect("resolve ObjStm member"),
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
    fn resolve_object_handle_stamps_the_public_canonical_root_and_nested_handles() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Nested << /Value 7 >> >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

        pdf.resolve_object_handle(&handle)
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
    fn resolve_object_handle_preserves_leading_whitespace_before_a_scalar_description() {
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

        pdf.resolve_object_handle(&handle)
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
    fn resolve_object_handle_gives_canonical_parser_children_the_document_resolver() {
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Outer << /Inner << /Value 7 >> >> >>\nendobj\n"],
            ObjectRef::new(1, 0),
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fixture");
        let handle = pdf.get_object_handle(ObjectRef::new(1, 0));

        pdf.resolve_object_handle(&handle)
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
        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "canonical replacement must not fall back to a legacy materialized memo"
        );
        assert!(
            pdf.legacy_materialized_replacement_refs.is_empty(),
            "canonical replacement must not leave a deferred legacy replacement marker"
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

        assert!(
            pdf.legacy_materialized_memo.is_empty(),
            "canonical replacements must not fall back to the legacy memo"
        );
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
            .resolve_object_handle(&canonical_handle)
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
}
