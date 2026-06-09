use crate::cache::{CacheEntry, ObjectCache};
use crate::error::EncryptedError;
use crate::parser::{parse_indirect_object_detailed, Parser};
use crate::security::password::{normalize_password, PasswordMode};
use crate::security::standard::{
    check_owner_password, check_owner_password_r5, check_owner_password_r6,
    check_owner_password_v4, check_user_password, check_user_password_r5, check_user_password_r6,
    check_user_password_v4, decrypt_cipher_bytes, decrypt_strings_in_object, per_object_key,
    ObjectKeyAlg, StandardHandlerInputs, StandardHandlerR5Inputs, StringCipher,
};
use crate::{
    load_xref_and_trailer, load_xref_and_trailer_with_repair, Diagnostic, Diagnostics, Dictionary,
    Error, Object, ObjectRef, Result, XrefForm, XrefOffset,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Seek, SeekFrom};

static NULL_OBJECT: Object = Object::Null;

/// Lazily parsed PDF document handle.
///
/// `Pdf` is the core type of the crate. Opening a document only reads the cross-reference
/// table and the trailer; individual objects are parsed on first access via
/// [`Pdf::resolve`]. The same handle is what every higher-level helper
/// ([`crate::pages`], [`crate::outline`], [`crate::fonts`], [`crate::write_pdf`])
/// consumes.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{ObjectRef, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// println!("version {}", pdf.version());
/// let catalog = pdf.resolve(pdf.root_ref().expect("root"))?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Pdf<R: Read + Seek> {
    reader: R,
    version: String,
    /// `true` when the file header carries the `%QDF-1.0` marker (qpdf/flpdf
    /// QDF form). Used by [`resolve`](Self::resolve) to disambiguate the
    /// exact-window indirect `/Length` case: QDF holders count the framing
    /// EOL (keep the parser's endstream-scan to preserve round-trip), whereas
    /// non-QDF holders are the spec content length.
    is_qdf: bool,
    trailer: Dictionary,
    startxref: u64,
    last_xref_form: XrefForm,
    repair_diagnostics: Diagnostics,
    cache: ObjectCache,
    compressed_member_parents: BTreeMap<ObjectRef, (ObjectRef, u32)>,
    source_xref_offsets: Vec<(ObjectRef, u64)>,
    source_xref_entries: BTreeMap<ObjectRef, XrefOffset>,
    dirty_object_refs: BTreeSet<ObjectRef>,
    encryption: Option<EncryptionState>,
}

#[derive(Debug, Clone)]
struct EncryptionState {
    file_key: Vec<u8>,
    stream_mode: EncryptionMode,
    string_mode: EncryptionMode,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncryptionMode {
    Rc4,
    Aes128,
    Identity,
    Aes256,
}

impl EncryptionMode {
    /// qpdf's `show_encryption_method()` spelling for this method.
    ///
    /// Source: qpdf `libqpdf/QPDFJob.cc` `show_encryption_method()` —
    /// `e_rc4`→"RC4", `e_aes`→"AESv2", `e_aesv3`→"AESv3", `e_none`→"none".
    /// flpdf's `Identity` (no-op crypt filter) maps to qpdf's `e_none`/"none".
    fn qpdf_name(self) -> &'static str {
        match self {
            EncryptionMode::Rc4 => "RC4",
            EncryptionMode::Aes128 => "AESv2",
            EncryptionMode::Aes256 => "AESv3",
            EncryptionMode::Identity => "none",
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
    /// qpdf-style method name for the embedded-file crypt filter (`EFF`),
    /// when the document declares one.
    pub eff_method: Option<&'static str>,
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

impl<R: Read + Seek> Pdf<R> {
    /// Open a document strictly: parse the cross-reference and trailer, but do not run
    /// the recovery heuristics. Returns an [`Error`] if the document is malformed.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`Pdf::open_with_options`] (called with default
    /// options): [`Error::Io`] / [`Error::Parse`] / [`Error::Missing`] from loading
    /// the cross-reference and trailer, and [`Error::Encrypted`] when the document
    /// is encrypted and cannot be authenticated.
    pub fn open(reader: R) -> Result<Self> {
        Self::open_with_options(reader, PdfOpenOptions::default())
    }

    /// Open a document, falling back to qpdf-style xref/trailer recovery when the
    /// strict parse fails. Diagnostics from the recovery pass are stored on the handle
    /// and exposed via [`Pdf::repair_diagnostics`].
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
    ///   `/Encrypt` dictionary ([`EncryptedError::Malformed`]), or an RC4 / R=5
    ///   document opened without `options.allow_weak_crypto`
    ///   ([`EncryptedError::WeakCryptoNotAllowed`]).
    pub fn open_with_options(reader: R, options: PdfOpenOptions) -> Result<Self> {
        Self::open_with_repair_mode(reader, options)
    }

    /// Diagnostics emitted while opening the document — typically warnings from the
    /// xref/trailer recovery path. Always non-empty when the parse hit a soft failure.
    pub fn repair_diagnostics(&self) -> &Diagnostics {
        &self.repair_diagnostics
    }

    /// Record a non-fatal processing warning on this handle.
    ///
    /// Used by recoverable code paths (e.g. form-field inheritance walks that hit
    /// a cyclic / over-deep / non-dictionary `/Parent` chain and fall back rather
    /// than aborting) so the soft failure is surfaced via [`Pdf::repair_diagnostics`]
    /// instead of being silently swallowed. Mirrors qpdf, which warns and continues
    /// on malformed field trees.
    pub(crate) fn push_warning(&mut self, message: impl Into<String>) {
        self.repair_diagnostics
            .push(Diagnostic::warning(message, None));
    }

    /// Whether this document authenticated an `/Encrypt` dictionary while opening.
    pub fn is_encrypted(&self) -> bool {
        self.encryption.is_some()
    }

    pub(crate) fn encryption_ref(&self) -> Option<ObjectRef> {
        self.encryption
            .as_ref()
            .and_then(|encryption| encryption.encrypt_ref)
    }

    /// Whether opening this document required the weak-crypto opt-in.
    pub fn uses_weak_crypto(&self) -> bool {
        self.encryption
            .as_ref()
            .is_some_and(|encryption| encryption.weak_crypto)
    }

    /// Advisory standard security handler permissions from `/P`, if the document is encrypted.
    pub fn permissions(&self) -> Option<Permissions> {
        self.encryption
            .as_ref()
            .map(|encryption| encryption.permissions)
    }

    /// Whether the password supplied at open time authenticated against the
    /// document's user password (`/U`). Always `false` for plaintext PDFs.
    pub fn user_password_matched(&self) -> bool {
        self.encryption
            .as_ref()
            .is_some_and(|encryption| encryption.user_password_matched)
    }

    /// Whether the password supplied at open time authenticated against the
    /// document's owner password (`/O`). Always `false` for plaintext PDFs.
    /// Many PDFs use an empty password for both, so this can be true at the
    /// same time as [`Pdf::user_password_matched`].
    pub fn owner_password_matched(&self) -> bool {
        self.encryption
            .as_ref()
            .is_some_and(|encryption| encryption.owner_password_matched)
    }

    /// The derived file encryption key, if the document was opened as an
    /// encrypted file. `None` for plaintext PDFs.
    ///
    /// Read-only accessor for the `show-encryption-key` inspection
    /// subcommand; does not run or alter authentication.
    pub fn encryption_file_key(&self) -> Option<&[u8]> {
        self.encryption
            .as_ref()
            .map(|encryption| encryption.file_key.as_slice())
    }

    /// Read-only snapshot of the `/Encrypt` parameters for the
    /// `show-encryption` inspection subcommand.
    ///
    /// Returns `None` for plaintext PDFs. Re-reads the `/Encrypt` dictionary
    /// (for V/R/Length/Filter/CF, which the authenticated `EncryptionState`
    /// does not retain verbatim) and combines it with the already-resolved
    /// crypt-filter methods. This does NOT re-run or alter authentication
    /// (layer-2 owns that ordering); it only reflects state from a document
    /// already opened successfully.
    ///
    /// # Errors
    ///
    /// - [`Error::Encrypted`] ([`EncryptedError::Malformed`]) when the re-read
    ///   `/Encrypt` dictionary is missing or has the wrong type for `/V`, `/R`,
    ///   `/Filter`, or the `/EFF` crypt-filter selector. Returns `Ok(None)` for a
    ///   plaintext document rather than an error.
    /// - [`Error::Io`] / [`Error::Parse`] when the `/Encrypt` entry is an indirect
    ///   reference whose resolution fails.
    pub fn encryption_info(&mut self) -> Result<Option<EncryptionInfo>> {
        if self.encryption.is_none() {
            return Ok(None);
        }
        let Some(encrypt) = self.encrypt_dictionary()? else {
            return Ok(None);
        };
        let v = required_integer(&encrypt, "V")?;
        let r = required_integer(&encrypt, "R")?;
        let filter = required_name(&encrypt, "Filter")?.to_string();
        // /Length is in bits and absent for V<5 (defaulting to 40 per the
        // Standard handler); V=5 always uses a 256-bit key.
        let length_bits = match encrypt.get("Length") {
            Some(Object::Integer(value)) => *value,
            _ if v >= 5 => 256,
            _ => 40,
        };

        // EFF: optional crypt-filter selector for embedded files. Resolve it
        // through the same named-CF map used for StmF/StrF.
        let eff_selector = crypt_filter_selector(&encrypt, "EFF")?;

        let encryption = self
            .encryption
            .as_ref()
            .expect("checked is_some above; authenticate_if_encrypted set it");
        let permissions = encryption.permissions;
        let encrypt_metadata = encryption.encrypt_metadata;
        let stream_method = encryption.stream_mode.qpdf_name();
        let string_method = encryption.string_mode.qpdf_name();
        let eff_method = eff_selector.and_then(|selector| {
            if selector == "Identity" {
                Some(EncryptionMode::Identity.qpdf_name())
            } else {
                encryption
                    .crypt_filters
                    .get(selector.as_bytes())
                    .map(|mode| mode.qpdf_name())
            }
        });
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

    fn open_with_repair_mode(mut reader: R, options: PdfOpenOptions) -> Result<Self> {
        let loaded = if options.repair {
            load_xref_and_trailer_with_repair(&mut reader, options.repair)?
        } else {
            load_xref_and_trailer(&mut reader)?
        };
        let source_xref_entries = loaded.entries.clone();
        let source_xref_offsets = loaded
            .entries
            .iter()
            .filter_map(|(object_ref, offset)| match offset {
                crate::XrefOffset::Free { .. } => None,
                crate::XrefOffset::Offset(offset) => Some((*object_ref, *offset)),
                crate::XrefOffset::Compressed { .. } => None,
            })
            .collect();
        let cache = ObjectCache::from_offsets(&loaded.entries);
        // Sniff the file header for the `%QDF-1.0` marker (flpdf-9hc.28). It
        // sits within the first few lines (`%PDF-x.y`, binary marker,
        // `%QDF-1.0`); 64 bytes is ample. Best-effort: any read error → false.
        let is_qdf = {
            // `Read::read` may return a short read (BufReader, pipes, …), so
            // a single call could split `%QDF-1.0` across reads and miss it.
            // `take(64).read_to_end` reads until 64 bytes or EOF regardless of
            // chunking. Best-effort: any error → false.
            let mut header = Vec::with_capacity(64);
            if reader.seek(SeekFrom::Start(0)).is_ok() {
                let _ = reader.by_ref().take(64).read_to_end(&mut header);
            }
            header.windows(b"%QDF-1.0".len()).any(|w| w == b"%QDF-1.0")
        };
        let mut pdf = Self {
            reader,
            version: loaded.version,
            is_qdf,
            trailer: loaded.trailer,
            startxref: loaded.startxref,
            last_xref_form: loaded.last_xref_form,
            repair_diagnostics: loaded.repair_diagnostics,
            cache,
            compressed_member_parents: BTreeMap::new(),
            source_xref_offsets,
            source_xref_entries,
            dirty_object_refs: BTreeSet::new(),
            encryption: None,
        };
        pdf.authenticate_if_encrypted(&options)?;
        Ok(pdf)
    }

    fn authenticate_if_encrypted(&mut self, options: &PdfOpenOptions) -> Result<()> {
        let encrypt_ref = self.trailer().get_ref("Encrypt");
        let Some(encrypt) = self.encrypt_dictionary()? else {
            return Ok(());
        };

        let revision = required_revision(&encrypt)?;
        let permissions = Permissions::new(required_permissions(&encrypt)?);
        let crypt_filters = crypt_filter_modes(&encrypt, revision)?;
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
        let (
            file_key,
            stream_mode,
            string_mode,
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
            // revision / crypt_filters / encrypt_ref / permissions are already
            // determined above and do NOT depend on the password. Modes and
            // /EncryptMetadata are likewise password-independent; compute them
            // with the SAME revision-aware split layer-2 uses (the V<5/V=4
            // helper returns RC4/RC4 unconditionally for V≠4, so it must not
            // be applied to a V=5 document).
            let file_key = decode_hex_file_key(&options.password)?;
            let (stream_mode, string_mode, encrypt_metadata, weak_crypto) =
                if matches!(revision, 5 | 6) {
                    let (stream_mode, string_mode) = standard_r5_or_r6_modes(&encrypt)?;
                    let encrypt_metadata = encrypt_metadata_flag(&encrypt)?;
                    // Same weak-crypto classification as layer-2's R5/R6 branch.
                    (stream_mode, string_mode, encrypt_metadata, revision == 5)
                } else {
                    let inputs = standard_handler_inputs(&encrypt, self.trailer())?;
                    let (stream_mode, string_mode) = standard_v4_or_legacy_modes(&encrypt)?;
                    let weak_crypto = matches!(stream_mode, EncryptionMode::Rc4)
                        || matches!(string_mode, EncryptionMode::Rc4)
                        || crypt_filters
                            .values()
                            .any(|mode| matches!(mode, EncryptionMode::Rc4));
                    (
                        stream_mode,
                        string_mode,
                        inputs.encrypt_metadata,
                        weak_crypto,
                    )
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
            (
                file_key,
                stream_mode,
                string_mode,
                encrypt_metadata,
                weak_crypto,
                false,
                false,
            )
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
            let (stream_mode, string_mode) = standard_r5_or_r6_modes(&encrypt)?;
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
                stream_mode,
                string_mode,
                encrypt_metadata,
                weak_crypto,
                user_password_matched,
                owner_password_matched,
            )
        } else {
            let inputs = standard_handler_inputs(&encrypt, self.trailer())?;
            let (stream_mode, string_mode) = standard_v4_or_legacy_modes(&encrypt)?;
            let encrypt_metadata = inputs.encrypt_metadata;
            let weak_crypto = matches!(stream_mode, EncryptionMode::Rc4)
                || matches!(string_mode, EncryptionMode::Rc4)
                || crypt_filters
                    .values()
                    .any(|mode| matches!(mode, EncryptionMode::Rc4));
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
                stream_mode,
                string_mode,
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
        self.encryption = Some(EncryptionState {
            file_key,
            stream_mode,
            string_mode,
            crypt_filters,
            encrypt_metadata,
            encrypt_ref,
            weak_crypto,
            permissions,
            user_password_matched,
            owner_password_matched,
        });
        if let Some(warning) = r6_perms_warning {
            self.repair_diagnostics
                .push(Diagnostic::warning(warning, None));
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

    /// PDF version header as written in the first line of the file (e.g. `"1.7"`).
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The trailer dictionary (or the dictionary attached to the trailing xref stream
    /// for cross-reference-stream documents). This is where you'd reach for `/Root`,
    /// `/Info`, `/Size`, `/ID`, etc.
    pub fn trailer(&self) -> &Dictionary {
        &self.trailer
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

    pub(crate) fn source_xref_entries(&self) -> BTreeMap<ObjectRef, XrefOffset> {
        self.source_xref_entries.clone()
    }

    pub(crate) fn compressed_parent(&self, object_ref: ObjectRef) -> Option<(ObjectRef, u32)> {
        self.compressed_member_parents.get(&object_ref).copied()
    }

    /// Replace `object_ref` with `object` in the in-memory object cache.
    ///
    /// The original on-disk bytes are not touched; an incremental rewrite via
    /// [`crate::write_pdf`] will see the updated value when it walks the cache and emit
    /// a new revision for the touched object.
    pub fn set_object(&mut self, object_ref: ObjectRef, object: Object) {
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
        self.cache.set_resolved(object_ref, object);
        self.dirty_object_refs.insert(object_ref);
    }

    pub fn delete_object(&mut self, object_ref: ObjectRef) {
        if object_ref.number == 0
            || matches!(
                self.cache.entry(object_ref),
                Some(CacheEntry::Deleted | CacheEntry::Missing)
            )
        {
            return;
        }
        self.cache.set_deleted(object_ref);
        self.dirty_object_refs.insert(object_ref);
    }

    pub(crate) fn source_bytes(&mut self) -> Result<Vec<u8>> {
        self.reader.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        self.reader.read_to_end(&mut bytes)?;
        Ok(bytes)
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

    /// Every object reference known from the cross-reference table, including objects
    /// that have not yet been parsed.
    pub fn object_refs(&self) -> Vec<ObjectRef> {
        self.cache.object_refs()
    }

    /// Object refs that the cross-reference table marks as live.
    ///
    /// Excludes:
    /// - `Deleted` — free entries (from `XrefOffset::Free`) and explicit
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

    /// `/Root` as listed in the trailer, when present.
    pub fn root_ref(&self) -> Option<ObjectRef> {
        self.trailer.get_ref("Root")
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
            Object::Integer(value) if *value > 0 => Some(candidate),
            Object::Real(value) if value.is_finite() && *value > 0.0 => Some(candidate),
            Object::Boolean(value) if *value => Some(candidate),
            _ => None,
        })
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
    pub fn resolve(&mut self, object_ref: ObjectRef) -> Result<Object> {
        Ok(self.resolve_borrowed(object_ref)?.clone())
    }

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
        if self.resolve_to_cache(object_ref)? {
            if let Some(CacheEntry::Resolved(object)) = self.cache.entry(object_ref) {
                return Ok(object);
            }
        }

        Ok(&NULL_OBJECT)
    }

    fn resolve_to_cache(&mut self, object_ref: ObjectRef) -> Result<bool> {
        let entry = self.cache.entry(object_ref);
        if matches!(entry, Some(CacheEntry::Resolved(_))) {
            return Ok(true);
        }

        match entry.cloned() {
            Some(CacheEntry::Unresolved { offset }) => {
                self.reader.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::new();
                self.reader.read_to_end(&mut bytes)?;
                let (parsed_ref, mut object, indirect_len) =
                    parse_indirect_object_detailed(&bytes)?;
                if parsed_ref != object_ref {
                    return Ok(false);
                }
                // When the stream's /Length is an indirect reference, the parser
                // had no xref and recorded the payload window instead of a
                // resolved length. Resolve the holder via the xref and re-slice
                // to the authoritative length. This MUST happen before
                // decryption: `object`/`bytes` are still ciphertext here, and
                // `decrypt_resolved_object` decrypts in place afterwards.
                if let Some(isl) = indirect_len {
                    self.apply_indirect_stream_length(
                        object_ref,
                        &mut object,
                        isl,
                        &bytes,
                        offset,
                    )?;
                }
                // `apply_indirect_stream_length` already restored the lazy
                // `Unresolved` entry, so a decryption error here leaves it
                // `Unresolved` (not `Reserved`) and the failure stays loud on a
                // retry.
                let object = self.decrypt_resolved_object(object_ref, object)?;
                self.cache.set_resolved(object_ref, object);
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

    /// Apply a parser-recorded indirect `/Length` ([`IndirectStreamLength`]) to a
    /// freshly parsed stream `object`, re-slicing `stream.data` to the
    /// xref-resolved authoritative length read from `bytes` (the raw object
    /// bytes).
    ///
    /// The re-slice resolves the holder, which may mark `object_ref` `Reserved`
    /// to guard against a cyclic length-holder chain. This wrapper ALWAYS
    /// restores the lazy `Unresolved { offset }` entry afterwards, on both
    /// success and error: on success the caller immediately overwrites it with
    /// the resolved object; on error the failure stays loud on a retry (a
    /// lingering `Reserved` entry would otherwise resolve to `Null`). The caller
    /// therefore needs no manual cache restore on its own later error paths
    /// (e.g. decryption).
    ///
    /// Shared by [`resolve_to_cache`](Self::resolve_to_cache) and
    /// [`resolve_compressed_entry`](Self::resolve_compressed_entry) so ObjStm
    /// containers get the same recovery as top-level streams.
    fn apply_indirect_stream_length(
        &mut self,
        object_ref: ObjectRef,
        object: &mut Object,
        isl: crate::parser::IndirectStreamLength,
        bytes: &[u8],
        offset: u64,
    ) -> Result<()> {
        let result = self.reslice_indirect_stream_length(object_ref, object, isl, bytes);
        self.cache.set_unresolved(object_ref, offset);
        result
    }

    /// Inner half of [`apply_indirect_stream_length`](Self::apply_indirect_stream_length):
    /// performs the holder resolution and re-slice. May leave `object_ref`
    /// `Reserved`; the wrapper restores the cache entry.
    fn reslice_indirect_stream_length(
        &mut self,
        object_ref: ObjectRef,
        object: &mut Object,
        isl: crate::parser::IndirectStreamLength,
        bytes: &[u8],
    ) -> Result<()> {
        match isl.endstream_pos {
            // The parser located a line-anchored `endstream` and set a usable
            // `stream.data`; the holder only REFINES it, overriding when the
            // authoritative length lands within the syntactic window.
            // Best-effort: a self-referential, cyclic, unresolvable, or
            // out-of-window holder keeps the parser's endstream-scan value.
            Some(endstream_pos) => {
                if isl.holder != object_ref {
                    // Mark this object resolution in-progress before recursing
                    // into the holder: a cyclic length-holder chain (A's /Length
                    // -> B -> ... -> A) would otherwise recurse forever
                    // (resolve() does not otherwise mark in-progress). The cyclic
                    // re-entry hits the `Reserved => Null` arm and the holder
                    // reads as non-Integer -> endstream-scan fallback. A holder
                    // resolution error is likewise non-fatal here: fall back
                    // rather than failing the whole stream.
                    self.cache.set_reserved(object_ref);
                    if let Ok(Object::Integer(n)) = self.resolve_borrowed(isl.holder) {
                        if let (Ok(n), Object::Stream(stream)) = (usize::try_from(*n), &mut *object)
                        {
                            let auth_end = isl.data_start.saturating_add(n);
                            // Override only when the authoritative length lands
                            // STRICTLY inside the window — i.e. the resolved
                            // /Length is the spec content length and `endstream`
                            // (plus its mandatory preceding EOL) still follows.
                            // Then the exact `n` content bytes are used verbatim
                            // (no EOL trim), fixing non-QDF streams whose data
                            // legitimately ends with a newline.
                            //
                            // STRICT `<` for QDF by design. `auth_end ==
                            // endstream_pos` is ambiguous: produced BOTH by
                            // flpdf's QDF holder convention (n counts the whole
                            // window incl. the framing EOL) AND by a
                            // non-conformant PDF lacking the ISO 32000-1 §7.3.8.1
                            // mandatory pre-`endstream` EOL. These want opposite
                            // handling and are indistinguishable from the object
                            // bytes alone. Treating the exact-window case as QDF
                            // (keep the parser's endstream-scan, which strips one
                            // framing EOL) preserves the pinned QDF round-trip /
                            // idempotence invariant; relaxing to `<=` empirically
                            // regresses qdf_tests
                            // `qdf_mode_round_trip_content_preserved` and
                            // `qdf_output_is_idempotent`. Whole-file QDF
                            // detection picks the branch: strict `<` for QDF,
                            // inclusive `<=` for non-QDF. A too-large/garbage
                            // holder also lands here → safe endstream-scan
                            // fallback.
                            let within_window = if self.is_qdf {
                                auth_end < endstream_pos
                            } else {
                                auth_end <= endstream_pos
                            };
                            if within_window && auth_end <= bytes.len() {
                                stream.data = bytes[isl.data_start..auth_end].to_vec();
                            }
                        }
                    }
                }
                Ok(())
            }
            // No line-anchored `endstream` existed: the writer used
            // `NewlineBeforeEndstream::Never` with a non-EOL-ending payload, so
            // `endstream` is adjacent to the last content byte and the parser
            // could not delimit the payload (it returned an empty placeholder).
            // The holder is the SOLE authority and the EXACT content length — it
            // must resolve to a non-negative integer whose endpoint lands on a
            // well-formed stream terminator (`endstream` ... `endobj`). Anything
            // else (self-referential, unresolvable, out of bounds, or a holder
            // pointing at an `endstream` byte sequence inside the payload) is
            // unrecoverable: fail loudly rather than surface the empty
            // placeholder or truncated data.
            None => {
                let resolved = if isl.holder == object_ref {
                    None
                } else {
                    self.cache.set_reserved(object_ref);
                    match self.resolve_borrowed(isl.holder) {
                        Ok(Object::Integer(n)) => usize::try_from(*n).ok(),
                        Ok(_) => None,
                        // A genuine resolution error (I/O, decryption, …) is more
                        // informative than the generic parse error below — the
                        // wrapper restores the cache entry, so just propagate it.
                        Err(err) => return Err(err),
                    }
                };
                let auth_end = resolved.and_then(|n| isl.data_start.checked_add(n));
                let valid = auth_end
                    .is_some_and(|end| end <= bytes.len() && stream_end_boundary_at(bytes, end));
                match (valid, &mut *object) {
                    (true, Object::Stream(stream)) => {
                        // `valid` guarantees `auth_end` is `Some` and in bounds.
                        let end = auth_end.unwrap();
                        stream.data = bytes[isl.data_start..end].to_vec();
                        Ok(())
                    }
                    _ => Err(Error::parse(isl.data_start, "stream data exceeds input")),
                }
            }
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
                self.reader.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::new();
                self.reader.read_to_end(&mut bytes)?;
                // Use the detailed parser so an ObjStm container whose own
                // /Length is an indirect reference (including the adjacent
                // no-EOL `endstream` case) goes through the same authoritative
                // re-slice as top-level streams; otherwise its compressed
                // members would be unreadable.
                let (parsed_ref, mut object, indirect_len) =
                    parse_indirect_object_detailed(&bytes)?;
                if parsed_ref != stream_ref {
                    return Ok(false);
                }
                if let Some(isl) = indirect_len {
                    self.apply_indirect_stream_length(
                        stream_ref,
                        &mut object,
                        isl,
                        &bytes,
                        offset,
                    )?;
                }
                // `apply_indirect_stream_length` already restored the lazy entry,
                // so a decryption error here leaves it `Unresolved`.
                let object = self.decrypt_resolved_object(stream_ref, object)?;
                self.cache.set_resolved(stream_ref, object.clone());
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

        let (parent_ref, parent_index, object) =
            self.parse_object_stream_chain_entry(stream_ref, &stream_object, index)?;
        let object = self.decrypt_resolved_object(object_ref, object)?;
        self.compressed_member_parents
            .insert(object_ref, (parent_ref, parent_index));
        self.cache.set_resolved(object_ref, object);
        Ok(true)
    }

    fn decrypt_resolved_object(&self, object_ref: ObjectRef, mut object: Object) -> Result<Object> {
        let Some(encryption) = &self.encryption else {
            return Ok(object);
        };
        if Some(object_ref) == encryption.encrypt_ref {
            return Ok(object);
        }

        decrypt_object_strings(
            object_ref,
            &mut object,
            encryption.string_mode,
            &encryption.file_key,
            encryption.encrypt_ref,
        )?;
        if let Object::Stream(stream) = &mut object {
            if !encryption.encrypt_metadata && is_metadata_stream(&stream.dict) {
                return Ok(object);
            }
            if stream_has_explicit_crypt_filter(&stream.dict) {
                apply_explicit_crypt_filters(object_ref, stream, encryption)?;
            } else {
                decrypt_stream_bytes(
                    object_ref,
                    &mut stream.data,
                    encryption.stream_mode,
                    &encryption.file_key,
                )?;
            }
        }
        Ok(object)
    }

    fn parse_object_stream_chain_entry(
        &mut self,
        stream_ref: ObjectRef,
        stream_object: &crate::Stream,
        target_index: u32,
    ) -> Result<(ObjectRef, u32, Object)> {
        let (member_stream_ref, member_index, member_stream) =
            self.object_stream_chain_member(stream_ref, stream_object, target_index)?;
        let object = parse_object_stream_entry(&member_stream, member_index)?;
        Ok((member_stream_ref, member_index, object))
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

impl<'a> Pdf<Cursor<&'a [u8]>> {
    /// Open a PDF document from a borrowed byte slice without wrapping it in a `Cursor` manually.
    ///
    /// This is a zero-copy convenience wrapper around [`Pdf::open`]. The resulting handle
    /// borrows `bytes` for its lifetime, so it is not `'static` and cannot be moved out of
    /// the scope that owns the original slice.
    ///
    /// For an owned, movable version see [`Pdf::open_mem_owned`].
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
    /// let mut pdf = Pdf::open_mem(&bytes)?;
    /// println!("version {}", pdf.version());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_mem(bytes: &'a [u8]) -> crate::Result<Self> {
        Self::open(Cursor::new(bytes))
    }

    /// Open a PDF document from a borrowed byte slice with explicit open options.
    ///
    /// Like [`Pdf::open_mem`] but accepts a [`PdfOpenOptions`] struct for repair and
    /// password configuration, mirroring [`Pdf::open_with_options`].
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
    /// let mut pdf = Pdf::open_mem_with_options(&bytes, opts)?;
    /// println!("version {}", pdf.version());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_mem_with_options(bytes: &'a [u8], options: PdfOpenOptions) -> crate::Result<Self> {
        Self::open_with_options(Cursor::new(bytes), options)
    }
}

impl Pdf<Cursor<Vec<u8>>> {
    /// Open a PDF document from an owned byte vector without wrapping it in a `Cursor` manually.
    ///
    /// This is the owned counterpart to [`Pdf::open_mem`]. The handle takes ownership of
    /// `bytes` and is therefore `'static`—it can be freely moved and stored in data structures.
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

/// `bytes[pos..]` matches `keyword` AND the byte after it is a PDF token
/// boundary (whitespace, a delimiter, or EOF). Returns the offset just past the
/// keyword. The boundary check prevents matching a prefix of a longer run of
/// regular characters (e.g. `endstreamendobj`, `endobjXX`) as the keyword token.
fn keyword_token_at(bytes: &[u8], pos: usize, keyword: &[u8]) -> Option<usize> {
    let end = pos.checked_add(keyword.len())?;
    if bytes.get(pos..end) != Some(keyword) {
        return None;
    }
    match bytes.get(end) {
        None => Some(end),
        Some(&c) if crate::parser::is_ws(c) || crate::parser::is_delimiter(c) => Some(end),
        Some(_) => None,
    }
}

/// True when a well-formed stream terminator begins exactly at `pos` in `bytes`:
/// the `endstream` keyword token, optional whitespace, then the `endobj` keyword
/// token.
///
/// Used to validate an indirect `/Length` holder before trusting it as the
/// authoritative payload boundary for the adjacent-`endstream` case
/// (`NewlineBeforeEndstream::Never`, non-EOL-ending payload), where the parser
/// could not delimit the stream syntactically. `pos` is the holder-derived end
/// of the content; by construction of that case there is no EOL before
/// `endstream` (otherwise it would have been line-anchored and taken the other
/// branch), so `endstream` must sit at `pos` directly. Requiring the trailing
/// `endobj` — the indirect object terminator that always follows a stream's
/// `endstream` — plus a PDF token boundary after each keyword rejects a corrupt
/// holder that points at an `endstream`/`endobj` byte sequence occurring INSIDE
/// the payload (e.g. `endstreamendobj`), which would otherwise truncate the data.
fn stream_end_boundary_at(bytes: &[u8], pos: usize) -> bool {
    let Some(mut p) = keyword_token_at(bytes, pos, b"endstream") else {
        return false;
    };
    // Whitespace between `endstream` and the `endobj` object terminator.
    while bytes.get(p).is_some_and(|&c| crate::parser::is_ws(c)) {
        p += 1;
    }
    keyword_token_at(bytes, p, b"endobj").is_some()
}

fn decrypt_object_strings(
    object_ref: ObjectRef,
    object: &mut Object,
    mode: EncryptionMode,
    file_key: &[u8],
    encrypt_ref: Option<ObjectRef>,
) -> Result<()> {
    match mode {
        EncryptionMode::Rc4 => {
            let key = per_object_key(
                file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                ObjectKeyAlg::Rc4,
            );
            decrypt_strings_in_object(
                object_ref,
                object,
                StringCipher::Rc4 { key: &key },
                encrypt_ref,
            )
        }
        EncryptionMode::Aes128 => {
            let key = per_object_key(
                file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                ObjectKeyAlg::Aes,
            );
            let key = aes128_object_key(&key)?;
            decrypt_strings_in_object(
                object_ref,
                object,
                StringCipher::Aes128 { key: &key },
                encrypt_ref,
            )
        }
        EncryptionMode::Identity => Ok(()),
        EncryptionMode::Aes256 => {
            let key = aes256_file_key(file_key)?;
            decrypt_strings_in_object(
                object_ref,
                object,
                StringCipher::Aes256 { key: &key },
                encrypt_ref,
            )
        }
    }
}

fn decrypt_stream_bytes(
    object_ref: ObjectRef,
    bytes: &mut Vec<u8>,
    mode: EncryptionMode,
    file_key: &[u8],
) -> Result<()> {
    match mode {
        EncryptionMode::Rc4 => {
            let key = per_object_key(
                file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                ObjectKeyAlg::Rc4,
            );
            decrypt_cipher_bytes(bytes, StringCipher::Rc4 { key: &key })
        }
        EncryptionMode::Aes128 => {
            let key = per_object_key(
                file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                ObjectKeyAlg::Aes,
            );
            let key = aes128_object_key(&key)?;
            decrypt_cipher_bytes(bytes, StringCipher::Aes128 { key: &key })
        }
        EncryptionMode::Identity => Ok(()),
        EncryptionMode::Aes256 => {
            let key = aes256_file_key(file_key)?;
            decrypt_cipher_bytes(bytes, StringCipher::Aes256 { key: &key })
        }
    }
}

fn apply_explicit_crypt_filters(
    object_ref: ObjectRef,
    stream: &mut crate::Stream,
    encryption: &EncryptionState,
) -> Result<()> {
    let decoded = crate::filters::decode_stream_data_with_crypt_filter(
        &stream.dict,
        &stream.data,
        |decode_params, bytes| {
            let mode = explicit_crypt_mode(encryption, decode_params)?;
            let mut decrypted = bytes.to_vec();
            decrypt_stream_bytes(object_ref, &mut decrypted, mode, &encryption.file_key)?;
            Ok(decrypted)
        },
    )?;
    stream.data = decoded;
    stream.dict.remove("Filter");
    stream.dict.remove("DecodeParms");
    Ok(())
}

fn explicit_crypt_mode(
    encryption: &EncryptionState,
    decode_params: Option<&Object>,
) -> Result<EncryptionMode> {
    let Some(params) = decode_params.and_then(Object::as_dict) else {
        return Ok(EncryptionMode::Identity);
    };
    let name: &[u8] = match params.get("Name") {
        None => return Ok(EncryptionMode::Identity),
        Some(object) => object.as_name().ok_or_else(|| EncryptedError::Malformed {
            reason: "/Crypt /DecodeParms /Name is not a name".into(),
        })?,
    };
    if name == b"Identity" {
        return Ok(EncryptionMode::Identity);
    }
    encryption.crypt_filters.get(name).copied().ok_or_else(|| {
        EncryptedError::Malformed {
            reason: format!("/CF entry '{}' not found", String::from_utf8_lossy(name)),
        }
        .into()
    })
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

fn aes256_file_key(file_key: &[u8]) -> Result<[u8; 32]> {
    file_key.try_into().map_err(|_| {
        EncryptedError::Malformed {
            reason: "AES-256 file key is not 32 bytes".into(),
        }
        .into()
    })
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
) -> Result<Object> {
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

    let mut header_parser = Parser::new(&stream_data);
    let mut object_offsets = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        let _object_number = parse_non_negative_u64(
            header_parser.integer_for_indirect()?,
            "object stream object number",
        )?;
        let object_offset = parse_non_negative_u64(
            header_parser.integer_for_indirect()?,
            "object stream object offset",
        )?;
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

    let mut object_parser = Parser::new(&stream_data[start..]);
    object_parser.object()
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

fn standard_v4_or_legacy_modes(encrypt: &Dictionary) -> Result<(EncryptionMode, EncryptionMode)> {
    if required_integer(encrypt, "V").ok() != Some(4) {
        return Ok((EncryptionMode::Rc4, EncryptionMode::Rc4));
    }
    Ok((
        v4_mode_for_selector(encrypt, crypt_filter_selector(encrypt, "StmF")?)?,
        v4_mode_for_selector(encrypt, crypt_filter_selector(encrypt, "StrF")?)?,
    ))
}

fn standard_r5_or_r6_modes(encrypt: &Dictionary) -> Result<(EncryptionMode, EncryptionMode)> {
    Ok((
        r5_or_r6_mode_for_selector(encrypt, crypt_filter_selector(encrypt, "StmF")?)?,
        r5_or_r6_mode_for_selector(encrypt, crypt_filter_selector(encrypt, "StrF")?)?,
    ))
}

fn v4_mode_for_selector(encrypt: &Dictionary, selector: Option<String>) -> Result<EncryptionMode> {
    let Some(selector) = selector else {
        return Ok(EncryptionMode::Identity);
    };
    if selector == "Identity" {
        return Ok(EncryptionMode::Identity);
    }
    let cfm = crypt_filter_method_for_name(encrypt, &selector)?.unwrap_or_else(|| "V2".into());
    match cfm.as_str() {
        "V2" => Ok(EncryptionMode::Rc4),
        "AESV2" => Ok(EncryptionMode::Aes128),
        "Identity" => Ok(EncryptionMode::Identity),
        _ => unsupported_crypt_filter(encrypt, Some(cfm)),
    }
}

fn r5_or_r6_mode_for_selector(
    encrypt: &Dictionary,
    selector: Option<String>,
) -> Result<EncryptionMode> {
    let Some(selector) = selector else {
        return Ok(EncryptionMode::Aes256);
    };
    if selector == "Identity" {
        return Ok(EncryptionMode::Identity);
    }
    let cfm = crypt_filter_method_for_name(encrypt, &selector)?.unwrap_or_else(|| "AESV3".into());
    match cfm.as_str() {
        "AESV3" => Ok(EncryptionMode::Aes256),
        "Identity" => Ok(EncryptionMode::Identity),
        _ => unsupported_crypt_filter(encrypt, Some(cfm)),
    }
}

fn crypt_filter_selector(encrypt: &Dictionary, key: &str) -> Result<Option<String>> {
    let Some(value) = encrypt.get(key) else {
        return Ok(None);
    };
    let name = value.as_name().ok_or_else(|| EncryptedError::Malformed {
        reason: format!("/{key} entry is not a name"),
    })?;
    Ok(Some(String::from_utf8_lossy(name).into_owned()))
}

fn crypt_filter_method_for_name(encrypt: &Dictionary, name: &str) -> Result<Option<String>> {
    let Some(cf) = encrypt.get("CF").and_then(Object::as_dict) else {
        return Err(EncryptedError::Malformed {
            reason: format!("/CF entry '{name}' not found"),
        }
        .into());
    };
    let Some(filter) = cf.get(name).and_then(Object::as_dict) else {
        return Err(EncryptedError::Malformed {
            reason: format!("/CF entry '{name}' not found"),
        }
        .into());
    };
    let Some(value) = filter.get("CFM") else {
        return Ok(None);
    };
    let cfm = value.as_name().ok_or_else(|| EncryptedError::Malformed {
        reason: format!("/CF/{name}/CFM entry is not a name"),
    })?;
    Ok(Some(String::from_utf8_lossy(cfm).into_owned()))
}

fn crypt_filter_modes(
    encrypt: &Dictionary,
    revision: i64,
) -> Result<BTreeMap<Vec<u8>, EncryptionMode>> {
    let mut modes = BTreeMap::new();
    let Some(cf) = encrypt.get("CF").and_then(Object::as_dict) else {
        return Ok(modes);
    };
    for (name, value) in cf.iter() {
        let Some(filter) = value.as_dict() else {
            return Err(EncryptedError::Malformed {
                reason: format!(
                    "/CF entry '{}' is not a dictionary",
                    String::from_utf8_lossy(name)
                ),
            }
            .into());
        };
        let cfm = match filter.get("CFM") {
            None if revision >= 5 => "AESV3".to_string(),
            None => "V2".to_string(),
            Some(Object::Name(cfm)) => String::from_utf8_lossy(cfm).to_string(),
            Some(_) => {
                return Err(EncryptedError::Malformed {
                    reason: format!(
                        "/CF/{}/CFM entry is not a name",
                        String::from_utf8_lossy(name)
                    ),
                }
                .into())
            }
        };
        let mode = match (revision, cfm.as_str()) {
            (_, "Identity") => EncryptionMode::Identity,
            (5 | 6, "AESV3") => EncryptionMode::Aes256,
            (5 | 6, _) => unsupported_crypt_filter(encrypt, Some(cfm))?,
            (_, "V2") => EncryptionMode::Rc4,
            (_, "AESV2") => EncryptionMode::Aes128,
            (_, _) => unsupported_crypt_filter(encrypt, Some(cfm))?,
        };
        modes.insert(name.to_vec(), mode);
    }
    Ok(modes)
}

fn unsupported_crypt_filter<T>(encrypt: &Dictionary, cfm: Option<String>) -> Result<T> {
    Err(EncryptedError::UnsupportedHandler {
        filter: required_name(encrypt, "Filter")?.to_string(),
        v: required_integer(encrypt, "V")?,
        r: required_integer(encrypt, "R")?,
        cfm,
    }
    .into())
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

    #[test]
    fn keyword_token_at_requires_token_boundary() {
        // Keyword at EOF (nothing after) is a valid token.
        assert_eq!(keyword_token_at(b"endobj", 0, b"endobj"), Some(6));
        // Followed by whitespace / a delimiter is a valid token.
        assert_eq!(keyword_token_at(b"endobj\n", 0, b"endobj"), Some(6));
        assert_eq!(keyword_token_at(b"endobj/", 0, b"endobj"), Some(6));
        // A longer run of regular chars (no boundary) is NOT the keyword token.
        assert_eq!(keyword_token_at(b"endobjX", 0, b"endobj"), None);
        // Non-match.
        assert_eq!(keyword_token_at(b"endstream", 0, b"endobj"), None);
    }

    #[test]
    fn stream_end_boundary_at_validates_terminator() {
        // `endstream` + whitespace + `endobj`, then EOF/boundary: valid.
        assert!(stream_end_boundary_at(b"endstream\nendobj", 0));
        assert!(stream_end_boundary_at(b"endstream endobj\n", 0));
        // `endstreamendobj` with no separator after `endstream`: rejected.
        assert!(!stream_end_boundary_at(b"endstreamendobj", 0));
        // `endstream` without the trailing `endobj`: rejected.
        assert!(!stream_end_boundary_at(b"endstream more", 0));
        // Not positioned on `endstream`: rejected.
        assert!(!stream_end_boundary_at(b"xendstream\nendobj", 0));
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
    // Acceptance (2): open_mem(&[u8]) opens an in-memory PDF
    // ------------------------------------------------------------------

    #[test]
    fn open_mem_opens_minimal_pdf() {
        let bytes = minimal_pdf_bytes();
        let mut pdf = Pdf::open_mem(&bytes).expect("open_mem should succeed");
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

        let mut pdf_mem = Pdf::open_mem(&bytes).expect("open_mem should succeed");
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
        let mut pdf = Pdf::open_mem_with_options(&bytes, opts).expect("open_mem_with_options");
        let refs = page_refs(&mut pdf).expect("page_refs");
        assert_eq!(refs.len(), 1);
    }
}
