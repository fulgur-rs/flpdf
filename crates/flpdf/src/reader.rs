pub(crate) mod file_object;

use crate::cache::{CacheEntry, ObjectCache};
use crate::error::EncryptedError;
use crate::object::collect_qpdf_object_references;
use crate::parser::{parse_indirect_object_detailed_qpdf, parse_qpdf_file_object, Parser};
use crate::security::password::{normalize_password, PasswordMode};
use crate::security::standard::{
    check_owner_password, check_owner_password_r5, check_owner_password_r6,
    check_owner_password_v4, check_user_password, check_user_password_r5, check_user_password_r6,
    check_user_password_v4, decrypt_cipher_bytes, decrypt_strings_in_object, per_object_key,
    ObjectKeyAlg, StandardHandlerInputs, StandardHandlerR5Inputs, StringCipher,
};
use crate::xref::load_xref_state_with_repair;
use crate::{
    Diagnostic, Diagnostics, Dictionary, Error, Object, ObjectRef, Result, XrefForm, XrefOffset,
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
    trailer: Dictionary,
    startxref: u64,
    last_xref_form: XrefForm,
    repair_diagnostics: Diagnostics,
    cache: ObjectCache,
    compressed_member_parents: BTreeMap<ObjectRef, (ObjectRef, u32)>,
    /// Every uncompressed object offset, sorted ascending and deduplicated. Used
    /// to bound a single object read to the start of the next object in the file
    /// (objects do not overlap in a well-formed PDF), so resolving one object
    /// cannot read/parse the whole remaining file — which would make resolving
    /// many objects quadratic, a CPU DoS on a crafted (e.g. repaired) document.
    sorted_object_offsets: Vec<u64>,
    /// Remaining read-to-end fallbacks allowed when a bounded object window does
    /// not contain a complete object (a corrupt offset pointing inside another
    /// object, or a header-like line recorded inside stream data during repair).
    /// Each fallback may scan to EOF, so the count is capped: a handful of bad
    /// boundaries in an otherwise valid file still resolve, but a document full
    /// of objects whose bodies run to EOF cannot revive the quadratic cost.
    resolution_fallbacks_remaining: u32,
    source_xref_offsets: Vec<(ObjectRef, u64)>,
    source_xref_entries: BTreeMap<ObjectRef, XrefOffset>,
    dirty_object_refs: BTreeSet<ObjectRef>,
    /// Exact source framing EOLs removed while a line-anchored `endstream`
    /// scan remained authoritative. Rewriting restores this private metadata
    /// before applying the selected stream policy, matching qpdf's recovered
    /// raw stream bytes for missing, invalid, and unresolved `/Length` values.
    recovered_stream_eols: BTreeMap<ObjectRef, crate::parser::RecoveredStreamEol>,
    /// Streams whose cached representation has already accounted for source
    /// framing. This includes actual decryption and selective explicit
    /// `/Crypt` removal. Recovered framing must not be appended later in either
    /// case: ciphertext framing is not plaintext, while explicit-filter
    /// framing is consumed while transforming the declared chain. Metadata and
    /// document-level Identity streams remain source-represented and absent.
    transformed_stream_refs: BTreeSet<ObjectRef>,
    /// Valid indirect references discovered while preparing qpdf JSON whose
    /// exact object generation has no live xref/cache target.
    qpdf_dangling_refs: BTreeSet<ObjectRef>,
    /// Valid indirect references parsed from every classic trailer and xref
    /// stream dictionary in the source `/Prev` chain.
    qpdf_trailer_references: BTreeSet<ObjectRef>,
    /// Xref stream objects parsed while following the source `/Prev` chain.
    /// Kept outside the public cache so superseded/free streams remain visible
    /// only through qpdf's raw object view.
    qpdf_parsed_xref_streams: BTreeMap<ObjectRef, Object>,
    /// Objects removed through [`Self::delete_object`]. qpdf's object cache
    /// removal is persistent across repeated JSON preparation; keep this
    /// separate from immutable source/trailer discovery so those seeds cannot
    /// resurrect an explicitly removed reference.
    qpdf_removed_refs: BTreeSet<ObjectRef>,
    /// Monotonic observation matching qpdf's `everCalledGetAllPages()`.
    ever_called_get_all_pages: bool,
    encryption: Option<EncryptionState>,
}

pub(crate) struct QpdfPreparedObjects {
    pub(crate) refs: Vec<ObjectRef>,
    pub(crate) max_object_id: u32,
}

struct FileObjectRead {
    bytes: Vec<u8>,
    object_ref: ObjectRef,
    object: Object,
    indirect_length: Option<crate::parser::IndirectStreamLength>,
    recovered_stream_eol: Option<crate::parser::RecoveredStreamEol>,
    empty_offset: Option<usize>,
    expected_endobj_offset: Option<usize>,
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

// Upper bound on read-to-end fallbacks during object resolution (see
// `resolution_fallbacks_remaining`). Each fallback may scan to EOF, so the total
// fallback work is bounded by this many file scans — O(file size), not the
// quadratic cost an unbounded read-to-end per object would incur. 64 tolerates a
// handful of corrupt/overlapping offsets in an otherwise valid file while still
// defeating a flood of objects whose bodies run to EOF.
const MAX_RESOLUTION_FALLBACKS: u32 = 64;

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
        let loaded_state = load_xref_state_with_repair(&mut reader, options.repair)?;
        let loaded = loaded_state.loaded;
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
        let mut sorted_object_offsets: Vec<u64> = loaded
            .entries
            .values()
            .filter_map(|offset| match offset {
                crate::XrefOffset::Offset(offset) => Some(*offset),
                _ => None,
            })
            .collect();
        sorted_object_offsets.sort_unstable();
        sorted_object_offsets.dedup();
        let cache = ObjectCache::from_offsets(&loaded.entries);
        let mut pdf = Self {
            reader,
            version: loaded.version,
            trailer: loaded.trailer,
            startxref: loaded.startxref,
            last_xref_form: loaded.last_xref_form,
            repair_diagnostics: loaded.repair_diagnostics,
            cache,
            compressed_member_parents: BTreeMap::new(),
            sorted_object_offsets,
            resolution_fallbacks_remaining: MAX_RESOLUTION_FALLBACKS,
            source_xref_offsets,
            source_xref_entries,
            dirty_object_refs: BTreeSet::new(),
            recovered_stream_eols: BTreeMap::new(),
            transformed_stream_refs: BTreeSet::new(),
            qpdf_dangling_refs: BTreeSet::new(),
            qpdf_trailer_references: loaded_state.trailer_references,
            qpdf_parsed_xref_streams: loaded_state.parsed_xref_streams,
            qpdf_removed_refs: BTreeSet::new(),
            ever_called_get_all_pages: false,
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

    /// Whether this document has been asked to enumerate its complete page tree.
    ///
    /// This is monotonic for the lifetime of the [`Pdf`] and mirrors qpdf's
    /// `everCalledGetAllPages()` observation used by JSON v2 metadata.
    pub fn ever_called_get_all_pages(&self) -> bool {
        self.ever_called_get_all_pages
    }

    pub(crate) fn mark_get_all_pages_called(&mut self) {
        self.ever_called_get_all_pages = true;
    }

    /// Adobe extension level from the catalog's `/Extensions /ADBE
    /// /ExtensionLevel`, resolving indirect references at each step. Returns
    /// `None` when any link in that chain is absent or is not the expected
    /// type. Only the `/ADBE` developer prefix is honoured, matching qpdf's
    /// `--check` version banner and the extension level qpdf accumulates into
    /// its `max_input_version`.
    pub fn adobe_extension_level(&mut self) -> Option<i64> {
        let root_ref = self.trailer().get_ref("Root")?;
        let catalog = self.resolve(root_ref).ok()?;
        let extensions = resolve_object_value(self, catalog.as_dict()?.get("Extensions")?.clone())?;
        let adbe = resolve_object_value(self, extensions.as_dict()?.get("ADBE")?.clone())?;
        let level = resolve_object_value(self, adbe.as_dict()?.get("ExtensionLevel")?.clone())?;
        level.as_integer()
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
        self.cache.set_resolved(object_ref, object);
        self.dirty_object_refs.insert(object_ref);
    }

    pub fn delete_object(&mut self, object_ref: ObjectRef) {
        if object_ref.number != 0 {
            self.qpdf_removed_refs.insert(object_ref);
        }
        self.qpdf_parsed_xref_streams.remove(&object_ref);
        self.qpdf_dangling_refs.remove(&object_ref);
        self.recovered_stream_eols.remove(&object_ref);
        self.transformed_stream_refs.remove(&object_ref);
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

    /// Resolve every live xref/cache object and register valid indirect
    /// references whose exact generation has no live target. This mirrors the
    /// object-cache preparation performed by qpdf's `fixDanglingReferences()`
    /// for JSON metadata without exposing placeholders through the public
    /// object enumeration APIs.
    pub(crate) fn prepare_qpdf_json_objects(&mut self) -> Result<QpdfPreparedObjects> {
        let live_snapshot = self.live_object_refs();
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
            let has_live_target = matches!(
                self.cache.entry(object_ref),
                Some(
                    CacheEntry::Unresolved { .. }
                        | CacheEntry::Compressed { .. }
                        | CacheEntry::Resolved(_)
                )
            );
            if !has_live_target {
                self.qpdf_dangling_refs.insert(object_ref);
                if self.cache.entry(object_ref).is_none() {
                    self.cache.set_missing(object_ref);
                }
            }
        }

        let mut refs = self.live_object_refs();
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
            // cov:ignore-start: rustfmt parks the match-arm guard on its own line, and llvm-cov instruments the guard head separately from the arm body — the body IS exercised (see linearized_hint_ref_accepts_real_literal_value) but the guard-only line always shows zero hits.
            Object::Real(value) | Object::RealLiteral { value, .. }
                if value.is_finite() && *value > 0.0 =>
            {
                Some(candidate)
            }
            // cov:ignore-end
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
    fn read_object_at(&mut self, offset: u64) -> Result<FileObjectRead> {
        let next = self.next_object_offset(offset);
        self.reader.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        match next {
            Some(next) => {
                let len = next.saturating_sub(offset);
                self.reader.by_ref().take(len).read_to_end(&mut bytes)?;
            }
            None => {
                self.reader.read_to_end(&mut bytes)?;
            }
        }

        match parse_indirect_object_detailed_qpdf(&bytes) {
            Ok(parsed) => Ok(FileObjectRead {
                bytes,
                object_ref: parsed.object_ref,
                object: parsed.object,
                indirect_length: parsed.indirect_length,
                recovered_stream_eol: parsed.recovered_stream_eol,
                empty_offset: parsed.empty_offset,
                expected_endobj_offset: parsed.expected_endobj_offset,
            }),
            // The window stopped short of a complete object. Only a bounded
            // window (`next` is `Some`) can do this; if we already read to EOF,
            // the input itself is the limit and the error is real.
            Err(window_err) if next.is_some() && self.resolution_fallbacks_remaining > 0 => {
                self.resolution_fallbacks_remaining -= 1;
                self.reader.seek(SeekFrom::Start(offset))?;
                let mut full = Vec::new();
                self.reader.read_to_end(&mut full)?;
                // Prefer the full parse, but keep the window error if even the
                // full read cannot parse (so the failure stays the parser's).
                match parse_indirect_object_detailed_qpdf(&full) {
                    Ok(parsed) => Ok(FileObjectRead {
                        bytes: full,
                        object_ref: parsed.object_ref,
                        object: parsed.object,
                        indirect_length: parsed.indirect_length,
                        recovered_stream_eol: parsed.recovered_stream_eol,
                        empty_offset: parsed.empty_offset,
                        expected_endobj_offset: parsed.expected_endobj_offset,
                    }),
                    Err(_) => Err(window_err),
                }
            }
            Err(window_err) => Err(window_err),
        }
    }

    pub(crate) fn resolve_qpdf_json_object(&mut self, object_ref: ObjectRef) -> Result<Object> {
        if self.resolve_to_cache(object_ref)? {
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
                let parsed = self.read_object_at(offset)?;
                if parsed.object_ref != object_ref {
                    return Ok(false);
                }
                let mut object = parsed.object;
                let mut endstream_scan_authoritative = parsed.recovered_stream_eol.is_some();
                // When the stream's /Length is an indirect reference, the parser
                // had no xref and recorded the payload window instead of a
                // resolved length. Resolve the holder via the xref and re-slice
                // to the authoritative length. This MUST happen before
                // decryption: `object`/`bytes` are still ciphertext here, and
                // `decrypt_resolved_object` decrypts in place afterwards.
                if let Some(isl) = parsed.indirect_length {
                    let used_endstream_scan = self.apply_indirect_stream_length(
                        object_ref,
                        &mut object,
                        isl,
                        &parsed.bytes,
                        offset,
                    )?;
                    if parsed.recovered_stream_eol.is_some() {
                        endstream_scan_authoritative = used_endstream_scan;
                    }
                }
                // `apply_indirect_stream_length` already restored the lazy
                // `Unresolved` entry, so a decryption error here leaves it
                // `Unresolved` (not `Reserved`) and the failure stays loud on a
                // retry.
                let recovered_eol = endstream_scan_authoritative
                    .then_some(parsed.recovered_stream_eol)
                    .flatten()
                    .map(crate::parser::RecoveredStreamEol::as_bytes);
                let (object, stream_payload_transformed) =
                    self.decrypt_resolved_object(object_ref, object, recovered_eol)?;
                self.cache.set_resolved(object_ref, object);
                if stream_payload_transformed {
                    self.transformed_stream_refs.insert(object_ref);
                } else {
                    self.transformed_stream_refs.remove(&object_ref);
                }
                if endstream_scan_authoritative {
                    if let Some(eol) = parsed.recovered_stream_eol {
                        self.recovered_stream_eols.insert(object_ref, eol);
                    }
                } else {
                    self.recovered_stream_eols.remove(&object_ref);
                }
                self.record_file_object_warnings(
                    object_ref,
                    offset,
                    parsed.empty_offset,
                    parsed.expected_endobj_offset,
                );
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

    fn record_file_object_warnings(
        &mut self,
        object_ref: ObjectRef,
        offset: u64,
        empty_offset: Option<usize>,
        expected_endobj_offset: Option<usize>,
    ) {
        if let Some(relative_offset) = empty_offset {
            self.push_warning(format!(
                "(object {} {}, offset {}): empty object treated as null",
                object_ref.number,
                object_ref.generation,
                offset.saturating_add(relative_offset as u64)
            ));
        }
        if let Some(relative_offset) = expected_endobj_offset {
            self.push_warning(format!(
                "(object {} {}, offset {}): expected endobj",
                object_ref.number,
                object_ref.generation,
                offset.saturating_add(relative_offset as u64)
            ));
        }
    }

    /// Apply a parser-recorded indirect `/Length` ([`crate::parser::IndirectStreamLength`]) to a
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
    ///
    /// Returns `true` only when a line-anchored `endstream` scan remains the
    /// payload authority; a valid in-window indirect integer re-slice returns
    /// `false`.
    fn apply_indirect_stream_length(
        &mut self,
        object_ref: ObjectRef,
        object: &mut Object,
        isl: crate::parser::IndirectStreamLength,
        bytes: &[u8],
        offset: u64,
    ) -> Result<bool> {
        let result = self.reslice_indirect_stream_length(object_ref, object, isl, bytes);
        self.cache.set_unresolved(object_ref, offset);
        result
    }

    /// Inner half of [`apply_indirect_stream_length`](Self::apply_indirect_stream_length):
    /// performs the holder resolution and re-slice. May leave `object_ref`
    /// `Reserved`; the wrapper restores the cache entry. Its boolean has the
    /// same endstream-scan-authority meaning as the wrapper's return value.
    fn reslice_indirect_stream_length(
        &mut self,
        object_ref: ObjectRef,
        object: &mut Object,
        isl: crate::parser::IndirectStreamLength,
        bytes: &[u8],
    ) -> Result<bool> {
        match isl.endstream_pos {
            // The parser located a line-anchored `endstream` and set a usable
            // `stream.data`; the holder only REFINES it, overriding when the
            // authoritative length lands within the syntactic window.
            // Best-effort: a self-referential, cyclic, unresolvable, or
            // out-of-window holder keeps the parser's endstream-scan value.
            Some(endstream_pos) => {
                let mut endstream_scan_authoritative = true;
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
                            let auth_end = isl.data_start.checked_add(n);
                            // Override whenever the authoritative length lands
                            // at or before `endstream`. qpdf QDF holders contain
                            // the logical payload length; `%QDF:
                            // ignore_newline` accounts for any extra framing LF
                            // in fix-qdf rather than changing reader semantics.
                            // A too-large/garbage holder falls back to the safe
                            // endstream scan.
                            if let Some(auth_end) =
                                auth_end.filter(|&end| end <= endstream_pos && end <= bytes.len())
                            {
                                stream.data = bytes[isl.data_start..auth_end].to_vec();
                                endstream_scan_authoritative = false;
                            }
                        }
                    }
                }
                Ok(endstream_scan_authoritative)
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
                        Ok(false)
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
                // Use the detailed parser (via `read_object_at`) so an ObjStm
                // container whose own /Length is an indirect reference (including
                // the adjacent no-EOL `endstream` case) goes through the same
                // authoritative re-slice as top-level streams; otherwise its
                // compressed members would be unreadable.
                let parsed = self.read_object_at(offset)?;
                if parsed.object_ref != stream_ref {
                    return Ok(false);
                }
                let mut object = parsed.object;
                let mut endstream_scan_authoritative = parsed.recovered_stream_eol.is_some();
                if let Some(isl) = parsed.indirect_length {
                    let used_endstream_scan = self.apply_indirect_stream_length(
                        stream_ref,
                        &mut object,
                        isl,
                        &parsed.bytes,
                        offset,
                    )?;
                    if parsed.recovered_stream_eol.is_some() {
                        endstream_scan_authoritative = used_endstream_scan;
                    }
                }
                // `apply_indirect_stream_length` already restored the lazy entry,
                // so a decryption error here leaves it `Unresolved`.
                let recovered_eol = endstream_scan_authoritative
                    .then_some(parsed.recovered_stream_eol)
                    .flatten()
                    .map(crate::parser::RecoveredStreamEol::as_bytes);
                let (object, stream_payload_transformed) =
                    self.decrypt_resolved_object(stream_ref, object, recovered_eol)?;
                self.cache.set_resolved(stream_ref, object.clone());
                if stream_payload_transformed {
                    self.transformed_stream_refs.insert(stream_ref);
                } else {
                    self.transformed_stream_refs.remove(&stream_ref);
                }
                self.record_file_object_warnings(
                    stream_ref,
                    offset,
                    parsed.empty_offset,
                    parsed.expected_endobj_offset,
                );
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
        let (object, _stream_payload_transformed) =
            self.decrypt_resolved_object(object_ref, object, None)?;
        self.compressed_member_parents
            .insert(object_ref, (parent_ref, parent_index));
        self.cache.set_resolved(object_ref, object);
        Ok(true)
    }

    fn decrypt_resolved_object(
        &self,
        object_ref: ObjectRef,
        mut object: Object,
        recovered_stream_eol: Option<&[u8]>,
    ) -> Result<(Object, bool)> {
        let Some(encryption) = &self.encryption else {
            return Ok((object, false));
        };
        if Some(object_ref) == encryption.encrypt_ref {
            return Ok((object, false));
        }

        decrypt_object_strings(
            object_ref,
            &mut object,
            encryption.string_mode,
            &encryption.file_key,
            encryption.encrypt_ref,
        )?;
        let mut stream_payload_transformed = false;
        if let Object::Stream(stream) = &mut object {
            if !encryption.encrypt_metadata && is_metadata_stream(&stream.dict) {
                return Ok((object, false));
            } else if stream_has_explicit_crypt_filter(&stream.dict) {
                apply_explicit_crypt_filters(object_ref, stream, encryption, recovered_stream_eol)?;
                stream_payload_transformed = true;
            } else {
                decrypt_stream_bytes(
                    object_ref,
                    &mut stream.data,
                    encryption.stream_mode,
                    &encryption.file_key,
                )?;
                stream_payload_transformed = encryption.stream_mode != EncryptionMode::Identity;
            }
        }
        Ok((object, stream_payload_transformed))
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

// Resolve `value` one level: follow an `Object::Reference` through `pdf`,
// or return a non-reference value unchanged.
fn resolve_object_value<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> Option<Object> {
    match value {
        Object::Reference(reference) => pdf.resolve(reference).ok(),
        other => Some(other),
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
    recovered_stream_eol: Option<&[u8]>,
) -> Result<()> {
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

    if matches!(filter, Object::Name(ref name) if name.as_slice() == b"Crypt") {
        let mode = explicit_crypt_mode(encryption, decode_params.as_ref())?;
        if mode != EncryptionMode::Identity {
            if let Some(eol) = recovered_stream_eol {
                stream.data.truncate(stream.data.len() - eol.len());
            }
            decrypt_stream_bytes(object_ref, &mut stream.data, mode, &encryption.file_key)?;
        }
        stream.dict.remove("Filter");
        stream.dict.remove("DecodeParms");
        return Ok(());
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
        let mode = explicit_crypt_mode(encryption, crypt_params.as_ref())?;

        if mode != EncryptionMode::Identity {
            let prefix_dict = filter_prefix_dict(&filters, decode_params.as_ref(), crypt_index);
            let mut encoded = stream.data.clone();
            if crypt_index == 0 {
                if let Some(eol) = framing.take() {
                    encoded.truncate(encoded.len() - eol.len());
                }
            }
            let mut decoded_prefix = crate::filters::decode_stream_data(&prefix_dict, &encoded)?;
            decrypt_stream_bytes(object_ref, &mut decoded_prefix, mode, &encryption.file_key)?;
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
    Ok(())
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

    parse_qpdf_file_object(&stream_data[start..])
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
    use crate::Stream;

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

    fn explicit_rc4_encryption_state() -> EncryptionState {
        EncryptionState {
            file_key: vec![0x11, 0x22, 0x33, 0x44, 0x55],
            stream_mode: EncryptionMode::Identity,
            string_mode: EncryptionMode::Identity,
            crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), EncryptionMode::Rc4)]),
            encrypt_metadata: true,
            encrypt_ref: None,
            weak_crypto: true,
            permissions: Permissions::new(-4),
            user_password_matched: true,
            owner_password_matched: false,
        }
    }

    fn rc4_ciphertext(
        object_ref: ObjectRef,
        plaintext: &[u8],
        encryption: &EncryptionState,
    ) -> Vec<u8> {
        let mut ciphertext = plaintext.to_vec();
        decrypt_stream_bytes(
            object_ref,
            &mut ciphertext,
            EncryptionMode::Rc4,
            &encryption.file_key,
        )
        .expect("RC4 encryption");
        ciphertext
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
        let encryption = explicit_rc4_encryption_state();
        let mut stream = explicit_identity_crypt_chain(17);
        let original = stream.clone();

        let err = apply_explicit_crypt_filters(
            ObjectRef::new(4, 0),
            &mut stream,
            &encryption,
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
        let encryption = explicit_rc4_encryption_state();
        let mut stream = explicit_identity_crypt_chain(16);

        apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &encryption, None)
            .expect("16 explicit Crypt filters are within the shared decode cap");

        assert_eq!(stream.data, b"identity");
        assert_eq!(stream.dict.get("Filter"), None);
        assert_eq!(stream.dict.get("DecodeParms"), None);
    }

    #[test]
    fn explicit_crypt_within_limit_still_rejects_malformed_name_param() {
        let encryption = explicit_rc4_encryption_state();
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

        let err =
            apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &encryption, None)
                .expect_err("malformed /Crypt /Name remains an error");

        assert!(
            matches!(
                err,
                Error::Encrypted(EncryptedError::Malformed { ref reason })
                    if reason == "/Crypt /DecodeParms /Name is not a name"
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn explicit_named_crypt_removes_recovered_framing_before_rc4() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = explicit_rc4_encryption_state();
        let plaintext = b"named explicit crypt";
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
        dict.insert("DecodeParms", crypt_params(b"StdCF"));
        let mut stream = Stream::new(dict, rc4_ciphertext(object_ref, plaintext, &encryption));

        apply_explicit_crypt_filters(object_ref, &mut stream, &encryption, Some(b"\n"))
            .expect("remove named Crypt");

        assert_eq!(stream.data, plaintext);
        assert_eq!(stream.dict.get("Filter"), None);
        assert_eq!(stream.dict.get("DecodeParms"), None);
    }

    #[test]
    fn explicit_crypt_first_removes_recovered_framing_before_rc4() {
        let object_ref = ObjectRef::new(4, 0);
        let encryption = explicit_rc4_encryption_state();
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

        apply_explicit_crypt_filters(object_ref, &mut stream, &encryption, Some(b"\n"))
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
        let encryption = explicit_rc4_encryption_state();
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

        apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &encryption, None)
            .expect("remove singleton Crypt");

        assert_eq!(stream.data, b"identity");
        assert_eq!(stream.dict.get("Filter"), None);
        assert_eq!(stream.dict.get("DecodeParms"), None);
    }

    #[test]
    fn explicit_crypt_array_without_decode_params_keeps_remaining_filter() {
        let encryption = explicit_rc4_encryption_state();
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

        apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &encryption, None)
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
        let encryption = explicit_rc4_encryption_state();
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

        apply_explicit_crypt_filters(ObjectRef::new(4, 0), &mut stream, &encryption, None)
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
            (b"2 0 R".as_slice(), Some(b"2 0 obj\n<<".as_slice()), false),
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
    fn qpdf_object_read_uses_bounded_fallback_and_preserves_strict_errors() {
        let stream_body =
            b"1 0 obj\n<< /Length 2 0 R >>\nstream\n9 0 obj\nnull\nendobj\nendstream\nendobj\n";
        let length_body = b"2 0 obj\n19\nendobj\n";
        let bytes = classic_pdf_with_bodies(&[stream_body, length_body], ObjectRef::new(1, 0));
        let stream_offset = bytes
            .windows(b"1 0 obj".len())
            .position(|window| window == b"1 0 obj")
            .unwrap() as u64;
        let embedded_offset = bytes
            .windows(b"9 0 obj".len())
            .position(|window| window == b"9 0 obj")
            .unwrap() as u64;
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open fallback fixture");
        pdf.sorted_object_offsets.push(embedded_offset);
        pdf.sorted_object_offsets.sort_unstable();

        let parsed = pdf
            .read_object_at(stream_offset)
            .expect("full read fallback must recover stream");
        assert_eq!(parsed.object_ref, ObjectRef::new(1, 0));
        assert!(parsed.object.as_stream().is_some());

        let malformed = classic_pdf_with_bodies(
            &[b"1 0 obj\n<< /Type /Catalog >>\nendobj\n", b"2 0 obj\n<<"],
            ObjectRef::new(1, 0),
        );
        let malformed_offset = malformed
            .windows(b"2 0 obj".len())
            .position(|window| window == b"2 0 obj")
            .unwrap() as u64;
        let mut pdf = Pdf::open_mem_owned(malformed).expect("open malformed lazy object");
        assert!(pdf.read_object_at(malformed_offset).is_err());
        pdf.sorted_object_offsets.push(malformed_offset + 5);
        pdf.sorted_object_offsets.sort_unstable();
        assert!(pdf.read_object_at(malformed_offset).is_err());
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
        assert!(pdf.resolve_qpdf_json_object(ObjectRef::new(1, 0)).is_err());
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
        let diagnostics = normal_first.repair_diagnostics().entries();
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
        let diagnostics = pdf.repair_diagnostics().entries();
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
            parse_object_stream_entry(&stream, 0).unwrap(),
            Object::Integer(6)
        );
        assert_eq!(
            parse_object_stream_entry(&stream, 1).unwrap(),
            Object::Array(vec![Object::Reference(ObjectRef::new(6, 0))])
        );
        let dictionary = parse_object_stream_entry(&stream, 2)
            .unwrap()
            .into_dict()
            .expect("dictionary member");
        assert_eq!(dictionary.get_ref("V"), Some(ObjectRef::new(6, 0)));
    }

    /// Build a minimal PDF whose object `(1, 0)` is a linearization
    /// parameter dictionary with `/Linearized` written as `.9` — a
    /// non-canonical literal that the parser stores as
    /// [`Object::RealLiteral`]. Exercises `linearized_hint_ref`'s
    /// `RealLiteral` arm.
    fn linearized_like_pdf_bytes_real_literal() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Linearized .9 >>\nendobj\n");
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

    /// `linearized_hint_ref` recognizes a `/Linearized` value stored as
    /// [`Object::RealLiteral`] (non-canonical source literal like `.9`) and
    /// returns `Some((1, 0))`. Regression guard for the
    /// `Object::Real | Object::RealLiteral` arm.
    #[test]
    fn linearized_hint_ref_accepts_real_literal_value() {
        let bytes = linearized_like_pdf_bytes_real_literal();
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open should succeed");
        let hint = pdf.linearized_hint_ref().expect("must succeed");
        assert_eq!(hint, Some(ObjectRef::new(1, 0)));
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
}
