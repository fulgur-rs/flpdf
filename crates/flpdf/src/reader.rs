//! qpdf correspondence: QPDF.cc object resolution, recovery, diagnostics, and authentication responsibilities.
pub(crate) mod file_object;
pub(crate) mod resolver;

use self::file_object::{
    finish_file_object, parse_file_object_syntax, FileObjectDiagnostic, PendingBody,
    PendingFileObject, RecoveryPolicy, ResolvedStreamLength,
};
use crate::cache::CacheEntry;
use crate::error::EncryptedError;
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
    check_user_password_v4, decrypt_cipher_bytes, decrypt_strings_in_object, StandardHandlerInputs,
    StandardHandlerR5Inputs, StringCipher,
};
use crate::tokenizer::Tokenizer;
use crate::{
    Diagnostics, Dictionary, Error, Object, ObjectHandle, ObjectRef, Result, XrefEntry, XrefForm,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::rc::Rc;

static NULL_OBJECT: Object = Object::Null;

use crate::pdf::Pdf;

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
enum EncryptionMode {
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfOpenOptions {
    /// Enable xref/trailer repair when strict parsing fails.
    pub repair: bool,
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
}

// Maximum number of object streams an `/Extends` chain may link before
// `collect_object_stream_chain` rejects it. The chain is followed by self
// recursion (one stack frame per link), so without this bound an adversarial
// non-cyclic chain — each object stream pointing at a distinct parent — recurses
// until the stack overflows and the process aborts (the no-panic/no-abort
// guarantee's failure mode, same class as the parser-nesting bound). Cycle
// detection alone does not help here because every link is a fresh reference.
// `/Extends` (ISO 32000-2 §7.5.7) chains object streams; real documents go at
// most one or two deep, so 100 only rejects pathological input and matches the
// crate's other tree-walk depth limits.
const MAX_OBJECT_STREAM_CHAIN_DEPTH: usize = 100;

// Stack-growth protection for this module's two recursive hubs: `lift_bounded`
// here, and `ResolverHandle::resolve_indirect` in the `resolver` child module,
// which reaches these as `super::READER_STACK_RED_ZONE`/
// `super::READER_STACK_GROWTH_SIZE`. The values mirror `parser.rs`'s own
// `STACK_RED_ZONE`/`STACK_GROWTH_SIZE` (kept as separate local constants
// rather than imported cross-module, matching this crate's existing per-module
// duplication of the same two numbers in `object_handle.rs`); `resolver.rs`
// shares *these* rather than minting a third pair because it is a child of this
// module, not a module across the crate from it.
const READER_STACK_RED_ZONE: usize = 32 * 1024;
const READER_STACK_GROWTH_SIZE: usize = 1024 * 1024;

impl<R: Read + Seek> Pdf<R> {
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
    pub(crate) fn push_warning(&mut self, message: impl Into<String>) {
        self.resolver.push_warning(message);
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
            self.push_warning(warning);
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

    pub(crate) fn startxref(&self) -> u64 {
        self.startxref
    }

    pub(crate) fn previous_xref_offset(&self) -> u64 {
        self.startxref()
    }

    pub(crate) fn last_xref_form(&self) -> XrefForm {
        self.last_xref_form
    }

    pub(crate) fn source_xref_offsets(&self) -> Vec<(ObjectRef, u64)> {
        self.source_xref_offsets.clone()
    }

    pub(crate) fn source_xref_entries(&self) -> BTreeMap<ObjectRef, XrefEntry> {
        self.resolver.xref_entries()
    }

    pub(crate) fn source_header_offset(&self) -> usize {
        self.resolver.header_offset()
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
        self.compressed_member_parents.get(&object_ref).copied()
    }

    /// Replace `object_ref` with `object` in the in-memory object cache.
    ///
    /// The original on-disk bytes are not touched; an incremental rewrite via
    /// [`crate::write_pdf`] will see the updated value when it walks the cache and emit
    /// a new revision for the touched object. Subsequent [`Pdf::resolve`]/
    /// [`Pdf::resolve_borrowed`] calls for `object_ref` observe `object`
    /// immediately.
    pub fn set_object(&mut self, object_ref: ObjectRef, object: Object) {
        self.qpdf_removed_refs.remove(&object_ref);
        self.qpdf_parsed_xref_streams.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.recovered_stream_eols.remove(&object_ref);
        self.transformed_stream_refs.remove(&object_ref);
        if let Some(CacheEntry::Compressed { stream, index }) =
            self.cache.entry(object_ref).cloned()
        {
            let stream_ref = ObjectRef::new(stream, 0);
            let (parent_ref, parent_index) = self
                .compressed_parent_for_entry(stream_ref, index)
                .unwrap_or((stream_ref, index));
            self.compressed_member_parents
                .insert(object_ref, (parent_ref, parent_index));
        }

        // Write through to the canonical handle graph too (not just
        // `self.cache`): `Pdf::resolve_borrowed` now materializes from the
        // handle, so leaving a stale already-resolved handle in place would
        // make a later resolve of this same ref keep observing the value
        // from *before* this call.
        self.legacy_materialized_memo.remove(&object_ref);
        let handle = self.get_object_handle(object_ref);
        match self.lift_for_set_object(&object, &handle) {
            Ok(value) => {
                handle.set_resolved(value);
                // The value is now caller-supplied, in-memory-constructed
                // data, not something parsed from a source position; any
                // previously recorded offset no longer describes it.
                handle.reset_parsed_offset();
            }
            Err(_) => {
                // `lift`'s bounded-depth guard (mirroring every other
                // post-parse structural walker over an `Object` tree in
                // this crate) cannot represent an excessively deep `object`
                // as an `ObjectHandle` tree. `set_object` is infallible, so
                // store `object` directly as the bridge's authoritative
                // materialized value instead: `resolve`/`resolve_borrowed`
                // must still hand back exactly what the caller set, so a
                // later structural walker (e.g. `optimization.rs`'s own
                // inline-depth guard) is what rejects the excess depth, not
                // `set_object` itself. This is the one authoritative value
                // the caller just supplied, not the "stale value" the
                // invalidate-not-reinsert rule above guards against —
                // `Pdf::resolve_borrowed`'s memo check always prefers it
                // over whatever the (in this case untouched) handle graph
                // holds.
                self.legacy_materialized_memo
                    .insert(object_ref, object.clone());
            }
        }

        self.cache.set_resolved(object_ref, object);
        self.handle_mutated_object_refs.remove(&object_ref);
        self.dirty_object_refs.insert(object_ref);
    }

    // Convert `object` for `Pdf::set_object`'s handle-graph write-through.
    // Identical to `Pdf::lift` except when `object` is a stream and
    // `existing_handle`'s current (pre-overwrite) value is also a stream:
    // the new dictionary is written into the existing stream's own
    // dictionary handle in place (`ObjectHandle::replace_direct_value`),
    // preserving its already-recorded parsed offset and shared identity,
    // rather than minting a fresh dictionary handle that would start at the
    // no-offset sentinel (`stream_dictionary_parsed_offset_survives_resolve_set_object_round_trip`
    // is the regression tripwire for getting this wrong).
    fn lift_for_set_object(
        &mut self,
        object: &Object,
        existing_handle: &ObjectHandle,
    ) -> Result<ObjectValue> {
        if let (Object::Stream(stream), Some(existing_dict)) =
            (object, existing_handle.as_stream_dict())
        {
            let dict_value = ObjectValue::Dictionary(self.lift_dictionary(&stream.dict, 0)?);
            existing_dict.replace_direct_value(dict_value);
            return Ok(ObjectValue::Stream {
                stream_dict: existing_dict,
                stream_data: Some(Rc::new(stream.data.clone())),
                stream_length: 0,
            });
        }
        self.lift(object, 0)
    }

    /// Remove `object_ref`, marking it deleted.
    ///
    /// Subsequent [`Pdf::resolve`]/[`Pdf::resolve_borrowed`] calls for
    /// `object_ref` observe [`Object::Null`], matching the behavior for any
    /// other unknown or freed reference.
    pub fn delete_object(&mut self, object_ref: ObjectRef) {
        if object_ref.number != 0 {
            self.qpdf_removed_refs.insert(object_ref);
        }
        self.qpdf_parsed_xref_streams.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.recovered_stream_eols.remove(&object_ref);
        self.transformed_stream_refs.remove(&object_ref);
        if object_ref.number == 0 {
            return;
        }

        // Invalidate the handle-graph bridge state unconditionally, before
        // the cache-state early return below: `Pdf::prepare_qpdf_json_objects`
        // can mark a ref's cache entry `Missing` (discovered as a dangling
        // reference from a live object's own content) before any handle for
        // it has ever been created via `Pdf::get_object_handle`. If the two
        // lines below ran only past the early return, that combination
        // (cache already `Missing`, handle still `NotYetResolved`) would
        // skip them entirely, leaving a stale-but-unresolved handle whose
        // correctness would then depend on `Pdf::resolve_object_handle`'s
        // fallback arm staying a wildcard forever.
        self.legacy_materialized_memo.remove(&object_ref);
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

    pub(crate) fn source_bytes(&mut self) -> Result<Vec<u8>> {
        self.resolver.read_physical_input()
    }

    /// Number of objects currently resolved in the cache. Useful when you want to
    /// confirm that lazy resolution actually deferred work.
    pub fn resolved_count(&self) -> usize {
        self.cache.resolved_count()
    }

    pub(crate) fn deleted_object_refs(&self) -> Vec<ObjectRef> {
        self.cache.deleted_refs()
    }

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

    /// Every object reference known from the cross-reference table, including objects
    /// that have not yet been parsed.
    pub fn object_refs(&self) -> Vec<ObjectRef> {
        self.cache
            .entries()
            .iter()
            .filter_map(|(object_ref, entry)| {
                (!matches!(entry, CacheEntry::Missing)).then_some(*object_ref)
            })
            .collect()
    }

    /// Object refs that the cross-reference table marks as live.
    ///
    /// Excludes:
    /// - `Deleted` — free entries (from `XrefEntry::Free`) and explicit
    ///   `delete_object()` calls,
    /// - `Missing` — referenced but never present in any xref,
    /// - `Reserved` — forward-reference placeholders that
    ///   [`Pdf::resolve`] returns as `Object::Null` (no real indirect
    ///   object behind them).
    ///
    /// A `live_object_refs()` entry may still resolve to `Object::Null`; that
    /// is a real null indirect object (e.g. `1 0 obj null endobj`), not an
    /// absent one.
    pub fn live_object_refs(&self) -> Vec<ObjectRef> {
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
    }

    /// Resolve every live xref/cache object and register valid indirect
    /// references whose exact generation has no live target. This mirrors the
    /// object-cache preparation performed by qpdf's `fixDanglingReferences()`
    /// for JSON metadata without exposing placeholders through the public
    /// object enumeration APIs.
    pub(crate) fn prepare_qpdf_json_objects(&mut self) -> Result<QpdfPreparedObjects> {
        let live_snapshot = self.qpdf_json_live_object_refs();
        let mut discovered = self.qpdf_trailer_references.clone();
        discovered.extend(self.qpdf_parsed_xref_streams.keys().copied());

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

    /// Locate the linearization hint dictionary if this document is linearized
    /// ("fast web view"). Returns `Ok(None)` for non-linearized documents.
    ///
    /// This resolves object `(1, 0)` and inspects its `/Linearized` entry.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::resolve_borrowed`] while resolving object
    /// `(1, 0)` (for example [`Error::Io`] / [`Error::Parse`] / [`Error::Encrypted`]).
    pub fn linearized_hint_ref(&mut self) -> Result<Option<ObjectRef>> {
        let candidate = ObjectRef::new(1, 0);
        let object = self.resolve_borrowed(candidate)?;
        let Some(dict) = object.as_dict() else {
            return Ok(None);
        };

        let Some(linearized) = dict.get("Linearized") else {
            return Ok(None);
        };

        Ok(match linearized {
            // QPDF::isLinearized accepts only a numeric /Linearized value
            // whose floor is one (QPDF_linearization.cc:139-141).
            Object::Integer(1) => Some(candidate),
            Object::Real(value) | Object::RealLiteral { value, .. }
                if value.is_finite() && value.floor() == 1.0 =>
            {
                Some(candidate)
            }
            _ => None,
        })
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
    pub fn make_indirect_object_handle(&mut self, handle: ObjectHandle) -> Result<ObjectHandle> {
        let Some(value) = handle.direct_value_clone() else {
            return Err(Error::Unsupported(
                "cannot make an already-indirect ObjectHandle indirect".to_string(),
            ));
        };
        let new_ref = self.next_available_object_ref()?;
        let indirect = self.get_object_handle(new_ref);
        indirect.set_resolved(value);
        // QPDF::makeIndirectFromQPDFObject installs the same shared object
        // pointer in obj_cache. `handle_registry` is this port's shared
        // state, and `prepare_qpdf_json_objects` recognizes its resolved
        // cache-miss handles directly; materializing here would duplicate a
        // direct stream's payload into the legacy `Object` cache.
        // A new object left out of the dirty set would never get its own
        // body or xref entry written by a default incremental write,
        // leaving any reference to it dangling — see `mark_object_dirty`'s
        // own doc comment for the full explanation.
        self.mark_object_dirty(new_ref);
        Ok(indirect)
    }

    /// Return an unused generation-zero object reference.
    ///
    /// Both the legacy object cache and the canonical handle registry own
    /// object numbers. A number absent from `object_refs()` may therefore
    /// still belong to an unmaterialized [`ObjectHandle`].
    pub(crate) fn next_available_object_ref(&self) -> Result<ObjectRef> {
        let next_number = self
            .object_refs()
            .iter()
            .map(|r| r.number)
            .chain(self.resolver.max_object_number())
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
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

    /// Mark `object_ref` dirty, so the next default (incremental) call to
    /// [`crate::write_pdf`] includes an updated body and xref entry for it.
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
        self.handle_mutated_object_refs.insert(object_ref);
        self.dirty_object_refs.insert(object_ref);
    }

    /// Mark the canonical owner of `handle` dirty after an in-place
    /// [`ObjectHandle`] mutation. Direct children carry their indirect
    /// containment refs from resolution/insertion, so this never resolves
    /// unrelated objects merely to rediscover an owner.
    pub(crate) fn mark_object_handle_dirty(&mut self, handle: &ObjectHandle) -> Result<()> {
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

    // This implements the ordering/registration contract of qpdf's
    // `getAllObjects()` (`libqpdf/QPDF.cc:1285-1294`) only. qpdf's own
    // dangling-reference preparation additionally walks every live object's
    // content to discover and register references that are syntactically
    // valid but have no live xref/cache target — that step ties into
    // xref/recovery semantics this layer does not own, and is out of scope
    // here; full dangling-reference-preparation parity is flpdf-egzr.3.3's
    // deliverable.
    //
    // qpdf's `m->xref_table` never contains free ("f"/type-0) entries in the
    // first place (`insertFreeXrefEntry` records them in a separate
    // `deleted_objects` set, never in `xref_table`), so `getAllObjects` never
    // sees them. This crate's `source_xref_entries` is a more literal
    // transcription of the on-disk table and *does* retain `XrefEntry::Free`
    // rows (including the object-0 free-list head), so they are filtered out
    // here to match what qpdf's object cache actually ends up holding.
    /// Every indirect object known to this document.
    ///
    /// The result is the union of every reference already registered in the
    /// canonical handle cache (see [`Pdf::get_object_handle`]) and every live
    /// (non-free) reference recorded in the source cross-reference table,
    /// registering any of the latter that is not yet cached. Returned in
    /// `ObjectRef` order (ascending object number, then generation); every
    /// handle in the result is indirect ([`ObjectHandle::is_indirect`]).
    ///
    /// # Errors
    ///
    /// This method never returns an error: every candidate reference is
    /// registered via [`Pdf::get_object_handle`], which cannot fail.
    pub fn get_all_object_handles(&mut self) -> Result<Vec<ObjectHandle>> {
        let refs_to_register: Vec<ObjectRef> = self
            .resolver
            .xref_refs_matching(|entry| !matches!(entry, XrefEntry::Free { .. }));
        for object_ref in refs_to_register {
            self.get_object_handle(object_ref);
        }
        Ok(self.resolver.all_object_handles())
    }

    // qpdf-cutover-delete(flpdf-25kg.3.3): one-hop legacy bridge. Delete
    // after all callers use ObjectHandle's slot-resident dereference; the new
    // primitive must not delegate here.
    //
    // Bridge implementation: delegates to the existing *private*
    // `resolve_to_cache` engine (unchanged) rather than reimplementing
    // decryption, ObjStm decoding, or the cyclic `/Length` guard it already
    // performs. Deliberately calls `resolve_to_cache` (private), NOT the
    // public `resolve_borrowed` — a later task repoints `resolve_borrowed` to
    // call *this* method, so routing through the public method here would
    // recurse.
    /// Resolve `handle` in place if it is an unresolved indirect handle.
    ///
    /// A direct handle, or an indirect handle that has already been
    /// resolved, is a no-op. An indirect handle whose reference is absent
    /// from — or broken in — the cross-reference table resolves to a null
    /// handle rather than an error, matching [`Pdf::resolve`].
    ///
    /// This does not chase through an already-resolved
    /// [`Pdf::set_object`]-driven bare-reference redirect to its terminal
    /// value — see [`Pdf::resolve_object_handle_to_terminal`] for that.
    /// [`Pdf::resolve_borrowed`] (and everything built on it, notably
    /// `ref_chain.rs`'s own bounded chain-follow primitive) depends on this
    /// method exposing exactly one hop per call, not silently collapsing a
    /// multi-hop chain.
    ///
    /// This reuses the same decryption, object-stream, and stream-`/Length`
    /// resolution behavior as [`Pdf::resolve`]. For a plain uncompressed file
    /// object, the resolved handle (and every direct child in its tree)
    /// carries a real parsed offset from a native parse of the object's own
    /// source bytes, per qpdf's `getParsedOffset` contract. A compressed
    /// (object-stream member) reference keeps the no-offset sentinel: its
    /// object-stream-relative parsed offset is not yet populated. In the
    /// rare case where the object's cross-reference layout is malformed or
    /// overlapping enough that the native parse's own read window is
    /// insufficient, this falls back to the no-offset sentinel rather than
    /// failing.
    ///
    /// This method never fails on a plain nesting/malformed-layout case
    /// alone: it lifts the already-resolved cached value against the
    /// parser's own nesting bound, not the tighter bound other structural
    /// walkers in this crate use, so it never rejects a value
    /// [`Pdf::resolve`]/[`Pdf::resolve_borrowed`] would have accepted.
    ///
    /// # Errors
    ///
    /// Has the same error behavior as [`Pdf::resolve_borrowed`]: I/O, parse,
    /// or decryption failures from resolution propagate. An unknown, freed,
    /// or compressed-but-broken reference is **not** an error; it resolves to
    /// a null handle.
    pub fn resolve_object_handle(&mut self, handle: &ObjectHandle) -> Result<()> {
        let Some(object_ref) = handle.object_ref() else {
            return Ok(()); // direct handle: already has a value
        };
        if handle.is_resolved() {
            return Ok(());
        }

        self.resolve_to_cache(object_ref)?;
        let object = match self.cache.entry(object_ref) {
            Some(CacheEntry::Resolved(object)) => object.clone(),
            // A cyclic/self-referential resolution already in progress
            // higher up the call stack (e.g. `resolve_pending_stream_length`
            // resolving this same ref's indirect `/Length` holder, directly
            // or transitively) leaves the cache entry `Reserved`, not
            // genuinely missing — `resolve_to_cache` itself uses this state
            // to break the cycle (see its own doc). Leave the handle
            // unresolved rather than marking it permanently `Missing`: once
            // the outer resolution completes, a later call must still
            // resolve the real value, not keep observing a placeholder.
            // `Pdf::resolve_borrowed` mirrors this same distinction so it
            // never memoizes this transient observation.
            Some(CacheEntry::Reserved) => return Ok(()),
            _ => {
                handle.set_missing();
                return Ok(());
            }
        };

        // Deciding native-parse vs. the legacy `lift` bridge requires the
        // object's *xref-entry* classification, not its already-resolved
        // `Object` shape (a `CacheEntry` alone can't distinguish "was always
        // uncompressed" from other cache states after mutation) — consult
        // the source xref table, matching the design's Parsed-Offset
        // Contract table row-by-row: only `Uncompressed` file objects gain
        // real offsets in this layer.
        //
        // The native parse below reads only the fast bounded-to-the-next-
        // object window (`read_bounded_object_window`), not
        // `read_object_at_with_policy`'s rare full-file fallback for a
        // malformed/overlapping source xref layout (e.g. a stray xref entry
        // whose offset lands inside another object's body, truncating the
        // window `next_object_offset` computes). `resolve_to_cache` above
        // already resolved `object` successfully — via that same fallback,
        // if the fast window needed it — so a native-parse failure here
        // does not mean the object is unresolvable; it means only this
        // reparse's narrower window was insufficient. Falling back to the
        // already-correct `lift(&object, 0)` (offset sentinel, exactly the
        // Compressed-branch expression below) guarantees
        // `resolve_object_handle` never fails where `resolve_borrowed`/
        // `resolve` already succeeded.
        // A stream in `self.transformed_stream_refs` had its payload bytes
        // *and*, for an explicit `/Crypt` filter, its own dictionary
        // (`/Filter`/`/DecodeParms` with the consumed `Crypt` entries
        // stripped) rewritten by `decrypt_resolved_object`/
        // `apply_explicit_crypt_filters` — transformations the native parse
        // below cannot see, since it reads the dictionary straight from raw
        // source bytes rather than from `object`. Native-parsing it anyway
        // would silently resurrect the pre-decryption `/Filter` (with a
        // still-present `/Crypt` entry the caller can no longer decode) and
        // reuse `object`'s already-transformed *data* against that stale
        // dictionary. Route it through `lift(&object, 0)` instead, which
        // copies both the transformed dictionary and the transformed data
        // from `object` directly — correct, at the cost of losing native
        // parsed offsets for this ref (conservative: this also applies to a
        // stream that was merely decrypted without an explicit crypt filter,
        // whose dictionary was not actually rewritten, since
        // `transformed_stream_refs` does not distinguish the two cases).
        //
        // Both arms below lift against `parser::MAX_PARSE_DEPTH`, not
        // `lift`'s default `MAX_INLINE_DEPTH`: `object` already parsed
        // successfully at that looser bound (via `resolve_to_cache` above),
        // so re-lifting it through the tighter structural-walker bound would
        // reject a value `resolve_borrowed`/`resolve` always accepted,
        // breaking this function's own "never fails where `resolve_borrowed`/
        // `resolve` already succeeded" guarantee for any Compressed
        // (ObjStm-member) object — or, more rarely, an Uncompressed one whose
        // fast native-parse window failed for an unrelated reason — with
        // literal nesting between the two bounds.
        let mut native_parsed = false;
        let (mut value, parsed_offset) = match self.resolver.xref_entry(object_ref) {
            Some(XrefEntry::Uncompressed { offset })
                if !self.transformed_stream_refs.contains(&object_ref) =>
            {
                match self.native_parse_uncompressed_value(offset, &object) {
                    Ok(native) => {
                        native_parsed = true;
                        native
                    }
                    Err(_) => (
                        self.lift_bounded(&object, 0, crate::parser::MAX_PARSE_DEPTH)?,
                        NO_PARSED_OFFSET,
                    ),
                }
            }
            _ => (
                self.lift_bounded(&object, 0, crate::parser::MAX_PARSE_DEPTH)?,
                NO_PARSED_OFFSET,
            ),
        };
        // Only the native-parse success path above reads raw (possibly
        // still-encrypted) source bytes directly into `ObjectValue::String`;
        // every other route here lifts from `object`, which
        // `resolve_to_cache` already decrypted through the legacy engine's
        // own pipeline -- decrypting it again would corrupt it. See
        // `native_parse_uncompressed_value`'s own doc for why this handle
        // graph population, not `Pdf::resolve_borrowed`'s later
        // materialization, is where this must happen: nested children here
        // are already-populated `ObjectHandle`s carrying their own recorded
        // parsed offsets, and round-tripping through `materialize`/`lift` to
        // reach a legacy-engine decryption path would silently reset them.
        if native_parsed {
            // `borrow_mut` for the same reason as `decrypt_resolved_object`:
            // qpdf's unknown-filter arm rewrites `cf_string`.
            let mut encryption_guard = self.encryption.borrow_mut();
            let warn = match encryption_guard.as_mut() {
                Some(encryption) => {
                    decrypt_object_value_strings(object_ref, &mut value, encryption)?
                }
                None => false,
            };
            drop(encryption_guard);
            self.warn_unknown_crypt_filters(warn, false);
        }
        handle.set_resolved(value);
        handle.set_parsed_offset_if_unset(parsed_offset);
        Ok(())
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
    /// If `handle`'s value is not a redirect at all — the common case —
    /// this returns `handle.clone()` unchanged: an `Rc` clone of the same
    /// live, canonical, still-indirect handle, so mutating it behaves
    /// exactly as mutating `handle` itself always would. Otherwise the
    /// returned handle is a **direct**, independent copy of the terminal
    /// value ([`ObjectHandle::shallow_copy`]'s own recursive-through-direct-
    /// descendants semantics: any nested indirect child stays canonically
    /// shared, but every direct one — including the top level — is its own
    /// copy), and `handle` itself, and every intermediate hop's own
    /// canonical handle (as [`Pdf::get_object_handle`] would return it), are
    /// never mutated. In this case mutating the returned handle (`replace_key`,
    /// `insert_key`, …) has no effect on anything the writer will ever
    /// observe: it carries no indirect identity of its own, so there is
    /// nothing a caller could pass to `mark_object_dirty` to make the edit
    /// visible even by mistake. This matters because `handle` and each
    /// intermediate hop are the *same* canonical handles
    /// [`Pdf::resolve_borrowed`] resolves through — overwriting one in place
    /// would permanently change what a later `resolve_borrowed` call for
    /// that same ref returns, silently discarding the redirect
    /// `Pdf::set_object` recorded.
    ///
    /// `qpdf-cutover-delete(flpdf-25kg.3.3)`: terminal-clone legacy API.
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
    /// back to a null handle. Otherwise it is `handle.object_ref()` when no
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
        for _ in 0..crate::ref_chain::MAX_REF_CHAIN_DEPTH {
            let hop = self.get_object_handle(current_ref);
            self.resolve_object_handle(&hop)?;
            match hop.as_reference() {
                Some(next) => current_ref = next,
                // `hop.shallow_copy()` recursively copies every *direct*
                // array/dictionary/stream-dict descendant independently,
                // sharing `Rc` identity only with a genuinely *indirect*
                // descendant (see its own doc) — required here, not just a
                // nicety: a single-level clone would leave a direct nested
                // child Rc-shared with `hop`'s own canonical value, so
                // mutating it through the returned handle (e.g.
                // `result.get_key(...).replace_key(...)`) would silently
                // mutate the real document too. It also already handles the
                // transient `CacheEntry::Reserved` cycle guard
                // `resolve_object_handle`'s own doc describes (`hop` stays
                // unresolved → `shallow_copy` gives a direct null handle,
                // matching `resolve_borrowed`'s own transient placeholder).
                // No canonical state is written either way, so a later call
                // simply redoes the chase rather than being stuck observing
                // a stale result.
                None => return Ok((hop.shallow_copy(), Some(current_ref))),
            }
        }
        self.push_warning(format!(
            "reference redirect chain reaching object {} {} exceeds \
             {} hops, treating as cyclic",
            current_ref.number,
            current_ref.generation,
            crate::ref_chain::MAX_REF_CHAIN_DEPTH
        ));
        // Ref and handle degrade together: a null value paired with a
        // live-looking ref would let a caller compute an "offset of
        // terminal" for an object it was just told is null.
        Ok((ObjectHandle::null(), None))
    }

    // Builds the resolved value for a plain uncompressed file object by
    // parsing its own source bytes directly into `ObjectValue`/`ObjectHandle`
    // in a single pass, so every node (including nested children) carries a
    // real parsed offset from construction — never from a second pass over
    // `object` (design, "Parser" section: no parallel metadata tree, no
    // reparse-for-provenance). `object` is the value `resolve_to_cache`
    // already fully resolved above (decryption, indirect `/Length`
    // resolution, and stream-boundary recovery already applied); this reuses
    // only its final stream *data* bytes below, which the native pass does
    // not attempt to rederive (recovery/decryption stay entirely on the
    // untouched legacy path).
    //
    // A native-parsed `ObjectValue::String` carries the *raw* (possibly
    // still-encrypted) source bytes, not `object`'s already-decrypted string
    // value (`ObjectValue::Name` has no such gap: PDF names are never
    // encrypted regardless of path). This is real, output-visible ciphertext
    // unless corrected: `Pdf::resolve_borrowed`'s `decrypt_materialized_strings`
    // is the accessor that corrects it, applying `decrypt_object_strings` to
    // the materialized `Object` for exactly a handle populated via this
    // function (gated on the handle's own non-sentinel parsed offset, which
    // only this function's success path sets). Do not remove or weaken that
    // gate without re-deriving this fix: doing so silently reintroduces
    // ciphertext leaking through the public `resolve`/`resolve_borrowed` API
    // for any encrypted document whose `/Info` (or any other string-bearing
    // dictionary) is not object-stream-compressed.
    //
    // `read_bounded_object_window` reads only the fast "bounded to the next
    // object's offset" window, not `read_object_at_with_policy`'s rare
    // full-file fallback for a malformed or overlapping source xref layout
    // (e.g. `stream_with_false_next_xref_offset` in this module's own
    // tests, whose bogus xref entry truncates the window mid-dictionary).
    // An `Err` here therefore does not mean the object is unresolvable —
    // `resolve_to_cache` already fully resolved `object` above, via that
    // same fallback if the fast window needed it — so the caller
    // (`resolve_object_handle`) treats this function's error as "fall back
    // to `lift`", never as a hard failure.
    fn native_parse_uncompressed_value(
        &mut self,
        offset: u64,
        object: &Object,
    ) -> Result<(ObjectValue, i64)> {
        let bytes = self.read_bounded_object_window(offset)?;
        let mut tokenizer = Tokenizer::new(&bytes);
        let _number = tokenizer.next_integer()?;
        let _generation = tokenizer.next_integer()?;
        tokenizer.expect_word(b"obj")?;
        tokenizer.skip_ignorable()?;
        let body_start = tokenizer.position();

        let file_origin = i64::try_from(offset).unwrap_or(i64::MAX);
        let base_offset = file_origin + body_start as i64;
        let (value, value_offset) =
            crate::parser::parse_qpdf_direct_object_handle(&bytes[body_start..], base_offset, self)
                .map_err(|error| error.rebase_offset(body_start))?;

        let Object::Stream(stream) = object else {
            return Ok((value, value_offset));
        };

        // The dictionary and stream-data-start positions came from the
        // exact same deterministic, recovery-independent scan
        // (`parse_file_object_syntax`, unmodified) that the legacy pipeline
        // above already used to classify this object as a stream in the
        // first place — reused here only for that one numeric fact, not
        // reparsed for its (discarded) `Object` value.
        let pending = parse_file_object_syntax(&bytes)?;
        let data_start = match pending.body {
            PendingBody::Stream { data_start, .. } => data_start,
            // Defensive, not reachable in practice: `object` is already
            // confirmed `Object::Stream` (checked above), and `object` can
            // only be `Object::Stream` if either (a) the bounded window
            // above already contains the "stream" keyword — so this
            // identical-bytes reparse would also classify it as
            // `PendingBody::Stream`, matching case `Stream{..}` above — or
            // (b) `resolve_to_cache` needed the full-file fallback, which
            // only triggers on a hard parse failure, meaning this
            // function's own dictionary parse (shared decision functions,
            // identical window) would have already propagated that same
            // failure via `?` a few lines up, never reaching this match at
            // all. See `resolve_object_handle_tracks_legacys_dictionary_vs_stream_classification`,
            // which empirically confirms the seemingly-plausible third case
            // (bounded window sees the dictionary but not "stream", while
            // `resolve_to_cache` still resolves a real stream) cannot occur:
            // the legacy engine hits the identical ambiguity first and
            // resolves `object` to a bare `Object::Dictionary` instead,
            // which the `Object::Stream` check above would already have
            // routed around. Erroring here rather than silently treating
            // some other position as the stream's own parsed offset is a
            // deliberate belt-and-suspenders choice: if this reasoning is
            // ever wrong, the caller's fallback to `lift(&object, 0)` is
            // strictly safer than fabricating a number.
            // cov:ignore-start: unreachable per the invariants noted above
            PendingBody::Direct { .. } => {
                return Err(Error::parse(0, "native parse: expected stream framing"));
            } // cov:ignore-end
        };
        let dict_handle = ObjectHandle::from_value(value);
        dict_handle.set_parsed_offset_if_unset(value_offset);
        let stream_offset = file_origin + data_start as i64;
        Ok((
            ObjectValue::Stream {
                stream_dict: dict_handle,
                stream_data: Some(Rc::new(stream.data.clone())),
                stream_length: 0,
            },
            stream_offset,
        ))
    }

    // Convert a legacy `Object` into an `ObjectValue`. Called for Compressed
    // (ObjStm-member) objects (which can never themselves be
    // `Object::Stream`), as `resolve_object_handle`'s fallback when the
    // native parse's own (narrower) read window fails on a malformed or
    // overlapping source xref layout even though the object resolved fine
    // via `resolve_to_cache`'s own full-file fallback, and by
    // `Pdf::set_object`/`Pdf::lift_for_set_object` to write a caller-supplied
    // `Object` through to the handle graph (the one caller for which
    // `object` need not already be a legacy-engine-resolved value — it can
    // be anything a consumer passes to `set_object`, including a bare
    // `Object::Reference` used throughout this crate to redirect or
    // collapse a holder chain in place — see `ObjectValue::Reference`'s own
    // doc). The content-stream-only `Object::Operator`/`Object::InlineImage`
    // variants return `Err`, routing `Pdf::set_object`'s caller to its own
    // "cannot be represented in the handle graph" fallback (see that
    // function's comment) instead of losing the value to a silent `Null`.
    //
    // `depth` bounds inline `Array`/`Dictionary`/`Stream`-dictionary nesting
    // against `MAX_INLINE_DEPTH`, mirroring every other post-parse structural
    // walker over an `Object` tree in this crate (`subset_prune.rs`,
    // `object_copy.rs`, `page_closure.rs`, `rewrite_renumber.rs`, and
    // others) — this is a separate, tighter bound than the parser's own
    // `MAX_PARSE_DEPTH`.
    pub(crate) fn lift(&mut self, object: &Object, depth: usize) -> Result<ObjectValue> {
        self.lift_bounded(object, depth, crate::object::MAX_INLINE_DEPTH)
    }

    // Same as `lift`, but against a caller-chosen `max_depth` instead of the
    // fixed `MAX_INLINE_DEPTH`. `resolve_object_handle`'s Compressed-member
    // (ObjStm) fallback is the only caller that needs this: `object` there
    // already parsed successfully up to the parser's own `MAX_PARSE_DEPTH`
    // (via `resolve_to_cache`), so re-lifting it through the tighter
    // structural-walker bound would reject a value `resolve_borrowed` always
    // accepted, breaking the "never fails where `resolve_borrowed`/`resolve`
    // already succeeded" invariant documented at that call site. Every other
    // caller keeps going through `lift` (i.e. `MAX_INLINE_DEPTH`) unchanged.
    fn lift_bounded(
        &mut self,
        object: &Object,
        depth: usize,
        max_depth: usize,
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
        // native parse path (`parser.rs`, itself `stacker`-protected), this
        // lift path has no protection of its own, and `Pdf::trailer_key_handle`
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
                        .map(|item| self.lift_to_handle_bounded(item, depth + 1, max_depth))
                        .collect::<Result<Vec<_>>>()?,
                ),
                Object::Dictionary(dict) => {
                    ObjectValue::Dictionary(self.lift_dictionary_bounded(dict, depth, max_depth)?)
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
                Object::Stream(stream) => ObjectValue::Stream {
                    stream_dict: ObjectHandle::from_value(ObjectValue::Dictionary(
                        self.lift_dictionary_bounded(&stream.dict, depth, max_depth)?,
                    )),
                    stream_data: Some(Rc::new(stream.data.clone())),
                    stream_length: 0,
                },
                // A bare top-level reference never comes from a file/ObjStm
                // parse (`top_level_no_reference` integerizes it there,
                // matching qpdf), but `Pdf::set_object` callers pass one
                // directly throughout this crate to redirect or collapse a
                // holder chain in place (`ObjectRef` -> `ObjectRef`, no
                // recursive follow) -- `ObjectValue::Reference` is the handle
                // graph's representation for exactly that case; see its own
                // doc.
                Object::Reference(object_ref) => ObjectValue::Reference(*object_ref),
                // Content-stream-only tokens; never a resolved file/ObjStm
                // object value, and not a value any caller passes to
                // `Pdf::set_object` in practice. `ObjectValue` has no variant to
                // represent either losslessly, so this returns `Err` rather than
                // silently discarding the caller-supplied value as `Null`:
                // `Pdf::set_object` already treats a `lift` failure as "cannot be
                // represented in the handle graph" and falls back to storing
                // `object` directly as the authoritative
                // `legacy_materialized_memo` value instead (see its own comment),
                // exactly the same route the excess-depth case already takes.
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

    // Shared by `lift`'s `Object::Dictionary`/`Object::Stream` arms and by
    // `Pdf::lift_for_set_object`: lift every entry of `dict` one level
    // deeper than `depth`, matching `lift`'s own depth bound.
    fn lift_dictionary(
        &mut self,
        dict: &Dictionary,
        depth: usize,
    ) -> Result<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> {
        self.lift_dictionary_bounded(dict, depth, crate::object::MAX_INLINE_DEPTH)
    }

    // Same as `lift_dictionary`, but threading a caller-chosen `max_depth`
    // through to every entry — see `lift_bounded`.
    fn lift_dictionary_bounded(
        &mut self,
        dict: &Dictionary,
        depth: usize,
        max_depth: usize,
    ) -> Result<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> {
        dict.iter()
            .map(|(k, v)| {
                Ok((
                    k.to_vec(),
                    self.lift_to_handle_bounded(v, depth + 1, max_depth)?,
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
    // and wrapped in a fresh direct handle. `max_depth` bounds inline nesting
    // the same way `lift_bounded` does — see its own comment.
    pub(crate) fn lift_to_handle_bounded(
        &mut self,
        object: &Object,
        depth: usize,
        max_depth: usize,
    ) -> Result<ObjectHandle> {
        match object {
            Object::Reference(object_ref) => Ok(self.get_object_handle(*object_ref)),
            direct => {
                let value = self.lift_bounded(direct, depth, max_depth)?;
                Ok(ObjectHandle::from_value(value))
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
        // Check the memo *before* resolving/materializing: `Pdf::set_object`
        // can write an authoritative override directly into
        // `legacy_materialized_memo` for a value `lift` cannot represent as
        // an `ObjectHandle` tree (see its own comment) without updating the
        // handle graph at all. If we resolved the handle unconditionally
        // first, `resolve_object_handle`'s own attempt to lift that same
        // value for a *different*, offset-less path (e.g. a freshly
        // allocated ref with no source xref entry) could propagate an `Err`
        // via `?` before this method ever reached the memo that already has
        // the right answer. Once the memo has an entry for `object_ref`, it
        // is authoritative and the handle is not consulted at all.
        if !self.legacy_materialized_memo.contains_key(&object_ref) {
            let handle = self.get_object_handle(object_ref);
            self.resolve_object_handle(&handle)?;

            if !handle.is_resolved() {
                // A cyclic/self-referential resolution already in progress
                // higher up the call stack (`resolve_pending_stream_length`
                // calls `resolve_borrowed` directly to look up an indirect
                // `/Length` holder, which can transitively re-enter this
                // same ref while it is still being resolved) left this
                // ref's cache entry `Reserved` — `resolve_object_handle`
                // deliberately leaves the handle unresolved for exactly
                // this case rather than marking it permanently `Missing`.
                // Return the same transient `Object::Null` the untouched
                // legacy engine's own cache-reading `resolve_borrowed`
                // always returned here, but do NOT memoize it: once the
                // outer resolution completes, a later call must still
                // resolve the real value instead of being stuck serving
                // this placeholder forever.
                return Ok(&NULL_OBJECT);
            }

            // A stream handle at the no-offset sentinel was populated by
            // `lift` (`Pdf::set_object`'s write-through, or
            // `resolve_object_handle`'s narrow native-parse-failure
            // fallback) directly from the `Object` already sitting in
            // `self.cache` for this same ref — `lift`'s `Object::Stream` arm
            // builds the handle's value from exactly that cached `Object`.
            // Serve the cache's own reference here instead of materializing
            // a second copy: `materialize()` would clone the stream's
            // (potentially huge) payload bytes yet again, on top of the
            // clone `lift` already made getting the value into the handle
            // graph in the first place — three copies of the same buffer
            // alive at once for a single `set_object` + `resolve_borrowed`
            // round trip. `borrowed_qpdf_resolution_preserves_historical_stream_fallback_without_clone`
            // is the regression test for this.
            //
            // Correctness here rests on an invariant spanning three
            // functions that mutate `self.cache` and `self.handle_registry`
            // independently: whenever a sentinel-offset handle resolves to
            // `ObjectValue::Stream`, `self.cache`'s entry for the same ref
            // must already be the value-equal `Object::Stream` that handle
            // was lifted from (true for `set_object`'s write-through and for
            // `resolve_object_handle`'s native-parse-failure fallback, both
            // of which lift directly from the same `object` that is — or
            // was just — written into `self.cache`). A future edit to
            // `Pdf::lift`, `Pdf::set_object`, or `Pdf::resolve_object_handle`
            // that breaks this pairing would only be caught by the
            // no-extra-clone regression test above if it also happened to
            // change the pointer; a value-only divergence would not be.
            // `mark_object_dirty` deliberately invalidates that invariant
            // after an in-place ObjectHandle mutation (for example an
            // EmbeddedFile `/Subtype` update): the cache still holds the
            // old stream while the live handle holds the new dictionary.
            // In that case materialize only on the next observation instead
            // of copying attachment data at mutation time.
            if handle.get_parsed_offset() < 0
                && !self.handle_mutated_object_refs.contains(&object_ref)
            {
                if let Some(CacheEntry::Resolved(cached @ Object::Stream(_))) =
                    self.cache.entry(object_ref)
                {
                    return Ok(cached);
                }
            }

            // `handle`'s own resolved value is already correctly decrypted
            // regardless of route: `resolve_object_handle`'s native-parse
            // branch decrypts strings at population time (see its own
            // comment), and every other route lifts from `object`, which
            // `resolve_to_cache` already decrypted through the legacy
            // engine's own pipeline. `materialize()` below is a plain
            // structural copy, nothing left to decrypt here.
            let materialized = handle.materialize()?;
            self.legacy_materialized_memo
                .insert(object_ref, materialized);
        }

        Ok(self
            .legacy_materialized_memo
            .get(&object_ref)
            .unwrap_or(&NULL_OBJECT))
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
    // window read and by `native_parse_uncompressed_value`'s single read —
    // extracted so the two never drift on how the fast-path window is
    // computed.
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
        let resolved_object = match self.resolve_borrowed(holder) {
            Ok(Object::Integer(value)) => Ok(Some(ResolvedStreamLength::Integer(*value))),
            Ok(Object::Null) => Ok(None),
            Ok(_) => Ok(Some(ResolvedStreamLength::Invalid)),
            Err(Error::Parse { .. }) => Ok(Some(ResolvedStreamLength::Invalid)),
            Err(err) => Err(err),
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
        if self.cache.entry(object_ref).is_none() {
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

        Ok(self
            .qpdf_parsed_xref_streams
            .get(&object_ref)
            .cloned()
            .unwrap_or(Object::Null))
    }

    /// Resolve a qpdf-visible object without cloning its cached value.
    ///
    /// Unlike [`Self::resolve_borrowed`], this retains the historical xref-stream
    /// fallback used by qpdf JSON preparation when the object is absent from the
    /// live object cache.
    pub(crate) fn resolve_qpdf_json_object_borrowed(
        &mut self,
        object_ref: ObjectRef,
    ) -> Result<&Object> {
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
            _ => Ok(self
                .qpdf_parsed_xref_streams
                .get(&object_ref)
                .unwrap_or(&NULL_OBJECT)),
        }
    }

    /// Offset of the first recorded object that starts strictly after `offset`,
    /// or `None` when `offset` belongs to the last object in the file.
    fn next_object_offset(&self, offset: u64) -> Option<u64> {
        let index = self.sorted_object_offsets.partition_point(|&o| o <= offset);
        self.sorted_object_offsets.get(index).copied()
    }

    fn resolve_to_cache(&mut self, object_ref: ObjectRef) -> Result<bool> {
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
                self.record_file_object_diagnostics(object_ref, offset, parsed.diagnostics);
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
    ) {
        for diagnostic in diagnostics {
            self.push_warning(format!(
                "(object {} {}, offset {}): {}",
                object_ref.number,
                object_ref.generation,
                offset.saturating_add(diagnostic.relative_offset as u64),
                diagnostic.kind.message()
            ));
        }
    }

    fn resolve_compressed_entry(
        &mut self,
        object_ref: ObjectRef,
        stream: u32,
        index: u32,
    ) -> Result<bool> {
        let stream_ref = ObjectRef::new(stream, 0);
        let stream_object = match self.cache.entry(stream_ref).cloned() {
            Some(CacheEntry::Resolved(object)) => object,
            Some(CacheEntry::Unresolved { offset }) => {
                let policy = RecoveryPolicy::RequireTokenTerminator;
                let mut parsed =
                    self.read_object_at_with_policy(stream_ref, offset, policy, policy)?;
                if parsed.object_ref != stream_ref {
                    return Ok(false);
                }
                let recovered_eol = parsed.remove_included_recovery_eol_for_decryption();
                let recovered_eol_bytes =
                    recovered_eol.map(crate::parser::RecoveredStreamEol::as_bytes);
                let (object, stream_payload_transformed) =
                    self.decrypt_resolved_object(stream_ref, parsed.object, recovered_eol_bytes)?;
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
                self.record_file_object_diagnostics(stream_ref, offset, parsed.diagnostics);
                object
            }
            Some(
                CacheEntry::Compressed { .. }
                | CacheEntry::Missing
                | CacheEntry::Deleted
                | CacheEntry::Reserved,
            )
            | None => return Ok(false),
        };

        let Some(stream_object) = stream_object.into_stream() else {
            return Ok(false);
        };

        let (parent_ref, parent_index, parsed) =
            self.parse_object_stream_chain_entry(stream_ref, &stream_object, index)?;
        let ParsedObjectStreamEntry {
            object,
            diagnostics,
        } = parsed;
        let (object, _stream_payload_transformed) =
            self.decrypt_resolved_object(object_ref, object, None)?;
        self.compressed_member_parents
            .insert(object_ref, (parent_ref, parent_index));
        self.cache.set_resolved(object_ref, object);
        self.record_object_stream_diagnostics(parent_ref, object_ref, diagnostics);
        Ok(true)
    }

    fn decrypt_resolved_object(
        &self,
        object_ref: ObjectRef,
        mut object: Object,
        recovered_stream_eol: Option<&[u8]>,
    ) -> Result<(Object, bool)> {
        // `borrow_mut` because qpdf's crypt-filter switch rewrites `cf_string`
        // and `cf_stream` on its unknown-filter arm. The guard is dropped
        // before any warning is pushed, since that borrows the resolver.
        let mut encryption_guard = self.encryption.borrow_mut();
        let Some(encryption) = encryption_guard.as_mut() else {
            return Ok((object, false));
        };
        if Some(object_ref) == encryption.encrypt_ref {
            return Ok((object, false));
        }

        // qpdf `QPDF::decryptString` (`libqpdf/QPDF_encryption.cc:977-1039`).
        let warn_unknown_string = decrypt_object_strings(object_ref, &mut object, encryption)?;

        let mut stream_payload_transformed = false;
        let mut warn_unknown_stream = false;
        if let Object::Stream(stream) = &mut object {
            if !encryption.encrypt_metadata && is_metadata_stream(&stream.dict) {
                drop(encryption_guard);
                self.warn_unknown_crypt_filters(warn_unknown_string, false);
                return Ok((object, false));
            } else if stream_has_explicit_crypt_filter(&stream.dict) {
                warn_unknown_stream = apply_explicit_crypt_filters(
                    object_ref,
                    stream,
                    encryption,
                    recovered_stream_eol,
                )?;
                stream_payload_transformed = true;
            } else {
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
        drop(encryption_guard);
        self.warn_unknown_crypt_filters(warn_unknown_string, warn_unknown_stream);
        Ok((object, stream_payload_transformed))
    }

    /// qpdf's unknown-crypt-filter warnings, in the order qpdf emits them for
    /// one object: strings from `QPDF::decryptString`
    /// (`libqpdf/QPDF_encryption.cc:1000-1001`) before streams from
    /// `QPDF::decryptStream` (`:1123-1129`).
    ///
    /// Each fires at most once per document because the selection that
    /// requests it also rewrites the crypt filter it complained about.
    fn warn_unknown_crypt_filters(&self, strings: bool, streams: bool) {
        if strings {
            self.resolver.push_warning(
                "unknown encryption filter for strings (check /StrF in /Encrypt dictionary); \
                 strings may be decrypted improperly",
            );
        }
        if streams {
            self.resolver.push_warning(
                "unknown encryption filter for streams (check /StmF from /Encrypt dictionary); \
                 streams may be decrypted improperly",
            );
        }
    }

    fn parse_object_stream_chain_entry(
        &mut self,
        stream_ref: ObjectRef,
        stream_object: &crate::Stream,
        target_index: u32,
    ) -> Result<(ObjectRef, u32, ParsedObjectStreamEntry)> {
        let (member_stream_ref, member_index, member_stream) =
            self.object_stream_chain_member(stream_ref, stream_object, target_index)?;
        let parsed = parse_object_stream_entry(&member_stream, member_index)?;
        Ok((member_stream_ref, member_index, parsed))
    }

    fn record_object_stream_diagnostics(
        &mut self,
        stream_ref: ObjectRef,
        object_ref: ObjectRef,
        diagnostics: Vec<crate::parser::ParserDiagnostic>,
    ) {
        for diagnostic in diagnostics {
            self.push_warning(format!(
                "object stream {} (object {} {}, offset {}): {}",
                stream_ref.number,
                object_ref.number,
                object_ref.generation,
                diagnostic.relative_offset,
                diagnostic.message
            ));
        }
    }

    fn compressed_parent_for_entry(
        &mut self,
        stream_ref: ObjectRef,
        target_index: u32,
    ) -> Result<(ObjectRef, u32)> {
        let stream_object = self.resolve_borrowed(stream_ref)?;
        let Some(stream_object) = stream_object.as_stream().cloned() else {
            return Err(Error::parse(0, "compressed parent is not an object stream"));
        };
        let (parent_ref, parent_index, _) =
            self.object_stream_chain_member(stream_ref, &stream_object, target_index)?;
        Ok((parent_ref, parent_index))
    }

    fn object_stream_chain_member(
        &mut self,
        stream_ref: ObjectRef,
        stream_object: &crate::Stream,
        target_index: u32,
    ) -> Result<(ObjectRef, u32, crate::Stream)> {
        let mut streams = Vec::new();
        self.collect_object_stream_chain(
            stream_ref,
            stream_object,
            &mut streams,
            &mut BTreeSet::new(),
        )?;

        let target_index = usize::try_from(target_index)
            .map_err(|_| Error::parse(0, "compressed object index does not fit usize"))?;
        let mut remaining = target_index;
        for (member_stream_ref, member_stream) in streams {
            let member_count = object_stream_count(&member_stream)?;
            if remaining < member_count {
                let member_index = u32::try_from(remaining)
                    .map_err(|_| Error::parse(0, "compressed object index does not fit u32"))?;
                return Ok((member_stream_ref, member_index, member_stream));
            }
            remaining -= member_count;
        }

        Err(Error::parse(
            0,
            "compressed object index out of range for object stream chain",
        ))
    }

    fn collect_object_stream_chain(
        &mut self,
        stream_ref: ObjectRef,
        stream_object: &crate::Stream,
        streams: &mut Vec<(ObjectRef, crate::Stream)>,
        seen: &mut BTreeSet<ObjectRef>,
    ) -> Result<()> {
        // `seen` starts empty at the entry call and grows by one per `/Extends`
        // hop, so `seen.len()` is the current recursion depth. Bound it before
        // descending another level to keep the stack from overflowing on a long
        // non-cyclic chain. Checked before the cycle insert below so a too-deep
        // chain and a cyclic one surface as distinct errors.
        if seen.len() >= MAX_OBJECT_STREAM_CHAIN_DEPTH {
            return Err(Error::parse(0, "object stream /Extends chain too deep"));
        }
        if !seen.insert(stream_ref) {
            return Err(Error::parse(0, "object stream /Extends cycle"));
        }

        if let Some(parent_ref) = stream_object.dict.get_ref("Extends") {
            let parent_object = self.resolve_borrowed(parent_ref)?;
            let Some(parent_stream) = parent_object.as_stream().cloned() else {
                return Err(Error::parse(0, "object stream /Extends is not a stream"));
            };
            self.collect_object_stream_chain(parent_ref, &parent_stream, streams, seen)?;
        }

        streams.push((stream_ref, stream_object.clone()));
        Ok(())
    }
}

// Lets `parser::Parser::object_handle` reach `Pdf::get_object_handle` for a
// nested `N G R` without `Parser` depending on `Pdf<R>`'s reader-generic
// type. Named `indirect_handle` (not `get_object_handle`) so this trait
// method and the inherent one it delegates to can never be confused for
// each other at the call site below.
impl<R: Read + Seek> crate::parser::HandleResolver for Pdf<R> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.get_object_handle(object_ref)
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

// Mirrors `decrypt_object_strings`'s mode dispatch exactly (same key
// derivation, same `decrypt_cipher_bytes` leaf primitive), but walks an
// `ObjectValue`/`ObjectHandle` tree in place rather than a legacy `Object`
// tree. Needed for `Pdf::resolve_object_handle`'s native-parse branch:
// `native_parse_uncompressed_value` builds `ObjectValue`/`ObjectHandle`
// directly from raw source bytes, never through an intermediate `Object`, so
// `decrypt_object_strings` (which only accepts `&mut Object`) cannot be
// called on it without materializing and re-lifting -- which would silently
// reset every nested child's already-recorded parsed offset, the exact
// invariant `native_parse_uncompressed_value`'s own doc protects.
//
// Returns whether the caller must emit the unknown-crypt-filter warning for
// strings, exactly as `decrypt_resolved_object`'s string half does; the crypt
// filter selection is shared, so a document only ever warns once.
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
// Getting this wrong is real, not cosmetic: a document with native parsing
// enabled but `/StmF /Identity` (so the legacy decryptor still runs, but
// native-parsed handles need this same-depth accounting to match it) can
// have a stream dictionary value nested exactly up to `MAX_INLINE_DEPTH`
// levels that the legacy path accepts and decrypts -- charging one extra
// level here would reject it, diverging `resolve_object_handle` from
// `resolve_to_cache` on input the legacy engine already handles correctly.
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

/// Returns whether the caller must emit the unknown-crypt-filter warning for
/// streams: a `/Crypt` filter naming a method qpdf does not recognise still
/// goes through `QPDF::decryptStream`'s switch, whose unknown arm warns and
/// resets `cf_stream` (`libqpdf/QPDF_encryption.cc:1121-1133`).
fn apply_explicit_crypt_filters(
    object_ref: ObjectRef,
    stream: &mut crate::Stream,
    encryption: &mut EncryptionState,
    recovered_stream_eol: Option<&[u8]>,
) -> Result<bool> {
    let filter = stream
        .dict
        .get("Filter")
        .cloned()
        .expect("caller checked for an explicit /Crypt filter");
    let mut decode_params = stream.dict.get("DecodeParms").cloned();
    if let Some(filters) = filter.as_array() {
        crate::filters::validate_filter_chain_len(filters)?;
    }
    if let Some(eol) = recovered_stream_eol {
        stream.data.extend_from_slice(eol);
    }

    let mut warn_unknown = false;

    if matches!(filter, Object::Name(ref name) if name.as_slice() == b"Crypt") {
        let mode = explicit_crypt_mode(encryption, decode_params.as_ref());
        let (use_aes, warn) = encryption.stream_method(Some(mode));
        warn_unknown |= warn;
        if let Some(use_aes) = use_aes {
            if let Some(eol) = recovered_stream_eol {
                stream.data.truncate(stream.data.len() - eol.len());
            }
            decrypt_stream_bytes(object_ref, &mut stream.data, use_aes, encryption)?;
        }
        stream.dict.remove("Filter");
        stream.dict.remove("DecodeParms");
        return Ok(warn_unknown);
    }

    let mut filters = filter
        .into_array()
        .expect("explicit /Crypt filter is either a name or an array");
    let mut framing = recovered_stream_eol;

    while let Some(crypt_index) = filters
        .iter()
        .position(|filter| matches!(filter, Object::Name(name) if name.as_slice() == b"Crypt"))
    {
        let crypt_params = decode_params_at(decode_params.as_ref(), crypt_index).cloned();
        let mode = explicit_crypt_mode(encryption, crypt_params.as_ref());
        let (use_aes, warn) = encryption.stream_method(Some(mode));
        warn_unknown |= warn;

        if let Some(use_aes) = use_aes {
            let prefix_dict = filter_prefix_dict(&filters, decode_params.as_ref(), crypt_index);
            let mut encoded = stream.data.clone();
            if crypt_index == 0 {
                if let Some(eol) = framing.take() {
                    encoded.truncate(encoded.len() - eol.len());
                }
            }
            let mut decoded_prefix = crate::filters::decode_stream_data(&prefix_dict, &encoded)?;
            decrypt_stream_bytes(object_ref, &mut decoded_prefix, use_aes, encryption)?;
            stream.data = crate::filters::encode_stream_data(&prefix_dict, &decoded_prefix)?;
        }

        // The endstream-scan EOL has now been accounted for in the source
        // representation and must not be appended again by the writer.
        // Identity keeps those exact recovered raw bytes.
        framing = None;
        filters.remove(crypt_index);
        if let Some(Object::Array(params)) = &mut decode_params {
            if crypt_index < params.len() {
                params.remove(crypt_index);
            }
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
    Ok(warn_unknown)
}

fn decode_params_at(decode_params: Option<&Object>, index: usize) -> Option<&Object> {
    let params = decode_params?;
    if params.as_dict().is_some() {
        Some(params)
    } else {
        params.as_array()?.get(index)
    }
}

fn filter_prefix_dict(
    filters: &[Object],
    decode_params: Option<&Object>,
    prefix_len: usize,
) -> Dictionary {
    let mut prefix = Dictionary::new();
    if prefix_len == 0 {
        return prefix;
    }
    prefix.insert("Filter", Object::Array(filters[..prefix_len].to_vec()));
    if let Some(params) = decode_params {
        let params = match params {
            Object::Array(params) => Object::Array(params[..prefix_len.min(params.len())].to_vec()),
            params => params.clone(),
        };
        prefix.insert("DecodeParms", params);
    }
    prefix
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
    use crate::write_pdf;
    use crate::Stream;
    use std::io::{Cursor, SeekFrom};
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
        let pdf = Pdf::open(std::io::BufReader::new(file)).expect("authenticate fixture");
        let encrypt_ref = pdf.encryption_ref().expect("indirect /Encrypt");
        let sentinel = Object::Integer(42);

        let (object, stream_payload_decrypted) = pdf
            .decrypt_resolved_object(encrypt_ref, sentinel.clone(), None)
            .expect("/Encrypt bypass");

        assert_eq!(object, sentinel);
        assert!(!stream_payload_decrypted);
    }

    #[test]
    fn object_handle_as_string_decrypts_native_parsed_encrypted_strings() {
        use crate::encrypt_setup::EncryptParams;
        use crate::writer::{write_pdf_with_options, CompressStreams, WriteOptions};
        use std::io::Cursor;

        // A minimal Catalog/Pages/Info fixture: /Info (trailer-reachable,
        // survives the writer's Catalog-first reachability walk) holds a
        // direct, uncompressed, plain-xref-table string -- exactly the shape
        // `native_parse_uncompressed_value` builds directly from raw source
        // bytes, the vulnerable path this test targets.
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
        let options = WriteOptions {
            full_rewrite: true,
            compress_streams: CompressStreams::No,
            encrypt: Some(EncryptParams::v4_aes128(
                b"user-pw".to_vec(),
                b"owner-pw".to_vec(),
            )),
            ..WriteOptions::default()
        };
        write_pdf_with_options(&mut pdf, &mut out, &options).expect("V=4 AES128 encrypted write");

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
        let title_handle = dict.get(b"Title".as_slice()).expect("/Title entry");
        let title = title_handle.as_string().expect("/Title must be a string");

        assert_eq!(
            title.as_slice(),
            b"TopSecretTitle",
            "ObjectHandle::as_string() must decrypt a native-parsed string from an \
             encrypted document, not return raw ciphertext -- native_parse_uncompressed_value \
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
        let mut value = ObjectValue::Dictionary(BTreeMap::from([(b"Nested".to_vec(), inner)]));

        decrypt_object_value_strings(object_ref, &mut value, &mut encryption)
            .expect("nested-dictionary string decryption");

        let ObjectValue::Dictionary(entries) = &value else {
            panic!("value must still be a dictionary"); // cov:ignore: unreachable given this test's own construction of value
        };
        let nested = entries.get(b"Nested".as_slice()).expect("Nested entry");
        let nested_dict = nested.as_dictionary().expect("Nested must be a dictionary");
        let title = nested_dict.get(b"Title".as_slice()).expect("Title entry");
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
        let title = stream_dict.get(b"Title".as_slice()).expect("Title entry");
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
        let title = nested_dict.get(b"Title".as_slice()).expect("Title entry");
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
        // just because native parsing routes it through this walk instead.
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

    fn crypt_params(name: &[u8]) -> Object {
        let mut params = Dictionary::new();
        params.insert("Name", Object::Name(name.to_vec()));
        Object::Dictionary(params)
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

        apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &mut encryption, None)
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

        let warn =
            apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &mut encryption, None)
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

        let warn = apply_explicit_crypt_filters(object_ref, &mut stream, &mut encryption, None)
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

        apply_explicit_crypt_filters(object_ref, &mut stream, &mut encryption, Some(b"\n"))
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

        apply_explicit_crypt_filters(object_ref, &mut stream, &mut encryption, Some(b"\n"))
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

        apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &mut encryption, None)
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

        apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &mut encryption, None)
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

        apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &mut encryption, None)
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

    #[test]
    fn explicit_crypt_helpers_apply_dictionary_decode_params_to_prefix() {
        let params = crypt_params(b"Identity");
        assert_eq!(decode_params_at(Some(&params), 7), Some(&params));

        let filters = vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"Crypt".to_vec()),
        ];
        let prefix = filter_prefix_dict(&filters, Some(&params), 1);
        assert_eq!(
            prefix.get("Filter"),
            Some(&Object::Array(vec![Object::Name(b"FlateDecode".to_vec())]))
        );
        assert_eq!(prefix.get("DecodeParms"), Some(&params));

        let prefix_without_params = filter_prefix_dict(&filters, None, 1);
        assert_eq!(prefix_without_params.get("DecodeParms"), None);
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
    fn make_indirect_object_handle_rejects_an_already_indirect_handle() {
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let already_indirect = pdf.get_object_handle(ObjectRef::new(1, 0));
        assert!(pdf.make_indirect_object_handle(already_indirect).is_err());
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
        assert_eq!(indirect.get_key(b"A").as_integer(), Some(1));
        // The new indirect handle is its own object; mutating the original
        // direct clone the caller kept does not affect it (Rust's Direct
        // and Indirect slots are distinct storage, unlike qpdf's uniform
        // shared_ptr<QPDFObject> -- see make_indirect_object_handle's own
        // doc comment).
        clone_kept_by_caller.replace_key(b"A", ObjectHandle::integer(99));
        assert_eq!(indirect.get_key(b"A").as_integer(), Some(1));
    }

    #[test]
    fn make_indirect_object_handle_gives_a_stream_its_own_independent_dict() {
        // Regression test: cloning a direct Stream value naively (the
        // #[derive(Clone)] every other variant gets) would Rc-share
        // `stream_dict` with the caller's original handle, so a later
        // `replace_stream_data` on either would rewrite the other's
        // `/Length` -- the same sharing `shallow_copy` privatizes the
        // dictionary to avoid. Mutating the *original* handle's stream data
        // after making it indirect must not affect the new indirect object's
        // dictionary.
        let dict = ObjectHandle::dictionary(vec![]);
        let direct_stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict.clone(),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_length: 0,
        });
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let indirect = pdf
            .make_indirect_object_handle(direct_stream)
            .expect("make indirect");

        dict.replace_key(b"Length", ObjectHandle::integer(999));

        assert!(
            indirect
                .as_stream_dict()
                .unwrap()
                .get_key(b"Length")
                .is_null(),
            "the new indirect object's dict must not observe a mutation \
             made through the original handle's dict"
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
    fn make_indirect_object_handle_survives_a_default_incremental_write() {
        // Regression test: the new object must be registered as dirty, or
        // the incremental writer's `collect_touched_object_refs` (which
        // seeds emission exclusively from `Pdf::dirty_object_refs`) never
        // writes its body or xref entry, leaving the reference below
        // dangling (resolving to `Object::Null` on reopen).
        use crate::write_pdf;

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

        let mut out = Vec::new();
        write_pdf(&mut pdf, &mut out).expect("incremental write");

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let resolved_root = reopened
            .resolve(root_ref)
            .expect("resolve root in reopened output");
        let extra_ref = resolved_root
            .into_dict()
            .and_then(|d| d.get("Extra").cloned())
            .expect("root has /Extra");
        assert_eq!(extra_ref, Object::Reference(new_ref));
        assert_eq!(
            reopened.resolve(new_ref).expect("resolve new object"),
            Object::Integer(777),
            "new object must not be dangling after an incremental write"
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
    fn mark_object_dirty_makes_a_replace_key_mutation_survive_a_default_incremental_write() {
        // Regression test: ObjectHandle::replace_key mutates the live
        // handle graph directly and has no path back to Pdf's dirty
        // bookkeeping. Without an explicit `mark_object_dirty` call, the
        // incremental writer's `collect_touched_object_refs` never sees
        // the mutated ref and silently drops the change from the output.
        use crate::write_pdf;

        let page_ref = ObjectRef::new(3, 0);
        let mut pdf = Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open");
        let page = pdf.get_object_handle(page_ref);
        pdf.resolve_object_handle(&page).expect("resolve page");
        page.replace_key(b"Rotate", ObjectHandle::integer(90));
        pdf.mark_object_dirty(page_ref);

        let mut out = Vec::new();
        write_pdf(&mut pdf, &mut out).expect("incremental write");

        let mut reopened = Pdf::open(Cursor::new(out)).expect("reopen written output");
        let resolved_page = reopened
            .resolve(page_ref)
            .expect("resolve page in reopened output")
            .into_dict()
            .expect("page is a dictionary");
        assert_eq!(
            resolved_page.get("Rotate"),
            Some(&Object::Integer(90)),
            "replace_key mutation must survive a default incremental write once marked dirty"
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
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open malformed-holder fixture");
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
        handle.replace_key(b"Value", ObjectHandle::integer(2));
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
        handle.replace_key(b"Value", ObjectHandle::integer(2));
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
        let foreign = source_owner.get_key(b"Child");
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
        owner.replace_key(b"Container", container);
        pdf.clear_dirty(object_ref);

        pdf.mark_object_handle_dirty(&inner).unwrap();
        assert!(pdf.is_dirty(object_ref));
    }

    #[test]
    fn detached_direct_child_neither_dirties_nor_emits_its_former_owner() {
        let owner_ref = ObjectRef::new(1, 0);
        let bytes = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog /Child << /Value 1 >> >>\nendobj\n"],
            owner_ref,
        );
        let mut pdf = Pdf::open_mem_owned(bytes.clone()).expect("open fixture");
        let owner = pdf.get_object_handle(owner_ref);
        pdf.resolve_object_handle(&owner).unwrap();
        let child = owner.get_key(b"Child");

        owner.remove_key(b"Child");
        pdf.clear_dirty(owner_ref);
        child.replace_key(b"Value", ObjectHandle::integer(2));
        pdf.mark_object_handle_dirty(&child).unwrap();

        assert!(!pdf.is_dirty(owner_ref));
        let mut out = Vec::new();
        write_pdf(&mut pdf, &mut out).expect("incremental write");
        assert!(out.starts_with(&bytes));
        assert_eq!(
            out.windows(b"1 0 obj\n".len())
                .filter(|window| *window == b"1 0 obj\n")
                .count(),
            1,
            "the detached child's former owner must not be appended again"
        );

        let mut reopened = Pdf::open_mem_owned(out).expect("reopen incremental output");
        let reopened_owner = reopened.get_object_handle(owner_ref);
        reopened.resolve_object_handle(&reopened_owner).unwrap();
        assert_eq!(
            reopened_owner
                .get_key(b"Child")
                .get_key(b"Value")
                .as_integer(),
            Some(1),
            "a detached child mutation must not change the former owner on disk"
        );
    }

    #[test]
    fn borrowed_qpdf_resolution_preserves_historical_stream_fallback_without_clone() {
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
        pdf.qpdf_parsed_xref_streams.insert(
            historical_ref,
            Object::Stream(Stream::new(Dictionary::new(), vec![0x5a; 1024 * 1024])),
        );
        let payload_ptr = pdf
            .qpdf_parsed_xref_streams
            .get(&historical_ref)
            .and_then(Object::as_stream)
            .expect("seeded historical stream")
            .data
            .as_ptr();

        let resolved = pdf
            .resolve_qpdf_json_object_borrowed(historical_ref)
            .expect("resolve historical stream");
        let stream = resolved.as_stream().expect("historical object is a stream");

        assert_eq!(stream.data.as_ptr(), payload_ptr);
        assert_eq!(stream.data.len(), 1024 * 1024);
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

    /// Regression for a spec-review finding: a malformed/overlapping source
    /// xref layout (this exact, pre-existing fixture — its object-2 entry
    /// deliberately points into the middle of object 1's own body) truncates
    /// `native_parse_uncompressed_value`'s bounded read window, but
    /// `resolve_to_cache`/`resolve_borrowed` already recover the real value
    /// via `read_object_at_with_policy`'s full-file fallback. Before the
    /// fallback added to `resolve_object_handle`, this made
    /// `resolve_object_handle` return `Err` where `resolve_borrowed`
    /// succeeds — this test pins that it no longer does.
    #[test]
    fn resolve_object_handle_falls_back_to_lift_when_the_native_window_is_truncated() {
        let object_ref = ObjectRef::new(1, 0);
        let mut pdf = Pdf::open_mem_owned(stream_with_false_next_xref_offset())
            .expect("open false-next-offset PDF");

        let legacy = pdf
            .resolve(object_ref)
            .expect("resolve_borrowed already succeeds via the full-file fallback");
        assert!(matches!(legacy, Object::Stream(_)));

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve_object_handle(&handle)
            .expect("resolve_object_handle must not fail where resolve_borrowed succeeds");

        // The fallback lands on the `lift` bridge, which (as of this task)
        // properly splits a stream's dict/data instead of falling back to
        // `ObjectValue::Null` — so the fallback path now carries the real
        // stream value, matching `resolve_borrowed`, at the no-offset
        // sentinel (this fallback never records a real parsed offset).
        assert_eq!(handle.as_stream_data(), Some(Rc::new(b"abc".to_vec())));
        assert_eq!(handle.get_parsed_offset(), -1);
    }

    /// A malformed/overlapping xref layout whose bogus "next object" offset
    /// lands between object 1's dictionary and its `stream` keyword: long
    /// enough for the dictionary to parse successfully within the bounded
    /// window, but too short to ever see `stream` itself. Investigated for
    /// a spec-review follow-up concern about `native_parse_uncompressed_value`
    /// silently misreporting a stream's parsed offset if its bounded window
    /// disagreed with `resolve_to_cache`'s classification of the same
    /// object — see `resolve_object_handle_tracks_legacys_dictionary_vs_stream_classification`
    /// for why that turns out not to be reachable.
    fn stream_with_false_next_xref_offset_between_dict_and_stream_keyword() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let stream_offset = bytes.len();
        let body = b"1 0 obj\n<< /Length 5 >>\nstream\nHello\nendstream\nendobj\n";
        bytes.extend_from_slice(body);
        let dict_and_newline_len = b"1 0 obj\n<< /Length 5 >>\n".len();
        let false_next = stream_offset + dict_and_newline_len;
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 3\n\
                 0000000000 65535 f \n\
                 {stream_offset:010} 00000 n \n\
                 {false_next:010} 00000 n \n\
                 trailer\n<< /Size 3 /Root 1 0 R >>\n\
                 startxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    /// Empirically closes the spec-review follow-up concern: a bounded
    /// window long enough for the dictionary to parse but too short to see
    /// the `stream` keyword does *not* let `native_parse_uncompressed_value`
    /// silently misclassify a genuine stream as a bare dictionary, because
    /// the *legacy* engine's own bounded attempt hits the exact same
    /// ambiguity first — and, since a missing `endobj`/`stream` is only ever
    /// a soft warning (`check_endobj`), never a hard failure, `resolve_to_cache`
    /// itself resolves `object` to a bare `Object::Dictionary` here (a
    /// pre-existing flpdf quirk this task neither introduces nor fixes; it
    /// affects `Pdf::resolve`/`resolve_borrowed` identically, with or
    /// without this task's changes). Since `object`'s *actual* classification
    /// is already `Dictionary`, not `Stream`, `native_parse_uncompressed_value`'s
    /// `let Object::Stream(stream) = object else { return Ok(..) }` early
    /// return takes over — the same code path exercised for any ordinary
    /// dictionary — so the `PendingBody::Direct` arm inside the `Some(stream)`
    /// branch is provably unreachable: `object` can only be `Object::Stream`
    /// when the bounded window already saw `stream` (case: the legacy
    /// bounded attempt's own classification would then agree), or when a
    /// hard parse failure forced the full-file retry (case: this task's own
    /// `resolve_object_handle_falls_back_to_lift_when_the_native_window_is_truncated`
    /// regression, where the *dictionary* parse itself fails identically on
    /// both paths, not just the stream-keyword visibility).
    #[test]
    fn resolve_object_handle_tracks_legacys_dictionary_vs_stream_classification() {
        let object_ref = ObjectRef::new(1, 0);
        let mut pdf = Pdf::open_mem_owned(
            stream_with_false_next_xref_offset_between_dict_and_stream_keyword(),
        )
        .expect("open false-next-offset PDF");

        let legacy = pdf
            .resolve(object_ref)
            .expect("legacy resolves, to the (pre-existing quirk's) wrong classification");
        assert!(matches!(legacy, Object::Dictionary(_)));

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve_object_handle(&handle)
            .expect("resolve_object_handle must not fail");

        let dict = handle
            .as_dictionary()
            .expect("must track object's actual Dictionary classification, not guess Stream");
        assert_eq!(
            dict.get(b"Length".as_slice())
                .and_then(ObjectHandle::as_integer),
            Some(5)
        );
        assert_ne!(
            handle.get_parsed_offset(),
            -1,
            "a plain dictionary still gets a real native-parsed offset"
        );
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

    /// Build a minimal PDF whose object `(1, 0)` is a linearization
    /// parameter dictionary with the supplied `/Linearized` literal.
    fn linearized_like_pdf_bytes_real_literal(linearized: &[u8]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Linearized ");
        pdf.extend_from_slice(linearized);
        pdf.extend_from_slice(b" >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 5 /Root 2 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// qpdf accepts `/Linearized` only when its numeric floor is exactly
    /// one. In particular, `.9` is not a linearization marker while `1.9`
    /// is, even though both are parsed as [`Object::RealLiteral`].
    #[test]
    fn linearized_hint_ref_uses_qpdf_numeric_floor() {
        let mut below_one = Pdf::open_mem_owned(linearized_like_pdf_bytes_real_literal(b".9"))
            .expect("open .9 PDF");
        assert_eq!(below_one.linearized_hint_ref().expect("check .9"), None);

        let mut one_or_above = Pdf::open_mem_owned(linearized_like_pdf_bytes_real_literal(b"1.9"))
            .expect("open 1.9 PDF");
        assert_eq!(
            one_or_above.linearized_hint_ref().expect("check 1.9"),
            Some(ObjectRef::new(1, 0))
        );
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

    // ------------------------------------------------------------------
    // collect_object_stream_chain: /Extends chain depth bound
    // ------------------------------------------------------------------

    /// Builds a classic-xref PDF whose object streams form an `/Extends` chain
    /// of `chain_len` links: objects `4..4+chain_len`, each linking to the next
    /// and the last without `/Extends`. The head object stream is object 4.
    ///
    /// The streams are empty (`/N 0`); `collect_object_stream_chain` only walks
    /// `/Extends` and never parses members, so empty streams exercise the depth
    /// guard fully without needing real compressed payloads.
    fn objstm_extends_chain_pdf(chain_len: usize) -> Vec<u8> {
        let mut bodies: Vec<Vec<u8>> = vec![
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_vec(),
        ];
        let first_objstm = 4u32;
        for i in 0..chain_len {
            let obj_num = first_objstm + i as u32;
            let extends = if i + 1 < chain_len {
                format!(" /Extends {} 0 R", obj_num + 1)
            } else {
                String::new()
            };
            bodies.push(
                format!(
                    "{obj_num} 0 obj\n<< /Type /ObjStm /N 0 /First 0 /Length 0{extends} >>\nstream\n\nendstream\nendobj\n"
                )
                .into_bytes(),
            );
        }

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.5\n");
        let mut offsets = Vec::with_capacity(bodies.len());
        for body in &bodies {
            offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(body);
        }

        let size = bodies.len() + 1; // +1 for the free object 0
        let xref_start = pdf.len() as u64;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    /// A chain exactly at the limit is collected in full (the depth guard's
    /// non-error path).
    #[test]
    fn collect_object_stream_chain_accepts_chain_at_limit() {
        let bytes = objstm_extends_chain_pdf(MAX_OBJECT_STREAM_CHAIN_DEPTH);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let head = ObjectRef::new(4, 0);
        let resolved = pdf.resolve(head).expect("resolve head");
        let head_stream = resolved.as_stream().expect("head object must be a stream");
        let mut streams = Vec::new();
        pdf.collect_object_stream_chain(head, head_stream, &mut streams, &mut BTreeSet::new())
            .expect("a chain at the depth limit must be accepted");
        assert_eq!(streams.len(), MAX_OBJECT_STREAM_CHAIN_DEPTH);
    }

    /// One link past the limit aborts with a catchable parse error rather than
    /// recursing until the stack overflows.
    #[test]
    fn collect_object_stream_chain_rejects_overlong_extends_chain() {
        let bytes = objstm_extends_chain_pdf(MAX_OBJECT_STREAM_CHAIN_DEPTH + 1);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open");
        let head = ObjectRef::new(4, 0);
        let resolved = pdf.resolve(head).expect("resolve head");
        let head_stream = resolved.as_stream().expect("head object must be a stream");
        let mut streams = Vec::new();
        let err = pdf
            .collect_object_stream_chain(head, head_stream, &mut streams, &mut BTreeSet::new())
            .expect_err("a chain past the depth limit must be rejected");
        assert!(
            matches!(err, Error::Parse { .. }),
            "expected a parse error, got: {err:?}"
        );
        assert!(
            err.to_string().contains("too deep"),
            "expected a depth error, got: {err}"
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

    #[test]
    fn trailer_handle_is_direct_with_a_canonical_indirect_root_child() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let handle = pdf.trailer_handle();
        assert!(handle.is_direct());
        let dict = handle
            .as_dictionary()
            .expect("trailer is a dictionary handle");
        assert!(dict.contains_key(b"Root".as_slice()) || dict.contains_key(b"Size".as_slice()));

        let root_handle = dict.get(b"Root".as_slice()).expect("trailer has /Root");
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
    fn trailer_handle_degrades_to_null_when_nesting_exceeds_the_inline_depth_bound() {
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        let depth = crate::object::MAX_INLINE_DEPTH + 5;
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
        // key to null once *any* sibling entry exceeds `MAX_INLINE_DEPTH` —
        // `trailer_key_handle` must not inherit that coupling: `/QTest`
        // itself is shallow here, only its unrelated sibling `/Deep` is not.
        let mut pdf = Pdf::open_mem_owned(minimal_pdf_bytes()).expect("open");
        pdf.trailer.insert("QTest", Object::Boolean(true));
        let depth = crate::object::MAX_INLINE_DEPTH + 5;
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

    // `lift`/`lift_bounded` (unlike the native parse path) has no
    // `stacker::maybe_grow` protection of its own — matching
    // `parser.rs`'s own `nesting_past_max_parse_depth_matches_between_legacy_and_native_paths`/
    // `native_handle_path_preserves_the_object_nesting_guard`, recursing it
    // all the way to `MAX_PARSE_DEPTH` needs a dedicated, larger-than-default
    // thread stack to avoid aborting the whole test process on a
    // small-default-stack CI runner (observed: Windows). Building the
    // `Pdf`/`Object` tree inside the spawned closure, not moving one in from
    // outside, sidesteps needing either type to be `Send`.
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
            dict.get(b"Pages".as_slice())
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
            handle.type_code(),
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
        assert_eq!(result.type_code(), 3, "ot_boolean, not 13/unresolved");
        assert_eq!(result.type_name(), "boolean");
        assert_eq!(
            result.object_ref(),
            None,
            "result is a direct, unregistered handle"
        );
        assert_eq!(
            result.unparse(),
            b"true",
            "direct: same as unparse_resolved()"
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
    fn resolve_object_handle_to_terminal_deep_copies_direct_nested_children() {
        // A single-level clone of the terminal `ObjectValue` would leave a
        // *direct* nested child Rc-shared with the canonical target's own
        // value (only an indirect child is meant to stay shared) — mutating
        // it through the returned handle would then silently mutate the
        // real document too. `ObjectHandle::shallow_copy` (used internally)
        // must recurse through direct descendants independently.
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
            .get(b"Nested".as_slice())
            .expect("Nested present")
            .clone();
        nested_handle.replace_key(b"Inner", ObjectHandle::integer(999));

        let canonical_target = pdf.get_object_handle(target_ref);
        pdf.resolve_object_handle(&canonical_target)
            .expect("resolve canonical target");
        let canonical_inner = canonical_target
            .as_dictionary()
            .expect("canonical target is a dictionary")
            .get(b"Nested".as_slice())
            .and_then(ObjectHandle::as_dictionary)
            .and_then(|nested| {
                nested
                    .get(b"Inner".as_slice())
                    .and_then(ObjectHandle::as_integer)
            });
        assert_eq!(
            canonical_inner,
            Some(1),
            "mutating the detached terminal's nested child must not affect the canonical document"
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
            None,
            "result is a direct, unregistered handle"
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

    /// White-box companion to the public-API
    /// `resolve_object_handle_distinguishes_a_literal_null_from_a_dangling_reference`
    /// integration test: proves the two null-observing cases actually take
    /// different internal routes through `self.cache`, not merely that both
    /// happen to read as `is_null() == true` from the outside. A literal
    /// `null` object present in the xref table resolves to a real
    /// `CacheEntry::Resolved(Object::Null)`; a reference entirely absent from
    /// the xref table never gains a cache entry at all.
    #[test]
    fn resolve_object_handle_literal_null_and_dangling_ref_take_different_cache_paths() {
        let bytes = classic_pdf_with_bodies(&[b"1 0 obj\nnull\nendobj\n"], ObjectRef::new(1, 0));
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open literal-null fixture");

        let literal_null_ref = ObjectRef::new(1, 0);
        let literal_null_handle = pdf.get_object_handle(literal_null_ref);
        pdf.resolve_object_handle(&literal_null_handle)
            .expect("resolve literal null");
        assert!(literal_null_handle.is_null());
        assert!(matches!(
            pdf.cache.entry(literal_null_ref),
            Some(CacheEntry::Resolved(Object::Null))
        ));

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
        // accepted by the parser that already produced this compressed
        // member's cached `Object` (via `resolve_to_cache`/
        // `parse_object_stream_chain_entry`). `resolve_object_handle` must
        // lift it at that same looser bound rather than `lift`'s default
        // `MAX_INLINE_DEPTH`, or a value `resolve_borrowed` always accepted
        // at this depth would spuriously fail here — see the comment on
        // `resolve_object_handle`'s call to `lift_bounded` for the
        // regression this pins.
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
    fn resolve_object_handle_uncompressed_now_accepts_the_same_nesting_depth_via_native_parse() {
        // Task 6's `resolve_object_handle` routed every indirect handle
        // (Uncompressed and Compressed alike) through the same `lift`
        // bridge, so this exact depth (between MAX_INLINE_DEPTH and
        // MAX_PARSE_DEPTH) used to be rejected for BOTH. This task reroutes
        // the Uncompressed case to a native parse bounded only by
        // MAX_PARSE_DEPTH (matching `object`/`object_inner` exactly, see
        // `Parser::object_handle`) — so it now succeeds, matching what
        // `resolve_borrowed` (which was never subject to MAX_INLINE_DEPTH)
        // already accepted at this depth. The Compressed case now also
        // accepts this depth (see
        // `resolve_object_handle_compressed_member_accepts_inline_nesting_past_max_inline_depth`),
        // via `lift_bounded` rather than native parse. This is an
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

    /// Now that this task reroutes the Uncompressed case away from `lift`,
    /// `lift`/`lift_to_handle`'s own scalar/dictionary/reference branches are
    /// reachable only through the Compressed (ObjStm-member) route this task
    /// deliberately leaves unchanged — this is the coverage anchor for that
    /// route, mirroring what `resolve_object_handle_lifts_every_scalar_object_value_variant`
    /// (the tests-crate parity file) already covers for the Uncompressed
    /// (native-parse) case.
    #[test]
    fn resolve_object_handle_compressed_member_lifts_every_scalar_dictionary_and_reference_variant()
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
        assert!(dict.contains_key(b"B".as_slice()));
        assert!(dict.contains_key(b"R".as_slice()));
        assert_eq!(
            dict.get(b"RL".as_slice())
                .and_then(ObjectHandle::as_real_literal),
            Some((0.5, b".5".to_vec()))
        );
        assert!(dict.contains_key(b"N".as_slice()));
        assert!(dict.contains_key(b"S".as_slice()));
        assert!(dict.get(b"Nul".as_slice()).expect("Nul entry").is_null());

        let kid = dict.get(b"Kid".as_slice()).expect("Kid entry");
        assert!(kid.is_indirect());
        assert_eq!(kid.object_ref(), Some(ObjectRef::new(5, 0)));

        let sub = dict
            .get(b"Sub".as_slice())
            .expect("Sub entry")
            .as_dictionary()
            .expect("nested dictionary");
        assert_eq!(
            sub.get(b"X".as_slice()).and_then(ObjectHandle::as_integer),
            Some(1)
        );
    }

    /// `resolve_to_cache` records a stream's recovered `endstream`-scan EOL
    /// alongside `CacheEntry::Resolved` (see `Self::recovered_stream_eol`).
    /// `resolve_object_handle` calls `resolve_to_cache` as its very first
    /// step for every indirect handle — this pins that it does not bypass
    /// that side table.
    ///
    /// This test needs the crate-private `recovered_stream_eol` accessor, so
    /// it lives here rather than in the `tests/` integration suite (which
    /// only sees the public API).
    #[test]
    fn resolve_object_handle_still_populates_recovered_stream_eol() {
        let bytes = recovered_stream_fixture(b"", b"\n", None);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open recovered-stream fixture");
        let object_ref = ObjectRef::new(1, 0);

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve_object_handle(&handle)
            .expect("resolve recovered stream");

        assert_eq!(handle.as_stream_data(), Some(Rc::new(b"abc".to_vec())));
        assert_eq!(pdf.recovered_stream_eol(object_ref), Some(&b"\n"[..]));
    }

    /// Companion to the above for the other side table
    /// (`transformed_stream_refs`): when a stream's payload is actually
    /// transformed (here, decrypted), `resolve_to_cache` marks that ref so
    /// `recovered_stream_eol` stops surfacing a recovered EOL that belongs to
    /// ciphertext framing, not the plaintext `resolve` returns. The
    /// `EncryptionState` is injected directly (matching
    /// `explicit_rc4_encryption_state`'s existing pattern) rather than
    /// authenticating a real encrypted fixture, since only the
    /// side-table bookkeeping is under test here, not decryption
    /// correctness (already covered elsewhere).
    #[test]
    fn resolve_object_handle_still_marks_transformed_stream_refs_for_a_decrypted_stream() {
        let bytes = recovered_stream_fixture(b"", b"\n", None);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open recovered-stream fixture");
        let object_ref = ObjectRef::new(1, 0);
        *pdf.encryption.borrow_mut() = Some(EncryptionState {
            encryption_v: 2,
            encryption_r: 3,
            cf_stream: EncryptionMode::Identity,
            ..explicit_rc4_encryption_state()
        });

        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve_object_handle(&handle)
            .expect("resolve recovered, (fake-)encrypted stream");

        assert!(
            pdf.transformed_stream_refs.contains(&object_ref),
            "a decrypted stream's payload transformation must still be tracked via the handle path"
        );
        assert_eq!(
            pdf.recovered_stream_eol(object_ref),
            None,
            "a transformed stream's recovered EOL belongs to ciphertext framing and must stay masked"
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
            "native parse must record a real offset before deletion"
        );

        pdf.delete_object(object_ref);

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        assert!(handle.is_null());
    }

    /// `Pdf::resolve_borrowed` now returns the *native*-parsed dictionary
    /// value for a plain Uncompressed object (via `Pdf::materialize`) rather
    /// than the legacy-resolved one, for the first time. The native
    /// `dictionary_handle` parser (`parser.rs`) and the legacy
    /// `Parser::dictionary` are two independently maintained left-to-right
    /// `BTreeMap::insert` passes over the same token stream; this pins that
    /// they still agree on a duplicate dictionary key (last write wins),
    /// rather than relying on that being merely "true today, unverified".
    #[test]
    fn native_and_legacy_dictionary_parsers_agree_on_a_duplicate_key() {
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

        // Bridge path: `resolve` materializes from a *native* parse of the
        // same source bytes (`native_parse_uncompressed_value`), a separate
        // `BTreeMap`-building pass over the identical duplicate-key
        // dictionary token stream.
        let mut native_pdf =
            Pdf::open_mem_owned(classic_pdf_with_bodies(&[body], object_ref)).expect("open");
        let native = native_pdf.resolve(object_ref).expect("bridge resolve");

        assert_eq!(legacy, native);
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
}
